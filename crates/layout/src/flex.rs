//! Flex layout (spec 0006, AC-3, AC-4): CSS Flexbox section 9 for the
//! chrome CSS profile's properties, in one line or many, along a row or a
//! column.
//!
//! The container lays its items out itself, with nested calls, because
//! the algorithm measures an item before it knows the item's final size:
//! its content size for the flex base size, then its cross size at the
//! resolved main size, then once more when it stretches. Every call goes
//! through the per box fragment cache, keyed by a [`ConstraintSpace`] that
//! carries the sizes flex fixes, so a stretched layout never reuses an
//! unstretched one.
//!
//! Each item starts its own formatting context, so no margin collapses
//! with the container or a sibling. Absolute children are not items: they
//! are handed on to their containing block. Baseline alignment is not in
//! the profile, and `align-content` behaves as its initial value, `normal`
//! (stretch).

use std::rc::Rc;

use base::{Au, BoxId, Point, SideOffsets};
use css::{AlignItems, BoxSizing, ComputedStyle, FlexBasis, FlexDirection, FlexWrap, Length};

use crate::block::{FlexFrame, FlexItems, InProgress, clamp, resolve};
use crate::box_tree::{BoxTree, LayoutBox};
use crate::fragment::{Fragment, PendingAbsolute};
use crate::{AvailableSize, ConstraintSpace, LayoutContext, border_widths, intrinsic, lay_out_box};

/// One flex item partway through the algorithm. Sizes are content box
/// sizes unless named outer (margin box) or border box.
struct Item {
    id: BoxId,
    style: Rc<ComputedStyle>,
    /// Main start, main end, cross start and cross end margins; `None` is
    /// `auto`.
    margins: [Option<Au>; 4],
    /// Border plus padding along each axis.
    edges_main: Au,
    edges_cross: Au,
    /// The main and cross size properties, when they are definite.
    size_main: Option<Au>,
    size_cross: Option<Au>,
    /// `None` for an `auto` minimum, resolved to a length later (AC-4).
    min_main: Option<Au>,
    max_main: Option<Au>,
    min_cross: Au,
    max_cross: Option<Au>,
    align: AlignItems,
    /// The flex base size and the hypothetical main size (section 9.2).
    base: Au,
    hypothetical: Au,
    /// The main size being resolved, then the used one (section 9.7).
    target: Au,
    frozen: bool,
    /// The outer hypothetical cross size, then the used outer one.
    outer_cross: Au,
    /// The item's last layout.
    fragment: Option<Rc<Fragment>>,
    /// It was laid out at its stretched cross size already.
    stretched: bool,
}

impl Item {
    fn margin(&self, side: usize) -> Au {
        self.margins[side].unwrap_or(Au::ZERO)
    }

    fn margins_main(&self) -> Au {
        self.margin(0) + self.margin(1)
    }

    fn margins_cross(&self) -> Au {
        self.margin(2) + self.margin(3)
    }

    /// Its margin box size along the main axis for a content size `main`.
    fn outer_main(&self, main: Au) -> Au {
        main + self.edges_main + self.margins_main()
    }

    fn min_main(&self) -> Au {
        self.min_main.unwrap_or(Au::ZERO)
    }

    /// Whether `align-self: stretch` sizes its cross axis: no definite
    /// cross size and no `auto` cross margin (section 9.4, step 11).
    fn stretches(&self) -> bool {
        self.align == AlignItems::Stretch
            && self.size_cross.is_none()
            && self.margins[2].is_some()
            && self.margins[3].is_some()
    }

    /// The border box cross size it stretches to in a line `line_cross`
    /// tall (or wide).
    fn stretched(&self, line_cross: Au) -> Au {
        let content = (line_cross - self.margins_cross() - self.edges_cross).max(Au::ZERO);
        clamp(content, self.min_cross, self.max_cross) + self.edges_cross
    }
}

/// One flex line: a run of items and its cross size.
struct Line {
    start: usize,
    end: usize,
    cross: Au,
}

/// Lays out the items of the flex container `container` and hands them to
/// it, placed.
pub(crate) fn lay_out(tree: &mut BoxTree, container: &mut InProgress, cx: &mut LayoutContext<'_>) {
    let Some(layout_box) = tree.get(container.id) else {
        return;
    };
    let style = Rc::clone(&layout_box.style);
    let child_ids = layout_box.children.clone();
    let frame = container.flex_frame();
    let space = container.child_space();
    let row = style.flex.direction == FlexDirection::Row;
    let flex = &style.flex;

    // Absolute children are not items; their static position is the
    // content box's start corner (spec 0006, Layout rules worth pinning).
    let (absolute_ids, item_ids): (Vec<BoxId>, Vec<BoxId>) = child_ids
        .iter()
        .partition(|&&id| tree.get(id).is_some_and(LayoutBox::is_absolute));
    let mut items: Vec<Item> = item_ids
        .iter()
        .filter_map(|&id| new_item(tree, id, &style, frame, row))
        .collect();
    // `order`, keeping tree order among equals (section 5.4).
    items.sort_by_key(|item| item.style.flex.order);

    let column_gap = flex.column_gap.resolve(Some(frame.content_width));
    let row_gap = flex.row_gap.resolve(frame.height);
    let (gap_main, gap_cross) = if row {
        (column_gap, row_gap)
    } else {
        (row_gap, column_gap)
    };
    let single_line = flex.wrap == FlexWrap::Nowrap;

    for item in &mut items {
        base_sizes(tree, item, frame, space, row, single_line, cx);
    }

    // Line breaking on outer hypothetical sizes (section 9.3, step 5).
    let available_main = if row {
        frame.content_width
    } else {
        frame.height.or(frame.max_height).unwrap_or(Au::MAX)
    };
    let mut lines = Vec::new();
    let mut start = 0;
    while start < items.len() {
        let mut end = start + 1;
        let mut used = items[start].outer_main(items[start].hypothetical);
        while end < items.len() {
            let next = used + gap_main + items[end].outer_main(items[end].hypothetical);
            if !single_line && next > available_main {
                break;
            }
            used = next;
            end += 1;
        }
        lines.push(Line {
            start,
            end,
            cross: Au::ZERO,
        });
        start = end;
    }

    // A column with an indefinite height is as tall as its longest line,
    // within its limits; its items then flex in that.
    let inner_main = if row {
        frame.content_width
    } else {
        frame.height.unwrap_or_else(|| {
            let longest = lines
                .iter()
                .map(|line| line_outer_main(&items[line.start..line.end], gap_main, false))
                .max()
                .unwrap_or(Au::ZERO);
            clamp(longest, frame.min_height, frame.max_height)
        })
    };
    for line in &lines {
        let gaps = gap_main * (line.end - line.start).saturating_sub(1) as i32;
        resolve_flexible_lengths(&mut items[line.start..line.end], inner_main - gaps);
    }

    // Hypothetical cross sizes (section 9.4, step 7).
    let definite_cross = if row {
        frame.height
    } else {
        Some(frame.content_width)
    };
    for item in &mut items {
        let known_stretch = (single_line && item.stretches())
            .then_some(definite_cross)
            .flatten()
            .map(|cross| item.stretched(cross));
        if row {
            let width = item.target + item.edges_main;
            item.stretched = known_stretch.is_some();
            item.fragment = measure(tree, item.id, space, Some(width), known_stretch, cx);
            let height = border_size(&item.fragment).1;
            item.outer_cross = height + item.margins_cross();
        } else {
            let width = column_cross(tree, item, frame, single_line, cx);
            item.stretched = item.stretches() && single_line;
            let height = item.target + item.edges_main;
            item.fragment = measure(tree, item.id, space, Some(width), Some(height), cx);
            item.outer_cross = width + item.margins_cross();
        }
    }

    // Line cross sizes (steps 8 and 15) and `align-content: normal`.
    for line in &mut lines {
        line.cross = match definite_cross {
            Some(cross) if single_line => cross,
            _ => {
                let tallest = items[line.start..line.end]
                    .iter()
                    .map(|item| item.outer_cross)
                    .max()
                    .unwrap_or(Au::ZERO);
                if single_line {
                    clamp(tallest, frame.min_height, frame.max_height)
                } else {
                    tallest
                }
            }
        };
    }
    let cross_gaps = gap_cross * lines.len().saturating_sub(1) as i32;
    let lines_cross = lines.iter().map(|line| line.cross).sum::<Au>() + cross_gaps;
    let container_cross =
        definite_cross.unwrap_or_else(|| clamp(lines_cross, frame.min_height, frame.max_height));
    let extra = container_cross - lines_cross;
    if !single_line && extra > Au::ZERO && !lines.is_empty() {
        let count = lines.len() as i32;
        for (i, line) in lines.iter_mut().enumerate() {
            line.cross += share(extra, count, i as i32);
        }
    }

    // Stretch (step 11): lay out again only when the size changed.
    for line in &lines {
        for item in &mut items[line.start..line.end] {
            if !item.stretches() || item.stretched {
                continue;
            }
            let stretched = item.stretched(line.cross);
            let (width, height) = border_size(&item.fragment);
            item.outer_cross = stretched + item.margins_cross();
            if row && stretched != height {
                item.fragment = measure(tree, item.id, space, Some(width), Some(stretched), cx);
            } else if !row && stretched != width {
                item.fragment = measure(tree, item.id, space, Some(stretched), Some(height), cx);
            }
        }
    }

    let mut placed = place(
        &mut items, &lines, inner_main, gap_main, gap_cross, row, &style,
    );
    placed
        .absolutes
        .extend(absolute_ids.into_iter().map(|box_id| PendingAbsolute {
            box_id,
            static_position: Point::default(),
        }));
    let height = if row { container_cross } else { inner_main };
    container.set_flex_items(FlexItems { height, ..placed }, child_ids.len());
}

/// An item's sizes and margins from its style, against the container's
/// content box.
fn new_item(
    tree: &BoxTree,
    id: BoxId,
    container: &ComputedStyle,
    frame: FlexFrame,
    row: bool,
) -> Option<Item> {
    let layout_box = tree.get(id)?;
    let style = Rc::clone(&layout_box.style);
    // Padding and margins resolve against the container's width on every
    // side, as in block layout.
    let basis_width = Some(frame.content_width);
    let padding = &style.padding;
    let padding = SideOffsets {
        top: padding.top.resolve(basis_width),
        right: padding.right.resolve(basis_width),
        bottom: padding.bottom.resolve(basis_width),
        left: padding.left.resolve(basis_width),
    };
    let border = border_widths(&style);
    let edges_x = border.horizontal() + padding.horizontal();
    let edges_y = border.vertical() + padding.vertical();
    let border_box = style.size.box_sizing == BoxSizing::BorderBox;
    let content = move |length: Option<Length>, basis: Option<Au>, edges: Au| {
        resolve(length, basis).map(|value| {
            if border_box {
                (value - edges).max(Au::ZERO)
            } else {
                value
            }
        })
    };
    let size = &style.size;
    let width = content(size.width, basis_width, edges_x);
    let height = content(size.height, frame.height, edges_y);
    let min_width = content(size.min_width, basis_width, edges_x);
    let min_height = content(size.min_height, frame.height, edges_y);
    let max_width = content(size.max_width, basis_width, edges_x);
    let max_height = content(size.max_height, frame.height, edges_y);
    let margin = |length: Option<Length>| length.map(|l| l.resolve(basis_width));
    let margins = &style.margin;
    let (left, right) = (margin(margins.left), margin(margins.right));
    let (top, bottom) = (margin(margins.top), margin(margins.bottom));
    let align = style.flex.align_self.unwrap_or(container.flex.align_items);
    let mut item = Item {
        id,
        style: Rc::clone(&style),
        margins: [left, right, top, bottom],
        edges_main: edges_x,
        edges_cross: edges_y,
        size_main: width,
        size_cross: height,
        min_main: min_width,
        max_main: max_width,
        min_cross: min_height.unwrap_or(Au::ZERO),
        max_cross: max_height,
        align,
        base: Au::ZERO,
        hypothetical: Au::ZERO,
        target: Au::ZERO,
        frozen: false,
        outer_cross: Au::ZERO,
        fragment: None,
        stretched: false,
    };
    if !row {
        item.margins = [top, bottom, left, right];
        item.edges_main = edges_y;
        item.edges_cross = edges_x;
        item.size_main = height;
        item.size_cross = width;
        item.min_main = min_height;
        item.max_main = max_height;
        item.min_cross = min_width.unwrap_or(Au::ZERO);
        item.max_cross = max_width;
    }
    // A scroll container's automatic minimum is zero (AC-4).
    if item.min_main.is_none() && style.display_box.is_scroll_container() {
        item.min_main = Some(Au::ZERO);
    }
    Some(item)
}

/// The flex base size, the automatic minimum and the hypothetical main
/// size (section 9.2, step 3, and section 4.5).
fn base_sizes(
    tree: &mut BoxTree,
    item: &mut Item,
    frame: FlexFrame,
    space: ConstraintSpace,
    row: bool,
    single_line: bool,
    cx: &mut LayoutContext<'_>,
) {
    let main_basis = if row {
        Some(frame.content_width)
    } else {
        frame.height
    };
    let border_box = item.style.size.box_sizing == BoxSizing::BorderBox;
    let basis = match item.style.flex.basis {
        FlexBasis::Length(length) => resolve(Some(length), main_basis).map(|value| {
            if border_box {
                (value - item.edges_main).max(Au::ZERO)
            } else {
                value
            }
        }),
        FlexBasis::Auto => item.size_main,
        FlexBasis::Content => None,
    };
    let needs_content = basis.is_none() || item.min_main.is_none();
    // The content's min and max content sizes along the main axis.
    let (min_content, max_content) = match (needs_content, row) {
        (false, _) => (Au::ZERO, Au::ZERO),
        (true, true) => intrinsic::content_sizes(tree, item.id, cx),
        (true, false) => {
            // A block's min and max content heights are both its height
            // at the width it gets, before its own height applies.
            let width = column_cross(tree, item, frame, single_line, cx);
            let fragment = measure(tree, item.id, space, Some(width), None, cx);
            let height = fragment
                .as_deref()
                .and_then(Fragment::as_box)
                .map_or(Au::ZERO, |b| b.auto_height);
            (height, height)
        }
    };
    item.base = basis.unwrap_or(max_content);
    if item.min_main.is_none() {
        // The content size suggestion, capped by the specified size
        // suggestion, then by the maximum.
        let suggestion = item
            .size_main
            .map_or(min_content, |size| size.min(min_content));
        item.min_main = Some(item.max_main.map_or(suggestion, |max| suggestion.min(max)));
    }
    item.hypothetical = clamp(item.base, item.min_main(), item.max_main);
}

/// A column item's border box width: its own width, its stretched width
/// in a single line, or else its content's width fitted to the container
/// (section 9.4, step 7, `fit-content`).
fn column_cross(
    tree: &BoxTree,
    item: &Item,
    frame: FlexFrame,
    single_line: bool,
    cx: &mut LayoutContext<'_>,
) -> Au {
    if let Some(width) = item.size_cross {
        return clamp(width, item.min_cross, item.max_cross) + item.edges_cross;
    }
    if single_line && item.stretches() {
        return item.stretched(frame.content_width);
    }
    let (min_content, max_content) = intrinsic::content_sizes(tree, item.id, cx);
    let available = frame.content_width - item.margins_cross() - item.edges_cross;
    let fit = max_content.min(min_content.max(available));
    clamp(fit, item.min_cross, item.max_cross) + item.edges_cross
}

/// Lays out item `id` in its container's child `space`, with the border
/// box sizes flex fixed.
fn measure(
    tree: &mut BoxTree,
    id: BoxId,
    space: ConstraintSpace,
    width: Option<Au>,
    height: Option<Au>,
    cx: &mut LayoutContext<'_>,
) -> Option<Rc<Fragment>> {
    let space = ConstraintSpace {
        available_inline: width.map_or(space.available_inline, AvailableSize::Definite),
        stretch_inline: width,
        stretch_block: height,
        ..space
    };
    lay_out_box(tree, id, space, cx)
}

/// A fragment's border box width and height, zero for none.
fn border_size(fragment: &Option<Rc<Fragment>>) -> (Au, Au) {
    fragment
        .as_deref()
        .map_or((Au::ZERO, Au::ZERO), |fragment| {
            let size = fragment.size();
            (size.width, size.height)
        })
}

/// The outer main sizes of a line's items plus the gaps between them: the
/// hypothetical sizes, or the used ones.
fn line_outer_main(items: &[Item], gap: Au, used: bool) -> Au {
    let sizes: Au = items
        .iter()
        .map(|item| item.outer_main(if used { item.target } else { item.hypothetical }))
        .sum();
    sizes + gap * items.len().saturating_sub(1) as i32
}

/// Resolves the flexible lengths of one line's items in `available`
/// space, the line's gaps already taken out (section 9.7).
fn resolve_flexible_lengths(items: &mut [Item], available: Au) {
    let growing = line_outer_main(items, Au::ZERO, false) < available;
    let factor = |item: &Item| {
        if growing {
            item.style.flex.grow
        } else {
            item.style.flex.shrink
        }
    };
    // Step 2: items that cannot flex keep their hypothetical size.
    for item in items.iter_mut() {
        item.frozen = factor(item) == 0.0
            || (growing && item.base > item.hypothetical)
            || (!growing && item.base < item.hypothetical);
        item.target = if item.frozen {
            item.hypothetical
        } else {
            item.base
        };
    }
    let free_space = |items: &[Item]| -> f64 {
        let used: Au = items
            .iter()
            .map(|item| item.outer_main(if item.frozen { item.target } else { item.base }))
            .sum();
        f64::from((available - used).0)
    };
    let initial_free = free_space(items);
    // Each pass freezes at least one item, so this ends.
    while items.iter().any(|item| !item.frozen) {
        let mut free = free_space(items);
        let factors: f64 = items
            .iter()
            .filter(|item| !item.frozen)
            .map(|item| f64::from(factor(item)))
            .sum();
        if factors < 1.0 && (initial_free * factors).abs() < free.abs() {
            free = initial_free * factors;
        }
        let scaled_shrink: f64 = items
            .iter()
            .filter(|item| !item.frozen)
            .map(|item| f64::from(item.style.flex.shrink) * f64::from(item.base.0))
            .sum();
        let mut total_violation = 0i64;
        let mut violations = Vec::with_capacity(items.len());
        for item in items.iter_mut() {
            if item.frozen {
                violations.push(0);
                continue;
            }
            let share = if growing && factors > 0.0 {
                free * f64::from(item.style.flex.grow) / factors
            } else if !growing && scaled_shrink > 0.0 {
                free * f64::from(item.style.flex.shrink) * f64::from(item.base.0) / scaled_shrink
            } else {
                0.0
            };
            let target = Au((f64::from(item.base.0) + share).round() as i32);
            let clamped = clamp(target, item.min_main(), item.max_main).max(Au::ZERO);
            let violation = i64::from((clamped - target).0);
            total_violation += violation;
            violations.push(violation);
            item.target = clamped;
        }
        for (item, violation) in items.iter_mut().zip(violations) {
            let freeze = match total_violation.signum() {
                0 => true,
                1 => violation > 0,
                _ => violation < 0,
            };
            item.frozen |= freeze;
        }
    }
}

/// Places every line's items along both axes (section 9.5, step 12, and
/// section 9.6, steps 13 and 14): `auto` margins, `justify-content`, then
/// the cross axis `auto` margins and `align-self`.
fn place(
    items: &mut [Item],
    lines: &[Line],
    inner_main: Au,
    gap_main: Au,
    gap_cross: Au,
    row: bool,
    style: &ComputedStyle,
) -> FlexItems {
    let mut out = FlexItems::default();
    let mut line_offset = Au::ZERO;
    for line in lines {
        let line_items = &mut items[line.start..line.end];
        let mut free = inner_main - line_outer_main(line_items, gap_main, true);
        let auto_margins = line_items
            .iter()
            .flat_map(|item| [item.margins[0], item.margins[1]])
            .filter(Option::is_none)
            .count() as i32;
        let auto_margin = if free > Au::ZERO && auto_margins > 0 {
            let each = Au(free.0 / auto_margins);
            free = Au::ZERO;
            each
        } else {
            Au::ZERO
        };
        let count = line_items.len() as i32;
        let (mut main, between) = justify(style.flex.justify_content, free, count);
        for item in line_items.iter_mut() {
            let start = item.margins[0].unwrap_or(auto_margin);
            let end = item.margins[1].unwrap_or(auto_margin);
            main += start;
            let cross = line_offset + cross_offset(item, line.cross);
            let at = if row {
                Point::new(main, cross)
            } else {
                Point::new(cross, main)
            };
            main += item.target + item.edges_main + end + gap_main + between;
            let Some(fragment) = item.fragment.take() else {
                continue;
            };
            let at = fragment
                .as_box()
                .map_or(at, |child| at.offset_by(child.relative_offset));
            if let Some(child) = fragment.as_box() {
                out.absolutes
                    .extend(child.absolutes.iter().map(|a| a.offset_by(at)));
                if out.baseline.is_none() {
                    out.baseline = child.baseline.map(|b| at.y + b);
                }
                if let Some(last) = child.last_baseline {
                    out.last_baseline = Some(at.y + last);
                }
            }
            out.children.push((at, fragment));
        }
        line_offset += line.cross + gap_cross;
    }
    out
}

/// Where an item's border box starts across its line: its cross start
/// margin plus the free space `auto` margins or `align-self` give it.
fn cross_offset(item: &Item, line_cross: Au) -> Au {
    let free = line_cross - item.outer_cross;
    let (start, end) = (item.margins[2], item.margins[3]);
    let margin_start = start.unwrap_or(Au::ZERO);
    if start.is_none() || end.is_none() {
        if free <= Au::ZERO {
            return margin_start;
        }
        return match (start, end) {
            (None, None) => Au(free.0 / 2),
            (None, Some(_)) => free,
            _ => Au::ZERO,
        } + margin_start;
    }
    margin_start
        + match item.align {
            AlignItems::Stretch | AlignItems::FlexStart => Au::ZERO,
            AlignItems::FlexEnd => free,
            AlignItems::Center => Au(free.0 / 2),
        }
}

/// The offset before a line's first item and the extra space between two
/// items, for `free` space over `count` items. Negative free space falls
/// back as the spec says: `space-between` to `flex-start`, the other
/// spacing keywords to `center`.
fn justify(justify: css::JustifyContent, free: Au, count: i32) -> (Au, Au) {
    use css::JustifyContent as J;
    let half = Au(free.0 / 2);
    match justify {
        J::FlexStart => (Au::ZERO, Au::ZERO),
        J::FlexEnd => (free, Au::ZERO),
        J::Center => (half, Au::ZERO),
        J::SpaceBetween if free > Au::ZERO && count > 1 => (Au::ZERO, Au(free.0 / (count - 1))),
        J::SpaceBetween => (Au::ZERO, Au::ZERO),
        J::SpaceAround if free > Au::ZERO => {
            let each = Au(free.0 / count.max(1));
            (Au(each.0 / 2), each)
        }
        J::SpaceEvenly if free > Au::ZERO => {
            let each = Au(free.0 / (count + 1));
            (each, each)
        }
        J::SpaceAround | J::SpaceEvenly => (half, Au::ZERO),
    }
}

/// Part `index` of `total` split into `count` parts whose sum is `total`
/// exactly: the remainder goes to the first parts.
fn share(total: Au, count: i32, index: i32) -> Au {
    let count = count.max(1);
    let each = total.0 / count;
    let rest = total.0 % count;
    Au(each + i32::from(index < rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(px: f32) -> Au {
        Au::from_px(px)
    }

    #[test]
    fn shares_add_up_to_the_total() {
        let parts: Vec<Au> = (0..3).map(|i| share(Au(100), 3, i)).collect();
        assert_eq!(parts, [Au(34), Au(33), Au(33)]);
        assert_eq!(parts.into_iter().sum::<Au>(), Au(100));
    }

    #[test]
    fn negative_free_space_falls_back_from_the_spacing_keywords() {
        use css::JustifyContent as J;
        assert_eq!(justify(J::SpaceBetween, px(-10.0), 3), (Au::ZERO, Au::ZERO));
        assert_eq!(justify(J::SpaceAround, px(-10.0), 3), (px(-5.0), Au::ZERO));
        assert_eq!(justify(J::SpaceEvenly, px(30.0), 2), (px(10.0), px(10.0)));
        assert_eq!(justify(J::SpaceAround, px(30.0), 3), (px(5.0), px(10.0)));
        assert_eq!(justify(J::Center, px(30.0), 3), (px(15.0), Au::ZERO));
    }
}
