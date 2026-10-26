//! Collapsed border resolution for tables (CSS 2.2 section 17.6.2.1).
//!
//! For each grid edge, the borders of the cells, rows, row groups, columns,
//! column groups and table touching it compete, and one wins:
//! `hidden` suppresses the edge entirely, then the widest border wins, then
//! the most preferred style (`double` before `solid` before `dashed` before
//! `dotted`), then the border from the cell wins over row over row group
//! over column over column group over table. The winner's color is what
//! paints the edge; each cell's border box takes half of each winning edge
//! (layout sizes cells with those half borders).

use base::{Au, BoxId, Point, Rect, Size};
use css::{BorderStyle, Color, ComputedStyle};

use crate::box_tree::{BoxKind, BoxTree};
use crate::table::grid::{Slot, TableGrid};
use crate::table::height::{span_cols, span_rows};

/// Where a border comes from, in CSS 2.2's tie-break order: lower wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Origin {
    /// A table cell (`td` or `th`).
    Cell,
    /// A table row (`tr`).
    Row,
    /// A table row group (`thead`, `tbody` or `tfoot`).
    RowGroup,
    /// A table column (`col`).
    Column,
    /// A table column group (`colgroup`).
    ColumnGroup,
    /// The table itself.
    Table,
}

/// One border contributed to one collapsed edge, with where it comes from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CollapsedBorder {
    /// The border's computed width: zero for `none`.
    pub width: Au,
    /// The border's computed style.
    pub style: BorderStyle,
    /// The border's computed color, with `currentcolor` already resolved.
    pub color: Color,
    /// Where the border comes from.
    pub origin: Origin,
}

/// How preferred a border style is when widths tie (higher wins). Styles
/// this crate does not parse yet (`ridge`, `outset`, `groove`, `inset`)
/// rank below `dotted`.
fn style_rank(style: BorderStyle) -> u8 {
    match style {
        BorderStyle::Double => 5,
        BorderStyle::Solid => 4,
        BorderStyle::Dashed => 3,
        BorderStyle::Dotted => 2,
        BorderStyle::None | BorderStyle::Hidden => 1,
    }
}

/// Resolves one collapsed edge: the winning border, or `None` when `hidden`
/// suppresses the edge or every border is `none`.
pub fn resolve_edge(borders: &[CollapsedBorder]) -> Option<CollapsedBorder> {
    // `hidden` wins and suppresses every other border at the edge.
    if borders.iter().any(|b| b.style == BorderStyle::Hidden) {
        return None;
    }
    let max_width = borders.iter().map(|b| b.width).max()?;
    if max_width == Au::ZERO {
        return None;
    }
    borders
        .iter()
        .filter(|b| b.width == max_width)
        .max_by(|a, b| {
            style_rank(a.style)
                .cmp(&style_rank(b.style))
                .then_with(|| a.origin.cmp(&b.origin).reverse())
        })
        .copied()
}

/// One resolved collapsed border edge, relative to the table's border box
/// top left corner (spec 0009, child 0004, AC-17).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CollapsedEdge {
    /// The edge's rect: a thin strip along the grid line.
    pub rect: Rect,
    /// The winning border's color.
    pub color: Color,
    /// The winning border's style.
    pub style: BorderStyle,
}

/// Which side of a box a border comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Top,
    Right,
    Bottom,
    Left,
}

/// One box's border on `side`, tagged with where it comes from.
fn side(style: &ComputedStyle, side: Side, origin: Origin) -> CollapsedBorder {
    let border = match side {
        Side::Top => &style.border.top,
        Side::Right => &style.border.right,
        Side::Bottom => &style.border.bottom,
        Side::Left => &style.border.left,
    };
    CollapsedBorder {
        width: border.width,
        style: border.style,
        color: border.color,
        origin,
    }
}

/// A box's border on `side`, with its origin from its kind.
fn box_side(style: &ComputedStyle, kind: BoxKind, side_kind: Side) -> Option<CollapsedBorder> {
    let origin = match kind {
        BoxKind::TableCell => Origin::Cell,
        BoxKind::TableRow => Origin::Row,
        BoxKind::TableRowGroup | BoxKind::TableHeaderGroup | BoxKind::TableFooterGroup => {
            Origin::RowGroup
        }
        BoxKind::TableColumn => Origin::Column,
        BoxKind::TableColumnGroup => Origin::ColumnGroup,
        BoxKind::Table => Origin::Table,
        _ => return None,
    };
    Some(side(style, side_kind, origin))
}

/// The origin cell covering grid slot `(row, col)`: its origin position and
/// its box id.
fn origin_of(
    grid: &TableGrid,
    cells: &[Option<BoxId>],
    row: usize,
    col: usize,
) -> Option<(usize, usize, BoxId)> {
    let ncols = grid.columns.len();
    let slot = row * ncols + col;
    let cell = cells.get(slot).copied().flatten()?;
    match grid.slots.get(slot)? {
        Slot::Origin(_) => Some((row, col, cell)),
        Slot::Spanned { origin } => Some((origin.0, origin.1, cell)),
    }
}

/// Resolves every collapsed edge of a table (spec 0009, child 0004, AC-17).
///
/// `column_left`/`widths` and `row_top`/`heights` are the used grid geometry
/// in the table's border box coordinates. Collapsed rows and columns are
/// skipped, and so is `visibility: collapse`.
#[allow(
    clippy::too_many_arguments,
    clippy::needless_range_loop,
    reason = "the grid geometry is already spread across the caller, and a boundary indexes several of its arrays"
)]
pub(crate) fn collapsed_edges(
    tree: &BoxTree,
    grid: &TableGrid,
    cells: &[Option<BoxId>],
    table: BoxId,
    table_width: Au,
    table_height: Au,
    column_left: &[Au],
    widths: &[Au],
    row_top: &[Au],
    heights: &[Au],
) -> Vec<CollapsedEdge> {
    let ncols = grid.columns.len();
    let nrows = grid.rows.len();
    let table_style = tree.get(table).map(|b| b.style.clone());
    let mut edges = Vec::new();

    // Vertical edges: one per boundary per visible row.
    for row in 0..nrows {
        if grid.rows[row].collapsed {
            continue;
        }
        let row_box = grid.rows[row].id.and_then(|id| tree.get(id));
        let group_box = row_box
            .and_then(|b| b.parent)
            .and_then(|parent| tree.get(parent));
        for boundary in 0..=ncols {
            let mut borders: Vec<CollapsedBorder> = Vec::new();
            if let Some(table_box) = &table_style {
                if boundary == 0 {
                    borders.push(side(table_box, Side::Left, Origin::Table));
                } else if boundary == ncols {
                    borders.push(side(table_box, Side::Right, Origin::Table));
                }
            }
            if let Some(row_box) = row_box {
                let side_kind = if boundary == 0 {
                    Side::Left
                } else {
                    Side::Right
                };
                borders.push(side(&row_box.style, side_kind, Origin::Row));
            }
            if let Some(group_box) = group_box {
                let side_kind = if boundary == 0 {
                    Side::Left
                } else {
                    Side::Right
                };
                borders.push(side(&group_box.style, side_kind, Origin::RowGroup));
            }
            if boundary > 0 {
                push_column(&mut borders, tree, grid, boundary - 1, Side::Right);
                if let Some((or, oc, cell)) = origin_of(grid, cells, row, boundary - 1)
                    && let Some(cell_box) = tree.get(cell)
                    && oc + span_cols(grid, or, oc) == boundary
                {
                    borders.push(side(&cell_box.style, Side::Right, Origin::Cell));
                }
            }
            if boundary < ncols {
                push_column(&mut borders, tree, grid, boundary, Side::Left);
                if let Some((_, oc, cell)) = origin_of(grid, cells, row, boundary)
                    && let Some(cell_box) = tree.get(cell)
                    && oc == boundary
                {
                    borders.push(side(&cell_box.style, Side::Left, Origin::Cell));
                }
            }
            let Some(winner) = resolve_edge(&borders) else {
                continue;
            };
            let x = if boundary == ncols {
                table_width
            } else {
                column_left[boundary]
            };
            let rect = Rect::new(
                Point::new(x - Au(winner.width.0 / 2), row_top[row]),
                Size::new(winner.width, heights[row]),
            );
            edges.push(CollapsedEdge {
                rect,
                color: winner.color,
                style: winner.style,
            });
        }
    }

    // Horizontal edges: one per boundary per visible column.
    for boundary in 0..=nrows {
        let above = boundary.checked_sub(1).filter(|&r| !grid.rows[r].collapsed);
        let below = (boundary < nrows && !grid.rows[boundary].collapsed).then_some(boundary);
        if above.is_none() && below.is_none() {
            continue;
        }
        for col in 0..ncols {
            if grid.columns[col].collapsed {
                continue;
            }
            let mut borders: Vec<CollapsedBorder> = Vec::new();
            if let Some(table_box) = &table_style {
                if boundary == 0 {
                    borders.push(side(table_box, Side::Top, Origin::Table));
                } else if boundary == nrows {
                    borders.push(side(table_box, Side::Bottom, Origin::Table));
                }
            }
            for row in [above, below].into_iter().flatten() {
                let is_above = Some(row) == above;
                let side_kind = if is_above { Side::Bottom } else { Side::Top };
                if let Some(row_id) = grid.rows[row].id {
                    if let Some(row_box) = tree.get(row_id) {
                        borders.push(side(&row_box.style, side_kind, Origin::Row));
                        if let Some(parent) = row_box.parent
                            && let Some(group_box) = tree.get(parent)
                        {
                            borders.push(side(&group_box.style, side_kind, Origin::RowGroup));
                        }
                    }
                    if let Some((or, oc, cell)) = origin_of(grid, cells, row, col)
                        && let Some(cell_box) = tree.get(cell)
                    {
                        let matches = if is_above {
                            or + span_rows(grid, or, oc) == boundary
                        } else {
                            or == boundary
                        };
                        if matches {
                            borders.push(side(&cell_box.style, side_kind, Origin::Cell));
                        }
                    }
                }
            }
            push_column(&mut borders, tree, grid, col, Side::Top);
            push_column(&mut borders, tree, grid, col, Side::Bottom);
            let Some(winner) = resolve_edge(&borders) else {
                continue;
            };
            let y = if boundary == nrows {
                table_height
            } else {
                row_top[boundary]
            };
            let rect = Rect::new(
                Point::new(column_left[col], y - Au(winner.width.0 / 2)),
                Size::new(widths[col], winner.width),
            );
            edges.push(CollapsedEdge {
                rect,
                color: winner.color,
                style: winner.style,
            });
        }
    }

    edges
}

/// Adds grid column `col`'s own border on `side_kind`, from its `<col>` or
/// `<colgroup>` box, if it has one.
fn push_column(
    borders: &mut Vec<CollapsedBorder>,
    tree: &BoxTree,
    grid: &TableGrid,
    col: usize,
    side_kind: Side,
) {
    let Some(id) = grid.columns[col].id else {
        return;
    };
    let Some(column_box) = tree.get(id) else {
        return;
    };
    if let Some(border) = box_side(&column_box.style, column_box.kind, side_kind) {
        borders.push(border);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn border(width_px: f32, style: BorderStyle, origin: Origin) -> CollapsedBorder {
        CollapsedBorder {
            width: Au::from_px(width_px),
            style,
            color: Color::BLACK,
            origin,
        }
    }

    #[test]
    fn the_widest_border_wins() {
        // AC-17: a 3 px cell border beats a 1 px table border on the same
        // edge; the cell's border box takes half of the winning edge.
        let winner = resolve_edge(&[
            border(1.0, BorderStyle::Solid, Origin::Table),
            border(3.0, BorderStyle::Solid, Origin::Cell),
        ]);
        let Some(w) = winner else {
            panic!("expected a winning border");
        };
        assert_eq!(w.width, Au::from_px(3.0));
        assert_eq!(w.origin, Origin::Cell);
    }

    #[test]
    fn hidden_suppresses_the_whole_edge() {
        // `hidden` wins regardless of width, so the edge draws nothing.
        let winner = resolve_edge(&[
            border(5.0, BorderStyle::Solid, Origin::Cell),
            border(1.0, BorderStyle::Hidden, Origin::Table),
        ]);
        assert_eq!(winner, None);
    }

    #[test]
    fn style_rank_breaks_width_ties() {
        // Equal widths: `double` is preferred over `solid`.
        let winner = resolve_edge(&[
            border(2.0, BorderStyle::Solid, Origin::Cell),
            border(2.0, BorderStyle::Double, Origin::Table),
        ]);
        let Some(w) = winner else {
            panic!("expected a winning border");
        };
        assert_eq!(w.style, BorderStyle::Double);
        assert_eq!(w.origin, Origin::Table);
    }

    #[test]
    fn origin_order_breaks_full_ties() {
        // Equal width and style: the cell wins over the table.
        let winner = resolve_edge(&[
            border(2.0, BorderStyle::Solid, Origin::Table),
            border(2.0, BorderStyle::Solid, Origin::Cell),
        ]);
        let Some(w) = winner else {
            panic!("expected a winning border");
        };
        assert_eq!(w.origin, Origin::Cell);
    }

    #[test]
    fn origins_rank_cell_first_table_last() {
        // Every origin is constructed, and CSS 2.2's tie-break order is
        // cell, row, row group, column, column group, table.
        let origins = [
            Origin::Cell,
            Origin::Row,
            Origin::RowGroup,
            Origin::Column,
            Origin::ColumnGroup,
            Origin::Table,
        ];
        let mut sorted = origins;
        sorted.sort();
        assert_eq!(sorted, origins);
        // Equal width and style: the lowest origin wins each pairwise contest.
        for i in 0..origins.len() {
            for j in (i + 1)..origins.len() {
                let a = origins[i];
                let b = origins[j];
                let winner = resolve_edge(&[
                    border(2.0, BorderStyle::Solid, a),
                    border(2.0, BorderStyle::Solid, b),
                ]);
                assert_eq!(
                    winner.as_ref().map(|b| b.origin),
                    Some(a),
                    "{a:?} should beat {b:?}"
                );
            }
        }
    }

    #[test]
    fn all_none_borders_mean_no_edge() {
        let winner = resolve_edge(&[
            border(0.0, BorderStyle::None, Origin::Cell),
            border(0.0, BorderStyle::None, Origin::Table),
        ]);
        assert_eq!(winner, None);
    }
}
