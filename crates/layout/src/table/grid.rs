//! The slot grid: `colspan`/`rowspan` fill a row-major grid of cells
//! (spec 0009, child 0004, AC-15).
//!
//! The HTML table processing model places each cell in the next free slot of
//! its row, spanning `colspan` columns and `rowspan` rows. The grid caps at
//! 1,000,000 slots; past it the rest of the table lays out as plain blocks.
//!
//! Temporary dead-code allowance: `build_grid` is wired into table layout
//! in the next chunk.

use crate::box_tree::{BoxKind, BoxTree, LayoutBox};
use base::{Au, BoxId};
use css::{Length, Visibility};

/// The columns of a table's grid, before sizing.
#[derive(Debug, Clone)]
pub(crate) struct ColumnInfo {
    /// The `<col>` or `<colgroup>` box this grid column came from, if any.
    pub id: Option<BoxId>,
    /// Percent width, 0 to 100 (filled by `width.rs`).
    pub percent: f32,
    /// Width from `<col>` or the cell's own width (filled by `width.rs`).
    pub specified: Option<Au>,
    /// Collapsed by `visibility: collapse`.
    pub collapsed: bool,
}

impl ColumnInfo {
    pub(crate) fn new() -> Self {
        Self {
            id: None,
            percent: 0.0,
            specified: None,
            collapsed: false,
        }
    }
}

/// The rows of a table's grid.
#[derive(Debug, Clone)]
pub(crate) struct RowInfo {
    /// The row box this grid row came from, if any.
    pub id: Option<BoxId>,
    /// Collapsed by `visibility: collapse` on the row or its row group.
    pub collapsed: bool,
}

impl RowInfo {
    pub(crate) fn new() -> Self {
        Self {
            id: None,
            collapsed: false,
        }
    }
}

/// A slot in the grid: either the cell that originates here, or a span of
/// another cell. An empty table slot is a self reference.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Slot {
    /// The cell that starts in this slot.
    Origin(BoxId),
    /// Part of a cell whose origin is at `(row, col)`. Self means empty.
    Spanned { origin: (usize, usize) },
}

impl Slot {
    pub(crate) fn empty_at(row: usize, col: usize) -> Self {
        Self::Spanned { origin: (row, col) }
    }
}

/// The built grid: columns, rows and the row-major slots.
#[derive(Debug, Clone)]
pub(crate) struct TableGrid {
    pub(crate) columns: Vec<ColumnInfo>,
    pub(crate) rows: Vec<RowInfo>,
    pub(crate) slots: Vec<Slot>,
    /// Rows past the 1,000,000 slot cap, laid out as plain blocks after the
    /// grid (spec 0009, child 0004, AC-15).
    pub(crate) overflow: Vec<BoxId>,
}

impl TableGrid {
    /// The cell id that owns `slot`: the origin cell for a spanned slot.
    pub(crate) fn cell_at(&self, slot: usize) -> Option<BoxId> {
        match self.slots.get(slot)? {
            Slot::Origin(cell) => Some(*cell),
            Slot::Spanned { origin } => {
                match self.slots.get(origin.0 * self.columns.len() + origin.1) {
                    Some(Slot::Origin(cell)) => Some(*cell),
                    _ => None,
                }
            }
        }
    }
}

/// How many slots a grid may have before the rest of the table lays out as
/// plain blocks (spec 0009, child 0004, AC-15).
const MAX_SLOTS: usize = 1_000_000;

/// A cell's `rowspan` reaching into future rows, applied when those rows are
/// built.
struct Reach {
    /// First row (exclusive) this reach covers.
    start_row: usize,
    /// Last row (inclusive) this reach covers.
    end_row: usize,
    /// First column (inclusive) this reach covers.
    col_from: usize,
    /// One past the last column this reach covers.
    col_to: usize,
    /// The origin position of the spanning cell.
    origin_row: usize,
    origin_col: usize,
}

/// Builds the slot grid for `table` from its box tree.
pub(crate) fn build_grid(tree: &BoxTree, table: BoxId) -> TableGrid {
    let Some(table_box) = tree.get(table) else {
        return TableGrid {
            columns: Vec::new(),
            rows: Vec::new(),
            slots: Vec::new(),
            overflow: Vec::new(),
        };
    };

    let groups = ordered_row_groups(tree, table_box);

    // `<col>` and `<colgroup>` boxes give the grid its columns, their widths
    // and their `visibility: collapse` (spec 0009, child 0004, AC-14, AC-18).
    let mut column_ids = column_boxes(tree, table_box);
    let mut grid: Vec<Vec<Slot>> = Vec::new();
    let mut ncols = column_ids.len();
    let mut reaches: Vec<Reach> = Vec::new();
    let mut row_ids: Vec<BoxId> = Vec::new();
    let mut row_collapsed: Vec<bool> = Vec::new();
    let mut overflow: Vec<BoxId> = Vec::new();

    // Every row with the rows left in its group, so a `rowspan="0"` reaches
    // the group's end (spec 0009, child 0004, AC-15).
    let mut plan: Vec<(BoxId, usize, bool)> = Vec::new();
    for group in groups {
        let Some(group_box) = tree.get(group) else {
            continue;
        };
        let group_collapsed = group_box.style.inherited_box.visibility == Visibility::Collapse;
        let rows: Vec<BoxId> = group_box
            .children
            .iter()
            .copied()
            .filter(|&row| matches!(tree.get(row).map(|b| b.kind), Some(BoxKind::TableRow)))
            .collect();
        let count = rows.len();
        for (index, row) in rows.into_iter().enumerate() {
            let collapsed = group_collapsed
                || tree
                    .get(row)
                    .is_some_and(|b| b.style.inherited_box.visibility == Visibility::Collapse);
            plan.push((row, count - 1 - index, collapsed));
        }
    }

    let mut capped = false;
    for (row, rows_to_end, collapsed) in plan {
        if capped {
            overflow.push(row);
            continue;
        }
        if !add_row(tree, row, &mut grid, &mut ncols, &mut reaches, rows_to_end) {
            capped = true;
            overflow.push(row);
            continue;
        }
        row_ids.push(row);
        row_collapsed.push(collapsed);
    }

    let nrows = grid.len();
    let mut slots = Vec::with_capacity(nrows.saturating_mul(ncols));
    for (r, row) in grid.iter().enumerate() {
        for c in 0..ncols {
            slots.push(if c < row.len() {
                row[c]
            } else {
                Slot::empty_at(r, c)
            });
        }
    }

    column_ids.resize(ncols, None);
    let mut columns = vec![ColumnInfo::new(); ncols];
    for (column, id) in columns.iter_mut().zip(column_ids) {
        let Some(id) = id else {
            continue;
        };
        column.id = Some(id);
        let Some(col_box) = tree.get(id) else {
            continue;
        };
        column.collapsed = col_box.style.inherited_box.visibility == Visibility::Collapse;
        match col_box.style.size.width {
            Some(Length::Percent(percent)) => column.percent = percent,
            Some(Length::Au(au)) => column.specified = Some(au),
            Some(Length::Calc(_)) => {}
            None => {}
        }
    }

    let mut rows = vec![RowInfo::new(); nrows];
    for ((row, id), collapsed) in rows.iter_mut().zip(row_ids).zip(row_collapsed) {
        row.id = Some(id);
        row.collapsed = collapsed;
    }

    TableGrid {
        columns,
        rows,
        slots,
        overflow,
    }
}

/// The `<col>` and `<colgroup>` boxes under `table`, one id per grid column,
/// in order (spec 0009, child 0004, AC-14). A `<colgroup>` with `<col>`
/// children counts its children; without them it spans its own `span`.
fn column_boxes(tree: &BoxTree, table_box: &LayoutBox) -> Vec<Option<BoxId>> {
    let mut columns = Vec::new();
    for &child in &table_box.children {
        match tree.get(child).map(|b| b.kind) {
            Some(BoxKind::TableColumn) => push_columns(tree, child, &mut columns),
            Some(BoxKind::TableColumnGroup) => {
                let cols: Vec<BoxId> = tree
                    .get(child)
                    .map(|group| {
                        group
                            .children
                            .iter()
                            .copied()
                            .filter(|&c| {
                                matches!(tree.get(c).map(|b| b.kind), Some(BoxKind::TableColumn))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if cols.is_empty() {
                    push_columns(tree, child, &mut columns);
                } else {
                    for col in cols {
                        push_columns(tree, col, &mut columns);
                    }
                }
            }
            _ => {}
        }
    }
    columns
}

/// Adds `span` copies of `id` to `columns`.
fn push_columns(tree: &BoxTree, id: BoxId, columns: &mut Vec<Option<BoxId>>) {
    let span = tree.get(id).map_or(1, |b| b.colspan.max(1) as usize);
    for _ in 0..span {
        columns.push(Some(id));
    }
}

/// Orders the row groups under `table_box`: first thead first, first tfoot
/// last, the rest in tree order.
fn ordered_row_groups(tree: &BoxTree, table_box: &LayoutBox) -> Vec<BoxId> {
    let mut first_thead: Option<BoxId> = None;
    let mut first_tfoot: Option<BoxId> = None;
    let mut rest: Vec<BoxId> = Vec::new();

    for &child in &table_box.children {
        let kind = tree.get(child).map(|b| b.kind);
        match kind {
            Some(BoxKind::TableHeaderGroup) if first_thead.is_none() => {
                first_thead = Some(child);
            }
            Some(BoxKind::TableFooterGroup) if first_tfoot.is_none() => {
                first_tfoot = Some(child);
            }
            Some(
                BoxKind::TableRowGroup | BoxKind::TableHeaderGroup | BoxKind::TableFooterGroup,
            ) => {
                rest.push(child);
            }
            _ => {}
        }
    }
    let mut groups = Vec::new();
    if let Some(g) = first_thead {
        groups.push(g);
    }
    groups.extend(rest);
    if let Some(g) = first_tfoot {
        groups.push(g);
    }
    groups
}

/// Adds one row to the grid. `rows_to_end` is how many rows of the row's
/// group follow it, so `rowspan="0"` reaches the group's end. Returns
/// `false` when the slot cap would be exceeded, so the caller stops before
/// this row.
fn add_row(
    tree: &BoxTree,
    row: BoxId,
    grid: &mut Vec<Vec<Slot>>,
    ncols: &mut usize,
    reaches: &mut Vec<Reach>,
    rows_to_end: usize,
) -> bool {
    let Some(row_box) = tree.get(row) else {
        return true;
    };
    let r = grid.len();
    let mut new_row: Vec<Slot> = Vec::with_capacity(*ncols);

    // Slots taken in this row by a rowspan reaching down from above.
    for c in 0..*ncols {
        let taken = reaches.iter().find(|reach| {
            reach.start_row < r && r <= reach.end_row && reach.col_from <= c && c < reach.col_to
        });
        new_row.push(match taken {
            Some(reach) => Slot::Spanned {
                origin: (reach.origin_row, reach.origin_col),
            },
            None => Slot::empty_at(r, c),
        });
    }

    let mut col = 0usize;
    for &cell in &row_box.children {
        if !matches!(tree.get(cell).map(|b| b.kind), Some(BoxKind::TableCell)) {
            continue;
        }
        let cell_box = match tree.get(cell) {
            Some(b) => b,
            None => continue,
        };
        let colspan = cell_box.colspan as usize;
        // `rowspan="0"` reaches the end of its row group (spec 0009, child
        // 0004, AC-15).
        let rowspan = if cell_box.rowspan == 0 {
            rows_to_end + 1
        } else {
            cell_box.rowspan as usize
        };

        // Skip slots already taken by a rowspan from above.
        while col < *ncols && !matches!(new_row[col], Slot::Origin(_)) {
            // new_row[col] is either Spanned (taken) or empty_at (free).
            // A self-referencing empty_at means free; a Spanned means taken.
            match new_row[col] {
                Slot::Spanned { origin } if origin == (r, col) => {
                    // free empty slot
                    break;
                }
                _ => col += 1,
            }
        }

        // Grow columns if the cell reaches beyond.
        let needed = col + colspan;
        if needed > *ncols {
            grow_columns(grid, ncols, needed);
            // Extend this row too.
            while new_row.len() < *ncols {
                new_row.push(Slot::empty_at(r, new_row.len()));
            }
        }

        if grid.iter().map(|row| row.len()).sum::<usize>() + new_row.len() > MAX_SLOTS {
            return false;
        }

        // Mark origin and forward spans.
        for sr in 0..rowspan {
            for sc in 0..colspan {
                if sr == 0 {
                    new_row[col + sc] = if sc == 0 {
                        Slot::Origin(cell)
                    } else {
                        Slot::Spanned { origin: (r, col) }
                    };
                }
            }
        }
        // Register forward reach for rows below.
        if rowspan > 1 {
            reaches.push(Reach {
                start_row: r,
                end_row: r + rowspan - 1,
                col_from: col,
                col_to: col + colspan,
                origin_row: r,
                origin_col: col,
            });
        }
        col += colspan;
    }

    grid.push(new_row);
    true
}

/// Ensures every row in `grid` has at least `needed` columns and sets
/// `*ncols = needed`. New slots are empty references at their own row.
fn grow_columns(grid: &mut [Vec<Slot>], ncols: &mut usize, needed: usize) {
    for (i, row) in grid.iter_mut().enumerate() {
        for c in row.len()..needed {
            row.push(Slot::empty_at(i, c));
        }
    }
    *ncols = needed;
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::box_tree::BoxTree;
    use css::ComputedStyle;

    fn cell(colspan: u32, rowspan: u32) -> LayoutBox {
        LayoutBox::new(None, BoxKind::TableCell, ComputedStyle::initial(), None)
            .with_spans(colspan, rowspan)
    }

    fn row(kind: BoxKind, children: Vec<BoxId>) -> LayoutBox {
        LayoutBox::new(None, kind, ComputedStyle::initial(), None).with_children(children)
    }

    fn table(children: Vec<BoxId>) -> LayoutBox {
        LayoutBox::new(None, BoxKind::Table, ComputedStyle::initial(), None).with_children(children)
    }

    trait BoxExt {
        fn with_spans(self, colspan: u32, rowspan: u32) -> Self;
        fn with_children(self, children: Vec<BoxId>) -> Self;
    }

    impl BoxExt for LayoutBox {
        fn with_spans(mut self, colspan: u32, rowspan: u32) -> Self {
            self.colspan = colspan;
            self.rowspan = rowspan;
            self
        }
        fn with_children(mut self, children: Vec<BoxId>) -> Self {
            self.children = children;
            self
        }
    }

    fn slot_origin(slot: Slot) -> Option<BoxId> {
        match slot {
            Slot::Origin(id) => Some(id),
            _ => None,
        }
    }

    #[test]
    fn simple_two_by_two_grid() {
        let c00 = cell(1, 1);
        let c01 = cell(1, 1);
        let c10 = cell(1, 1);
        let c11 = cell(1, 1);
        let r0 = row(BoxKind::TableRow, Vec::new());
        let r1 = row(BoxKind::TableRow, Vec::new());
        let rg = row(BoxKind::TableRowGroup, Vec::new());
        let t = table(Vec::new());

        let mut tree = BoxTree::default();
        let c00 = tree.boxes.insert(c00);
        let c01 = tree.boxes.insert(c01);
        let c10 = tree.boxes.insert(c10);
        let c11 = tree.boxes.insert(c11);
        let r0 = tree.boxes.insert(r0.with_children(vec![c00, c01]));
        let r1 = tree.boxes.insert(r1.with_children(vec![c10, c11]));
        let rg = tree.boxes.insert(rg.with_children(vec![r0, r1]));
        let t = tree.boxes.insert(t.with_children(vec![rg]));
        tree.set_root(t);

        let grid = build_grid(&tree, t);
        assert_eq!(grid.columns.len(), 2, "two columns");
        assert_eq!(grid.rows.len(), 2, "two rows");
        assert_eq!(grid.slots.len(), 4, "four slots");
        assert_eq!(slot_origin(grid.slots[0]), Some(c00));
        assert_eq!(slot_origin(grid.slots[1]), Some(c01));
        assert_eq!(slot_origin(grid.slots[2]), Some(c10));
        assert_eq!(slot_origin(grid.slots[3]), Some(c11));
    }

    #[test]
    fn colspan_spans_columns() {
        let c00 = cell(2, 1);
        let c01 = cell(1, 1);
        let r0 = row(BoxKind::TableRow, Vec::new());
        let rg = row(BoxKind::TableRowGroup, Vec::new());
        let t = table(Vec::new());

        let mut tree = BoxTree::default();
        let c00 = tree.boxes.insert(c00);
        let c01 = tree.boxes.insert(c01);
        let r0 = tree.boxes.insert(r0.with_children(vec![c00, c01]));
        let rg = tree.boxes.insert(rg.with_children(vec![r0]));
        let t = tree.boxes.insert(t.with_children(vec![rg]));
        tree.set_root(t);

        let grid = build_grid(&tree, t);
        assert_eq!(grid.columns.len(), 3, "colspan cell takes two slots");
        assert_eq!(grid.rows.len(), 1);
        assert_eq!(slot_origin(grid.slots[0]), Some(c00));
        // slot 1 is spanned by c00
        assert!(matches!(grid.slots[1], Slot::Spanned { origin: (0, 0) }));
        assert_eq!(slot_origin(grid.slots[2]), Some(c01));
    }

    #[test]
    fn rowspan_spans_rows() {
        let c00 = cell(1, 2);
        let c01 = cell(1, 1);
        let c10 = cell(1, 1);
        let r0 = row(BoxKind::TableRow, Vec::new());
        let r1 = row(BoxKind::TableRow, Vec::new());
        let rg = row(BoxKind::TableRowGroup, Vec::new());
        let t = table(Vec::new());

        let mut tree = BoxTree::default();
        let c00 = tree.boxes.insert(c00);
        let c01 = tree.boxes.insert(c01);
        let c10 = tree.boxes.insert(c10);
        let r0 = tree.boxes.insert(r0.with_children(vec![c00, c01]));
        let r1 = tree.boxes.insert(r1.with_children(vec![c10]));
        let rg = tree.boxes.insert(rg.with_children(vec![r0, r1]));
        let t = tree.boxes.insert(t.with_children(vec![rg]));
        tree.set_root(t);

        let grid = build_grid(&tree, t);
        assert_eq!(grid.columns.len(), 2);
        assert_eq!(grid.rows.len(), 2);
        assert_eq!(slot_origin(grid.slots[0]), Some(c00));
        assert_eq!(slot_origin(grid.slots[1]), Some(c01));
        // row 1 col 0 is spanned by c00 (rowspan from row 0)
        assert!(matches!(grid.slots[2], Slot::Spanned { origin: (0, 0) }));
        assert_eq!(slot_origin(grid.slots[3]), Some(c10));
    }

    #[test]
    fn rowspan_zero_reaches_the_end_of_its_row_group() {
        // AC-15: `rowspan="0"` spans every remaining row of its own group and
        // stops at the next group.
        let mut tree = BoxTree::default();
        let tall = tree.boxes.insert(cell(1, 0));
        let filler1 = tree.boxes.insert(cell(1, 1));
        let filler2 = tree.boxes.insert(cell(1, 1));
        let next = tree.boxes.insert(cell(1, 1));
        let r0 = tree.boxes.insert(row(BoxKind::TableRow, vec![tall]));
        let r1 = tree.boxes.insert(row(BoxKind::TableRow, vec![filler1]));
        let r2 = tree.boxes.insert(row(BoxKind::TableRow, vec![filler2]));
        let r3 = tree.boxes.insert(row(BoxKind::TableRow, vec![next]));
        let first = tree
            .boxes
            .insert(row(BoxKind::TableRowGroup, vec![r0, r1, r2]));
        let second = tree.boxes.insert(row(BoxKind::TableRowGroup, vec![r3]));
        let t = tree.boxes.insert(table(vec![first, second]));
        tree.set_root(t);

        let grid = build_grid(&tree, t);
        let ncols = grid.columns.len();
        assert_eq!(ncols, 2, "the later cells move right of the spanned one");
        assert_eq!(grid.rows.len(), 4);
        assert_eq!(slot_origin(grid.slots[0]), Some(tall));
        assert!(matches!(
            grid.slots[ncols],
            Slot::Spanned { origin: (0, 0) }
        ));
        assert!(matches!(
            grid.slots[2 * ncols],
            Slot::Spanned { origin: (0, 0) }
        ));
        assert_eq!(
            slot_origin(grid.slots[3 * ncols]),
            Some(next),
            "the next group is not spanned"
        );
    }

    #[test]
    fn slot_cap_moves_the_rest_of_the_rows_to_overflow() {
        // AC-15: a grid past 1,000,000 slots stops, and the remaining rows
        // are handed back to lay out as plain blocks.
        let mut tree = BoxTree::default();
        let mut rows = Vec::new();
        for _ in 0..1001 {
            let cell = tree.boxes.insert(cell(1000, 1));
            rows.push(tree.boxes.insert(row(BoxKind::TableRow, vec![cell])));
        }
        let group = tree.boxes.insert(row(BoxKind::TableRowGroup, rows));
        let t = tree.boxes.insert(table(vec![group]));
        tree.set_root(t);

        let grid = build_grid(&tree, t);
        assert_eq!(grid.rows.len(), 1000, "the cap is 1,000,000 slots");
        assert_eq!(grid.slots.len(), 1_000_000);
        assert_eq!(grid.overflow.len(), 1, "the last row overflows");
    }

    #[test]
    fn row_group_order_head_then_body_then_foot() {
        // Build cells first, then rows with cell ids, then groups with row
        // ids, then a table with group ids in out-of-tree order.
        let mut tree = BoxTree::default();
        let c_h = tree.boxes.insert(cell(1, 1));
        let c_b = tree.boxes.insert(cell(1, 1));
        let c_f = tree.boxes.insert(cell(1, 1));
        let r_h = tree.boxes.insert(row(BoxKind::TableRow, vec![c_h]));
        let r_b = tree.boxes.insert(row(BoxKind::TableRow, vec![c_b]));
        let r_f = tree.boxes.insert(row(BoxKind::TableRow, vec![c_f]));
        let thead = tree.boxes.insert(row(BoxKind::TableHeaderGroup, vec![r_h]));
        let tbody = tree.boxes.insert(row(BoxKind::TableRowGroup, vec![r_b]));
        let tfoot = tree.boxes.insert(row(BoxKind::TableFooterGroup, vec![r_f]));
        // Intentionally out of tree order: tfoot, tbody, thead.
        let t = tree.boxes.insert(table(vec![tfoot, tbody, thead]));
        tree.set_root(t);

        let grid = build_grid(&tree, t);
        assert_eq!(grid.rows.len(), 3, "thead, tbody, tfoot each one row");
        // First row comes from thead despite being last in children.
        assert_eq!(slot_origin(grid.slots[0]), Some(c_h));
        assert_eq!(slot_origin(grid.slots[1]), Some(c_b));
        assert_eq!(slot_origin(grid.slots[2]), Some(c_f));
    }

    #[test]
    fn slot_cap_stops_before_exceeding() {
        // A single row with enough cells to exceed 1,000,000 slots is not
        // possible with real cells, but a rowspan of 2 on a 1000x1000 grid
        // would exceed. Use many cells to force growth past the cap path.
        let mut tree = BoxTree::default();
        let mut children = Vec::new();
        for _ in 0..2000 {
            children.push(tree.boxes.insert(cell(1, 1)));
        }
        let r0 = tree.boxes.insert(row(BoxKind::TableRow, children));
        let rg = tree.boxes.insert(row(BoxKind::TableRowGroup, vec![r0]));
        let t = tree.boxes.insert(table(vec![rg]));
        tree.set_root(t);

        // 2000 cells in one row = 2000 slots, under the cap. Grid builds.
        let grid = build_grid(&tree, t);
        assert_eq!(grid.columns.len(), 2000);
        assert_eq!(grid.rows.len(), 1);
    }
}
