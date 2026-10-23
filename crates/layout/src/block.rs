//! Block layout: one block box's sizes and the stacking of its children
//! with collapsing margins (spec 0004, child 0004, Block formatting, AC-23).
//!
//! Widths follow CSS 2.2 section 10.3.3, heights section 10.6.3, and
//! vertical margins collapse as section 8.3.1 says. There are no floats,
//! and absolute boxes are laid out by their containing block afterwards
//! ([`crate::positioned`]), so where a block sits never changes how its
//! content lays out: margin collapsing is done entirely while placing
//! finished children. Each fragment carries the struts of the margins that collapse
//! through its top and bottom edges ([`BlockMargins`]), so a cached child
//! collapses the same way as a fresh one.

use std::rc::Rc;

use base::{Au, BoxId, Point, Rect, SideOffsets, Size};
use css::{BoxSizing, ComputedStyle, Length, WritingMode};
use dom::NodeId;

use crate::Fonts;
use crate::box_tree::{BoxKind, LayoutBox, NaturalSize};
use crate::fragment::{
    BlockMargins, BoxFragment, Fragment, MarginStrut, PendingAbsolute, TextFragment,
};
use crate::inline::{Lines, font_descriptor, text_fragment};
use crate::{AvailableSize, ConstraintSpace, border_widths, positioned};

/// A box partway through layout: its children are being laid out.
pub(crate) struct InProgress {
    pub(crate) id: BoxId,
    node: Option<NodeId>,
    style: Rc<ComputedStyle>,
    pub(crate) space: ConstraintSpace,
    /// The box had no dirty flags: only the space changed, so intrinsic
    /// sizes cached on it still hold.
    pub(crate) was_clean: bool,
    writing_mode: WritingMode,
    border: SideOffsets,
    padding: SideOffsets,
    /// Used left and right margins, and the box's own top and bottom ones.
    margins: BlockMargins,
    own_top: Au,
    own_bottom: Au,
    is_scroll_container: bool,
    /// It centers its content in a taller content box: an `<input>` or
    /// `<button>` laid out as a block (spec 0006, AC-17).
    centers_content: bool,
    pub(crate) content_width: Au,
    /// The content height `height` sets, when it resolves.
    height: Option<Au>,
    min_height: Au,
    max_height: Option<Au>,
    /// Its bottom margin may collapse with its last child's.
    bottom_adjoining: bool,
    /// Its own top and bottom margins may collapse through it, if it turns
    /// out empty.
    may_collapse_through: bool,
    pub(crate) next_child: usize,
    /// The content height so far: where the next child's top margin starts.
    cursor: Au,
    /// The margins after the last placed child, not resolved yet because
    /// the next one may collapse with them.
    pending: MarginStrut,
    /// No child with content placed yet, and the margins in `pending` still
    /// adjoin this box's top margin: it starts no formatting context and
    /// has no top border or padding.
    at_top: bool,
    /// Children's top margins that collapsed into this box's top margin.
    absorbed_top: MarginStrut,
    /// The box's inline content made at least one line box.
    has_lines: bool,
    children: Vec<(Point, Rc<Fragment>)>,
    /// The first and last line's baselines from the border box top.
    baseline: Option<Au>,
    last_baseline: Option<Au>,
    marker: Option<String>,
    /// How far `position: relative` moves the box.
    relative_offset: Point,
    /// Absolute descendants waiting for their containing block, from the
    /// border box corner.
    absolutes: Vec<PendingAbsolute>,
}

/// A flex container's own sizes, which its items are laid out in.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FlexFrame {
    pub(crate) content_width: Au,
    /// The content height, when it is definite.
    pub(crate) height: Option<Au>,
    pub(crate) min_height: Au,
    pub(crate) max_height: Option<Au>,
}

/// A flex container's laid out items, from its content box origin.
#[derive(Debug, Default)]
pub(crate) struct FlexItems {
    pub(crate) children: Vec<(Point, Rc<Fragment>)>,
    /// The content height the items need.
    pub(crate) height: Au,
    pub(crate) baseline: Option<Au>,
    pub(crate) last_baseline: Option<Au>,
    /// Absolute children and the absolute descendants the items hand on.
    pub(crate) absolutes: Vec<PendingAbsolute>,
}

/// Resolves a length against an optional basis: `None` when it is a
/// percentage and the basis is not definite.
pub(crate) fn resolve(length: Option<Length>, basis: Option<Au>) -> Option<Au> {
    match length? {
        Length::Au(au) => Some(au),
        percent @ Length::Percent(_) => basis.map(|b| percent.resolve(Some(b))),
    }
}

/// `value` clamped by `max` and then `min`, so `min` wins (CSS 2.2 section
/// 10.4).
pub(crate) fn clamp(value: Au, min: Au, max: Option<Au>) -> Au {
    max.map_or(value, |max| value.min(max)).max(min)
}

impl InProgress {
    /// Resolves the box's own sizes from its style and the space it gets.
    /// `intrinsic` is its content's min and max content widths, given for
    /// an inline block whose width shrinks to fit. A flex item starts its
    /// own formatting context, as the root does.
    pub(crate) fn start(
        id: BoxId,
        layout_box: &LayoutBox,
        space: ConstraintSpace,
        is_root: bool,
        is_flex_item: bool,
        intrinsic: Option<(Au, Au)>,
    ) -> Self {
        let style = &layout_box.style;
        // Margins and padding resolve percentages against the containing
        // block's width on every side.
        let basis_width = space.percentage_basis.width;
        let basis_height = space.percentage_basis.height;
        let padding = SideOffsets {
            top: style.padding.top.resolve(basis_width),
            right: style.padding.right.resolve(basis_width),
            bottom: style.padding.bottom.resolve(basis_width),
            left: style.padding.left.resolve(basis_width),
        };
        let border = border_widths(style);
        let edges_x = border.horizontal() + padding.horizontal();
        let edges_y = border.vertical() + padding.vertical();
        let border_box = style.size.box_sizing == BoxSizing::BorderBox;
        let content_x = |w: Au| {
            if border_box {
                (w - edges_x).max(Au::ZERO)
            } else {
                w
            }
        };
        let content_y = |h: Au| {
            if border_box {
                (h - edges_y).max(Au::ZERO)
            } else {
                h
            }
        };

        let size = &style.size;
        let width = resolve(size.width, basis_width).map(content_x);
        let min_width = resolve(size.min_width, basis_width).map_or(Au::ZERO, content_x);
        let max_width = resolve(size.max_width, basis_width).map(content_x);
        let height = resolve(size.height, basis_height).map(content_y);
        let min_height = resolve(size.min_height, basis_height).map_or(Au::ZERO, content_y);
        let max_height = resolve(size.max_height, basis_height).map(content_y);

        let side = |length: Option<Length>| length.map(|l| l.resolve(basis_width));
        let margin_left = side(style.margin.left);
        let margin_right = side(style.margin.right);
        let own_top = side(style.margin.top).unwrap_or(Au::ZERO);
        let own_bottom = side(style.margin.bottom).unwrap_or(Au::ZERO);

        let available = space.available_inline.definite().unwrap_or(Au::ZERO);
        let replaced = layout_box.kind == BoxKind::Replaced;
        let inline_level = layout_box.inline_level;
        let (content_width, height) = if replaced {
            let (w, h) = replaced_size(
                layout_box.natural,
                (width, height),
                (min_width, max_width),
                (min_height, max_height),
            );
            (w, Some(h))
        } else if let Some(stretch) = space.stretch_inline {
            // The parent already applied the limits.
            ((stretch - edges_x).max(Au::ZERO), height)
        } else {
            let width = width.unwrap_or_else(|| {
                let fill = (available
                    - margin_left.unwrap_or(Au::ZERO)
                    - margin_right.unwrap_or(Au::ZERO)
                    - edges_x)
                    .max(Au::ZERO);
                match (inline_level, intrinsic) {
                    // Shrink to fit (CSS 2.2 section 10.3.9).
                    (true, Some((min, max))) => fill.max(min).min(max),
                    _ => fill,
                }
            });
            (clamp(width, min_width, max_width), height)
        };
        let content_width = match (replaced, space.stretch_inline) {
            (true, Some(stretch)) => (stretch - edges_x).max(Au::ZERO),
            _ => content_width,
        };
        let height = space
            .stretch_block
            .map(|stretch| (stretch - edges_y).max(Au::ZERO))
            .or(height);

        let is_absolute = layout_box.is_absolute();
        let (left, right) = if inline_level || is_flex_item || is_absolute {
            // `auto` margins of an inline level box are zero; a flex item's
            // and an absolute box's are resolved by whatever places it.
            (
                margin_left.unwrap_or(Au::ZERO),
                margin_right.unwrap_or(Au::ZERO),
            )
        } else {
            used_margins(
                available,
                content_width + edges_x,
                margin_left,
                margin_right,
            )
        };

        let is_scroll_container = style.display_box.is_scroll_container();
        let is_flex = layout_box.kind == BoxKind::FlexContainer;
        let starts_context = is_root
            || inline_level
            || is_scroll_container
            || replaced
            || is_flex
            || is_flex_item
            || is_absolute;
        let top_adjoining = !starts_context && border.top == Au::ZERO && padding.top == Au::ZERO;
        let bottom_open =
            !starts_context && border.bottom == Au::ZERO && padding.bottom == Au::ZERO;
        Self {
            id,
            node: layout_box.node,
            style: Rc::clone(style),
            space,
            was_clean: layout_box.flags == crate::BoxFlags::empty(),
            writing_mode: style.inherited_text.writing_mode,
            border,
            padding,
            margins: BlockMargins {
                left,
                right,
                ..BlockMargins::default()
            },
            own_top,
            own_bottom,
            is_scroll_container,
            centers_content: layout_box.centers_content
                && layout_box.kind == BoxKind::BlockContainer,
            content_width,
            height,
            min_height,
            max_height,
            bottom_adjoining: bottom_open && height.is_none(),
            may_collapse_through: top_adjoining
                && bottom_open
                && height.is_none_or(|h| h == Au::ZERO)
                && min_height == Au::ZERO,
            next_child: 0,
            cursor: Au::ZERO,
            pending: MarginStrut::default(),
            at_top: top_adjoining,
            absorbed_top: MarginStrut::default(),
            has_lines: false,
            children: Vec::new(),
            baseline: None,
            last_baseline: None,
            marker: layout_box.marker.clone(),
            relative_offset: positioned::relative_offset(style, space.percentage_basis),
            absolutes: Vec::new(),
        }
    }

    /// The space the box's children (and its inline content) get.
    pub(crate) fn child_space(&self) -> ConstraintSpace {
        ConstraintSpace {
            available_inline: AvailableSize::Definite(self.content_width),
            available_block: self
                .height
                .map_or(AvailableSize::MaxContent, AvailableSize::Definite),
            percentage_basis: Size::new(Some(self.content_width), self.height),
            writing_mode: self.writing_mode,
            stretch_inline: None,
            stretch_block: None,
        }
    }

    /// The sizes a flex container lays its items out in.
    pub(crate) fn flex_frame(&self) -> FlexFrame {
        FlexFrame {
            content_width: self.content_width,
            height: self.height,
            min_height: self.min_height,
            max_height: self.max_height,
        }
    }

    /// Takes a flex container's laid out items as its content. They are
    /// all placed, so the layout stack visits no children.
    pub(crate) fn set_flex_items(&mut self, items: FlexItems, child_count: usize) {
        let origin = self.content_origin();
        self.children = items
            .children
            .into_iter()
            .map(|(at, fragment)| (origin.offset_by(at), fragment))
            .collect();
        self.baseline = items.baseline.map(|b| origin.y + b);
        self.last_baseline = items.last_baseline.map(|b| origin.y + b);
        self.cursor = items.height;
        self.next_child = child_count;
        self.absolutes
            .extend(items.absolutes.into_iter().map(|a| a.offset_by(origin)));
    }

    fn content_origin(&self) -> Point {
        Point::new(
            self.border.left + self.padding.left,
            self.border.top + self.padding.top,
        )
    }

    /// Takes the box's laid out lines as its content.
    pub(crate) fn set_lines(&mut self, lines: Lines) {
        let origin = self.content_origin();
        self.children = lines
            .fragments
            .into_iter()
            .map(|(at, fragment)| (origin.offset_by(at), fragment))
            .collect();
        self.baseline = lines.first_baseline.map(|b| origin.y + b);
        self.last_baseline = lines.last_baseline.map(|b| origin.y + b);
        self.cursor = lines.height;
        self.has_lines = lines.has_content;
        if self.has_lines {
            self.at_top = false;
        }
        self.absolutes
            .extend(lines.absolutes.into_iter().map(|a| a.offset_by(origin)));
    }

    /// Records an absolute child's static position: where the next block's
    /// top margin edge would be (spec 0006, Value sourcing, Static
    /// position).
    pub(crate) fn place_absolute(&mut self, id: BoxId) {
        let origin = self.content_origin();
        let y = if self.at_top {
            self.cursor
        } else {
            self.cursor + self.pending.resolve()
        };
        self.absolutes.push(PendingAbsolute {
            box_id: id,
            static_position: Point::new(origin.x, origin.y + y),
        });
    }

    /// Adds a placed child, moved by its relative offset, and takes over
    /// the absolute descendants it hands on.
    fn push_child(&mut self, at: Point, fragment: Rc<Fragment>) {
        if let Some(child) = fragment.as_box() {
            let at = at.offset_by(child.relative_offset);
            self.absolutes
                .extend(child.absolutes.iter().map(|a| a.offset_by(at)));
            self.children.push((at, fragment));
        }
    }

    /// Places a finished child below the ones before it, collapsing its
    /// top margin with the margins before it.
    pub(crate) fn place(&mut self, fragment: Rc<Fragment>) {
        let Some(child) = fragment.as_box() else {
            return;
        };
        let origin = self.content_origin();
        let margins = child.margins;
        let x = origin.x + margins.left;
        self.pending.append(margins.top);
        if margins.collapses_through {
            // An empty block: its margins join the ones around it. It sits
            // where the next block's border edge would.
            let y = if self.at_top {
                self.cursor
            } else {
                self.cursor + self.pending.resolve()
            };
            self.pending.append(margins.bottom);
            self.push_child(Point::new(x, origin.y + y), fragment);
            return;
        }
        let y = if self.at_top {
            // The margins so far collapse into this box's own top margin.
            self.absorbed_top.append(self.pending);
            self.at_top = false;
            self.cursor
        } else {
            self.cursor + self.pending.resolve()
        };
        if self.baseline.is_none() {
            self.baseline = child.baseline.map(|b| origin.y + y + b);
        }
        if let Some(last) = child.last_baseline {
            self.last_baseline = Some(origin.y + y + last);
        }
        self.cursor = y + child.size.height;
        self.pending = margins.bottom;
        self.push_child(Point::new(x, origin.y + y), fragment);
    }

    /// Finishes the box: its height, its margins as its parent sees them,
    /// its list marker, and its overflow. Absolute boxes it holds for its
    /// descendants are still waiting in the fragment.
    pub(crate) fn finish(mut self, fonts: &mut Fonts<'_>) -> BoxFragment {
        let mut top = MarginStrut::new(self.own_top);
        let mut bottom = MarginStrut::new(self.own_bottom);
        let mut collapses_through = false;
        let auto_height = if self.at_top {
            // Nothing with content inside: whatever margins the empty
            // children had adjoin this box's top margin.
            top.append(self.pending);
            if self.may_collapse_through && !self.has_lines {
                collapses_through = true;
            }
            self.cursor
        } else {
            top.append(self.absorbed_top);
            if self.bottom_adjoining {
                bottom = self.pending;
                bottom.push(self.own_bottom);
                self.cursor
            } else {
                self.cursor + self.pending.resolve()
            }
        };
        let content_height = clamp(
            self.height.unwrap_or(auto_height),
            self.min_height,
            self.max_height,
        )
        .max(Au::ZERO);
        if self.centers_content && content_height > auto_height {
            self.center_content((content_height - auto_height).scale_by(0.5));
        }
        let size = Size::new(
            self.content_width + self.padding.horizontal() + self.border.horizontal(),
            content_height + self.padding.vertical() + self.border.vertical(),
        );
        self.margins.top = top;
        self.margins.bottom = bottom;
        self.margins.collapses_through = collapses_through;
        self.add_marker(fonts);
        let scrollable_overflow = self.scrollable_overflow(size);
        BoxFragment {
            box_id: self.id,
            node: self.node,
            size,
            children: self.children,
            scrollable_overflow,
            baseline: self.baseline,
            last_baseline: self.last_baseline,
            is_scroll_container: self.is_scroll_container,
            margins: self.margins,
            padding: self.padding,
            inline_edges: None,
            auto_height,
            relative_offset: self.relative_offset,
            absolutes: self.absolutes,
        }
    }

    /// Moves the placed content, its baselines and its absolute
    /// descendants' static positions down by `shift`.
    fn center_content(&mut self, shift: Au) {
        let by = Point::new(Au::ZERO, shift);
        for (at, _) in &mut self.children {
            *at = at.offset_by(by);
        }
        self.baseline = self.baseline.map(|b| b + shift);
        self.last_baseline = self.last_baseline.map(|b| b + shift);
        for absolute in &mut self.absolutes {
            *absolute = absolute.offset_by(by);
        }
    }

    /// Sets a list item's marker outside its first line: to the left of the
    /// content box with a gap of half an em, on the first line's baseline
    /// (spec 0004, child 0004, List markers). The marker takes no space.
    fn add_marker(&mut self, fonts: &mut Fonts<'_>) {
        let (Some(marker), Some(node)) = (self.marker.take(), self.node) else {
            return;
        };
        let style = &self.style;
        let runs = fonts.shape(&marker, &font_descriptor(style), style.font.size);
        let width: Au = runs.iter().map(|run| run.width()).sum();
        let origin = self.content_origin();
        let ascent = runs.first().map_or(Au::ZERO, |run| run.metrics.ascent);
        let baseline = self.baseline.unwrap_or(origin.y + ascent);
        let mut x = origin.x - style.font.size.scale_by(0.5) - width;
        for run in &runs {
            let fragment: TextFragment =
                text_fragment(node, &marker, style, run, 0..run.glyphs.len());
            let at = Point::new(x, baseline - fragment.baseline);
            x += fragment.size.width;
            self.children.push((at, Rc::new(Fragment::Text(fragment))));
        }
    }

    /// What the box and its children cover. A child that scrolls its own
    /// overflow counts with its border box only. A scroll container starts
    /// from its padding box and adds its end padding after the children,
    /// so its scroll range ends where the content plus padding ends.
    fn scrollable_overflow(&self, size: Size) -> Rect {
        let children = self.children.iter().map(|(at, child)| {
            let area = overflow_of(child);
            Rect::new(at.offset_by(area.origin), area.size)
        });
        let children = children.reduce(union);
        if !self.is_scroll_container {
            let border_box = Rect::new(Point::default(), size);
            return children.map_or(border_box, |c| union(border_box, c));
        }
        let padding_box = Rect::new(
            Point::new(self.border.left, self.border.top),
            Size::new(
                size.width - self.border.horizontal(),
                size.height - self.border.vertical(),
            ),
        );
        let Some(children) = children else {
            return padding_box;
        };
        let padded = Rect::new(
            children.origin,
            Size::new(
                children.size.width + self.padding.right,
                children.size.height + self.padding.bottom,
            ),
        );
        union(padding_box, padded)
    }
}

/// The area a fragment covers, from its own top left corner, as its
/// parent's overflow counts it.
pub(crate) fn overflow_of(fragment: &Fragment) -> Rect {
    match fragment {
        Fragment::Box(child) if !child.is_scroll_container => child.scrollable_overflow,
        Fragment::Line(line) => line
            .children
            .iter()
            .map(|(at, child)| {
                let area = overflow_of(child);
                Rect::new(at.offset_by(area.origin), area.size)
            })
            .fold(Rect::new(Point::default(), line.size), union),
        child => Rect::new(Point::default(), child.size()),
    }
}

/// The smallest rectangle covering both.
pub(crate) fn union(a: Rect, b: Rect) -> Rect {
    let left = a.origin.x.min(b.origin.x);
    let top = a.origin.y.min(b.origin.y);
    let right = (a.origin.x + a.size.width).max(b.origin.x + b.size.width);
    let bottom = (a.origin.y + a.size.height).max(b.origin.y + b.size.height);
    Rect::new(Point::new(left, top), Size::new(right - left, bottom - top))
}

/// The used left and right margins of a block level box whose border box
/// is `border_box` wide in a containing block `available` wide (CSS 2.2
/// section 10.3.3). `None` is `auto`. Both `auto` centers the box; one
/// `auto` takes what is left; none, and the right margin gives way.
fn used_margins(available: Au, border_box: Au, left: Option<Au>, right: Option<Au>) -> (Au, Au) {
    let fixed = left.unwrap_or(Au::ZERO) + right.unwrap_or(Au::ZERO);
    // A box wider than its containing block treats `auto` margins as zero.
    let (left, right) = if border_box + fixed > available {
        (
            Some(left.unwrap_or(Au::ZERO)),
            Some(right.unwrap_or(Au::ZERO)),
        )
    } else {
        (left, right)
    };
    let remaining = available - border_box;
    match (left, right) {
        (None, None) => {
            let half = Au(remaining.0 / 2);
            (half, remaining - half)
        }
        (None, Some(right)) => (remaining - right, right),
        (Some(left), _) => (left, remaining - left),
    }
}

/// A replaced box's content size (CSS 2.2 sections 10.3.2 and 10.6.2):
/// `width` and `height` where set, the other kept to the natural aspect
/// ratio, else the natural size, else zero; then the limits.
fn replaced_size(
    natural: NaturalSize,
    (width, height): (Option<Au>, Option<Au>),
    (min_width, max_width): (Au, Option<Au>),
    (min_height, max_height): (Au, Option<Au>),
) -> (Au, Au) {
    let ratio = match (natural.width, natural.height) {
        (Some(w), Some(h)) if w > Au::ZERO && h > Au::ZERO => Some(w.to_f32_px() / h.to_f32_px()),
        _ => None,
    };
    let natural_width = natural.width.unwrap_or(Au::ZERO);
    let natural_height = natural.height.unwrap_or(Au::ZERO);
    let (w, h) = match (width, height) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, ratio.map_or(natural_height, |r| w.scale_by(1.0 / r))),
        (None, Some(h)) => (ratio.map_or(natural_width, |r| h.scale_by(r)), h),
        (None, None) => (natural_width, natural_height),
    };
    (
        clamp(w, min_width, max_width),
        clamp(h, min_height, max_height),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(px: f32) -> Au {
        Au::from_px(px)
    }

    #[test]
    fn auto_margins_center_and_overflow_gives_way_on_the_right() {
        let available = px(100.0);
        assert_eq!(
            used_margins(available, px(60.0), None, None),
            (px(20.0), px(20.0))
        );
        assert_eq!(
            used_margins(available, px(60.0), Some(px(10.0)), None),
            (px(10.0), px(30.0))
        );
        assert_eq!(
            used_margins(available, px(60.0), None, Some(px(10.0))),
            (px(30.0), px(10.0))
        );
        // Over constrained: the right margin absorbs the difference.
        assert_eq!(
            used_margins(available, px(60.0), Some(px(10.0)), Some(px(10.0))),
            (px(10.0), px(30.0))
        );
        // Too wide: `auto` counts as zero.
        assert_eq!(
            used_margins(available, px(120.0), None, None),
            (Au::ZERO, px(-20.0))
        );
    }

    #[test]
    fn replaced_sizes_keep_the_natural_ratio() {
        let natural = NaturalSize {
            width: Some(px(200.0)),
            height: Some(px(100.0)),
        };
        let free = ((Au::ZERO, None), (Au::ZERO, None));
        let size = |w, h| replaced_size(natural, (w, h), free.0, free.1);
        assert_eq!(size(None, None), (px(200.0), px(100.0)));
        assert_eq!(size(Some(px(50.0)), None), (px(50.0), px(25.0)));
        assert_eq!(size(None, Some(px(50.0))), (px(100.0), px(50.0)));
        let unknown = replaced_size(NaturalSize::default(), (None, None), free.0, free.1);
        assert_eq!(unknown, (Au::ZERO, Au::ZERO));
        let limited = replaced_size(natural, (None, None), (Au::ZERO, Some(px(120.0))), free.1);
        assert_eq!(limited.0, px(120.0));
    }

    #[test]
    fn struts_take_the_largest_and_most_negative_margins() {
        let mut strut = MarginStrut::new(px(10.0));
        strut.push(px(20.0));
        assert_eq!(strut.resolve(), px(20.0));
        strut.push(px(-5.0));
        assert_eq!(strut.resolve(), px(15.0));
        strut.append(MarginStrut::new(px(-8.0)));
        assert_eq!(strut.resolve(), px(12.0));
    }
}
