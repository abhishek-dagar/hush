//! Intrinsic sizes: the min content and max content widths an inline block
//! shrinks to fit between and a row flex item's content size comes from
//! (spec 0004, child 0004, Intrinsic sizes; spec 0006, AC-3).
//!
//! A box's sizes depend on its children's, so they are computed bottom up
//! with an explicit stack (a deep page cannot overflow the thread stack),
//! and kept in `LayoutCache::intrinsic` while the box stays clean.

use std::collections::HashMap;

use base::{Au, BoxId};
use css::{BoxSizing, ComputedStyle, FlexDirection, FlexWrap, Length};

use crate::box_tree::{BoxKind, BoxTree, InlineItem, LayoutBox};
use crate::{LayoutContext, border_widths, inline};

/// An absolute length; a percentage counts as unknown when sizing to
/// content.
fn absolute(length: Option<Length>) -> Option<Au> {
    match length? {
        Length::Au(au) => Some(au),
        Length::Percent(_) | Length::Calc(_) => None,
    }
}

/// The min and max content widths of `id`'s content box.
pub(crate) fn content_sizes(tree: &BoxTree, id: BoxId, cx: &mut LayoutContext<'_>) -> (Au, Au) {
    let mut stack = vec![(id, false)];
    while let Some((current, expanded)) = stack.pop() {
        if known(tree, current, cx).is_some() {
            continue;
        }
        let Some(layout_box) = tree.get(current) else {
            continue;
        };
        if !expanded {
            stack.push((current, true));
            stack.extend(
                dependencies(tree, layout_box)
                    .filter(|&dep| known(tree, dep, cx).is_none())
                    .map(|dep| (dep, false)),
            );
            continue;
        }
        let sizes = compute(tree, current, layout_box, cx);
        cx.intrinsic.insert(current, sizes);
    }
    known(tree, id, cx).unwrap_or_default()
}

/// Sizes already known: computed this pass, or cached on a clean box.
fn known(tree: &BoxTree, id: BoxId, cx: &LayoutContext<'_>) -> Option<(Au, Au)> {
    if let Some(&sizes) = cx.intrinsic.get(&id) {
        return Some(sizes);
    }
    let layout_box = tree.get(id)?;
    if layout_box.flags != crate::BoxFlags::empty() {
        return None;
    }
    layout_box.cache.as_ref()?.intrinsic
}

/// The boxes whose sizes `layout_box`'s depend on: its children and its
/// atomic inlines. Absolute boxes take no space, so they are not among
/// them (spec 0006, AC-5).
fn dependencies<'a>(
    tree: &'a BoxTree,
    layout_box: &'a LayoutBox,
) -> impl Iterator<Item = BoxId> + 'a {
    let atomics = layout_box.inline.iter().filter_map(|item| match item {
        InlineItem::Atomic(id) => Some(*id),
        _ => None,
    });
    in_flow_children(tree, layout_box).chain(atomics)
}

/// A box's children without its absolute ones.
fn in_flow_children<'a>(
    tree: &'a BoxTree,
    layout_box: &'a LayoutBox,
) -> impl Iterator<Item = BoxId> + 'a {
    layout_box
        .children
        .iter()
        .copied()
        .filter(|&child| !tree.get(child).is_some_and(LayoutBox::is_absolute))
}

fn compute(
    tree: &BoxTree,
    id: BoxId,
    layout_box: &LayoutBox,
    cx: &mut LayoutContext<'_>,
) -> (Au, Au) {
    if layout_box.text_field {
        // The HTML `size` default: 20 times the advance of `0` (spec 0006,
        // AC-17), whatever text the field holds.
        let style = &layout_box.style;
        let zero = cx
            .fonts
            .shape("0", &inline::font_descriptor(style), style.font.size)
            .iter()
            .fold(Au::ZERO, |width, run| width + run.width());
        let width = zero * 20;
        return (width, width);
    }
    if layout_box.kind == BoxKind::Replaced {
        let width = absolute(layout_box.style.size.width.clone())
            .or(layout_box.natural.width)
            .unwrap_or(Au::ZERO);
        return (width, width);
    }
    if layout_box.kind == BoxKind::Table {
        return crate::table::intrinsic_sizes(tree, id, layout_box, cx);
    }
    if layout_box.kind == BoxKind::GridContainer {
        return crate::grid::intrinsic_sizes(tree, id, layout_box, cx);
    }
    if !layout_box.inline.is_empty() {
        let atomics: HashMap<BoxId, (Au, Au)> = dependencies(tree, layout_box)
            .filter_map(|id| Some((id, contribution(tree, id, cx)?)))
            .collect();
        return inline::intrinsic_sizes(tree, &layout_box.inline, &atomics, &mut cx.fonts);
    }
    if layout_box.kind == BoxKind::TableWrapper {
        // A caption widens the wrapper only to its min content width and
        // wraps to the table's width (CSS Tables 3, computing the table width).
        return in_flow_children(tree, layout_box)
            .filter_map(|child| {
                let (min, max) = contribution(tree, child, cx)?;
                let caption = tree
                    .get(child)
                    .is_some_and(|b| b.kind == BoxKind::TableCaption);
                Some(if caption { (min, min) } else { (min, max) })
            })
            .fold((Au::ZERO, Au::ZERO), |(min, max), (a, b)| {
                (min.max(a), max.max(b))
            });
    }
    let contributions =
        in_flow_children(tree, layout_box).filter_map(|child| contribution(tree, child, cx));
    let flex = &layout_box.style.flex;
    if layout_box.kind == BoxKind::FlexContainer && flex.direction == FlexDirection::Row {
        // A row's items side by side with the gaps between them; a row
        // that wraps can narrow to its widest item (spec 0006, Layout
        // rules worth pinning). A column takes the largest of each, below.
        let gap = absolute(Some(layout_box.style.alignment.column_gap.clone())).unwrap_or(Au::ZERO);
        let (count, min_sum, min_largest, max) = contributions.fold(
            (0, Au::ZERO, Au::ZERO, Au::ZERO),
            |(count, min_sum, min_largest, max), (a, b)| {
                (count + 1, min_sum + a, min_largest.max(a), max + b)
            },
        );
        let gaps = gap * (count.max(1) - 1);
        let min = match flex.wrap {
            FlexWrap::Nowrap => min_sum + gaps,
            FlexWrap::Wrap => min_largest,
        };
        return (min, max + gaps);
    }
    contributions.fold((Au::ZERO, Au::ZERO), |(min, max), (a, b)| {
        (min.max(a), max.max(b))
    })
}

/// A box's min and max content contribution: its margin box width when
/// sized to its content, or to its `width` when that is absolute.
pub(crate) fn contribution(tree: &BoxTree, id: BoxId, cx: &LayoutContext<'_>) -> Option<(Au, Au)> {
    let layout_box = tree.get(id)?;
    let style: &ComputedStyle = &layout_box.style;
    let border = border_widths(style);
    let padding = absolute(Some(style.padding.left.clone())).unwrap_or(Au::ZERO)
        + absolute(Some(style.padding.right.clone())).unwrap_or(Au::ZERO);
    // With collapsed borders the table's padding is ignored and the width
    // counts half of each outer border (CSS 2.2 section 17.6.2), so only the
    // half outer borders count toward the content width.
    let edges = if layout_box.kind == BoxKind::Table
        && style.inherited_table.border_collapse == css::BorderCollapse::Collapse
    {
        Au(border.horizontal().0 / 2)
    } else {
        border.horizontal() + padding
    };
    let content = |width: Au| match style.size.box_sizing {
        BoxSizing::BorderBox => (width - edges).max(Au::ZERO),
        BoxSizing::ContentBox => width,
    };
    let (mut min, mut max) = match absolute(style.size.width.clone()) {
        Some(width) if layout_box.kind != BoxKind::Replaced => (content(width), content(width)),
        _ => known(tree, id, cx)?,
    };
    if let Some(limit) = absolute(style.size.max_width.clone()).map(content) {
        min = min.min(limit);
        max = max.min(limit);
    }
    if let Some(limit) = absolute(style.size.min_width.clone()).map(content) {
        min = min.max(limit);
        max = max.max(limit);
    }
    let margins = absolute(style.margin.left.clone()).unwrap_or(Au::ZERO)
        + absolute(style.margin.right.clone()).unwrap_or(Au::ZERO);
    Some((min + edges + margins, max + edges + margins))
}
