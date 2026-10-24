//! The display list: the drawing instructions the content process sends to
//! the browser process once per frame (spec 0002, Display list).
//!
//! Everything here is a plain `serde` type with `f32` coordinates in CSS px,
//! so the render crate never pulls in engine types. A list carries a spatial
//! tree (the root, scroll frames and reference frames), a clip tree, and the
//! items in paint order. The render side owns scroll offsets ([`ScrollState`]),
//! and checks every list it receives before drawing it ([`validate`]).

#![forbid(unsafe_code)]

mod dump;
mod scroll;
mod validate;

use serde::{Deserialize, Serialize};

pub use scroll::{ScrollState, sticky_offset};
pub use validate::{ListError, ResourceKeys, validate};

/// The largest encoded list either side accepts (spec 0002, Security model).
pub const MAX_ENCODED_LEN: usize = 256 * 1024 * 1024;

/// The largest image side, in pixels, the browser accepts (spec 0002,
/// Security model).
pub const MAX_IMAGE_SIDE: u32 = 16_384;

/// One tab. Assigned by the browser process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PageId(pub u32);

/// Counts the lists a page has sent. Only ever increases.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct Epoch(pub u64);

impl Epoch {
    /// The epoch after this one.
    pub fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// One scroll container in a page, stable while its node lives.
///
/// Packed from the frame id, the node's index and the node's generation
/// (spec 0002, Ids at a glance). The spec does not fix the bit widths, and a
/// `u64` cannot hold all three `u32`s, so this keeps the low 16 bits of the
/// frame id, all 32 bits of the index and the low 16 bits of the generation.
/// The shape is owed a decision (it cannot yet name its page, which AC-20
/// asks for); keep packing inside [`ScrollId::new`] so it changes in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ScrollId(u64);

impl ScrollId {
    /// The id for the scroll container node at `index` with `generation`,
    /// in frame `frame`.
    pub fn new(frame: u32, index: u32, generation: u32) -> Self {
        let frame = u64::from(frame & 0xffff);
        let generation = u64::from(generation & 0xffff);
        Self(frame << 48 | u64::from(index) << 16 | generation)
    }
}

/// A point in CSS px.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct PointF {
    /// Distance from the left.
    pub x: f32,
    /// Distance from the top.
    pub y: f32,
}

/// A size in CSS px.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct SizeF {
    /// Horizontal extent.
    pub width: f32,
    /// Vertical extent.
    pub height: f32,
}

/// A rectangle in CSS px.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct RectF {
    /// The top left corner.
    pub origin: PointF,
    /// The width and height.
    pub size: SizeF,
}

impl RectF {
    /// Whether `point` is inside, counting the top and left edges but not
    /// the bottom and right ones.
    pub fn contains(&self, point: PointF) -> bool {
        point.x >= self.origin.x
            && point.y >= self.origin.y
            && point.x < self.origin.x + self.size.width
            && point.y < self.origin.y + self.size.height
    }
}

/// An sRGB color with straight (not premultiplied) alpha, channels 0 to 1.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct ColorF {
    /// Red.
    pub r: f32,
    /// Green.
    pub g: f32,
    /// Blue.
    pub b: f32,
    /// Alpha.
    pub a: f32,
}

/// A font registered with `AddFont`. How a font is identified is decided
/// with text (feature 5); items only name it by key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FontKey(pub u32);

/// An image registered with `AddImage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ImageKey(pub u32);

/// How an image's pixels are laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageFormat {
    /// Four bytes per pixel: red, green, blue, alpha, premultiplied.
    Rgba8,
}

impl ImageFormat {
    /// Bytes per pixel.
    pub fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgba8 => 4,
        }
    }
}

/// An index into [`DisplayList::spatial`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SpatialId(pub u32);

impl SpatialId {
    /// The root spatial node every list starts with.
    pub const ROOT: Self = Self(0);
}

/// The scroll position of one scroll frame, and which script scroll set it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct ScrollOffset {
    /// How far the content is scrolled left, in CSS px.
    pub x: f32,
    /// How far the content is scrolled up, in CSS px.
    pub y: f32,
    /// Bumped by one on every script scroll. The render side applies an
    /// offset only when this is newer than the last one it applied (AC-13).
    pub generation: u32,
}

/// A coordinate space items are drawn in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpatialNode {
    /// The space this one sits in; `None` only for the root. Always an
    /// earlier node in the list.
    pub parent: Option<SpatialId>,
    /// What kind of space it is.
    pub kind: SpatialKind,
}

/// The kinds of spatial node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SpatialKind {
    /// The page's own coordinate space.
    Root,
    /// The content of a scroll container. A point `p` in this space sits at
    /// `frame_rect.origin + p - offset` in the parent space.
    ScrollFrame {
        /// Which scroll container.
        scroll_id: ScrollId,
        /// The scrollport (the padding box), in the parent space.
        frame_rect: RectF,
        /// How big the scrollable content is.
        content_size: SizeF,
        /// The current offset, as the content process knows it.
        offset: ScrollOffset,
        /// Whether the user may scroll this frame on each axis (x, y).
        /// `scroll` and `auto` are scrollable; `hidden` and `clip` are not
        /// (spec 0009, child 0002, AC-6 and AC-7).
        user_scrollable: [bool; 2],
    },
    /// A transformed space: `transform` maps points in it to the parent
    /// space. A 4 by 4 matrix, column major.
    ReferenceFrame {
        /// The matrix.
        transform: [f32; 16],
    },
    /// A sticky box: laid out in flow, then shifted at full frame rate to
    /// stay inside its nearest scroll frame while that frame scrolls
    /// (spec 0009, child 0003, AC-11).
    Sticky {
        /// Where the box sits in its parent space before the shift.
        sticky_rect: RectF,
        /// The remaining inset to keep on each side (top, right, bottom,
        /// left); `None` means that edge has no inset.
        margins: [Option<f32>; 4],
        /// How far the box may move along each axis, min then max.
        vertical_limits: (f32, f32),
        /// How far the box may move along the inline axis, min then max.
        horizontal_limits: (f32, f32),
        /// The shift to apply, computed by [`sticky_offset`] from this
        /// node's scroll frame and written by the render side on each frame.
        offset: PointF,
    },
}

impl SpatialKind {
    /// Maps `point` from this space into its parent space.
    fn to_parent(&self, point: PointF) -> PointF {
        match self {
            Self::Root => point,
            Self::ScrollFrame {
                frame_rect, offset, ..
            } => PointF {
                x: frame_rect.origin.x + point.x - offset.x,
                y: frame_rect.origin.y + point.y - offset.y,
            },
            Self::ReferenceFrame { transform: m } => PointF {
                x: m[0] * point.x + m[4] * point.y + m[12],
                y: m[1] * point.x + m[5] * point.y + m[13],
            },
            Self::Sticky { sticky_rect, .. } => PointF {
                x: sticky_rect.origin.x + point.x,
                y: sticky_rect.origin.y + point.y,
            },
        }
    }
}

/// An index into [`DisplayList::clips`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ClipId(pub u32);

/// A clip: items that name it, or a clip below it, draw only inside `rect`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipNode {
    /// The clip this one sits inside. Always an earlier clip in the list.
    pub parent: Option<ClipId>,
    /// The space `rect` is in.
    pub spatial: SpatialId,
    /// The clip rectangle.
    pub rect: RectF,
    /// Corner radii, x then y for top left, top right, bottom right, bottom left.
    pub radii: Option<[f32; 8]>,
}

/// `radii` (as in [`ClipNode::radii`]) made fit `rect`: negative or non
/// finite radii become 0, and when two corners on one side need more than
/// the side's length, every radius shrinks by the same factor until they
/// fit (CSS Backgrounds 3, section 5.5, Overlapping curves).
pub fn clamp_radii(rect: RectF, radii: [f32; 8]) -> [f32; 8] {
    let radii = radii.map(|r| if r.is_finite() { r.max(0.0) } else { 0.0 });
    let [tl_x, tl_y, tr_x, tr_y, br_x, br_y, bl_x, bl_y] = radii;
    let (width, height) = (rect.size.width.max(0.0), rect.size.height.max(0.0));
    let factor = [
        (width, tl_x + tr_x),
        (height, tr_y + br_y),
        (width, bl_x + br_x),
        (height, tl_y + bl_y),
    ]
    .into_iter()
    .filter(|&(_, sum)| sum > 0.0)
    .map(|(length, sum)| length / sum)
    .fold(1.0f32, f32::min);
    radii.map(|r| r * factor)
}

/// One edge's line style, in the order top, right, bottom, left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BorderStyle {
    /// No line.
    None,
    /// No line, and it wins border conflicts in tables.
    Hidden,
    /// A solid line.
    Solid,
    /// A dashed line.
    Dashed,
    /// A dotted line.
    Dotted,
    /// Two lines.
    Double,
}

/// How a stacking context blends with what is below it. More modes arrive
/// with `mix-blend-mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlendMode {
    /// Plain source over.
    Normal,
}

/// One glyph to draw from a font, at a position.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GlyphInstance {
    /// The glyph id in the font.
    pub index: u32,
    /// Where its origin goes.
    pub point: PointF,
}

/// One drawing instruction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisplayItem {
    /// The space the item's coordinates are in.
    pub spatial: SpatialId,
    /// The clip it draws inside, if any.
    pub clip: Option<ClipId>,
    /// What to draw.
    pub kind: ItemKind,
}

/// What a display item draws.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ItemKind {
    /// A solid rectangle.
    Rect {
        /// Where.
        rect: RectF,
        /// Fill color.
        color: ColorF,
        /// Corner radii, as in [`ClipNode::radii`]; `None` for square
        /// corners. A box's background follows its `border-radius`.
        radii: Option<[f32; 8]>,
    },
    /// A box border. Sides are top, right, bottom, left.
    Border {
        /// The border box.
        rect: RectF,
        /// Each side's width.
        widths: [f32; 4],
        /// Each side's color.
        colors: [ColorF; 4],
        /// Each side's line style.
        styles: [BorderStyle; 4],
        /// Corner radii, as in [`ClipNode::radii`].
        radii: Option<[f32; 8]>,
    },
    /// A run of glyphs from one font.
    Text {
        /// The font, registered with `AddFont`.
        font: FontKey,
        /// The font size in CSS px.
        size: f32,
        /// Fill color.
        color: ColorF,
        /// The glyphs and where they go.
        glyphs: Vec<GlyphInstance>,
    },
    /// An image stretched over a rectangle.
    Image {
        /// The image, registered with `AddImage`.
        key: ImageKey,
        /// Where.
        rect: RectF,
    },
    /// Starts a group the items up to the matching pop draw into.
    PushStackingContext {
        /// The group's opacity, 0 to 1.
        opacity: f32,
        /// How the group blends with what is below it.
        blend_mode: BlendMode,
    },
    /// Ends the innermost group.
    PopStackingContext,
}

/// One frame of drawing for one page. Items are in paint order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisplayList {
    /// The page it belongs to.
    pub page: PageId,
    /// Which list this is; the render side ignores lists that are not newer.
    pub epoch: Epoch,
    /// The viewport it was painted for, in CSS px.
    pub viewport: SizeF,
    /// Coordinate spaces; index 0 is the root, and parents come before
    /// their children.
    pub spatial: Vec<SpatialNode>,
    /// Clips; parents come before their children.
    pub clips: Vec<ClipNode>,
    /// Drawing instructions in paint order.
    pub items: Vec<DisplayItem>,
}

impl DisplayList {
    /// An empty list with just the root spatial node.
    pub fn new(page: PageId, epoch: Epoch, viewport: SizeF) -> Self {
        Self {
            page,
            epoch,
            viewport,
            spatial: vec![SpatialNode {
                parent: None,
                kind: SpatialKind::Root,
            }],
            clips: Vec::new(),
            items: Vec::new(),
        }
    }

    /// Adds a spatial node and returns its id.
    pub fn push_spatial(&mut self, parent: SpatialId, kind: SpatialKind) -> SpatialId {
        self.spatial.push(SpatialNode {
            parent: Some(parent),
            kind,
        });
        SpatialId(index_u32(self.spatial.len() - 1))
    }

    /// Adds a clip and returns its id.
    pub fn push_clip(&mut self, clip: ClipNode) -> ClipId {
        self.clips.push(clip);
        ClipId(index_u32(self.clips.len() - 1))
    }

    /// The page's root viewport scroll frame: the first scroll frame directly
    /// in the root space, which paint pushes before any other (spec 0004,
    /// child 0005, Root scroll frame).
    pub fn viewport_frame(&self) -> Option<SpatialId> {
        self.spatial
            .iter()
            .position(|node| {
                node.parent == Some(SpatialId::ROOT)
                    && matches!(node.kind, SpatialKind::ScrollFrame { .. })
            })
            .map(|index| SpatialId(index as u32))
    }

    /// Maps `point` from space `spatial` into the root space, or `None` if
    /// `spatial`, or a parent of it, is out of range. Stops after as many
    /// steps as there are nodes, so a list with a parent cycle cannot hang it.
    pub fn to_root(&self, spatial: SpatialId, mut point: PointF) -> Option<PointF> {
        let mut current = Some(spatial);
        for _ in 0..=self.spatial.len() {
            let Some(id) = current else {
                return Some(point);
            };
            let node = self.spatial.get(id.0 as usize)?;
            point = node.kind.to_parent(point);
            current = node.parent;
        }
        None
    }

    /// Every scroll frame in the list, by spatial id.
    pub fn scroll_frames(&self) -> impl Iterator<Item = (SpatialId, ScrollId)> + '_ {
        self.spatial
            .iter()
            .enumerate()
            .filter_map(|(index, node)| match node.kind {
                SpatialKind::ScrollFrame { scroll_id, .. } => {
                    Some((SpatialId(index_u32(index)), scroll_id))
                }
                _ => None,
            })
    }
}

/// A list index as a `u32` id. Lists never get near 4 billion entries
/// (they are capped at [`MAX_ENCODED_LEN`] bytes), so this saturates rather
/// than wrapping in the impossible case.
fn index_u32(index: usize) -> u32 {
    u32::try_from(index).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_ids_keep_frame_index_and_generation_apart() {
        let a = ScrollId::new(0, 5, 1);
        assert_ne!(a, ScrollId::new(1, 5, 1), "frame");
        assert_ne!(a, ScrollId::new(0, 6, 1), "index");
        assert_ne!(a, ScrollId::new(0, 5, 2), "generation");
        assert_eq!(a, ScrollId::new(0, 5, 1));
    }

    #[test]
    fn points_map_through_scroll_and_reference_frames_to_the_root() {
        let mut list = DisplayList::new(PageId(1), Epoch(1), SizeF::default());
        let mut translate = [0.0; 16];
        (translate[0], translate[5], translate[10], translate[15]) = (1.0, 1.0, 1.0, 1.0);
        (translate[12], translate[13]) = (100.0, 0.0);
        let moved = list.push_spatial(
            SpatialId::ROOT,
            SpatialKind::ReferenceFrame {
                transform: translate,
            },
        );
        let scrolled = list.push_spatial(
            moved,
            SpatialKind::ScrollFrame {
                scroll_id: ScrollId::new(0, 1, 1),
                frame_rect: RectF {
                    origin: PointF { x: 10.0, y: 20.0 },
                    size: SizeF {
                        width: 50.0,
                        height: 50.0,
                    },
                },
                content_size: SizeF {
                    width: 50.0,
                    height: 500.0,
                },
                offset: ScrollOffset {
                    x: 0.0,
                    y: 30.0,
                    generation: 0,
                },
                user_scrollable: [true, true],
            },
        );
        assert_eq!(
            list.to_root(scrolled, PointF { x: 1.0, y: 40.0 }),
            Some(PointF { x: 111.0, y: 30.0 })
        );
        assert_eq!(list.to_root(SpatialId(9), PointF::default()), None);
        assert_eq!(
            list.scroll_frames().collect::<Vec<_>>(),
            [(scrolled, ScrollId::new(0, 1, 1))]
        );
    }

    #[test]
    fn radii_that_overlap_shrink_together() {
        let rect = RectF {
            origin: PointF::default(),
            size: SizeF {
                width: 100.0,
                height: 40.0,
            },
        };
        assert_eq!(clamp_radii(rect, [10.0; 8]), [10.0; 8], "they fit");
        // 30 + 30 on the 40 px sides: everything scales by 40 / 60.
        let clamped = clamp_radii(rect, [30.0; 8]);
        assert!(clamped.iter().all(|r| (r - 20.0).abs() < 1e-4));
        assert_eq!(
            clamp_radii(
                rect,
                [-5.0, f32::NAN, 0.0, 0.0, 0.0, 0.0, 0.0, f32::INFINITY]
            ),
            [0.0; 8]
        );
    }

    #[test]
    fn a_parent_cycle_does_not_hang_the_mapping() {
        let mut list = DisplayList::new(PageId(1), Epoch(1), SizeF::default());
        list.spatial[0].parent = Some(SpatialId(0));
        assert_eq!(list.to_root(SpatialId::ROOT, PointF::default()), None);
    }
}
