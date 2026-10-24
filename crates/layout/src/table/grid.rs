//! The slot grid: `colspan`/`rowspan` fill a row-major grid of cells
//! (spec 0009, child 0004, AC-15).
//!
//! Temporary dead-code allowance: `build_grid` is wired into table layout
//! in the next chunk.
#![allow(dead_code, reason = "build_grid is wired into table layout next")]

use crate::box_tree::BoxTree;
use base::BoxId;

/// The columns of a table's grid, before sizing.
pub(crate) struct ColumnInfo {}

/// The rows of a table's grid.
pub(crate) struct RowInfo {}

/// A slot in the grid: either the cell that originates here, or a span of
/// another cell.
pub(crate) enum Slot {}

/// The built grid: columns, rows and the row-major slots.
pub(crate) struct TableGrid {
    pub(crate) columns: Vec<ColumnInfo>,
    pub(crate) rows: Vec<RowInfo>,
    pub(crate) slots: Vec<Slot>,
}

/// Builds the slot grid for `table` from its box tree.
pub(crate) fn build_grid(_tree: &BoxTree, _table: BoxId) -> TableGrid {
    TableGrid {
        columns: Vec::new(),
        rows: Vec::new(),
        slots: Vec::new(),
    }
}
