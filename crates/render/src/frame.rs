//! One frame's draw data, built on the CPU from a display list (spec 0004,
//! child 0005, Renderer pipelines).
//!
//! Each item becomes instances for the rect, glyph or image pipeline, in
//! root target pixels; consecutive instances of one pipeline and texture
//! batch into one draw. An opacity group below 1 becomes a pass of its own
//! into an offscreen layer sized to what it draws, which its parent then
//! draws like an image with that opacity. Plain data in, data out: no GPU
//! is needed, so it is tested on its own.

use std::ops::Range;

use display_list::{
    BorderStyle, ColorF, DisplayItem, DisplayList, FontKey, GlyphInstance, ImageKey, ItemKind,
    PageId, PointF, RectF, SpatialId, clamp_radii,
};
use tracing::warn;

use crate::atlas::{GlyphKey, Placed};
use crate::{Chrome, ScrollIndicator};

/// Floats per rect pipeline instance: quad, color, box, radii, widths and
/// clip (four each), then the clip radius and the kind.
pub(crate) const SHAPE_FLOATS: usize = 26;
/// Floats per glyph instance: quad, atlas UVs, color and clip (four each),
/// then the clip radius.
pub(crate) const GLYPH_FLOATS: usize = 17;
/// Floats per image instance: quad, UVs and clip (four each), then the
/// alpha and the clip radius.
pub(crate) const IMAGE_FLOATS: usize = 14;

// Rect pipeline kinds, as in `rect.wgsl`.
const FILL: f32 = 0.0;
const RING: f32 = 1.0;
/// Plus the side: 0 top, 1 right, 2 bottom, 3 left.
const SIDE: f32 = 2.0;
const DOT: f32 = 6.0;

/// A dash's length, in border widths (spec 0004, child 0005).
const DASH: f32 = 3.0;
/// The gap between dashes aimed for, in border widths. Gaps stretch so a
/// side starts and ends on a dash.
const DASH_GAP: f32 = 3.0;
/// The distance between dot centers aimed for, in border widths.
const DOT_SPACING: f32 = 2.0;
/// The most dashes or dots one side expands to. The list is untrusted: a
/// longer side draws solid instead of flooding the instance buffer.
const MAX_PIECES: usize = 10_000;

/// The color of an empty page, in sRGB.
const BLANK_PAGE_COLOR: ColorF = ColorF {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 1.0,
};

/// A box in root target pixels: left, top, right, bottom.
pub(crate) type Edges = [f32; 4];

/// A rounded clip box in root target pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Clip {
    pub(crate) rect: Edges,
    pub(crate) radius: f32,
}

/// A clip box larger than any target: it cuts nothing.
pub(crate) const NO_CLIP: Clip = Clip {
    rect: [-1.0e6, -1.0e6, 1.0e6, 1.0e6],
    radius: 0.0,
};

impl Clip {
    /// This clip cut down to `rect` as well. The shaders take one rounded
    /// box, so a cut clip keeps the corner radius only while the cut leaves
    /// it whole; display list clip radii are ignored in slice 1 (rounded
    /// clips are feature 11).
    fn cut(self, rect: Edges) -> Self {
        let [left, top, right, bottom] = self.rect;
        let cut = [
            left.max(rect[0]),
            top.max(rect[1]),
            right.min(rect[2]),
            bottom.min(rect[3]),
        ];
        // An empty cut stays empty rather than turning inside out.
        let cut = [cut[0], cut[1], cut[2].max(cut[0]), cut[3].max(cut[1])];
        Self {
            radius: if cut == self.rect { self.radius } else { 0.0 },
            rect: cut,
        }
    }
}

/// What an image draw samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Texture {
    /// An image a page registered with `AddImage`.
    Image(PageId, ImageKey),
    /// The offscreen layer [`Frame::passes`] at this index drew.
    Layer(usize),
}

/// Which pipeline a draw uses, and with which texture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Batch {
    Shapes,
    Glyphs,
    Images(Texture),
}

/// One draw call: a run of one pipeline's instances.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Draw {
    pub(crate) batch: Batch,
    pub(crate) instances: Range<u32>,
}

/// One render pass.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Pass {
    /// Where its target sits in the root target, in whole pixels: x, y,
    /// width, height. The root pass covers the whole target.
    pub(crate) bounds: [u32; 4],
    pub(crate) draws: Vec<Draw>,
}

/// Everything one frame draws.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Frame {
    pub(crate) shapes: Vec<f32>,
    pub(crate) glyphs: Vec<f32>,
    pub(crate) images: Vec<f32>,
    /// In drawing order: each layer before the pass that draws it, the
    /// root pass last.
    pub(crate) passes: Vec<Pass>,
    /// What the root pass clears to.
    pub(crate) clear: ColorF,
}

/// A pass still collecting draws.
struct Layer {
    draws: Vec<Draw>,
    opacity: f32,
    /// The union of what it draws, cut by the clips.
    ink: Option<Edges>,
}

/// Collects instances and passes for one frame.
pub(crate) struct FrameBuilder {
    frame: Frame,
    size: [f32; 2],
    /// The root layer first, then one per open group below opacity 1.
    layers: Vec<Layer>,
    /// One entry per open group: whether it opened a layer.
    groups: Vec<bool>,
}

impl FrameBuilder {
    /// A frame for a `width` by `height` target cleared to `clear`.
    pub(crate) fn new(width: u32, height: u32, clear: ColorF) -> Self {
        Self {
            frame: Frame {
                shapes: Vec::new(),
                glyphs: Vec::new(),
                images: Vec::new(),
                passes: Vec::new(),
                clear,
            },
            size: [width as f32, height as f32],
            layers: vec![Layer {
                draws: Vec::new(),
                opacity: 1.0,
                ink: None,
            }],
            groups: Vec::new(),
        }
    }

    /// How many groups are open.
    pub(crate) fn depth(&self) -> usize {
        self.groups.len()
    }

    /// Adds instance `index` of `batch` to the innermost layer.
    fn record(&mut self, batch: Batch, index: usize, quad: Edges, clip: &Clip) {
        let Some(layer) = self.layers.last_mut() else {
            return;
        };
        let index = index as u32;
        match layer.draws.last_mut() {
            Some(draw) if draw.batch == batch && draw.instances.end == index => {
                draw.instances.end += 1;
            }
            _ => layer.draws.push(Draw {
                batch,
                instances: index..index + 1,
            }),
        }
        let inked = intersect(quad, clip.rect);
        if !is_empty(inked) {
            layer.ink = Some(match layer.ink {
                Some(ink) => union(ink, inked),
                None => inked,
            });
        }
    }

    /// A rectangle filled with `color`, its corners rounded by `radii`
    /// (top left, top right, bottom right, bottom left).
    pub(crate) fn fill(&mut self, rect: Edges, color: ColorF, radii: [f32; 4], clip: Clip) {
        self.shape(rect, color, rect, radii, [0.0; 4], clip, FILL);
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one argument per instance attribute"
    )]
    fn shape(
        &mut self,
        quad: Edges,
        color: ColorF,
        outer: Edges,
        radii: [f32; 4],
        widths: [f32; 4],
        clip: Clip,
        kind: f32,
    ) {
        if color.a <= 0.0 || is_empty(quad) {
            return;
        }
        let index = self.frame.shapes.len() / SHAPE_FLOATS;
        let shapes = &mut self.frame.shapes;
        shapes.extend(quad);
        shapes.extend([color.r, color.g, color.b, color.a]);
        shapes.extend(outer);
        shapes.extend(radii);
        shapes.extend(widths);
        shapes.extend(clip.rect);
        shapes.extend([clip.radius, kind]);
        self.record(Batch::Shapes, index, quad, &clip);
    }

    /// A border around `rect`. Widths and colors go top, right, bottom,
    /// left; radii top left, top right, bottom right, bottom left, already
    /// clamped. Four solid sides of one color draw as one ring; otherwise
    /// each side draws on its own, `dashed` and `dotted` ones as dash and
    /// dot pieces, `double` as `solid` (spec 0004, child 0005).
    pub(crate) fn border(
        &mut self,
        rect: Edges,
        widths: [f32; 4],
        colors: [ColorF; 4],
        styles: [BorderStyle; 4],
        radii: [f32; 4],
        clip: Clip,
    ) {
        let widths = widths.map(|w| if w.is_finite() { w.max(0.0) } else { 0.0 });
        let drawn: [bool; 4] = std::array::from_fn(|i| {
            widths[i] > 0.0
                && colors[i].a > 0.0
                && !matches!(styles[i], BorderStyle::None | BorderStyle::Hidden)
        });
        let solid = |style: &BorderStyle| matches!(style, BorderStyle::Solid | BorderStyle::Double);
        if drawn == [true; 4]
            && colors.iter().all(|color| *color == colors[0])
            && styles.iter().all(solid)
        {
            self.shape(rect, colors[0], rect, radii, widths, clip, RING);
            return;
        }
        let [left, top, right, bottom] = rect;
        let [tl, tr, br, bl] = radii;
        // Each side's strip reaches as far in as its width or its corners'
        // radii; the shader splits the corners between sides.
        let strips = [
            [
                left,
                top,
                right,
                (top + widths[0].max(tl).max(tr)).min(bottom),
            ],
            [
                (right - widths[1].max(tr).max(br)).max(left),
                top,
                right,
                bottom,
            ],
            [
                left,
                (bottom - widths[2].max(bl).max(br)).max(top),
                right,
                bottom,
            ],
            [
                left,
                top,
                (left + widths[3].max(tl).max(bl)).min(right),
                bottom,
            ],
        ];
        let dotted = |side: usize| drawn[side] && styles[side] == BorderStyle::Dotted;
        for side in 0..4 {
            if !drawn[side] {
                continue;
            }
            let (color, width, strip) = (colors[side], widths[side], strips[side]);
            let kind = SIDE + side as f32;
            let horizontal = side.is_multiple_of(2);
            match styles[side] {
                BorderStyle::Dashed => {
                    let (start, end) = if horizontal {
                        (strip[0], strip[2])
                    } else {
                        (strip[1], strip[3])
                    };
                    for (from, to) in dash_spans(start, end, width) {
                        let piece = if horizontal {
                            [from, strip[1], to, strip[3]]
                        } else {
                            [strip[0], from, strip[2], to]
                        };
                        self.shape(piece, color, rect, radii, widths, clip, kind);
                    }
                }
                BorderStyle::Dotted => {
                    // A corner dot is drawn once, by the top or bottom side.
                    let ends = if horizontal {
                        (false, false)
                    } else {
                        (dotted(0), dotted(2))
                    };
                    match dot_centers(rect, radii, side, width, ends) {
                        Some(centers) => {
                            let r = width / 2.0;
                            for [x, y] in centers {
                                let dot = [x - r, y - r, x + r, y + r];
                                self.shape(dot, color, rect, radii, widths, clip, DOT);
                            }
                        }
                        None => self.shape(strip, color, rect, radii, widths, clip, kind),
                    }
                }
                _ => self.shape(strip, color, rect, radii, widths, clip, kind),
            }
        }
    }

    /// A glyph's coverage from the atlas texels `uv` (as UVs), filled with
    /// `color` over `quad`.
    pub(crate) fn glyph(&mut self, quad: Edges, uv: Edges, color: ColorF, clip: Clip) {
        if color.a <= 0.0 || is_empty(quad) {
            return;
        }
        let index = self.frame.glyphs.len() / GLYPH_FLOATS;
        let glyphs = &mut self.frame.glyphs;
        glyphs.extend(quad);
        glyphs.extend(uv);
        glyphs.extend([color.r, color.g, color.b, color.a]);
        glyphs.extend(clip.rect);
        glyphs.push(clip.radius);
        self.record(Batch::Glyphs, index, quad, &clip);
    }

    /// The part `uv` of `texture` stretched over `quad`, faded by `alpha`.
    pub(crate) fn image(
        &mut self,
        texture: Texture,
        quad: Edges,
        uv: Edges,
        alpha: f32,
        clip: Clip,
    ) {
        if alpha <= 0.0 || is_empty(quad) {
            return;
        }
        let index = self.frame.images.len() / IMAGE_FLOATS;
        let images = &mut self.frame.images;
        images.extend(quad);
        images.extend(uv);
        images.extend(clip.rect);
        images.extend([alpha, clip.radius]);
        self.record(Batch::Images(texture), index, quad, &clip);
    }

    /// Opens a group with `opacity`. Below 1 it draws into a layer of its
    /// own; at 1 it changes nothing.
    pub(crate) fn push_group(&mut self, opacity: f32) {
        let opacity = if opacity.is_finite() {
            opacity.clamp(0.0, 1.0)
        } else {
            1.0
        };
        let layered = opacity < 1.0;
        if layered {
            self.layers.push(Layer {
                draws: Vec::new(),
                opacity,
                ink: None,
            });
        }
        self.groups.push(layered);
    }

    /// Closes the innermost group. A layer becomes a pass sized to its ink
    /// (in whole pixels, inside the target), drawn into its parent with
    /// its opacity. A layer that draws nothing, or at opacity 0, is
    /// dropped. A pop with no open group is ignored.
    pub(crate) fn pop_group(&mut self) {
        if self.groups.pop() != Some(true) {
            return;
        }
        let Some(layer) = self.layers.pop() else {
            return;
        };
        let Some(ink) = layer.ink.filter(|_| layer.opacity > 0.0) else {
            return;
        };
        let bounds = [
            ink[0].floor().max(0.0),
            ink[1].floor().max(0.0),
            ink[2].ceil().min(self.size[0]),
            ink[3].ceil().min(self.size[1]),
        ];
        if is_empty(bounds) {
            return;
        }
        self.frame.passes.push(Pass {
            bounds: [
                bounds[0] as u32,
                bounds[1] as u32,
                (bounds[2] - bounds[0]) as u32,
                (bounds[3] - bounds[1]) as u32,
            ],
            draws: layer.draws,
        });
        let pass = self.frame.passes.len() - 1;
        self.image(
            Texture::Layer(pass),
            bounds,
            [0.0, 0.0, 1.0, 1.0],
            layer.opacity,
            NO_CLIP,
        );
    }

    /// Closes any group left open and returns the frame.
    pub(crate) fn finish(mut self) -> Frame {
        while !self.groups.is_empty() {
            self.pop_group();
        }
        let draws = self.layers.pop().map(|root| root.draws).unwrap_or_default();
        self.frame.passes.push(Pass {
            bounds: [0, 0, self.size[0] as u32, self.size[1] as u32],
            draws,
        });
        self.frame
    }
}

/// What one frame shows.
pub(crate) struct Scene<'a> {
    /// The page's list, or `None` for a blank page.
    pub(crate) list: Option<&'a DisplayList>,
    /// Device pixels per page CSS px.
    pub(crate) scale: f32,
    /// The browser chrome around the page, if the app draws one.
    pub(crate) chrome: Option<&'a Chrome<'a>>,
    /// The overlay scroll indicator, drawn over the page.
    pub(crate) indicator: Option<&'a ScrollIndicator>,
    /// The target size in pixels.
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl Scene<'_> {
    /// Where the page's origin lands in the target and the clip around it,
    /// or `None` when the chrome shows no page.
    fn page_area(&self) -> Option<([f32; 2], Clip)> {
        match self.chrome {
            Some(chrome) => chrome.slot.map(|slot| {
                (
                    [slot.rect.origin.x, slot.rect.origin.y],
                    Clip {
                        rect: edges(slot.rect),
                        radius: slot.radius,
                    },
                )
            }),
            None => Some(([0.0, 0.0], NO_CLIP)),
        }
    }

    /// The lists to draw in order, each with where it lands and whether
    /// it is the page: the chrome over the whole target, then the page in
    /// its area, then the chrome's overlay over both (spec 0008).
    fn layers(&self) -> Vec<(&DisplayList, Geometry, bool)> {
        let mut layers = Vec::with_capacity(3);
        let target = [0.0, 0.0, self.width as f32, self.height as f32];
        if let Some(chrome) = self.chrome {
            let geometry = Geometry::new(chrome.list, chrome.scale, [0.0, 0.0], NO_CLIP, target);
            layers.push((chrome.list, geometry, false));
        }
        if let (Some(list), Some((offset, clip))) = (self.list, self.page_area()) {
            let geometry = Geometry::new(list, self.scale, offset, clip, target);
            layers.push((list, geometry, true));
        }
        if let Some(chrome) = self.chrome
            && let Some(overlay) = chrome.overlay
        {
            let geometry = Geometry::new(overlay, chrome.scale, [0.0, 0.0], NO_CLIP, target);
            layers.push((overlay, geometry, false));
        }
        layers
    }
}

/// A built frame, and the item to carry on from with an empty atlas when
/// the atlas filled up partway (spec 0004, AC-26).
pub(crate) struct Built {
    pub(crate) frame: Frame,
    pub(crate) resume: Option<usize>,
}

/// Where each spatial node's origin lands, and each clip, in target pixels.
struct Geometry {
    origins: Vec<Option<PointF>>,
    clips: Vec<Option<Clip>>,
    page_clip: Clip,
    /// The target, in its own pixels: nothing outside it can show.
    target: Edges,
    scale: f32,
    offset: [f32; 2],
}

impl Geometry {
    /// Spatial nodes only move their content (scroll offsets and the
    /// translation part of a reference frame), which is all paint emits.
    fn new(
        list: &DisplayList,
        scale: f32,
        offset: [f32; 2],
        page_clip: Clip,
        target: Edges,
    ) -> Self {
        let origins: Vec<Option<PointF>> = (0..list.spatial.len())
            .map(|i| list.to_root(SpatialId(i as u32), PointF::default()))
            .collect();
        let mut geometry = Self {
            origins,
            clips: Vec::with_capacity(list.clips.len()),
            page_clip,
            target,
            scale,
            offset,
        };
        // Parents come first in a validated list.
        for node in &list.clips {
            let outer = match node.parent {
                Some(parent) => geometry.clips.get(parent.0 as usize).copied().flatten(),
                None => Some(page_clip),
            };
            let rect = geometry
                .origin(node.spatial)
                .map(|origin| geometry.edges(node.rect, origin));
            let clip = outer.zip(rect).map(|(outer, rect)| outer.cut(rect));
            geometry.clips.push(clip);
        }
        geometry
    }

    fn origin(&self, spatial: SpatialId) -> Option<PointF> {
        self.origins.get(spatial.0 as usize).copied().flatten()
    }

    /// `rect` in the space whose origin is `origin`, in target pixels.
    fn edges(&self, rect: RectF, origin: PointF) -> Edges {
        let left = (origin.x + rect.origin.x) * self.scale + self.offset[0];
        let top = (origin.y + rect.origin.y) * self.scale + self.offset[1];
        [
            left,
            top,
            left + rect.size.width * self.scale,
            top + rect.size.height * self.scale,
        ]
    }

    /// The item's space origin and its clip, or `None` when either is
    /// missing (a list the validator would refuse).
    fn place(&self, item: &DisplayItem) -> Option<(PointF, Clip)> {
        let clip = match item.clip {
            Some(id) => self.clips.get(id.0 as usize).copied().flatten()?,
            None => self.page_clip,
        };
        Some((self.origin(item.spatial)?, clip))
    }

    /// Whether anything of `quad` can show: some of it lies inside both
    /// `clip` and the target. Items scrolled out of view fail this, so a
    /// frame costs what the window shows, not the whole page.
    fn shows(&self, quad: Edges, clip: &Clip) -> bool {
        !is_empty(intersect(intersect(quad, clip.rect), self.target))
    }

    /// Whether a glyph whose pen sits at `pen` can show. Its box is not
    /// known before the atlas rasterizes it, so this uses a generous box
    /// around the pen (an em back and up, two forward, one down) that holds
    /// any real glyph, however far it overhangs.
    fn glyph_shows(&self, pen: [f32; 2], size: f32, clip: &Clip) -> bool {
        let em = size * self.scale;
        let quad = [
            pen[0] - em,
            pen[1] - 2.0 * em,
            pen[0] + 2.0 * em,
            pen[1] + em,
        ];
        self.shows(quad, clip)
    }

    /// A glyph's atlas key and its pen position: x in target pixels with
    /// its fraction kept for the subpixel bin, y on a whole pixel.
    fn glyph(
        &self,
        page: PageId,
        font: FontKey,
        size: f32,
        glyph: &GlyphInstance,
        origin: PointF,
    ) -> (GlyphKey, [f32; 2]) {
        let x = (origin.x + glyph.point.x) * self.scale + self.offset[0];
        let y = ((origin.y + glyph.point.y) * self.scale + self.offset[1]).round();
        let key = GlyphKey::new(page, font, glyph.index, size * self.scale, x);
        (key, [x.floor(), y])
    }
}

/// Every glyph `scene` draws, keyed as the atlas stores it, without
/// repeats.
pub(crate) fn glyph_keys(scene: &Scene<'_>) -> Vec<GlyphKey> {
    let mut keys = Vec::new();
    for (list, geometry, _) in scene.layers() {
        for item in &list.items {
            let ItemKind::Text {
                font, size, glyphs, ..
            } = &item.kind
            else {
                continue;
            };
            let Some((origin, clip)) = geometry.place(item) else {
                continue;
            };
            keys.extend(glyphs.iter().filter_map(|glyph| {
                let (key, pen) = geometry.glyph(list.page, *font, *size, glyph, origin);
                geometry.glyph_shows(pen, *size, &clip).then_some(key)
            }));
        }
    }
    keys.sort_unstable_by_key(|k| (k.page.0, k.font, k.glyph, k.size, k.bin));
    keys.dedup();
    keys
}

/// Builds the frame for `scene` from item `start` on, counting the chrome's
/// items first, the page's after them, then the chrome overlay's. `place` finds a glyph in the
/// atlas, adding it if needed, or says the atlas is full (`None`). When it
/// fills up, the frame ends before the text item that did not fit, so the
/// caller can draw it, empty the atlas and build the rest from `resume`.
/// Splits happen only outside groups; inside one, or when the very first
/// text cannot fit an empty atlas, the glyphs that do not fit are left out
/// with a warning.
pub(crate) fn build(
    scene: &Scene<'_>,
    start: usize,
    atlas_size: u32,
    mut place: impl FnMut(GlyphKey) -> Option<Placed>,
) -> Built {
    let mut builder = FrameBuilder::new(scene.width, scene.height, BLANK_PAGE_COLOR);
    let texel = 1.0 / atlas_size.max(1) as f32;
    let mut drew_text = false;
    let mut warned = false;
    let mut index = 0;
    for (list, geometry, is_page) in &scene.layers() {
        for item in &list.items {
            let at = index;
            index += 1;
            if at < start {
                continue;
            }
            // Group markers carry no geometry worth checking.
            match &item.kind {
                ItemKind::PushStackingContext { opacity, .. } => {
                    builder.push_group(*opacity);
                    continue;
                }
                ItemKind::PopStackingContext => {
                    builder.pop_group();
                    continue;
                }
                _ => {}
            }
            let Some((origin, clip)) = geometry.place(item) else {
                continue;
            };
            let scale = geometry.scale;
            match &item.kind {
                ItemKind::Rect { rect, color, radii } => {
                    let quad = geometry.edges(*rect, origin);
                    if geometry.shows(quad, &clip) {
                        let radii = corner_radii(*rect, *radii, scale);
                        builder.fill(quad, *color, radii, clip);
                    }
                }
                ItemKind::Border {
                    rect,
                    widths,
                    colors,
                    styles,
                    radii,
                } => {
                    let quad = geometry.edges(*rect, origin);
                    if !geometry.shows(quad, &clip) {
                        continue;
                    }
                    builder.border(
                        quad,
                        widths.map(|w| w * scale),
                        *colors,
                        *styles,
                        corner_radii(*rect, *radii, scale),
                        clip,
                    );
                }
                ItemKind::Text {
                    font,
                    size,
                    color,
                    glyphs,
                } => {
                    let mut placed = Vec::with_capacity(glyphs.len());
                    let mut full = false;
                    for glyph in glyphs {
                        let (key, pen) = geometry.glyph(list.page, *font, *size, glyph, origin);
                        // Matches `glyph_keys`, so an unseen glyph never
                        // takes atlas room.
                        if !geometry.glyph_shows(pen, *size, &clip) {
                            continue;
                        }
                        match place(key) {
                            Some(slot) => placed.push((slot, pen)),
                            None => full = true,
                        }
                    }
                    if full {
                        if builder.depth() == 0 && drew_text {
                            return Built {
                                frame: builder.finish(),
                                resume: Some(at),
                            };
                        }
                        if !warned {
                            warn!("glyphs do not fit in the glyph atlas; some are not drawn");
                            warned = true;
                        }
                    }
                    for (slot, pen) in placed {
                        let Some([u, v]) = slot.texels else {
                            continue;
                        };
                        let left = pen[0] + slot.left as f32;
                        let top = pen[1] + slot.top as f32;
                        let quad = [
                            left,
                            top,
                            left + slot.width as f32,
                            top + slot.height as f32,
                        ];
                        let uv = [
                            u as f32 * texel,
                            v as f32 * texel,
                            (u + slot.width) as f32 * texel,
                            (v + slot.height) as f32 * texel,
                        ];
                        builder.glyph(quad, uv, *color, clip);
                    }
                    drew_text = true;
                }
                ItemKind::Image { key, rect } => {
                    let quad = geometry.edges(*rect, origin);
                    if geometry.shows(quad, &clip) {
                        builder.image(
                            Texture::Image(list.page, *key),
                            quad,
                            [0.0, 0.0, 1.0, 1.0],
                            1.0,
                            clip,
                        );
                    }
                }
                ItemKind::PushStackingContext { .. } | ItemKind::PopStackingContext => {}
            }
        }
        if *is_page && let Some(indicator) = scene.indicator {
            let edges = geometry.edges(indicator.rect, PointF::default());
            let radius = (edges[2] - edges[0]) / 2.0;
            builder.fill(edges, indicator.color, [radius; 4], geometry.page_clip);
        }
    }
    Built {
        frame: builder.finish(),
        resume: None,
    }
}

/// An item's corner radii in target pixels: top left, top right, bottom
/// right, bottom left, clamped to fit `rect`. Elliptical corners arrive
/// with a later slice, so each corner draws circular, at its smaller
/// radius.
fn corner_radii(rect: RectF, radii: Option<[f32; 8]>, scale: f32) -> [f32; 4] {
    radii.map_or([0.0; 4], |radii| {
        let r = clamp_radii(rect, radii);
        [0, 2, 4, 6].map(|i| r[i].min(r[i + 1]) * scale)
    })
}

fn edges(rect: RectF) -> Edges {
    [
        rect.origin.x,
        rect.origin.y,
        rect.origin.x + rect.size.width,
        rect.origin.y + rect.size.height,
    ]
}

fn is_empty(edges: Edges) -> bool {
    // Written so NaN counts as empty too.
    !(edges[2] > edges[0] && edges[3] > edges[1])
}

fn intersect(a: Edges, b: Edges) -> Edges {
    [
        a[0].max(b[0]),
        a[1].max(b[1]),
        a[2].min(b[2]),
        a[3].min(b[3]),
    ]
}

fn union(a: Edges, b: Edges) -> Edges {
    [
        a[0].min(b[0]),
        a[1].min(b[1]),
        a[2].max(b[2]),
        a[3].max(b[3]),
    ]
}

/// The dashes along a side from `start` to `end` with line `width`: the
/// side starts and ends on a dash, gaps stretch to fit. A side too short
/// for two dashes (or too long to expand) draws as one.
fn dash_spans(start: f32, end: f32, width: f32) -> Vec<(f32, f32)> {
    let (dash, gap) = (DASH * width, DASH_GAP * width);
    let count = ((end - start + gap) / (dash + gap)).floor();
    if !(2.0..=MAX_PIECES as f32).contains(&count) {
        return vec![(start, end)];
    }
    let count = count as usize;
    let gap = (end - start - dash * count as f32) / (count - 1) as f32;
    (0..count)
        .map(|i| {
            let from = start + i as f32 * (dash + gap);
            (from, from + dash)
        })
        .collect()
}

/// The dot centers along `side` (0 top, 1 right, 2 bottom, 3 left) of
/// `rect`: on the side's center line, between its corners' radii, evenly
/// spread about [`DOT_SPACING`] widths apart, with dots at both ends
/// unless `skip` (start, end) leaves them to the neighboring side. `None`
/// when the side would need more than [`MAX_PIECES`] dots.
fn dot_centers(
    rect: Edges,
    radii: [f32; 4],
    side: usize,
    width: f32,
    skip: (bool, bool),
) -> Option<Vec<[f32; 2]>> {
    let [left, top, right, bottom] = rect;
    let [tl, tr, br, bl] = radii;
    let half = width / 2.0;
    let (line, start, end) = match side {
        0 => (top + half, left + tl.max(half), right - tr.max(half)),
        1 => (right - half, top + tr.max(half), bottom - br.max(half)),
        2 => (bottom - half, left + bl.max(half), right - br.max(half)),
        _ => (left + half, top + tl.max(half), bottom - bl.max(half)),
    };
    let mut along = if end <= start {
        vec![(start + end) / 2.0]
    } else {
        let gaps = ((end - start) / (DOT_SPACING * width)).round().max(1.0);
        if !gaps.is_finite() || gaps >= MAX_PIECES as f32 {
            return None;
        }
        let gaps = gaps as usize;
        (0..=gaps)
            .map(|i| start + (end - start) * i as f32 / gaps as f32)
            .collect()
    };
    if along.len() > 1 {
        if skip.1 {
            along.pop();
        }
        if skip.0 {
            along.remove(0);
        }
    }
    Some(
        along
            .into_iter()
            .map(|a| {
                if side.is_multiple_of(2) {
                    [a, line]
                } else {
                    [line, a]
                }
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use display_list::{BlendMode, Epoch, SizeF};

    use super::*;
    use crate::PageSlot;

    const RED: ColorF = ColorF {
        r: 1.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };
    const BLUE: ColorF = ColorF {
        r: 0.0,
        g: 0.0,
        b: 1.0,
        a: 1.0,
    };

    fn rect(x: f32, y: f32, w: f32, h: f32) -> RectF {
        RectF {
            origin: PointF { x, y },
            size: SizeF {
                width: w,
                height: h,
            },
        }
    }

    fn item(kind: ItemKind) -> DisplayItem {
        DisplayItem {
            spatial: SpatialId::ROOT,
            clip: None,
            kind,
        }
    }

    fn list(kinds: Vec<ItemKind>) -> DisplayList {
        let mut list = DisplayList::new(PageId(1), Epoch(1), SizeF::default());
        list.items = kinds.into_iter().map(item).collect();
        list
    }

    fn scene<'a>(list: &'a DisplayList, scale: f32) -> Scene<'a> {
        Scene {
            list: Some(list),
            scale,
            chrome: None,
            indicator: None,
            width: 200,
            height: 100,
        }
    }

    fn filled(x: f32, color: ColorF) -> ItemKind {
        ItemKind::Rect {
            rect: rect(x, 0.0, 10.0, 10.0),
            color,
            radii: None,
        }
    }

    fn text(glyphs: u32) -> ItemKind {
        ItemKind::Text {
            font: FontKey(1),
            size: 10.0,
            color: BLUE,
            glyphs: (0..glyphs)
                .map(|i| GlyphInstance {
                    index: i,
                    point: PointF {
                        x: i as f32 * 8.0,
                        y: 20.0,
                    },
                })
                .collect(),
        }
    }

    fn slot() -> Option<Placed> {
        Some(Placed {
            texels: Some([0, 0]),
            width: 4,
            height: 6,
            left: 0,
            top: -6,
        })
    }

    fn push(opacity: f32) -> ItemKind {
        ItemKind::PushStackingContext {
            opacity,
            blend_mode: BlendMode::Normal,
        }
    }

    fn batches(pass: &Pass) -> Vec<(Batch, Range<u32>)> {
        pass.draws
            .iter()
            .map(|d| (d.batch, d.instances.clone()))
            .collect()
    }

    fn shape(frame: &Frame, index: usize) -> &[f32] {
        &frame.shapes[index * SHAPE_FLOATS..(index + 1) * SHAPE_FLOATS]
    }

    #[test]
    fn items_batch_by_pipeline_in_paint_order_at_the_device_scale() {
        let list = list(vec![
            filled(0.0, RED),
            filled(20.0, RED),
            text(2),
            filled(40.0, BLUE),
        ]);
        let frame = build(&scene(&list, 2.0), 0, 64, |_| slot()).frame;

        assert_eq!(frame.passes.len(), 1, "no groups: the root pass only");
        assert_eq!(
            batches(&frame.passes[0]),
            [
                (Batch::Shapes, 0..2),
                (Batch::Glyphs, 0..2),
                (Batch::Shapes, 2..3)
            ]
        );
        assert_eq!(frame.passes[0].bounds, [0, 0, 200, 100]);
        // The second rect at x 20 CSS px is at 40 device px.
        assert_eq!(&shape(&frame, 1)[..4], &[40.0, 0.0, 60.0, 20.0]);
        assert_eq!(shape(&frame, 1)[25], FILL);
        // Glyph 1 sits 8 CSS px on, its pen on the baseline at y 40.
        assert_eq!(
            &frame.glyphs[GLYPH_FLOATS..GLYPH_FLOATS + 4],
            &[16.0, 34.0, 20.0, 40.0]
        );
        assert_eq!(frame.clear, BLANK_PAGE_COLOR);
    }

    #[test]
    fn an_opacity_group_draws_into_a_layer_its_parent_composites() {
        let list = list(vec![
            filled(0.0, RED),
            push(0.5),
            filled(20.0, BLUE),
            filled(35.5, BLUE),
            ItemKind::PopStackingContext,
            filled(60.0, RED),
        ]);
        let frame = build(&scene(&list, 1.0), 0, 64, |_| slot()).frame;

        assert_eq!(frame.passes.len(), 2);
        let layer = &frame.passes[0];
        assert_eq!(batches(layer), [(Batch::Shapes, 1..3)]);
        // Its ink, rounded out to whole pixels.
        assert_eq!(layer.bounds, [20, 0, 26, 10]);
        let root = &frame.passes[1];
        assert_eq!(
            batches(root),
            [
                (Batch::Shapes, 0..1),
                (Batch::Images(Texture::Layer(0)), 0..1),
                (Batch::Shapes, 3..4)
            ]
        );
        assert_eq!(
            &frame.images[..IMAGE_FLOATS],
            &[
                20.0, 0.0, 46.0, 10.0, 0.0, 0.0, 1.0, 1.0, -1.0e6, -1.0e6, 1.0e6, 1.0e6, 0.5, 0.0
            ]
        );
    }

    #[test]
    fn opaque_groups_change_nothing_and_invisible_ones_draw_nothing() {
        let list = list(vec![
            push(1.0),
            filled(0.0, RED),
            ItemKind::PopStackingContext,
            push(0.0),
            filled(20.0, RED),
            ItemKind::PopStackingContext,
            push(0.5),
            ItemKind::PopStackingContext,
            // An extra pop is ignored, and a group left open is closed.
            ItemKind::PopStackingContext,
            push(0.5),
            filled(40.0, RED),
        ]);
        let frame = build(&scene(&list, 1.0), 0, 64, |_| slot()).frame;
        assert_eq!(frame.passes.len(), 2);
        assert_eq!(
            batches(&frame.passes[1]),
            [
                (Batch::Shapes, 0..1),
                (Batch::Images(Texture::Layer(0)), 0..1)
            ]
        );
    }

    #[test]
    fn nested_layers_finish_before_their_parents() {
        let list = list(vec![
            push(0.5),
            filled(0.0, RED),
            push(0.5),
            filled(20.0, RED),
            ItemKind::PopStackingContext,
            ItemKind::PopStackingContext,
        ]);
        let frame = build(&scene(&list, 1.0), 0, 64, |_| slot()).frame;
        assert_eq!(frame.passes.len(), 3);
        assert_eq!(frame.passes[0].bounds, [20, 0, 10, 10], "the inner layer");
        assert_eq!(
            batches(&frame.passes[1]),
            [
                (Batch::Shapes, 0..1),
                (Batch::Images(Texture::Layer(0)), 0..1)
            ]
        );
        assert_eq!(frame.passes[1].bounds, [0, 0, 30, 10]);
    }

    #[test]
    fn a_full_atlas_splits_the_frame_before_the_text_that_did_not_fit() {
        let list = list(vec![text(2), filled(0.0, RED), text(3), text(1)]);
        let mut room = 3;
        let built = build(&scene(&list, 1.0), 0, 64, |_| {
            if room == 0 {
                return None;
            }
            room -= 1;
            slot()
        });
        assert_eq!(built.resume, Some(2));
        assert_eq!(
            batches(&built.frame.passes[0]),
            [(Batch::Glyphs, 0..2), (Batch::Shapes, 0..1)]
        );

        // Carrying on with an empty atlas draws the rest.
        let rest = build(&scene(&list, 1.0), 2, 64, |_| slot());
        assert_eq!(rest.resume, None);
        assert_eq!(batches(&rest.frame.passes[0]), [(Batch::Glyphs, 0..4)]);

        // Text that cannot fit even an empty atlas draws what fits.
        let list = super::tests::list(vec![text(3)]);
        let mut room = 1;
        let built = build(&scene(&list, 1.0), 0, 64, |_| {
            if room == 0 {
                return None;
            }
            room -= 1;
            slot()
        });
        assert_eq!(built.resume, None);
        assert_eq!(batches(&built.frame.passes[0]), [(Batch::Glyphs, 0..1)]);
    }

    #[test]
    fn a_uniform_solid_border_is_one_ring() {
        let mut builder = FrameBuilder::new(100, 100, RED);
        let rect = [10.0, 10.0, 60.0, 40.0];
        builder.border(
            rect,
            [2.0; 4],
            [RED; 4],
            [
                BorderStyle::Solid,
                BorderStyle::Double,
                BorderStyle::Solid,
                BorderStyle::Solid,
            ],
            [4.0; 4],
            NO_CLIP,
        );
        let frame = builder.finish();
        assert_eq!(frame.shapes.len(), SHAPE_FLOATS);
        let ring = shape(&frame, 0);
        assert_eq!(&ring[..4], &rect);
        assert_eq!(&ring[12..20], &[4.0, 4.0, 4.0, 4.0, 2.0, 2.0, 2.0, 2.0]);
        assert_eq!(ring[25], RING);
    }

    #[test]
    fn mixed_sides_draw_one_strip_each_and_skip_what_does_not_show() {
        let mut builder = FrameBuilder::new(100, 100, RED);
        let transparent = ColorF { a: 0.0, ..RED };
        builder.border(
            [0.0, 0.0, 50.0, 30.0],
            [2.0, 4.0, 0.0, 2.0],
            [RED, BLUE, RED, transparent],
            [BorderStyle::Solid; 4],
            [6.0, 0.0, 0.0, 0.0],
            NO_CLIP,
        );
        let frame = builder.finish();
        assert_eq!(frame.shapes.len(), 2 * SHAPE_FLOATS, "top and right only");
        // The top strip reaches as deep as the rounded corner.
        assert_eq!(&shape(&frame, 0)[..4], &[0.0, 0.0, 50.0, 6.0]);
        assert_eq!(shape(&frame, 0)[25], SIDE);
        assert_eq!(&shape(&frame, 1)[..4], &[46.0, 0.0, 50.0, 30.0]);
        assert_eq!(shape(&frame, 1)[25], SIDE + 1.0);
    }

    #[test]
    fn dashes_start_and_end_on_a_dash_and_dots_spread_evenly() {
        // 2 px wide over 50 px: dashes of 6, gaps stretched from 6.
        let spans = dash_spans(0.0, 50.0, 2.0);
        assert_eq!(spans.len(), 4);
        assert_eq!(spans[0], (0.0, 6.0));
        assert!((spans[3].1 - 50.0).abs() < 1e-3);
        assert_eq!(dash_spans(0.0, 8.0, 2.0), [(0.0, 8.0)], "too short: solid");

        // Dots of 2 px along the top of a 20 px box: centers 1 to 19, about
        // 4 px apart (18 / 4 rounds to 5 gaps).
        let dots = dot_centers([0.0, 0.0, 20.0, 10.0], [0.0; 4], 0, 2.0, (false, false));
        let dots = dots.unwrap();
        assert_eq!(dots.first(), Some(&[1.0, 1.0]));
        assert_eq!(dots.last(), Some(&[19.0, 1.0]));
        assert_eq!(dots.len(), 6);
        // The left side leaves its corners to the dotted top and bottom.
        let left = dot_centers([0.0, 0.0, 20.0, 10.0], [0.0; 4], 3, 2.0, (true, true));
        assert!(
            left.unwrap()
                .iter()
                .all(|&[x, y]| x == 1.0 && y > 1.0 && y < 9.0)
        );
        // Far too many dots: drawn solid instead.
        assert_eq!(
            dot_centers([0.0, 0.0, 1.0e9, 1.0], [0.0; 4], 0, 0.01, (false, false)),
            None
        );
    }

    #[test]
    fn clips_cut_their_items_and_the_chrome_slot_frames_the_page() {
        let mut list = list(vec![filled(0.0, RED)]);
        let clip = list.push_clip(display_list::ClipNode {
            parent: None,
            spatial: SpatialId::ROOT,
            rect: rect(2.0, 2.0, 4.0, 4.0),
            radii: None,
        });
        list.items[0].clip = Some(clip);
        let mut chrome_list = DisplayList::new(PageId(0), Epoch(1), SizeF::default());
        chrome_list.items = vec![item(filled(0.0, BLUE))];
        let chrome = Chrome {
            list: &chrome_list,
            scale: 2.0,
            slot: Some(PageSlot {
                rect: rect(10.0, 20.0, 100.0, 50.0),
                radius: 8.0,
            }),
            overlay: None,
        };
        let scene = Scene {
            chrome: Some(&chrome),
            ..scene(&list, 1.0)
        };
        let frame = build(&scene, 0, 64, |_| slot()).frame;
        // The chrome first, at its own scale, then the page moved into the
        // slot.
        assert_eq!(&shape(&frame, 0)[..4], &[0.0, 0.0, 20.0, 20.0]);
        let item = shape(&frame, 1);
        assert_eq!(&item[..4], &[10.0, 20.0, 20.0, 30.0]);
        assert_eq!(&item[20..24], &[12.0, 22.0, 16.0, 26.0], "cut by its clip");
        assert_eq!(item[24], 0.0, "the cut drops the slot's radius");
    }

    #[test]
    fn a_chrome_without_a_slot_shows_no_page() {
        let list = list(vec![filled(0.0, RED)]);
        let mut chrome_list = DisplayList::new(PageId(0), Epoch(1), SizeF::default());
        chrome_list.items = vec![item(filled(0.0, BLUE))];
        let chrome = Chrome {
            list: &chrome_list,
            scale: 1.0,
            slot: None,
            overlay: None,
        };
        let scene = Scene {
            chrome: Some(&chrome),
            ..scene(&list, 1.0)
        };
        let frame = build(&scene, 0, 64, |_| slot()).frame;
        assert_eq!(frame.shapes.len(), SHAPE_FLOATS, "only the chrome's rect");
    }

    #[test]
    fn a_split_resumes_counting_the_chrome_first() {
        let list = list(vec![text(1), text(1)]);
        let mut chrome_list = DisplayList::new(PageId(0), Epoch(1), SizeF::default());
        chrome_list.items = vec![item(text(1))];
        let chrome = Chrome {
            list: &chrome_list,
            scale: 1.0,
            slot: Some(PageSlot {
                rect: rect(0.0, 0.0, 100.0, 100.0),
                radius: 0.0,
            }),
            overlay: None,
        };
        let scene = Scene {
            chrome: Some(&chrome),
            ..scene(&list, 1.0)
        };
        let mut room = 2;
        let built = build(&scene, 0, 64, |_| {
            if room == 0 {
                return None;
            }
            room -= 1;
            slot()
        });
        assert_eq!(built.resume, Some(2), "the page's second text item");
    }

    #[test]
    fn the_chrome_overlay_draws_after_the_page_at_the_chrome_scale() {
        // Spec 0008, Decision: chrome base, then the page, then the overlay.
        let list = list(vec![filled(0.0, RED)]);
        let mut chrome_list = DisplayList::new(PageId(0), Epoch(1), SizeF::default());
        chrome_list.items = vec![item(filled(0.0, BLUE))];
        let mut overlay = chrome_list.clone();
        overlay.items = vec![item(filled(1.0, BLUE))];
        let chrome = Chrome {
            list: &chrome_list,
            scale: 2.0,
            slot: Some(PageSlot {
                rect: rect(10.0, 20.0, 100.0, 50.0),
                radius: 0.0,
            }),
            overlay: Some(&overlay),
        };
        let scene = Scene {
            chrome: Some(&chrome),
            ..scene(&list, 1.0)
        };
        let frame = build(&scene, 0, 64, |_| slot()).frame;
        assert_eq!(frame.shapes.len(), 3 * SHAPE_FLOATS);
        assert_eq!(
            &shape(&frame, 1)[..2],
            &[10.0, 20.0],
            "the page in its slot"
        );
        assert_eq!(
            &shape(&frame, 2)[..2],
            &[2.0, 0.0],
            "the overlay last, at the chrome's scale and not in the slot"
        );
    }

    #[test]
    fn items_and_glyphs_outside_the_target_are_skipped() {
        let below = |kind: ItemKind| match kind {
            ItemKind::Rect { color, radii, .. } => ItemKind::Rect {
                rect: rect(0.0, 5000.0, 10.0, 10.0),
                color,
                radii,
            },
            ItemKind::Text {
                font,
                size,
                color,
                mut glyphs,
            } => {
                for glyph in &mut glyphs {
                    glyph.point.y += 5000.0;
                }
                ItemKind::Text {
                    font,
                    size,
                    color,
                    glyphs,
                }
            }
            other => other,
        };
        let list = list(vec![
            filled(0.0, RED),
            below(filled(0.0, BLUE)),
            text(2),
            below(text(3)),
            ItemKind::Image {
                key: ImageKey(1),
                rect: rect(-50.0, 0.0, 10.0, 10.0),
            },
        ]);
        let scene = scene(&list, 1.0);

        assert_eq!(
            glyph_keys(&scene).len(),
            2,
            "only the visible text's glyphs"
        );
        let mut placed = 0;
        let frame = build(&scene, 0, 64, |_| {
            placed += 1;
            slot()
        })
        .frame;
        assert_eq!(placed, 2, "unseen glyphs never ask the atlas");
        assert_eq!(frame.shapes.len(), SHAPE_FLOATS, "only the visible rect");
        assert_eq!(frame.glyphs.len(), 2 * GLYPH_FLOATS);
        assert!(frame.images.is_empty(), "the image left of the target");
    }

    #[test]
    fn a_glyph_partly_in_view_still_draws() {
        // The pen sits just below the target's bottom edge at 100, but the
        // glyph rises above its baseline into view.
        let mut kind = text(1);
        if let ItemKind::Text { glyphs, .. } = &mut kind {
            glyphs[0].point.y = 104.0;
        }
        let list = list(vec![kind]);
        assert_eq!(glyph_keys(&scene(&list, 1.0)).len(), 1);
    }
}
