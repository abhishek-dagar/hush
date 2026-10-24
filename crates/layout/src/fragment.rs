//! The fragment tree: layout's immutable output (spec 0002, Fragment tree;
//! spec 0004, child 0004).

use std::rc::Rc;

use base::{Au, BoxId, Point, Rect, SideOffsets, Size};
use css::ComputedStyle;
use dom::NodeId;
use text::FontKey;

use crate::exclusion::ExclusionSpace;

/// Margins that meet and collapse into one (CSS 2.2 section 8.3.1): the
/// largest positive margin plus the most negative one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MarginStrut {
    /// The largest positive margin, or zero.
    pub positive: Au,
    /// The most negative margin, or zero.
    pub negative: Au,
}

impl MarginStrut {
    /// A strut holding one margin.
    pub fn new(margin: Au) -> Self {
        let mut strut = Self::default();
        strut.push(margin);
        strut
    }

    /// Collapses one more margin into the strut.
    pub fn push(&mut self, margin: Au) {
        self.positive = self.positive.max(margin);
        self.negative = self.negative.min(margin);
    }

    /// Collapses every margin of `other` into the strut.
    pub fn append(&mut self, other: Self) {
        self.push(other.positive);
        self.push(other.negative);
    }

    /// The single margin the collapsed ones amount to.
    pub fn resolve(self) -> Au {
        self.positive + self.negative
    }
}

/// A block's used margins as its parent places it. The vertical ones are
/// struts, because they may already hold margins of the block's children
/// that collapsed through its top or bottom edge. They live on the
/// fragment, so a cached fragment carries its collapse with it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BlockMargins {
    /// The used left margin (`auto` resolved, e.g. for centering).
    pub left: Au,
    /// The used right margin.
    pub right: Au,
    /// The top margin, with any margins collapsed into it from inside.
    pub top: MarginStrut,
    /// The bottom margin, with any margins collapsed into it from inside.
    pub bottom: MarginStrut,
    /// The block is empty and its top and bottom margins collapse through
    /// it into one.
    pub collapses_through: bool,
}

/// Which edges of an inline box a piece of it carries. An inline box split
/// across lines draws its start side (left, in slice 1) only on its first
/// piece and its end side only on its last (spec 0004, child 0004, Inline
/// formatting step 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InlineEdges {
    /// This piece holds the box's start edge.
    pub start: bool,
    /// This piece holds the box's end edge.
    pub end: bool,
}

/// An absolutely positioned box waiting for its containing block, which
/// sits further up the tree (spec 0006, AC-5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingAbsolute {
    /// The absolute box.
    pub box_id: BoxId,
    /// Its static position: where its margin box's top left corner would
    /// have been in flow, from the border box corner of the fragment that
    /// carries it.
    pub static_position: Point,
}

impl PendingAbsolute {
    /// The same box, seen from a fragment that holds this one's at `at`.
    pub(crate) fn offset_by(self, at: Point) -> Self {
        Self {
            static_position: at.offset_by(self.static_position),
            ..self
        }
    }
}

/// A laid out box. Immutable once made; children are placed relative to
/// their parent's border box, so a cached fragment stays valid when its
/// parent moves.
#[derive(Debug, PartialEq)]
pub struct BoxFragment {
    /// The box it was laid out from.
    pub box_id: BoxId,
    /// The node that box came from, if any.
    pub node: Option<NodeId>,
    /// The border box size.
    pub size: Size,
    /// Children with their offsets from this fragment's top left corner.
    pub children: Vec<(Point, Rc<Fragment>)>,
    /// The area the box and its content cover, relative to the border box's
    /// top left corner. For a scroll container it is what can be scrolled
    /// to: the padding box, the children and the end padding.
    pub scrollable_overflow: Rect,
    /// The first line's baseline from the border box top, for a box with
    /// lines of text in it.
    pub baseline: Option<Au>,
    /// The last line's baseline from the border box top: where an
    /// `inline-block` sits on its line (CSS 2.2 section 10.8.1).
    pub last_baseline: Option<Au>,
    /// Whether the box scrolls its overflow (`overflow` is `hidden`,
    /// `scroll` or `auto`); paint gives it a scroll frame.
    pub is_scroll_container: bool,
    /// The used margins, for the parent to place it by.
    pub margins: BlockMargins,
    /// The used padding, so paint finds the padding and content boxes
    /// (spec 0004, child 0005, Backgrounds, Images).
    pub padding: SideOffsets,
    /// For one line's piece of an inline box, which of the box's edges it
    /// carries. `None` for every other box.
    pub inline_edges: Option<InlineEdges>,
    /// The content height the box's content needs, before `height`,
    /// `min-height` and `max-height` apply: a column flex item's content
    /// size (spec 0006, AC-4).
    pub auto_height: Au,
    /// How far `position: relative` moves the box from where its parent
    /// would place it. The parent adds it when placing, so no other box
    /// moves (spec 0006, AC-5).
    pub relative_offset: Point,
    /// Absolute descendants whose containing block is above this box, so
    /// a cached fragment still hands them on. A containing block lays its
    /// own out and adds them to its children, after the in flow ones.
    pub absolutes: Vec<PendingAbsolute>,
    /// The exclusion space after this box's content, for the next sibling
    /// in its BFC (spec 0009, child 0001).
    pub exclusions_out: Option<Rc<ExclusionSpace>>,
}

impl Drop for BoxFragment {
    // Dropping nested `Rc`s recurses once per level, which a very deep page
    // would turn into a stack overflow. Unlink the children onto a heap stack instead.
    fn drop(&mut self) {
        let mut pending = std::mem::take(&mut self.children);
        while let Some((_, child)) = pending.pop() {
            match Rc::try_unwrap(child) {
                Ok(Fragment::Box(mut inner)) => pending.append(&mut inner.children),
                Ok(Fragment::Line(mut line)) => pending.append(&mut line.children),
                _ => {}
            }
        }
    }
}

/// One line box (CSS 2.2 section 9.4.2): the text runs, inline box pieces
/// and atomic inlines set on one line.
#[derive(Debug, PartialEq)]
pub struct LineFragment {
    /// The line's width (the block's content width, or the content's when
    /// it overflows) and height.
    pub size: Size,
    /// The baseline from the line's top.
    pub baseline: Au,
    /// What sits on the line, offset from its top left corner.
    pub children: Vec<(Point, Rc<Fragment>)>,
}

/// One glyph placed in a text fragment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlacedGlyph {
    /// The glyph id in the fragment's font.
    pub id: u32,
    /// Its origin from the fragment's left edge.
    pub x: Au,
    /// Its origin below the fragment's baseline (negative is above).
    pub y: Au,
    /// The byte offset in the shaped text of the cluster it draws, which
    /// places a text input's caret and selection (spec 0008, AC-14).
    pub cluster: u32,
}

/// The part of one text node in one font that falls on one line (spec
/// 0004, child 0004, Inline formatting step 7). A list marker is one too,
/// carrying its list item's node.
#[derive(Debug, PartialEq)]
pub struct TextFragment {
    /// The text node (or the list item, for a marker).
    pub node: NodeId,
    /// The style the text draws with: its parent element's.
    pub style: Rc<ComputedStyle>,
    /// The font it was shaped in.
    pub font: FontKey,
    /// The font size.
    pub font_size: Au,
    /// The width the glyphs take, and the font's ascent plus descent.
    pub size: Size,
    /// The baseline from the fragment's top: the font's ascent.
    pub baseline: Au,
    /// From the baseline up to the top of an underline (negative is below).
    pub underline_offset: Au,
    /// How thick an underline is.
    pub underline_thickness: Au,
    /// The glyphs, left to right.
    pub glyphs: Vec<PlacedGlyph>,
    /// The characters the glyphs draw, for dumps.
    pub text: String,
    /// Where `text` starts in the shaped text, in bytes: the offset glyph
    /// clusters count from.
    pub text_start: u32,
    /// Laid out in a fallback while its web face loads inside the block
    /// period: paint skips it (spec 0006, AC-11).
    pub hidden_font: bool,
}

/// One piece of layout output.
#[derive(Debug, PartialEq)]
pub enum Fragment {
    /// A box, or one line's piece of an inline box.
    Box(BoxFragment),
    /// A line box.
    Line(LineFragment),
    /// A run of text on one line.
    Text(TextFragment),
}

impl Fragment {
    /// The box, if this is one.
    pub fn as_box(&self) -> Option<&BoxFragment> {
        match self {
            Self::Box(fragment) => Some(fragment),
            Self::Line(_) | Self::Text(_) => None,
        }
    }

    /// The fragment's size: a box's border box, a line box, or a text run's
    /// extent.
    pub fn size(&self) -> Size {
        match self {
            Self::Box(fragment) => fragment.size,
            Self::Line(fragment) => fragment.size,
            Self::Text(fragment) => fragment.size,
        }
    }

    /// The fragment's children, if it can have any.
    pub fn children(&self) -> &[(Point, Rc<Fragment>)] {
        match self {
            Self::Box(fragment) => &fragment.children,
            Self::Line(fragment) => &fragment.children,
            Self::Text(_) => &[],
        }
    }
}

/// The result of laying out one document.
#[derive(Debug, PartialEq)]
pub struct FragmentTree {
    /// The viewport it was laid out for.
    pub viewport: Size,
    /// The root box's fragment and its position in the viewport.
    pub root: Option<(Point, Rc<Fragment>)>,
}
