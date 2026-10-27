//! Table column sizing (spec 0009, child 0004, AC-16).
//!
//! `table-layout: auto` sizes columns from each cell's min and max content
//! and percent widths, spreading spanning cells by the CSS Tables 3 width
//! distribution. `table-layout: fixed` with a non-`auto` width sizes columns
//! from `<col>` widths and the first row's cells only.
//!
//! Temporary dead-code allowance: wired into `layout::step` in the next chunk.

use crate::box_tree::{BoxTree, LayoutBox};
use crate::intrinsic::contribution;
use crate::table::grid::{Slot, TableGrid, build_grid};
use crate::{LayoutContext, border_widths};
use base::{Au, BoxId};
use css::{BorderCollapse, BoxSizing, ComputedStyle};

/// The used width of each grid column, in CSS px, after sizing.
pub(crate) type ColumnWidths = Vec<Au>;

fn add_au(a: Au, b: Au) -> Au {
    Au(a.0.saturating_add(b.0))
}
/// `a - b`, never below zero: a width left over or still missing.
fn sub_au(a: Au, b: Au) -> Au {
    Au(a.0.saturating_sub(b.0).max(0))
}

/// Sizes a table's columns (spec 0009, child 0004, AC-16).
///
/// `table_width` is the table's used border box width (definite).
/// `cells` holds, per grid slot, the cell box id that owns it (`None` for
/// empty slots). For a spanned slot the origin cell's id is stored.
/// `cell_min`/`cell_max` hold each cell's min and max content width per
/// slot, or `None` when unknown. `fixed` is `table-layout: fixed`.
///
/// Returns one width per column.
pub(crate) fn size_columns(
    tree: &BoxTree,
    grid: &TableGrid,
    table_width: Au,
    cells: &[Option<BoxId>],
    cell_min: &[Option<Au>],
    cell_max: &[Option<Au>],
    fixed: bool,
) -> ColumnWidths {
    let ncols = grid.columns.len();
    let mut widths = vec![Au::ZERO; ncols];
    if ncols == 0 {
        return widths;
    }

    let (col_min, col_max) = column_content(grid, cells, cell_min, cell_max);

    if fixed {
        return fixed_widths(tree, grid, table_width, cells, &col_min);
    }

    // A percent column aims at its share of the table, never below its min
    // content (CSS Tables 3 §3.9.3).
    let percents = column_percents(tree, grid, cells);
    let target: Vec<Option<Au>> = (0..ncols)
        .map(|col| {
            (percents[col] > 0.0).then(|| {
                let share = Au::from_px((table_width.to_f32_px() * percents[col] / 100.0).round());
                share.max(col_min[col])
            })
        })
        .collect();
    let (mut auto_min, mut auto_max, mut pct_min, mut pct_target) =
        (Au::ZERO, Au::ZERO, Au::ZERO, Au::ZERO);
    for col in 0..ncols {
        match target[col] {
            Some(t) => {
                pct_min = add_au(pct_min, col_min[col]);
                pct_target = add_au(pct_target, t);
            }
            None => {
                auto_min = add_au(auto_min, col_min[col]);
                auto_max = add_au(auto_max, col_max[col]);
            }
        }
    }

    // When the percents and the auto columns' min content overflow the
    // table, auto columns keep their min and percent columns share the rest.
    if add_au(pct_target, auto_min) > table_width {
        let room = sub_au(sub_au(table_width, auto_min), pct_min);
        let scale = ratio(room, sub_au(pct_target, pct_min)).min(1.0);
        for (col, width) in widths.iter_mut().enumerate() {
            *width = match target[col] {
                Some(t) => col_min[col] + Au::from_px(sub_au(t, col_min[col]).to_f32_px() * scale),
                None => col_min[col],
            };
        }
        clamp_widths(&mut widths, &col_min, table_width);
        return widths;
    }

    // Auto columns grow from min towards max content as room allows, then
    // share any excess in proportion to their max content, so the columns
    // always fill the table. Columns with no content share it equally.
    let remaining = sub_au(table_width, pct_target);
    let auto_cols = target.iter().filter(|t| t.is_none()).count();
    for (col, width) in widths.iter_mut().enumerate() {
        *width = match target[col] {
            Some(t) => t,
            None if remaining <= auto_max => {
                let scale = ratio(sub_au(remaining, auto_min), sub_au(auto_max, auto_min));
                col_min[col] + Au::from_px(sub_au(col_max[col], col_min[col]).to_f32_px() * scale)
            }
            None if auto_max > Au::ZERO => {
                let excess = sub_au(remaining, auto_max).to_f32_px();
                col_max[col] + Au::from_px(excess * ratio(col_max[col], auto_max))
            }
            None => Au(remaining.0 / auto_cols as i32),
        };
    }
    // With no auto column to take it, the excess goes to the percent columns.
    if auto_cols == 0 && pct_target > Au::ZERO {
        let excess = sub_au(table_width, pct_target).to_f32_px();
        for width in widths.iter_mut() {
            *width += Au::from_px(excess * ratio(*width, pct_target));
        }
    }

    clamp_widths(&mut widths, &col_min, table_width);
    widths
}

/// `part / whole`, or 0 when `whole` is empty.
fn ratio(part: Au, whole: Au) -> f32 {
    if whole > Au::ZERO {
        part.to_f32_px() / whole.to_f32_px()
    } else {
        0.0
    }
}

/// Each column's percent width: the largest of its `<col>` percent and the
/// percent `width` of every cell that spans only it (CSS Tables 3 §3.9.3).
fn column_percents(tree: &BoxTree, grid: &TableGrid, cells: &[Option<BoxId>]) -> Vec<f32> {
    let mut percents: Vec<f32> = grid.columns.iter().map(|column| column.percent).collect();
    for (slot, cell) in cells.iter().enumerate() {
        let Some(cell) = cell else {
            continue;
        };
        if slot_col_span(grid, slot) != (slot % grid.columns.len(), 1) {
            continue;
        }
        if let Some(css::Length::Percent(percent)) =
            tree.get(*cell).and_then(|b| b.style.size.width.clone())
        {
            let col = slot % grid.columns.len();
            percents[col] = percents[col].max(percent);
        }
    }
    percents
}

/// A table's min and max content widths: its columns' content plus the
/// `border-spacing` around and between them. The table's own padding and
/// borders are edges, added by [`crate::intrinsic::contribution`]
/// (spec 0009, child 0004, AC-16).
pub(crate) fn intrinsic_sizes(
    tree: &BoxTree,
    id: BoxId,
    layout_box: &LayoutBox,
    cx: &LayoutContext<'_>,
) -> (Au, Au) {
    let grid = build_grid(tree, id);
    let ncols = grid.columns.len();
    if ncols == 0 || grid.rows.is_empty() {
        return (Au::ZERO, Au::ZERO);
    }
    let cells: Vec<Option<BoxId>> = (0..grid.slots.len())
        .map(|slot| grid.cell_at(slot))
        .collect();
    let mut cell_min = vec![None; grid.slots.len()];
    let mut cell_max = vec![None; grid.slots.len()];
    for slot in 0..grid.slots.len() {
        if !matches!(grid.slots[slot], Slot::Origin(_)) {
            continue;
        }
        if let Some(cell) = cells[slot]
            && let Some((min, max)) = contribution(tree, cell, cx)
        {
            cell_min[slot] = Some(min);
            cell_max[slot] = Some(max);
        }
    }
    let (col_min, col_max) = column_content(&grid, &cells, &cell_min, &cell_max);
    let spacing = match layout_box.style.inherited_table.border_collapse {
        BorderCollapse::Collapse => Au::ZERO,
        BorderCollapse::Separate => layout_box.style.inherited_table.border_spacing.0,
    };
    let visible = grid
        .columns
        .iter()
        .filter(|column| !column.collapsed)
        .count();
    let spacing_total = spacing * (visible + 1) as i32;
    let mut min = spacing_total;
    let mut max = spacing_total;
    for (col, column) in grid.columns.iter().enumerate() {
        if column.collapsed {
            continue;
        }
        min += col_min[col];
        max += col_max[col];
    }
    (min, max.max(min))
}

/// Per column min and max content widths, from the cells covering each
/// column and the `<col>` widths (spec 0009, child 0004, AC-16).
///
/// A spanning cell's sizes count for every column it covers; the distribution
/// pass in [`size_columns`] spreads the excess it brings.
pub(crate) fn column_content(
    grid: &TableGrid,
    cells: &[Option<BoxId>],
    cell_min: &[Option<Au>],
    cell_max: &[Option<Au>],
) -> (Vec<Au>, Vec<Au>) {
    let ncols = grid.columns.len();
    let mut col_min = vec![Au::ZERO; ncols];
    let mut col_max = vec![Au::ZERO; ncols];

    let slots = cells.len().min(cell_min.len()).min(cell_max.len());
    let mut spanning = Vec::new();
    for slot in 0..slots {
        if cells[slot].is_none() {
            continue;
        }
        let (col, span) = slot_col_span(grid, slot);
        match span {
            0 => continue, // spanned slot, not an origin
            1 => {
                if let Some(min) = cell_min[slot] {
                    col_min[col] = col_min[col].max(min);
                }
                if let Some(max) = cell_max[slot] {
                    col_max[col] = col_max[col].max(max);
                }
            }
            _ => spanning.push((slot, col, (col + span).min(ncols))),
        }
    }
    // A spanning cell adds only what its columns lack, spread by their max
    // content, or equally when they have none (CSS Tables 3 §3.9.3).
    for (slot, start, end) in spanning {
        let max_sum: Au = col_max[start..end].iter().copied().sum();
        let shares: Vec<f32> = col_max[start..end]
            .iter()
            .map(|&max| {
                if max_sum > Au::ZERO {
                    ratio(max, max_sum)
                } else {
                    1.0 / (end - start) as f32
                }
            })
            .collect();
        let spread = |sizes: &mut [Au], need: Option<Au>| {
            let Some(need) = need else {
                return;
            };
            let excess = sub_au(need, sizes[start..end].iter().copied().sum());
            for (size, share) in sizes[start..end].iter_mut().zip(&shares) {
                *size += Au::from_px(excess.to_f32_px() * share);
            }
        };
        spread(&mut col_min, cell_min[slot]);
        spread(&mut col_max, cell_max[slot]);
    }

    // A `<col>` or `<colgroup>` width is a minimum in auto layout.
    for (c, column) in grid.columns.iter().enumerate() {
        if let Some(width) = column.specified {
            col_min[c] = col_min[c].max(width);
            col_max[c] = col_max[c].max(width);
        }
    }
    (col_min, col_max)
}

/// `table-layout: fixed`: `<col>` widths, then the first row's cell widths,
/// then the leftover shared equally (CSS Tables 3 §3.9.3). Content never
/// changes a column's width.
fn fixed_widths(
    tree: &BoxTree,
    grid: &TableGrid,
    table_width: Au,
    cells: &[Option<BoxId>],
    col_min: &[Au],
) -> ColumnWidths {
    let ncols = grid.columns.len();
    let mut widths: Vec<Option<Au>> = vec![None; ncols];
    let mut remaining = table_width;
    for (col, column) in grid.columns.iter().enumerate() {
        let width = if column.percent > 0.0 {
            Some(Au::from_px(
                (table_width.to_f32_px() * column.percent / 100.0).round(),
            ))
        } else {
            column.specified
        };
        if let Some(width) = width {
            widths[col] = Some(width);
            remaining = sub_au(remaining, width);
        }
    }

    // The first row's cells size the columns they span, unless a `<col>` did.
    if !grid.rows.is_empty() {
        for col in 0..ncols {
            let slot = col;
            if !matches!(grid.slots.get(slot), Some(Slot::Origin(_))) {
                continue;
            }
            let Some(cell) = cells.get(slot).copied().flatten() else {
                continue;
            };
            let Some(width) = tree
                .get(cell)
                .and_then(|b| specified_outer_width(&b.style, table_width))
            else {
                continue;
            };
            let span = crate::table::height::span_cols(grid, 0, col);
            let each = Au(width.0 / span.max(1) as i32);
            for slot in widths.iter_mut().take((col + span).min(ncols)).skip(col) {
                if slot.is_none() {
                    *slot = Some(each);
                    remaining = sub_au(remaining, each);
                }
            }
        }
    }

    let unspecified = widths.iter().filter(|width| width.is_none()).count();
    let each = if unspecified > 0 {
        Au(remaining.0.max(0) / unspecified as i32)
    } else {
        Au::ZERO
    };
    let mut out: ColumnWidths = widths
        .into_iter()
        .map(|width| width.unwrap_or(each))
        .collect();
    clamp_widths(&mut out, col_min, table_width);
    out
}

/// The border box width a cell's `width` asks for, `None` when `width` is
/// `auto`. A cell's `width` sets its content box, so its padding and borders
/// grow the column it sizes (spec 0009, child 0004, AC-16).
fn specified_outer_width(style: &ComputedStyle, basis: Au) -> Option<Au> {
    let width = style.size.width.clone()?.resolve(Some(basis));
    match style.size.box_sizing {
        BoxSizing::BorderBox => Some(width),
        BoxSizing::ContentBox => Some(
            width
                + border_widths(style).horizontal()
                + style.padding.left.resolve(Some(basis))
                + style.padding.right.resolve(Some(basis)),
        ),
    }
}

/// Clamp every column into [min, max] and keep the total at most
/// `table_width`, scaling down proportionally if over.
fn clamp_widths(widths: &mut [Au], col_min: &[Au], table_width: Au) {
    for (col, width) in widths.iter_mut().enumerate() {
        let min = col_min.get(col).copied().unwrap_or(Au::ZERO);
        *width = (*width).max(min);
    }
    let mut total = Au::ZERO;
    for width in widths.iter() {
        total = add_au(total, *width);
    }
    if total > table_width && total > Au::ZERO {
        let scale = table_width.to_f32_px() / total.to_f32_px();
        for (col, width) in widths.iter_mut().enumerate() {
            let min = col_min.get(col).copied().unwrap_or(Au::ZERO);
            *width = Au::from_px((width.to_f32_px() * scale).round()).max(min);
        }
    }
}

/// The starting column and horizontal span (in columns) of the cell at
/// `slot`. Returns `(col, 0)` for a spanned (non-origin) slot.
fn slot_col_span(grid: &TableGrid, slot: usize) -> (usize, usize) {
    match grid.slots.get(slot) {
        Some(Slot::Origin(_)) => {
            let ncols = grid.columns.len();
            let col = slot % ncols;
            let origin = slot;
            let mut span = 1usize;
            while origin + span < grid.slots.len()
                && !((origin + span).is_multiple_of(ncols))
                && matches!(
                    grid.slots[origin + span],
                    Slot::Spanned { origin: o } if o == (slot / ncols, col)
                )
            {
                span += 1;
            }
            (col, span)
        }
        Some(Slot::Spanned { .. }) => (0, 0),
        None => (0, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::box_tree::{BoxKind, LayoutBox};
    use crate::table::grid::ColumnInfo;
    use css::ComputedStyle;

    /// A two-column grid: cells at (0,0)=id0, (0,1)=id1, (1,0)=id2,
    /// (1,1) spanned from (1,1).
    fn two_col_grid(tree: &mut BoxTree) -> (TableGrid, [Option<BoxId>; 4]) {
        let c00 = tree.boxes.insert(LayoutBox::new(
            None,
            BoxKind::TableCell,
            ComputedStyle::initial(),
            None,
        ));
        let c01 = tree.boxes.insert(LayoutBox::new(
            None,
            BoxKind::TableCell,
            ComputedStyle::initial(),
            None,
        ));
        let c10 = tree.boxes.insert(LayoutBox::new(
            None,
            BoxKind::TableCell,
            ComputedStyle::initial(),
            None,
        ));
        let mut slots = Vec::new();
        for r in 0..2 {
            for c in 0..2 {
                slots.push(if r == 0 && c == 0 {
                    Slot::Origin(c00)
                } else if r == 0 && c == 1 {
                    Slot::Origin(c01)
                } else if r == 1 && c == 0 {
                    Slot::Origin(c10)
                } else {
                    Slot::Spanned { origin: (1, 1) }
                });
            }
        }
        let cells: [Option<BoxId>; 4] = [Some(c00), Some(c01), Some(c10), None];
        (
            TableGrid {
                columns: vec![ColumnInfo::new(); 2],
                rows: vec![crate::table::grid::RowInfo::new(); 2],
                slots,
                overflow: Vec::new(),
            },
            cells,
        )
    }

    fn cell_widths(table_width: Au, mins: [Option<Au>; 4], maxs: [Option<Au>; 4]) -> ColumnWidths {
        let mut tree = BoxTree::default();
        let (grid, cells) = two_col_grid(&mut tree);
        size_columns(&tree, &grid, table_width, &cells, &mins, &maxs, false)
    }

    #[test]
    fn auto_splits_by_max_content_under_table_width() {
        // Two columns, min 50 each, max 200 each, table 300 wide.
        // leftover after mins = 200, max_sum = 400, scale = 0.5 → each min
        // 50 plus a 100 share, so the columns fill the table: 150 each.
        let widths = cell_widths(
            Au::from_px(300.0),
            [Some(Au::from_px(50.0)), Some(Au::from_px(50.0)), None, None],
            [
                Some(Au::from_px(200.0)),
                Some(Au::from_px(200.0)),
                None,
                None,
            ],
        );
        assert_eq!(widths.len(), 2);
        assert!((widths[0] - Au::from_px(150.0)).0.abs() < 1);
        assert!((widths[1] - Au::from_px(150.0)).0.abs() < 1);
    }

    #[test]
    fn clamps_to_min_when_table_narrow() {
        // Table 60 wide, mins 50 each → each clamped to min (50 px).
        let widths = cell_widths(
            Au::from_px(60.0),
            [Some(Au::from_px(50.0)), Some(Au::from_px(50.0)), None, None],
            [
                Some(Au::from_px(200.0)),
                Some(Au::from_px(200.0)),
                None,
                None,
            ],
        );
        assert_eq!(widths.len(), 2);
        assert!((widths[0] - Au::from_px(50.0)).0.abs() < 1);
        assert!((widths[1] - Au::from_px(50.0)).0.abs() < 1);
    }

    #[test]
    fn empty_grid_returns_empty() {
        let grid = TableGrid {
            columns: Vec::new(),
            rows: Vec::new(),
            slots: Vec::new(),
            overflow: Vec::new(),
        };
        let tree = BoxTree::default();
        let widths = size_columns(&tree, &grid, Au::from_px(300.0), &[], &[], &[], false);
        assert!(widths.is_empty());
    }
}
