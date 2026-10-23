//! The wgpu renderer. Linked into the browser process only (spec 0001, render boundary).
//!
//! It draws display lists (spec 0004, child 0005, Renderer pipelines) with
//! three instanced pipelines: rects and borders (corners rounded by a
//! signed distance, dashed and dotted sides expanded into pieces), glyphs
//! from the glyph atlas, and images, which also composite the opacity
//! groups drawn into offscreen layers. Items are placed by the list's
//! spatial tree (so scroll frames move their content) and cut by its
//! clips. Colors blend in gamma space, as in every browser: the target is
//! always viewed in a non sRGB format. The list must already have passed
//! `display_list::validate`; anything it still gets wrong is skipped,
//! never a panic.
//!
//! The browser app also draws its chrome: the chrome document's own list
//! over the whole window, with the page composited into the `#page-slot`
//! rect under a rounded clip (spec 0003, milestone 3). A headless renderer
//! draws into an offscreen texture instead of a window, for pixel tests
//! (AC-27).

#![forbid(unsafe_code)]

mod atlas;
mod frame;
mod textures;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc;

use display_list::{ColorF, DisplayList, FontKey, ImageFormat, ImageKey, PageId, RectF};
use skrifa::MetadataProvider;
use tracing::{debug, info};

use crate::atlas::{FontFile, GlyphAtlas};
use crate::frame::{Batch, Frame, GLYPH_FLOATS, IMAGE_FLOATS, SHAPE_FLOATS, Scene, Texture};
use crate::textures::Textures;

/// The most variation axis settings `add_font` reads for one face; fonts
/// have a handful of axes.
pub const MAX_VARIATIONS: usize = 64;

/// Bytes between passes in the uniform buffer: each pass reads its target
/// at its own dynamic offset, which wgpu aligns to at most 256.
const UNIFORM_STRIDE: u64 = 256;
/// Bytes of one pass's target: its size, then its origin.
const UNIFORM_SIZE: u64 = 4 * size_of::<f32>() as u64;
/// The format headless frames render in, and `read_pixels` returns.
const HEADLESS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The browser chrome around the page (spec 0003, milestone 3): the
/// chrome document's list, drawn over the whole target, and where the page
/// goes in it.
#[derive(Debug, Clone, Copy)]
pub struct Chrome<'a> {
    /// The chrome document's list, in chrome px.
    pub list: &'a DisplayList,
    /// Device pixels per chrome px: the window's scale factor times the
    /// chrome zoom.
    pub scale: f32,
    /// The page card, or `None` when the chrome shows no page (the
    /// gallery).
    pub slot: Option<PageSlot>,
}

/// Where the page is composited: `#page-slot`'s box and its corner radius,
/// in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageSlot {
    /// The page card, in physical pixels.
    pub rect: RectF,
    /// Its corner radius, in physical pixels.
    pub radius: f32,
}

/// The overlay scroll indicator (spec 0004, child 0005, AC-30): a rounded
/// bar over the page, in page CSS px, its corners half its width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollIndicator {
    /// Where, in page CSS px.
    pub rect: RectF,
    /// Its color, faded as it hides.
    pub color: ColorF,
}

/// Errors raised while setting up or drawing to the GPU.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    /// The window could not give wgpu a surface to draw on.
    #[error("could not create a drawing surface for the window")]
    CreateSurface(#[from] wgpu::CreateSurfaceError),
    /// No GPU adapter is compatible with the window's surface.
    #[error("no GPU adapter can draw to this window")]
    RequestAdapter(#[from] wgpu::RequestAdapterError),
    /// The adapter was found but would not open a device.
    #[error("the GPU adapter refused to open a device")]
    RequestDevice(#[from] wgpu::RequestDeviceError),
    /// The surface has no default configuration for the chosen adapter.
    #[error("the surface does not support this adapter")]
    UnsupportedSurface,
    /// The surface is gone and must be recreated.
    #[error("the drawing surface was lost")]
    SurfaceLost,
    /// A headless frame could not be copied back from the GPU, or the
    /// renderer draws to a window.
    #[error("the frame could not be read back")]
    Readback,
}

/// Where frames go.
enum Target {
    /// A window's surface, presented with vsync.
    Window {
        /// Kept to recreate a lost surface.
        instance: wgpu::Instance,
        surface: wgpu::Surface<'static>,
        config: wgpu::SurfaceConfiguration,
    },
    /// An offscreen texture `read_pixels` copies back.
    Offscreen {
        texture: wgpu::Texture,
        width: u32,
        height: u32,
    },
}

/// The three pipelines (spec 0004, child 0005): rects and borders share
/// one, as both are colored quads cut to a rounded box, so they batch
/// together.
struct Pipelines {
    shape: wgpu::RenderPipeline,
    glyph: wgpu::RenderPipeline,
    image: wgpu::RenderPipeline,
}

/// The per pass uniform buffer and its bind group.
struct Uniforms {
    buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    /// How many passes it has room for.
    passes: usize,
}

/// Owns the GPU device and what frames draw into.
pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    target: Target,
    /// The format every pass renders in: the target's, without sRGB.
    format: wgpu::TextureFormat,
    pipelines: Pipelines,
    uniform_layout: wgpu::BindGroupLayout,
    uniforms: Option<Uniforms>,
    /// Instance buffers, reused across frames and grown when a frame needs
    /// more room: shapes, glyphs, images.
    instances: [Option<wgpu::Buffer>; 3],
    atlas: GlyphAtlas,
    /// Fonts by page and key, from `AddFont`.
    fonts: HashMap<(PageId, FontKey), FontFile>,
    textures: Textures,
}

impl Renderer {
    /// Opens a GPU device that can draw to `target`, sized `width` × `height` physical pixels.
    pub fn new(
        target: impl Into<wgpu::SurfaceTarget<'static>>,
        width: u32,
        height: u32,
    ) -> Result<Self, RenderError> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let surface = instance.create_surface(target)?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            ..Default::default()
        }))?;
        let (device, queue) = open_device(&adapter)?;

        let mut config = surface
            .get_default_config(&adapter, width.max(1), height.max(1))
            .ok_or(RenderError::UnsupportedSurface)?;
        let format = config.format.remove_srgb_suffix();
        if format != config.format {
            config.view_formats.push(format);
        }
        // Vsync paces the render thread's frame clock (AC-28).
        config.present_mode = wgpu::PresentMode::Fifo;
        surface.configure(&device, &config);
        let target = Target::Window {
            instance,
            surface,
            config,
        };
        Ok(Self::with_target(device, queue, target, format))
    }

    /// Opens a GPU device that draws `width` × `height` pixel frames into an
    /// offscreen texture, for [`Renderer::read_pixels`] (AC-27).
    pub fn new_headless(width: u32, height: u32) -> Result<Self, RenderError> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))?;
        let (device, queue) = open_device(&adapter)?;
        let (width, height) = (width.max(1), height.max(1));
        let texture = offscreen_texture(&device, width, height);
        let target = Target::Offscreen {
            texture,
            width,
            height,
        };
        Ok(Self::with_target(device, queue, target, HEADLESS_FORMAT))
    }

    fn with_target(
        device: wgpu::Device,
        queue: wgpu::Queue,
        target: Target,
        format: wgpu::TextureFormat,
    ) -> Self {
        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pass target layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(UNIFORM_SIZE),
                },
                count: None,
            }],
        });
        let atlas_layout = atlas_layout(&device);
        let textures = Textures::new(&device);
        let pipelines = Pipelines {
            shape: pipeline(
                &device,
                format,
                "shape",
                concat!(include_str!("common.wgsl"), include_str!("rect.wgsl")),
                &[Some(&uniform_layout)],
                &wgpu::vertex_attr_array![
                    0 => Float32x4,
                    1 => Float32x4,
                    2 => Float32x4,
                    3 => Float32x4,
                    4 => Float32x4,
                    5 => Float32x4,
                    6 => Float32x2,
                ],
                SHAPE_FLOATS,
                wgpu::BlendState::ALPHA_BLENDING,
            ),
            glyph: pipeline(
                &device,
                format,
                "glyph",
                concat!(include_str!("common.wgsl"), include_str!("glyph.wgsl")),
                &[Some(&uniform_layout), Some(&atlas_layout)],
                &wgpu::vertex_attr_array![
                    0 => Float32x4,
                    1 => Float32x4,
                    2 => Float32x4,
                    3 => Float32x4,
                    4 => Float32,
                ],
                GLYPH_FLOATS,
                wgpu::BlendState::ALPHA_BLENDING,
            ),
            image: pipeline(
                &device,
                format,
                "image",
                concat!(include_str!("common.wgsl"), include_str!("image.wgsl")),
                &[Some(&uniform_layout), Some(textures.layout())],
                &wgpu::vertex_attr_array![
                    0 => Float32x4,
                    1 => Float32x4,
                    2 => Float32x4,
                    3 => Float32x2,
                ],
                IMAGE_FLOATS,
                wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
            ),
        };
        let atlas = GlyphAtlas::new(&device, atlas_layout);
        Self {
            device,
            queue,
            target,
            format,
            pipelines,
            uniform_layout,
            uniforms: None,
            instances: [None, None, None],
            atlas,
            fonts: HashMap::new(),
            textures,
        }
    }

    /// Keeps a font a page registered (`AddFont`): face `index` of the font
    /// file `data`, at the variation axis settings `variations` (tag, user
    /// space value). Glyphs are rasterized from it as lists need them. A key
    /// registered again (a relaunched content process counts from the start)
    /// drops the glyphs drawn from its old data.
    ///
    /// The settings come from the content process, so they are untrusted:
    /// at most [`MAX_VARIATIONS`] are read, non finite values are skipped,
    /// and the font's own axis ranges clamp the rest.
    pub fn add_font(
        &mut self,
        page: PageId,
        key: FontKey,
        data: &[u8],
        index: u32,
        variations: &[(u32, f32)],
    ) {
        self.atlas.forget_font(page, key);
        let data: Arc<[u8]> = data.into();
        let coords = skrifa::FontRef::from_index(&data, index)
            .map(|font| {
                let settings = variations
                    .iter()
                    .take(MAX_VARIATIONS)
                    .filter(|(_, value)| value.is_finite())
                    .map(|&(tag, value)| (skrifa::Tag::from_be_bytes(tag.to_be_bytes()), value));
                font.axes().location(settings).coords().to_vec()
            })
            .unwrap_or_default();
        self.fonts.insert(
            (page, key),
            FontFile {
                data,
                index,
                coords,
            },
        );
    }

    /// Drops a font the page freed (`DeleteFont`).
    pub fn delete_font(&mut self, page: PageId, key: FontKey) {
        self.atlas.forget_font(page, key);
        self.fonts.remove(&(page, key));
    }

    /// Uploads an image a page registered (`AddImage`): `size` pixels laid
    /// out as `format`, rows top to bottom. Pixels that do not match the
    /// size, or a size the GPU cannot hold, are refused and logged.
    pub fn add_image(
        &mut self,
        page: PageId,
        key: ImageKey,
        size: (u32, u32),
        format: ImageFormat,
        pixels: &[u8],
    ) {
        self.textures
            .add_image(&self.device, &self.queue, (page, key), size, format, pixels);
    }

    /// Frees an image the page freed (`DeleteImage`).
    pub fn delete_image(&mut self, page: PageId, key: ImageKey) {
        self.textures.delete_image((page, key));
    }

    /// Matches the target to a new size. A zero size (a minimized window)
    /// is ignored.
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        match &mut self.target {
            Target::Window {
                surface, config, ..
            } => {
                config.width = width;
                config.height = height;
                surface.configure(&self.device, config);
            }
            Target::Offscreen {
                texture,
                width: w,
                height: h,
            } => {
                *texture = offscreen_texture(&self.device, width, height);
                (*w, *h) = (width, height);
            }
        }
    }

    /// Makes a new surface for `target` after the old one was lost
    /// (spec 0004, Failure handling: the render thread tries this once).
    pub fn recreate_surface(
        &mut self,
        target: impl Into<wgpu::SurfaceTarget<'static>>,
    ) -> Result<(), RenderError> {
        let Target::Window {
            instance,
            surface,
            config,
        } = &mut self.target
        else {
            return Ok(());
        };
        *surface = instance.create_surface(target)?;
        surface.configure(&self.device, config);
        Ok(())
    }

    /// The target's size in pixels.
    pub fn size(&self) -> (u32, u32) {
        match &self.target {
            Target::Window { config, .. } => (config.width, config.height),
            Target::Offscreen { width, height, .. } => (*width, *height),
        }
    }

    /// Draws one frame: `list` (or a blank page without one) at `scale`
    /// device pixels per CSS px, and the scroll `indicator` over it. With
    /// `chrome`, its list draws first and the page goes into its slot;
    /// without, the page fills the target. Frames a window's surface cannot
    /// take right now are skipped, not errors.
    pub fn render(
        &mut self,
        list: Option<&DisplayList>,
        scale: f32,
        chrome: Option<&Chrome<'_>>,
        indicator: Option<&ScrollIndicator>,
    ) -> Result<(), RenderError> {
        let (width, height) = self.size();
        let scene = Scene {
            list,
            scale,
            chrome,
            indicator,
            width,
            height,
        };
        // Every glyph this frame draws goes into the atlas first, so none
        // goes missing when the atlas has to be rebuilt partway (AC-26).
        let keys = frame::glyph_keys(&scene);
        self.atlas
            .ensure(&self.device, &self.queue, &self.fonts, &keys);

        match &self.target {
            Target::Offscreen { texture, .. } => {
                let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                self.draw_scene(&scene, &view);
                Ok(())
            }
            Target::Window {
                surface, config, ..
            } => {
                let (frame, suboptimal) = match surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(frame) => (frame, false),
                    // Draw this frame, then reconfigure so the next one
                    // matches the surface.
                    wgpu::CurrentSurfaceTexture::Suboptimal(frame) => (frame, true),
                    wgpu::CurrentSurfaceTexture::Outdated => {
                        surface.configure(&self.device, config);
                        return Ok(());
                    }
                    wgpu::CurrentSurfaceTexture::Timeout
                    | wgpu::CurrentSurfaceTexture::Occluded
                    | wgpu::CurrentSurfaceTexture::Validation => {
                        debug!("skipping a frame the surface could not provide");
                        return Ok(());
                    }
                    wgpu::CurrentSurfaceTexture::Lost => return Err(RenderError::SurfaceLost),
                };
                let view = frame.texture.create_view(&wgpu::TextureViewDescriptor {
                    format: Some(self.format),
                    ..Default::default()
                });
                self.draw_scene(&scene, &view);
                self.queue.present(frame);
                if suboptimal
                    && let Target::Window {
                        surface, config, ..
                    } = &self.target
                {
                    surface.configure(&self.device, config);
                }
                Ok(())
            }
        }
    }

    /// Draws `scene` into `view`, in parts when its glyphs do not all fit
    /// the atlas at once: each part is drawn, then the atlas is emptied for
    /// the next (spec 0004, child 0005, Glyph atlas).
    fn draw_scene(&mut self, scene: &Scene<'_>, view: &wgpu::TextureView) {
        let atlas_size = self.atlas.size();
        let mut start = 0;
        let mut first = true;
        loop {
            let built = {
                let (atlas, queue, fonts) = (&mut self.atlas, &self.queue, &self.fonts);
                frame::build(scene, start, atlas_size, |key| {
                    atlas.place(queue, fonts, key)
                })
            };
            self.draw_frame(&built.frame, view, first);
            match built.resume {
                Some(next) if next > start => {
                    self.atlas.reset();
                    start = next;
                    first = false;
                }
                _ => return,
            }
        }
    }

    /// Records and submits every pass of `frame`: each opacity layer into
    /// its own texture, then the root pass into `view`, cleared first when
    /// `clear` (a later part of a split frame draws over the earlier one).
    fn draw_frame(&mut self, frame: &Frame, view: &wgpu::TextureView, clear: bool) {
        let [shapes, glyphs, images] = &mut self.instances;
        let shapes = upload(&self.device, &self.queue, shapes, &frame.shapes, "shapes");
        let glyphs = upload(&self.device, &self.queue, glyphs, &frame.glyphs, "glyphs");
        let images = upload(&self.device, &self.queue, images, &frame.images, "images");
        let uniforms = self.write_uniforms(frame);
        let layer_count = frame.passes.len().saturating_sub(1);
        let layers = self.textures.take_layers(
            &self.device,
            self.format,
            frame.passes[..layer_count]
                .iter()
                .map(|pass| (pass.bounds[2], pass.bounds[3])),
        );

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        for (index, pass) in frame.passes.iter().enumerate() {
            let (target, load) = match layers.get(index) {
                Some(layer) => (&layer.view, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)),
                None if clear => (view, wgpu::LoadOp::Clear(wgpu_color(frame.clear))),
                None => (view, wgpu::LoadOp::Load),
            };
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            let offset = (index as u64 * UNIFORM_STRIDE) as u32;
            render_pass.set_bind_group(0, &uniforms, &[offset]);
            for draw in &pass.draws {
                match draw.batch {
                    Batch::Shapes => {
                        let Some(buffer) = &shapes else { continue };
                        render_pass.set_pipeline(&self.pipelines.shape);
                        render_pass.set_vertex_buffer(0, buffer.slice(..));
                    }
                    Batch::Glyphs => {
                        let Some(buffer) = &glyphs else { continue };
                        render_pass.set_pipeline(&self.pipelines.glyph);
                        render_pass.set_bind_group(1, &self.atlas.bind_group, &[]);
                        render_pass.set_vertex_buffer(0, buffer.slice(..));
                    }
                    Batch::Images(texture) => {
                        let bind_group = match texture {
                            Texture::Image(page, key) => self.textures.image((page, key)),
                            Texture::Layer(pass) => layers.get(pass).map(|l| &l.bind_group),
                        };
                        let (Some(buffer), Some(bind_group)) = (&images, bind_group) else {
                            continue;
                        };
                        render_pass.set_pipeline(&self.pipelines.image);
                        render_pass.set_bind_group(1, bind_group, &[]);
                        render_pass.set_vertex_buffer(0, buffer.slice(..));
                    }
                }
                render_pass.draw(0..6, draw.instances.clone());
            }
        }
        self.queue.submit([encoder.finish()]);
        self.textures.recycle(layers);
    }

    /// Writes each pass's target (size and origin in the root target) at
    /// its own offset, growing the buffer when needed, and returns the bind
    /// group reading it.
    fn write_uniforms(&mut self, frame: &Frame) -> wgpu::BindGroup {
        let passes = frame.passes.len().max(1);
        let uniforms = match self.uniforms.take() {
            Some(uniforms) if uniforms.passes >= passes => uniforms,
            _ => {
                let room = passes.next_power_of_two();
                let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("pass targets"),
                    size: room as u64 * UNIFORM_STRIDE,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("pass targets"),
                    layout: &self.uniform_layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &buffer,
                            offset: 0,
                            size: wgpu::BufferSize::new(UNIFORM_SIZE),
                        }),
                    }],
                });
                Uniforms {
                    buffer,
                    bind_group,
                    passes: room,
                }
            }
        };
        let mut bytes = vec![0u8; passes * UNIFORM_STRIDE as usize];
        for (index, pass) in frame.passes.iter().enumerate() {
            let [x, y, width, height] = pass.bounds.map(|v| v as f32);
            let start = index * UNIFORM_STRIDE as usize;
            for (i, value) in [width.max(1.0), height.max(1.0), x, y].iter().enumerate() {
                bytes[start + i * 4..start + i * 4 + 4].copy_from_slice(&value.to_ne_bytes());
            }
        }
        self.queue.write_buffer(&uniforms.buffer, 0, &bytes);
        let bind_group = uniforms.bind_group.clone();
        self.uniforms = Some(uniforms);
        bind_group
    }

    /// Copies the last headless frame back as RGBA rows, top to bottom
    /// (AC-27). Fails on a window renderer.
    pub fn read_pixels(&self) -> Result<Vec<u8>, RenderError> {
        let Target::Offscreen {
            texture,
            width,
            height,
        } = &self.target
        else {
            return Err(RenderError::Readback);
        };
        let row = width * 4;
        let padded =
            row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: u64::from(padded) * u64::from(*height),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("readback"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(*height),
                },
            },
            wgpu::Extent3d {
                width: *width,
                height: *height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);
        let (sender, receiver) = mpsc::channel();
        buffer.map_async(wgpu::MapMode::Read, .., move |result| {
            let _ = sender.send(result);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|_| RenderError::Readback)?;
        match receiver.recv() {
            Ok(Ok(())) => {}
            _ => return Err(RenderError::Readback),
        }
        let pixels = {
            let mapped = buffer
                .get_mapped_range(..)
                .map_err(|_| RenderError::Readback)?;
            let (row, padded) = (row as usize, padded as usize);
            mapped
                .chunks(padded)
                .flat_map(|line| &line[..row])
                .copied()
                .collect()
        };
        buffer.unmap();
        Ok(pixels)
    }
}

fn open_device(adapter: &wgpu::Adapter) -> Result<(wgpu::Device, wgpu::Queue), RenderError> {
    info!(adapter = ?adapter.get_info(), "gpu adapter selected");
    Ok(pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("render device"),
            ..Default::default()
        },
    ))?)
}

fn offscreen_texture(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("headless frame"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: HEADLESS_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

/// Returns a buffer holding `data`, reusing `cached` when it is big
/// enough. `None` when there is nothing to draw.
fn upload(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    cached: &mut Option<wgpu::Buffer>,
    data: &[f32],
    label: &'static str,
) -> Option<wgpu::Buffer> {
    if data.is_empty() {
        return None;
    }
    let bytes: Vec<u8> = data.iter().flat_map(|f| f.to_ne_bytes()).collect();
    let size = bytes.len() as u64;
    let buffer = match cached.take() {
        Some(buffer) if buffer.size() >= size => buffer,
        _ => device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: size.next_power_of_two().max(1024),
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }),
    };
    queue.write_buffer(&buffer, 0, &bytes);
    *cached = Some(buffer.clone());
    Some(buffer)
}

/// One instanced pipeline: six vertices per instance, `floats` floats per
/// instance read as `attributes`.
#[allow(
    clippy::too_many_arguments,
    reason = "the pipelines differ in every one of them"
)]
fn pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    label: &'static str,
    source: &'static str,
    groups: &[Option<&wgpu::BindGroupLayout>],
    attributes: &[wgpu::VertexAttribute],
    floats: usize,
    blend: wgpu::BlendState,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: groups,
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: (floats * size_of::<f32>()) as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes,
            })],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(blend),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

/// The glyph atlas binding: the coverage texture and its sampler.
fn atlas_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("glyph atlas layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

/// A display list color as a clear color. The target is viewed without
/// sRGB, so sRGB values go in as they are.
fn wgpu_color(color: ColorF) -> wgpu::Color {
    wgpu::Color {
        r: f64::from(color.r),
        g: f64::from(color.g),
        b: f64::from(color.b),
        a: f64::from(color.a),
    }
}
