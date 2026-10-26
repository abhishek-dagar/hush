//! Table row heights and cell vertical alignment (spec 0009, child 0004,
//! AC-16).
//!
//! Each cell lays out at its column width. A row's height is the max of its
//! single row cells' heights and the row's own `height`. Rowspan cells then
//! spread any extra over the rows they span, evenly, ending rows growing
//! first. A specified table `height` larger than the rows' total spreads the
//! extra over rows in proportion to their heights, or evenly when all are
//! zero.

use base::Au;
use css::VerticalAlign;

use crate::table::grid::{Slot, TableGrid};

/// How many grid rows the origin cell at `(row, col)` covers.
pub(crate) fn span_rows(grid: &TableGrid, row: usize, col: usize) -> usize {
    let ncols = grid.columns.len();
    let mut span = 1;
    while row + span < grid.rows.len() {
        let slot = (row + span) * ncols + col;
        match grid.slots.get(slot) {
            Some(Slot::Spanned { origin }) if *origin == (row, col) => span += 1,
            _ => break,
        }
    }
    span
}

/// How many grid columns the origin cell at `(row, col)` covers.
pub(crate) fn span_cols(grid: &TableGrid, row: usize, col: usize) -> usize {
    let ncols = grid.columns.len();
    let base = row * ncols;
    let mut span = 1;
    while col + span < ncols {
        match grid.slots.get(base + col + span) {
            Some(Slot::Spanned { origin }) if *origin == (row, col) => span += 1,
            _ => break,
        }
    }
    span
}

/// Distributes `extra` over `rows` in proportion to their heights; when every
/// row is zero, it spreads evenly. Rows take rounding remainders in order so
/// nothing is dropped.
fn distribute(extra: Au, rows: &[Au]) -> Vec<Au> {
    let n = rows.len();
    if n == 0 || extra <= Au::ZERO {
        return vec![Au::ZERO; n];
    }
    let total: Au = rows.iter().copied().sum();
    let mut out = vec![Au::ZERO; n];
    if total <= Au::ZERO {
        let mut left = extra;
        let count = n;
        for (i, slot) in out.iter_mut().enumerate() {
            let rows_left = count - i;
            let take = Au(left.0 / rows_left as i32);
            *slot = take;
            left -= take;
        }
        return out;
    }
    let mut left = extra;
    let count = n;
    for (i, (slot, &row_h)) in out.iter_mut().zip(rows).enumerate() {
        let rows_left = count - i;
        let share = if rows_left == 1 {
            left
        } else {
            let want = extra.0 as i64 * row_h.0 as i64 / total.0.max(1) as i64;
            Au((want as i32).min(left.0))
        };
        *slot = share;
        left -= share;
    }
    out
}

/// The height of each grid row (spec 0009, child 0004, AC-16).
///
/// `cell_heights` holds each cell's laid out height per origin slot (`None`
/// for empty and spanned slots). `row_specified` holds each row's own
/// `height`. `table_height` is the table's specified `height`, if any.
pub(crate) fn row_heights(
    grid: &TableGrid,
    cell_heights: &[Option<Au>],
    row_specified: &[Option<Au>],
    table_height: Option<Au>,
) -> Vec<Au> {
    let nrows = grid.rows.len();
    let ncols = grid.columns.len();
    let mut heights: Vec<Au> = vec![Au::ZERO; nrows];

    // Single row cells and each row's own height. Rowspan cells are skipped
    // here; the spreading pass below handles them.
    for (r, height) in heights.iter_mut().enumerate() {
        let mut h = row_specified.get(r).copied().flatten().unwrap_or(Au::ZERO);
        for c in 0..ncols {
            let slot = r * ncols + c;
            if !matches!(grid.slots.get(slot), Some(Slot::Origin(_))) {
                continue;
            }
            if span_rows(grid, r, c) > 1 {
                continue;
            }
            h = h.max(
                cell_heights
                    .get(slot)
                    .copied()
                    .flatten()
                    .unwrap_or(Au::ZERO),
            );
        }
        *height = h;
    }

    // Each rowspan cell's height beyond the total of the rows it touches
    // spreads over the rows it spans, evenly, ending rows growing first.
    for r in 0..nrows {
        for c in 0..ncols {
            let slot = r * ncols + c;
            if !matches!(grid.slots.get(slot), Some(Slot::Origin(_))) {
                continue;
            }
            let span = span_rows(grid, r, c);
            if span <= 1 {
                continue;
            }
            let end = (r + span).min(nrows);
            let cell_h = cell_heights
                .get(slot)
                .copied()
                .flatten()
                .unwrap_or(Au::ZERO);
            let total: Au = heights[r..end].iter().copied().sum();
            let mut left = (cell_h - total).max(Au::ZERO);
            if left <= Au::ZERO {
                continue;
            }
            let count = end - r;
            for (i, rr) in (r..end).rev().enumerate() {
                let rows_left = count - i;
                let take = Au(left.0 / rows_left as i32);
                heights[rr] += take;
                left -= take;
            }
        }
    }

    // A specified table height larger than the rows' total spreads over rows
    // in proportion to their heights (Chrome's rule), evenly when all zero.
    if let Some(table_h) = table_height {
        let total: Au = heights.iter().copied().sum();
        if table_h > total {
            let added = distribute(table_h - total, &heights);
            for (height, extra) in heights.iter_mut().zip(added) {
                *height += extra;
            }
        }
    }

    heights
}

/// A cell's content offset within its row height for `vertical-align`
/// (spec 0009, child 0004, AC-16). `top` and `baseline` place the content at
/// the top; `middle` and `bottom` centre or lower it.
pub(crate) fn cell_vertical_offset(align: VerticalAlign, cell_height: Au, row_height: Au) -> Au {
    if cell_height >= row_height {
        return Au::ZERO;
    }
    match align {
        VerticalAlign::Top | VerticalAlign::Baseline => Au::ZERO,
        VerticalAlign::Bottom => row_height - cell_height,
        VerticalAlign::Middle => Au((row_height - cell_height).0 / 2),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base::{BoxId, Id};
    use std::num::NonZeroU32;

    use crate::table::grid::{ColumnInfo, RowInfo};

    fn id(index: u32) -> BoxId {
        Id::from_parts(index, NonZeroU32::MIN)
    }

    /// A 2 by 2 grid whose four cells start at (0, 0), (0, 1), (1, 0) and
    /// (1, 1).
    fn grid_2x2() -> TableGrid {
        TableGrid {
            columns: vec![ColumnInfo::new(); 2],
            rows: vec![RowInfo::new(); 2],
            slots: vec![
                Slot::Origin(id(0)),
                Slot::Origin(id(1)),
                Slot::Origin(id(2)),
                Slot::Origin(id(3)),
            ],
            overflow: Vec::new(),
        }
    }

    #[test]
    fn row_height_takes_tallest_cell() {
        let grid = grid_2x2();
        let heights = row_heights(
            &grid,
            &[Some(Au::from_px(10.0)), None, Some(Au::from_px(30.0)), None],
            &[None, None],
            None,
        );
        assert_eq!(heights, vec![Au::from_px(10.0), Au::from_px(30.0)]);
    }

    #[test]
    fn row_own_height_beats_cell_height() {
        let grid = grid_2x2();
        let heights = row_heights(
            &grid,
            &[Some(Au::from_px(5.0)), None, None, None],
            &[None, Some(Au::from_px(40.0))],
            None,
        );
        assert_eq!(heights, vec![Au::from_px(5.0), Au::from_px(40.0)]);
    }

    #[test]
    fn table_height_spreads_over_rows_proportionally() {
        let grid = grid_2x2();
        // Two rows 10 px each, table set to 40 px: 20 px extra splits 10/10.
        let heights = row_heights(
            &grid,
            &[Some(Au::from_px(10.0)), None, Some(Au::from_px(10.0)), None],
            &[None, None],
            Some(Au::from_px(40.0)),
        );
        assert_eq!(heights, vec![Au::from_px(20.0), Au::from_px(20.0)]);
    }

    #[test]
    fn rowspan_spreads_extra_ending_rows_first() {
        let mut grid = grid_2x2();
        // The first cell spans both rows and is 60 px tall; the other cells
        // are empty, so the 60 px spreads evenly over the two rows.
        grid.slots[2] = Slot::Spanned { origin: (0, 0) };
        let heights = row_heights(
            &grid,
            &[Some(Au::from_px(60.0)), None, None, None],
            &[None, None],
            None,
        );
        assert_eq!(heights, vec![Au::from_px(30.0), Au::from_px(30.0)]);
    }

    #[test]
    fn span_rows_counts_only_the_origin_cells_slots() {
        let mut grid = grid_2x2();
        grid.slots[2] = Slot::Spanned { origin: (0, 0) };
        assert_eq!(span_rows(&grid, 0, 0), 2);
        assert_eq!(span_rows(&grid, 0, 1), 1);
    }

    #[test]
    fn span_cols_counts_the_origin_cells_columns() {
        let mut grid = grid_2x2();
        grid.slots[1] = Slot::Spanned { origin: (0, 0) };
        assert_eq!(span_cols(&grid, 0, 0), 2);
        assert_eq!(span_cols(&grid, 1, 0), 1);
    }

    #[test]
    fn cell_vertical_offset_top() {
        assert_eq!(
            cell_vertical_offset(VerticalAlign::Top, Au::from_px(10.0), Au::from_px(30.0)),
            Au::ZERO
        );
    }

    #[test]
    fn cell_vertical_offset_bottom() {
        assert_eq!(
            cell_vertical_offset(VerticalAlign::Bottom, Au::from_px(10.0), Au::from_px(30.0)),
            Au::from_px(20.0)
        );
    }

    #[test]
    fn cell_vertical_offset_middle() {
        assert_eq!(
            cell_vertical_offset(VerticalAlign::Middle, Au::from_px(10.0), Au::from_px(30.0)),
            Au::from_px(10.0)
        );
    }

    #[test]
    fn cell_taller_than_row_has_no_offset() {
        assert_eq!(
            cell_vertical_offset(VerticalAlign::Middle, Au::from_px(40.0), Au::from_px(30.0)),
            Au::ZERO
        );
    }

    #[test]
    fn baseline_acts_like_top() {
        assert_eq!(
            cell_vertical_offset(
                VerticalAlign::Baseline,
                Au::from_px(10.0),
                Au::from_px(30.0)
            ),
            Au::ZERO
        );
    }
}
