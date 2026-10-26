//! Textures the image pipeline samples (spec 0004, child 0005): the images
//! pages register with `AddImage`, and the offscreen layers opacity groups
//! draw into. Both bind as a texture and a linear sampler.

use std::collections::HashMap;

use display_list::{ImageFormat, ImageKey, MAX_IMAGE_SIDE, PageId};
use tracing::warn;

/// A texture and the bind group that samples it.
struct Sampled {
    /// Kept alive while the bind group is used.
    _texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    /// Width and height, for the memory report (spec 0032, AC-10).
    size: (u32, u32),
}

/// An offscreen layer: drawn into by one pass, then sampled by its parent.
pub(crate) struct LayerTexture {
    size: (u32, u32),
    format: wgpu::TextureFormat,
    pub(crate) view: wgpu::TextureView,
    pub(crate) bind_group: wgpu::BindGroup,
}

/// Every texture the image pipeline can sample.
pub(crate) struct Textures {
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    images: HashMap<(PageId, ImageKey), Sampled>,
    /// The last frame's layers, reused by size.
    spare_layers: Vec<LayerTexture>,
}

impl Textures {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("image layout"),
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
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("image sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            layout,
            sampler,
            images: HashMap::new(),
            spare_layers: Vec::new(),
        }
    }

    /// The bind group layout every image draw uses.
    pub(crate) fn layout(&self) -> &wgpu::BindGroupLayout {
        &self.layout
    }

    fn bind(&self, device: &wgpu::Device, view: &wgpu::TextureView) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("image"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        })
    }

    /// Uploads an image a page registered. The pixels come from the content
    /// process, so an image whose size is zero, too big for the GPU or
    /// [`MAX_IMAGE_SIDE`], or does not match its data is refused and logged;
    /// lists naming it then draw nothing for it.
    pub(crate) fn add_image(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        image: (PageId, ImageKey),
        size: (u32, u32),
        format: ImageFormat,
        pixels: &[u8],
    ) {
        let (width, height) = size;
        let max_side = device.limits().max_texture_dimension_2d.min(MAX_IMAGE_SIDE);
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(format.bytes_per_pixel()));
        if width == 0
            || height == 0
            || width > max_side
            || height > max_side
            || expected != Some(pixels.len())
        {
            warn!(width, height, bytes = pixels.len(), "image refused");
            return;
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("image"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // Premultiplied sRGB values, blended in gamma space like the
            // rest of the page.
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * format.bytes_per_pixel() as u32),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.bind(device, &view);
        self.images.insert(
            image,
            Sampled {
                _texture: texture,
                bind_group,
                size,
            },
        );
    }

    /// Frees an image the page freed.
    pub(crate) fn delete_image(&mut self, image: (PageId, ImageKey)) {
        self.images.remove(&image);
    }

    /// The image textures' bytes, as width × height × 4 (spec 0032, AC-10).
    pub(crate) fn images_bytes(&self) -> u64 {
        self.images
            .values()
            .map(|sampled| u64::from(sampled.size.0) * u64::from(sampled.size.1) * 4)
            .sum()
    }

    /// The spare layer textures' bytes, as width × height × 4 (spec 0032,
    /// AC-10).
    pub(crate) fn layers_bytes(&self) -> u64 {
        self.spare_layers
            .iter()
            .map(|layer| u64::from(layer.size.0) * u64::from(layer.size.1) * 4)
            .sum()
    }

    /// Frees the spare layer textures: a page's layers must not outlive it
    /// (spec 0032, AC-7).
    pub(crate) fn drop_spares(&mut self) {
        self.spare_layers.clear();
    }

    /// The bind group sampling a registered image.
    pub(crate) fn image(&self, image: (PageId, ImageKey)) -> Option<&wgpu::BindGroup> {
        self.images.get(&image).map(|sampled| &sampled.bind_group)
    }

    /// One layer texture per size in `sizes`, in order, reusing the last
    /// frame's where the size and format match.
    pub(crate) fn take_layers(
        &mut self,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        sizes: impl Iterator<Item = (u32, u32)>,
    ) -> Vec<LayerTexture> {
        sizes
            .map(|size| {
                let spare = self
                    .spare_layers
                    .iter()
                    .position(|layer| layer.size == size && layer.format == format);
                match spare {
                    Some(index) => self.spare_layers.swap_remove(index),
                    None => self.new_layer(device, format, size),
                }
            })
            .collect()
    }

    /// Keeps this frame's layers for the next one; older spares are freed.
    pub(crate) fn recycle(&mut self, layers: Vec<LayerTexture>) {
        self.spare_layers = layers;
    }

    fn new_layer(
        &self,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        size: (u32, u32),
    ) -> LayerTexture {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("opacity layer"),
            size: wgpu::Extent3d {
                width: size.0.max(1),
                height: size.1.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.bind(device, &view);
        LayerTexture {
            size,
            format,
            view,
            bind_group,
        }
    }
}
