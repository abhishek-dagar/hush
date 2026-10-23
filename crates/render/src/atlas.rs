//! The glyph atlas: rasterized glyphs packed into one coverage texture
//! (spec 0004, child 0005, Glyph atlas, AC-26).
//!
//! Glyphs are drawn from `skrifa` outlines by `zeno` into 8 bit coverage
//! masks at the window's device scale, unhinted, with the pen's subpixel
//! position in quarters of a pixel. A frame first makes sure every glyph
//! it draws is in the atlas; when the atlas is full it doubles (up to
//! 4096 by 4096) or starts over with only this frame's glyphs; when even
//! that is not enough the frame is drawn in parts, so no glyph
//! on screen is ever missing.

use std::collections::HashMap;
use std::sync::Arc;

use display_list::{FontKey, PageId};
use skrifa::instance::{LocationRef, NormalizedCoord, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{GlyphId, MetadataProvider};
use tracing::debug;
use zeno::{Command, Mask, Vector};

/// The atlas's first side length, in texels.
const START_SIZE: u32 = 1024;
/// The largest side length it grows to.
const MAX_SIZE: u32 = 4096;
/// Empty texels around each glyph, so linear neighbors never bleed in.
const PADDING: u32 = 1;

/// A font file the page registered with `AddFont`.
pub(crate) struct FontFile {
    pub(crate) data: Arc<[u8]>,
    pub(crate) index: u32,
    /// The variable face's position on its axes, normalized; empty for a
    /// static face or the default instance.
    pub(crate) coords: Vec<NormalizedCoord>,
}

/// One glyph rasterization: which glyph, at which device size and
/// subpixel offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct GlyphKey {
    pub(crate) page: PageId,
    pub(crate) font: FontKey,
    pub(crate) glyph: u32,
    /// The size in 1/64 device px.
    pub(crate) size: u32,
    /// The pen's horizontal fraction, in quarters of a pixel (0 to 3).
    pub(crate) bin: u8,
}

impl GlyphKey {
    /// The key for drawing `glyph` at `size` device px with its origin at
    /// device x position `x`.
    pub(crate) fn new(page: PageId, font: FontKey, glyph: u32, size: f32, x: f32) -> Self {
        Self {
            page,
            font,
            glyph,
            size: (size * 64.0).round().max(0.0) as u32,
            bin: ((x - x.floor()) * 4.0).floor().clamp(0.0, 3.0) as u8,
        }
    }
}

/// Where a glyph sits in the atlas, and where its mask goes relative to the
/// pen position (whole pixels, y down).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Placed {
    /// The texels, or `None` for a glyph with no ink (a space).
    pub(crate) texels: Option<[u32; 2]>,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) left: i32,
    pub(crate) top: i32,
}

/// Packs rectangles in rows ("shelves"), each as tall as its first item,
/// left to right.
#[derive(Debug)]
pub(crate) struct ShelfPacker {
    size: u32,
    /// Each shelf's top, height and next free x.
    shelves: Vec<(u32, u32, u32)>,
    next_top: u32,
}

impl ShelfPacker {
    pub(crate) fn new(size: u32) -> Self {
        Self {
            size,
            shelves: Vec::new(),
            next_top: 0,
        }
    }

    /// Finds room for a `width` by `height` rectangle plus padding, and
    /// returns its top left, or `None` when the atlas is full.
    pub(crate) fn allocate(&mut self, width: u32, height: u32) -> Option<[u32; 2]> {
        let (w, h) = (width + PADDING, height + PADDING);
        if w > self.size || h > self.size {
            return None;
        }
        // The first shelf tall enough (but not much taller) with room left.
        let fits = self
            .shelves
            .iter_mut()
            .find(|(_, shelf_h, x)| h <= *shelf_h && h * 2 > *shelf_h && x + w <= self.size);
        if let Some((top, _, x)) = fits {
            let at = [*x, *top];
            *x += w;
            return Some(at);
        }
        if self.next_top + h > self.size {
            return None;
        }
        let top = self.next_top;
        self.shelves.push((top, h, w));
        self.next_top += h;
        Some([0, top])
    }
}

/// The GPU texture and what is in it.
pub(crate) struct GlyphAtlas {
    size: u32,
    texture: wgpu::Texture,
    pub(crate) bind_group: wgpu::BindGroup,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    packer: ShelfPacker,
    glyphs: HashMap<GlyphKey, Placed>,
}

impl GlyphAtlas {
    pub(crate) fn new(device: &wgpu::Device, layout: wgpu::BindGroupLayout) -> Self {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("glyph sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let (texture, bind_group) = Self::texture(device, &layout, &sampler, START_SIZE);
        Self {
            size: START_SIZE,
            texture,
            bind_group,
            layout,
            sampler,
            packer: ShelfPacker::new(START_SIZE),
            glyphs: HashMap::new(),
        }
    }

    fn texture(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        size: u32,
    ) -> (wgpu::Texture, wgpu::BindGroup) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("glyph atlas"),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("glyph atlas"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });
        (texture, bind_group)
    }

    /// The side length, for texel to UV conversion.
    pub(crate) fn size(&self) -> u32 {
        self.size
    }

    pub(crate) fn get(&self, key: &GlyphKey) -> Option<Placed> {
        self.glyphs.get(key).copied()
    }

    /// Forgets every glyph of `font`, whose key now names other font data or
    /// none. Their space comes back when the atlas is next rebuilt.
    pub(crate) fn forget_font(&mut self, page: PageId, font: FontKey) {
        self.glyphs
            .retain(|key, _| key.page != page || key.font != font);
    }

    /// Makes sure every glyph in `keys` is in the atlas, and says whether
    /// they all fit. When one does not, the atlas doubles (or, at its
    /// largest, empties) and every glyph of this frame goes in again. When
    /// even the largest atlas cannot hold them all, it is left empty and
    /// the frame places its glyphs as it draws, in parts (AC-26).
    pub(crate) fn ensure(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        fonts: &HashMap<(PageId, FontKey), FontFile>,
        keys: &[GlyphKey],
    ) -> bool {
        // Whether the atlas holds only this frame's glyphs.
        let mut fresh = false;
        loop {
            let full = keys
                .iter()
                .any(|key| !self.glyphs.contains_key(key) && !self.add(queue, fonts, *key));
            if !full {
                return true;
            }
            if fresh && self.size == MAX_SIZE {
                debug!("this frame's glyphs need more than the largest atlas; drawing in parts");
                self.reset();
                return false;
            }
            let size = (self.size * 2).min(MAX_SIZE);
            debug!(size, "glyph atlas full, rebuilding");
            if size != self.size {
                let (texture, bind_group) =
                    Self::texture(device, &self.layout, &self.sampler, size);
                self.texture = texture;
                self.bind_group = bind_group;
                self.size = size;
            }
            self.reset();
            fresh = true;
        }
    }

    /// Empties the atlas, keeping its size.
    pub(crate) fn reset(&mut self) {
        self.packer = ShelfPacker::new(self.size);
        self.glyphs.clear();
    }

    /// The glyph's place in the atlas, rasterizing it first if needed, or
    /// `None` when there is no room left for it. Never grows the atlas.
    pub(crate) fn place(
        &mut self,
        queue: &wgpu::Queue,
        fonts: &HashMap<(PageId, FontKey), FontFile>,
        key: GlyphKey,
    ) -> Option<Placed> {
        if !self.glyphs.contains_key(&key) && !self.add(queue, fonts, key) {
            return None;
        }
        self.get(&key)
    }

    /// Rasterizes one glyph into the atlas. False only when there is no
    /// room; a glyph that cannot be drawn is stored as empty.
    fn add(
        &mut self,
        queue: &wgpu::Queue,
        fonts: &HashMap<(PageId, FontKey), FontFile>,
        key: GlyphKey,
    ) -> bool {
        let raster = fonts
            .get(&(key.page, key.font))
            .and_then(|font| rasterize(font, key));
        let Some((mask, placement)) = raster.filter(|(mask, _)| !mask.is_empty()) else {
            self.glyphs.insert(
                key,
                Placed {
                    texels: None,
                    width: 0,
                    height: 0,
                    left: 0,
                    top: 0,
                },
            );
            return true;
        };
        let Some(at) = self.packer.allocate(placement.width, placement.height) else {
            return false;
        };
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: at[0],
                    y: at[1],
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            &mask,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(placement.width),
                rows_per_image: Some(placement.height),
            },
            wgpu::Extent3d {
                width: placement.width,
                height: placement.height,
                depth_or_array_layers: 1,
            },
        );
        self.glyphs.insert(
            key,
            Placed {
                texels: Some(at),
                width: placement.width,
                height: placement.height,
                left: placement.left,
                top: placement.top,
            },
        );
        true
    }
}

/// Collects an outline as `zeno` path commands, flipped to y down and moved
/// by the subpixel offset.
struct ZenoPen {
    commands: Vec<Command>,
    x_offset: f32,
}

impl ZenoPen {
    fn point(&self, x: f32, y: f32) -> Vector {
        Vector::new(x + self.x_offset, -y)
    }
}

impl OutlinePen for ZenoPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.commands.push(Command::MoveTo(self.point(x, y)));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.commands.push(Command::LineTo(self.point(x, y)));
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.commands
            .push(Command::QuadTo(self.point(cx0, cy0), self.point(x, y)));
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.commands.push(Command::CurveTo(
            self.point(cx0, cy0),
            self.point(cx1, cy1),
            self.point(x, y),
        ));
    }

    fn close(&mut self) {
        self.commands.push(Command::Close);
    }
}

/// The glyph's coverage mask and where it goes relative to the pen.
/// `None` when the font or glyph cannot be read; an empty mask for a glyph
/// with no outline. Bitmap and color glyphs draw nothing in slice 1.
fn rasterize(file: &FontFile, key: GlyphKey) -> Option<(Vec<u8>, zeno::Placement)> {
    let font = skrifa::FontRef::from_index(&file.data, file.index).ok()?;
    let outline = font.outline_glyphs().get(GlyphId::new(key.glyph))?;
    let mut pen = ZenoPen {
        commands: Vec::new(),
        x_offset: f32::from(key.bin) / 4.0,
    };
    let size = Size::new(key.size as f32 / 64.0);
    outline
        .draw(
            DrawSettings::unhinted(size, LocationRef::new(&file.coords)),
            &mut pen,
        )
        .ok()?;
    if pen.commands.is_empty() {
        return Some((Vec::new(), zeno::Placement::default()));
    }
    Some(Mask::new(&pen.commands).render())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_packer_fills_shelves_and_reports_full() {
        let mut packer = ShelfPacker::new(32);
        assert_eq!(packer.allocate(10, 10), Some([0, 0]));
        assert_eq!(packer.allocate(10, 9), Some([11, 0]), "same shelf");
        assert_eq!(
            packer.allocate(10, 4),
            Some([0, 11]),
            "far shorter: a new shelf"
        );
        assert_eq!(
            packer.allocate(10, 10),
            Some([0, 16]),
            "shelf 1 is full: a third"
        );
        assert_eq!(packer.allocate(10, 10), Some([11, 16]));
        assert_eq!(
            packer.allocate(10, 10),
            None,
            "every shelf full, no height left"
        );
        assert_eq!(packer.allocate(40, 1), None, "wider than the atlas");
    }

    #[test]
    fn keys_quantize_size_and_subpixel_position() {
        let key = GlyphKey::new(PageId(1), FontKey(2), 3, 16.0, 10.6);
        assert_eq!((key.size, key.bin), (1024, 2));
        assert_eq!(GlyphKey::new(PageId(1), FontKey(2), 3, 16.0, 10.0).bin, 0);
        assert_eq!(GlyphKey::new(PageId(1), FontKey(2), 3, 16.0, 10.99).bin, 3);
    }
}
