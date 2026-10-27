//! Positioned layout (spec 0006, AC-5): relative offsets, and absolute
//! boxes laid out against their containing block.
//!
//! An absolute box takes no space in flow. Where it would have sat, its
//! parent records a [`PendingAbsolute`] with its static position, and the
//! fragments above hand it on until it reaches its containing block: the
//! nearest positioned ancestor, else the root, which stands in for the
//! initial containing block (the viewport). Once that box's own size is
//! final, it lays its absolute boxes out against its padding box and adds
//! their fragments to its children, after the in flow ones (spec 0006,
//! Layout rules worth pinning, Hoisting). Paint and hit testing then walk
//! one tree, and an `overflow: hidden` box clips exactly the absolute boxes
//! whose containing block is inside it (AC-7).
//!
//! Widths and heights follow CSS 2.2 sections 10.3.7 and 10.6.4, left to
//! right only: both insets set and an `auto` size fills the space between
//! them, otherwise an `auto` width shrinks to fit and an `auto` height
//! takes the content's.

use std::rc::Rc;

use base::{Au, BoxId, Point, Rect, Size};
use css::{BoxSizing, ComputedStyle, Length, Position};

use crate::block::{clamp, overflow_of, resolve, union};
use crate::box_tree::{BoxKind, BoxTree};
use crate::fragment::{BlockMargins, BoxFragment, Fragment, PendingAbsolute};
use crate::{
    AvailableSize, ConstraintSpace, LayoutContext, MAX_NESTING, border_widths, intrinsic,
    lay_out_box,
};

/// How far `position: relative` moves a box whose containing block is
/// `basis` (its parent's content box): `left` else minus `right`, and `top`
/// else minus `bottom`. A percentage against an indefinite basis is zero.
pub(crate) fn relative_offset(style: &ComputedStyle, basis: Size<Option<Au>>) -> Point {
    if style.display_box.position != Position::Relative {
        return Point::default();
    }
    let insets = &style.position_offsets;
    let axis = |start: Option<Length>, end: Option<Length>, basis: Option<Au>| {
        let side =
            |length: Option<Length>| resolve(length.clone(), basis).or(length.map(|_| Au::ZERO));
        match (side(start), side(end)) {
            (Some(start), _) => start,
            (None, Some(end)) => -end,
            (None, None) => Au::ZERO,
        }
    };
    Point::new(
        axis(insets.left.clone(), insets.right.clone(), basis.width),
        axis(insets.top.clone(), insets.bottom.clone(), basis.height),
    )
}

/// The rectangle the absolute boxes `fragment` holds are laid out in, from
/// its border box corner, or `None` when it holds none: a positioned box's
/// padding box, or for a root that is not positioned, the initial
/// containing block. The root sits at its margins in the viewport, so the
/// viewport starts that far up and to the left of it.
pub(crate) fn containing_block(
    tree: &BoxTree,
    id: BoxId,
    fragment: &BoxFragment,
    space: ConstraintSpace,
) -> Option<Rect> {
    let style = &tree.get(id)?.style;
    // A grid container establishes a containing block for its abspos
    // children even when it is not positioned (Grid §9; spec 0010, AC-5).
    let is_grid = matches!(
        style.display_box.display,
        css::Display::Grid | css::Display::InlineGrid
    );
    if style.display_box.is_positioned() || is_grid {
        let border = border_widths(style);
        return Some(Rect::new(
            Point::new(border.left, border.top),
            Size::new(
                fragment.size.width - border.horizontal(),
                fragment.size.height - border.vertical(),
            ),
        ));
    }
    if tree.root() != Some(id) {
        return None;
    }
    let margins = fragment.margins;
    let viewport = Size::new(
        space.available_inline.definite().unwrap_or(Au::ZERO),
        space.available_block.definite().unwrap_or(Au::ZERO),
    );
    Some(Rect::new(
        Point::new(-margins.left, -margins.top.resolve()),
        viewport,
    ))
}

/// The initial containing block (the viewport), anchored at the root's
/// border box corner. Fixed boxes lay out against this and add nothing to
/// the viewport's scrollable overflow (spec 0009, child 0003, AC-10).
pub(crate) fn viewport_containing_block(margins: &BlockMargins, space: &ConstraintSpace) -> Rect {
    let viewport = Size::new(
        space.available_inline.definite().unwrap_or(Au::ZERO),
        space.available_block.definite().unwrap_or(Au::ZERO),
    );
    Rect::new(Point::new(-margins.left, -margins.top.resolve()), viewport)
}

/// Lays out the absolute boxes waiting in `fragment` against `cb` and adds
/// them to its children and its scrollable overflow. Past the nesting cap
/// they are left out. Fixed boxes are not laid out here: they hoist to the
/// viewport and are laid out by [`lay_out_fixed`] at the root.
pub(crate) fn lay_out_absolutes(
    tree: &mut BoxTree,
    fragment: &mut BoxFragment,
    cb: Rect,
    cx: &mut LayoutContext<'_>,
) {
    let pending = std::mem::take(&mut fragment.absolutes);
    if pending.is_empty() || cx.depth >= MAX_NESTING {
        return;
    }
    cx.depth += 1;
    for absolute in pending {
        if absolute.fixed {
            // Fixed boxes hoist to the viewport: keep them so the root lays
            // them out against the initial containing block.
            fragment.absolutes.push(absolute);
            continue;
        }
        // A grid item's area overrides the container's padding box for an
        // absolutely positioned child with a definite area (AC-5).
        let cb = absolute.containing_block.unwrap_or(cb);
        if let Some(placed) = lay_out_absolute(tree, absolute, cb, cx) {
            let (at, child) = &placed;
            let area = overflow_of(child);
            let area = Rect::new(at.offset_by(area.origin), area.size);
            fragment.scrollable_overflow = union(fragment.scrollable_overflow, area);
            fragment.children.push(placed);
        }
    }
    cx.depth -= 1;
}

/// Lays out the fixed boxes waiting in `fragment` against `cb` (the
/// viewport's initial containing block) and adds them to its children.
/// Fixed boxes paint in root space and add nothing to the viewport's
/// scrollable overflow (spec 0009, child 0003, AC-10). Past the nesting cap
/// they are left out.
pub(crate) fn lay_out_fixed(
    tree: &mut BoxTree,
    fragment: &mut BoxFragment,
    cb: Rect,
    cx: &mut LayoutContext<'_>,
) {
    let pending = std::mem::take(&mut fragment.absolutes);
    if pending.is_empty() || cx.depth >= MAX_NESTING {
        return;
    }
    cx.depth += 1;
    for absolute in pending {
        if !absolute.fixed {
            // Non fixed boxes were already laid out at their containing
            // block; keep them for any ancestor that needs them.
            fragment.absolutes.push(absolute);
            continue;
        }
        if let Some(placed) = lay_out_absolute(tree, absolute, cb, cx) {
            fragment.children.push(placed);
        }
    }
    cx.depth -= 1;
}

/// One axis of an absolute box: its two insets, its margins (`None` is
/// `auto`), its border plus padding, and its size.
struct Axis {
    start: Option<Au>,
    end: Option<Au>,
    margin_start: Option<Au>,
    margin_end: Option<Au>,
    edges: Au,
    /// The content size the size property gives, clamped.
    size: Option<Au>,
    min: Au,
    max: Option<Au>,
}

impl Axis {
    fn margins(&self) -> Au {
        self.margin_start.unwrap_or(Au::ZERO) + self.margin_end.unwrap_or(Au::ZERO)
    }

    /// The content size an `auto` size fills between two set insets in
    /// `cb` (CSS 2.2 sections 10.3.7 and 10.6.4, `auto` margins as zero).
    fn fill(&self, cb: Au) -> Option<Au> {
        let (start, end) = (self.start?, self.end?);
        Some((cb - start - end - self.margins() - self.edges).max(Au::ZERO))
    }

    /// Where the border box starts, from the containing block's start
    /// edge, for a border box `outer` long in `cb`. `static_start` is the
    /// static position from that edge. With both insets and the size set,
    /// `auto` margins share what is left; over constrained, the end inset
    /// gives way.
    fn place(&self, outer: Au, cb: Au, static_start: Au, size_set: bool) -> Au {
        let mut margin_start = self.margin_start.unwrap_or(Au::ZERO);
        let mut margin_end = self.margin_end.unwrap_or(Au::ZERO);
        if let (Some(start), Some(end), true) = (self.start, self.end, size_set) {
            let left = cb - start - end - outer - margin_start - margin_end;
            match (self.margin_start, self.margin_end) {
                (None, None) if left >= Au::ZERO => {
                    margin_start = Au(left.0 / 2);
                }
                (None, None) => margin_end = left,
                (None, Some(_)) => margin_start = left,
                (Some(_), None) => margin_end = left,
                (Some(_), Some(_)) => {}
            }
        }
        match (self.start, self.end) {
            (Some(start), _) => start + margin_start,
            (None, Some(end)) => cb - end - margin_end - outer,
            (None, None) => static_start + margin_start,
        }
    }
}

/// The insets, margins, edges and sizes of an absolute box with `style`
/// along one axis of a containing block of size `cb`; `horizontal` picks
/// the axis. Margins and padding resolve against the containing block's
/// width on both axes.
fn axis(style: &ComputedStyle, cb: Size, horizontal: bool) -> Axis {
    let basis_width = Some(cb.width);
    let along = Some(if horizontal { cb.width } else { cb.height });
    let insets = &style.position_offsets;
    let margin = &style.margin;
    let padding = &style.padding;
    let size = &style.size;
    let border = border_widths(style);
    let (start, end, margin_start, margin_end, edges, value, min, max) = if horizontal {
        (
            insets.left.clone(),
            insets.right.clone(),
            margin.left.clone(),
            margin.right.clone(),
            border.horizontal()
                + padding.left.resolve(basis_width)
                + padding.right.resolve(basis_width),
            size.width.clone(),
            size.min_width.clone(),
            size.max_width.clone(),
        )
    } else {
        (
            insets.top.clone(),
            insets.bottom.clone(),
            margin.top.clone(),
            margin.bottom.clone(),
            border.vertical()
                + padding.top.resolve(basis_width)
                + padding.bottom.resolve(basis_width),
            size.height.clone(),
            size.min_height.clone(),
            size.max_height.clone(),
        )
    };
    let border_box = size.box_sizing == BoxSizing::BorderBox;
    let content = |length: Option<Length>| {
        resolve(length, along).map(|value| {
            if border_box {
                (value - edges).max(Au::ZERO)
            } else {
                value
            }
        })
    };
    let min = content(min).unwrap_or(Au::ZERO);
    let max = content(max);
    Axis {
        start: start.map(|l| l.resolve(along)),
        end: end.map(|l| l.resolve(along)),
        margin_start: margin_start.map(|l| l.resolve(basis_width)),
        margin_end: margin_end.map(|l| l.resolve(basis_width)),
        edges,
        size: content(value).map(|value| clamp(value, min, max)),
        min,
        max,
    }
}

/// Lays out one absolute box against `cb` (from the containing block's
/// border box corner) and says where its border box goes.
fn lay_out_absolute(
    tree: &mut BoxTree,
    absolute: PendingAbsolute,
    cb: Rect,
    cx: &mut LayoutContext<'_>,
) -> Option<(Point, Rc<Fragment>)> {
    let layout_box = tree.get(absolute.box_id)?;
    let style = Rc::clone(&layout_box.style);
    let replaced = layout_box.kind == BoxKind::Replaced;
    let x_axis = axis(&style, cb.size, true);
    let y_axis = axis(&style, cb.size, false);
    let static_x = absolute.static_position.x - cb.origin.x;
    let static_y = absolute.static_position.y - cb.origin.y;

    // A replaced box keeps its natural size rules; any other box gets the
    // width these rules give, fixed by the space like a flex item's.
    let width = if replaced {
        None
    } else {
        Some(
            x_axis
                .size
                .or_else(|| x_axis.fill(cb.size.width))
                .unwrap_or_else(|| {
                    let start = match (x_axis.start, x_axis.end) {
                        (Some(start), _) => start,
                        (None, Some(_)) => Au::ZERO,
                        (None, None) => static_x,
                    };
                    let end = x_axis.end.unwrap_or(Au::ZERO);
                    let available = (cb.size.width - start - end - x_axis.margins() - x_axis.edges)
                        .max(Au::ZERO);
                    let (min_content, max_content) =
                        intrinsic::content_sizes(tree, absolute.box_id, cx);
                    let fit = max_content.min(min_content.max(available));
                    clamp(fit, x_axis.min, x_axis.max)
                }),
        )
    };
    let height = match (replaced, y_axis.size) {
        (false, None) => y_axis.fill(cb.size.height),
        _ => None,
    };
    let space = ConstraintSpace {
        available_inline: AvailableSize::Definite(cb.size.width),
        available_block: AvailableSize::Definite(cb.size.height),
        percentage_basis: Size::new(Some(cb.size.width), Some(cb.size.height)),
        writing_mode: style.inherited_text.writing_mode,
        stretch_inline: width.map(|w| w + x_axis.edges),
        stretch_block: height.map(|h| h + y_axis.edges),
        bfc_offset: None,
        exclusions: None,
        clearance: None,
    };
    let fragment = lay_out_box(tree, absolute.box_id, space, cx)?;
    let outer = fragment.size();
    let x = x_axis.place(
        outer.width,
        cb.size.width,
        static_x,
        replaced || style.size.width.is_some(),
    );
    let y = y_axis.place(
        outer.height,
        cb.size.height,
        static_y,
        replaced || style.size.height.is_some(),
    );
    Some((Point::new(cb.origin.x + x, cb.origin.y + y), fragment))
}

#[cfg(test)]
mod tests {
    use css::Padding;

    use super::*;

    fn px(px: f32) -> Au {
        Au::from_px(px)
    }

    #[test]
    fn relative_offsets_prefer_left_and_top_and_ignore_static_boxes() {
        let mut style = ComputedStyle::default();
        let offsets = Rc::make_mut(&mut style.position_offsets);
        offsets.left = Some(Length::px(4.0));
        offsets.right = Some(Length::px(50.0));
        offsets.bottom = Some(Length::Percent(10.0));
        let basis = Size::new(Some(px(200.0)), Some(px(100.0)));
        assert_eq!(relative_offset(&style, basis), Point::default());
        Rc::make_mut(&mut style.display_box).position = Position::Relative;
        assert_eq!(
            relative_offset(&style, basis),
            Point::new(px(4.0), px(-10.0))
        );
        let indefinite = Size::new(Some(px(200.0)), None);
        assert_eq!(
            relative_offset(&style, indefinite),
            Point::new(px(4.0), Au::ZERO)
        );
    }

    #[test]
    fn auto_margins_center_between_set_insets() {
        let mut style = ComputedStyle::default();
        let offsets = Rc::make_mut(&mut style.position_offsets);
        offsets.left = Some(Length::px(10.0));
        offsets.right = Some(Length::px(10.0));
        Rc::make_mut(&mut style.margin).left = None;
        Rc::make_mut(&mut style.margin).right = None;
        Rc::make_mut(&mut style.size).width = Some(Length::px(80.0));
        style.padding = Rc::new(Padding::all(Length::px(5.0)));
        let cb = Size::new(px(200.0), px(50.0));
        let x = axis(&style, cb, true);
        assert_eq!(x.size, Some(px(80.0)));
        assert_eq!(x.edges, px(10.0), "5px of padding a side");
        // 200 - 10 - 10 - 90 leaves 90, half on each side.
        assert_eq!(x.place(px(90.0), cb.width, Au::ZERO, true), px(55.0));
        // Without a width the box fills, and the margins count as zero.
        assert_eq!(x.fill(cb.width), Some(px(170.0)));
    }
}
