//! Table layout: sizes columns and places cells side by side in rows
//! (spec 0009, child 0004, AC-16).
//!
//! A table's columns are sized from their cells' min and max content, then
//! each cell is laid out at its column width and rows stack vertically. Row
//! heights then stretch each cell to the rows it spans and `vertical-align`
//! places the cell's content within it.

use std::rc::Rc;

use crate::box_tree::{BoxKind, BoxTree};
use crate::fragment::{BackgroundLayer, BlockMargins, BoxFragment, Fragment};
use crate::intrinsic::{content_sizes, contribution};
use crate::table::borders::collapsed_edges;
use crate::table::grid::{Slot, TableGrid, build_grid};
use crate::table::height::{cell_vertical_offset, row_heights, span_cols, span_rows};
use crate::table::size_columns;
use crate::{AvailableSize, ConstraintSpace, LayoutContext, border_widths, lay_out_box};
use base::{Au, BoxId, Point, Rect, SideOffsets, Size};
use css::{BorderCollapse, BoxSizing, TableLayout, VerticalAlign, WritingMode};

/// Lays out `table` in `space`, producing its fragment: cells sized into
/// columns by [`size_columns`], placed side by side in rows, rows stacked
/// vertically at their computed heights (spec 0009, child 0004, AC-16).
pub(crate) fn lay_out_table(
    tree: &mut BoxTree,
    id: BoxId,
    space: ConstraintSpace,
    cx: &mut LayoutContext<'_>,
) -> Option<Rc<BoxFragment>> {
    let layout_box = tree.get(id)?;
    let node = layout_box.node;
    let style = &layout_box.style;
    let basis = space.percentage_basis.width;
    let padding = SideOffsets {
        top: style.padding.top.resolve(basis),
        right: style.padding.right.resolve(basis),
        bottom: style.padding.bottom.resolve(basis),
        left: style.padding.left.resolve(basis),
    };
    // `border-spacing` around and between cells, except when borders collapse
    // (spec 0009, child 0004, AC-17).
    let border_collapse = layout_box.style.inherited_table.border_collapse;
    let (spacing_x, spacing_y) = match border_collapse {
        BorderCollapse::Collapse => (Au::ZERO, Au::ZERO),
        BorderCollapse::Separate => layout_box.style.inherited_table.border_spacing,
    };
    // With separate borders the grid sits inside the table's border and
    // padding. Collapsed borders resolve with the cells' and the table has
    // no padding (CSS 2.2 section 17.6.2).
    let (padding, inset) = match border_collapse {
        BorderCollapse::Collapse => (SideOffsets::default(), SideOffsets::default()),
        BorderCollapse::Separate => {
            let border = border_widths(style);
            let inset = SideOffsets {
                top: border.top + padding.top,
                right: border.right + padding.right,
                bottom: border.bottom + padding.bottom,
                left: border.left + padding.left,
            };
            (padding, inset)
        }
    };
    let grid = build_grid(tree, id);
    let ncols = grid.columns.len();
    let nrows = grid.rows.len();
    if ncols == 0 || nrows == 0 {
        return Some(Rc::new(empty_fragment(id, node, padding)));
    }

    // Cell min and max widths per grid slot, border box: a cell's padding
    // and borders widen its column too.
    let mut cell_min = vec![None; grid.slots.len()];
    let mut cell_max = vec![None; grid.slots.len()];
    let cells: Vec<Option<BoxId>> = (0..grid.slots.len())
        .map(|slot| grid.cell_at(slot))
        .collect();
    for slot in 0..grid.slots.len() {
        if let Some(cell_id) = cells[slot] {
            let (min, max) = content_sizes(tree, cell_id, cx);
            let (min, max) = contribution(tree, cell_id, cx).unwrap_or((min, max));
            cell_min[slot] = Some(min);
            cell_max[slot] = Some(max);
        }
    }

    let available = space.available_inline.definite()?;
    let specified_width = tree
        .get(id)
        .and_then(|b| b.style.size.width.clone())
        .map(|length| length.resolve(space.percentage_basis.width));
    // The wrapper already carries the table's width and resolves it once, so
    // the table fills the wrapper (spec 0009, child 0004, AC-14).
    let in_wrapper = tree.get(id).and_then(|b| b.parent).is_some_and(|parent| {
        tree.get(parent)
            .is_some_and(|b| b.kind == BoxKind::TableWrapper)
    });
    // `width: auto` makes a table shrink to fit, not fill its containing
    // block: its used width is its max content width, narrowed to what is
    // available but never below its min content width (CSS 2.2 section
    // 17.5.2.2).
    // `table_width` is the border box; the wrapper's width already holds the
    // table's border and padding.
    let table_width = match specified_width {
        _ if in_wrapper => available,
        Some(width) => match layout_box.style.size.box_sizing {
            BoxSizing::BorderBox => width,
            BoxSizing::ContentBox => width + inset.horizontal(),
        },
        None => {
            let (min, max) = content_sizes(tree, id, cx);
            (max + inset.horizontal())
                .min(available)
                .max(min + inset.horizontal())
        }
    };
    // `table-layout: fixed` only applies when the table has a definite width
    // (spec 0009, child 0004, AC-16).
    let fixed =
        layout_box.style.table.table_layout == TableLayout::Fixed && specified_width.is_some();
    // The grid sits inside `border-spacing`, so the columns share what is
    // left after the spacing around and between them.
    let spacing_total = spacing_x * (ncols + 1) as i32;
    let grid_width = (table_width - inset.horizontal() - spacing_total).max(Au::ZERO);
    let mut widths = size_columns(tree, &grid, grid_width, &cells, &cell_min, &cell_max, fixed);
    // Widths are computed before collapsing, so the other columns keep their
    // sizes and a collapsed column gives up its width (spec 0009, child 0004,
    // AC-18).
    for (col, width) in widths.iter_mut().enumerate() {
        if grid.columns[col].collapsed {
            *width = Au::ZERO;
        }
    }

    // Where each column starts: after the leading spacing and one spacing
    // between each pair of visible columns.
    let mut column_left = vec![Au::ZERO; ncols];
    let mut x = spacing_x;
    for (col, left) in column_left.iter_mut().enumerate() {
        *left = x;
        if !grid.columns[col].collapsed {
            x += widths[col] + spacing_x;
        }
    }

    // Pass 1: lay out every origin cell once, at the width of its column
    // span, and record its content height.
    let mut fragments: Vec<Option<Rc<Fragment>>> = vec![None; grid.slots.len()];
    let mut cell_heights: Vec<Option<Au>> = vec![None; grid.slots.len()];
    for row in 0..nrows {
        for col in 0..ncols {
            let slot = row * ncols + col;
            if !matches!(grid.slots.get(slot), Some(Slot::Origin(_))) {
                continue;
            }
            let Some(cell_id) = cells[slot] else {
                continue;
            };
            let span = span_cols(&grid, row, col);
            let span_width = widths[col..(col + span).min(ncols)]
                .iter()
                .copied()
                .sum::<Au>()
                + spacing_x * (span - 1) as i32;
            let cell_box = lay_out_box(tree, cell_id, cell_space(span_width), cx)?;
            let height = match &*cell_box {
                Fragment::Box(fragment) => fragment.size.height,
                _ => Au::ZERO,
            };
            fragments[slot] = Some(cell_box);
            cell_heights[slot] = Some(height);
        }
    }

    // Row heights from the cell heights, row `height`s and the table's own
    // `height` (spec 0009, child 0004, AC-16).
    let height_basis = space.percentage_basis.height;
    let table_height = tree
        .get(id)
        .and_then(|b| b.style.size.height.clone())
        .map(|length| length.resolve(height_basis));
    let row_specified: Vec<Option<Au>> = grid
        .rows
        .iter()
        .map(|row| {
            row.id
                .and_then(|row_id| tree.get(row_id))
                .and_then(|b| b.style.size.height.clone())
                .map(|length| length.resolve(height_basis))
        })
        .collect();
    let mut heights = row_heights(&grid, &cell_heights, &row_specified, table_height);
    // A collapsed row takes no height (spec 0009, child 0004, AC-18).
    for (row, height) in heights.iter_mut().enumerate() {
        if grid.rows[row].collapsed {
            *height = Au::ZERO;
        }
    }

    // A collapsed row or column paints nothing, and its cells do not size the
    // rows they sit in.
    for row in 0..nrows {
        for col in 0..ncols {
            if grid.rows[row].collapsed || grid.columns[col].collapsed {
                cell_heights[row * ncols + col] = None;
            }
        }
    }

    let mut row_top = vec![Au::ZERO; nrows];
    let mut row_bottom = vec![Au::ZERO; nrows];
    let mut y = spacing_y;
    for row in 0..nrows {
        row_top[row] = y;
        if !grid.rows[row].collapsed {
            y += heights[row];
            row_bottom[row] = y;
            y += spacing_y;
        } else {
            row_bottom[row] = y;
        }
    }
    let table_height = y;

    // Pass 2: place each cell at its row, stretch it over its row span and
    // shift its content for `vertical-align`.
    let mut children: Vec<(Point, Rc<Fragment>)> = Vec::new();
    for (row, &top) in row_top.iter().enumerate() {
        if grid.rows[row].collapsed {
            continue;
        }
        let row_baseline = baseline_of_row(&grid, &fragments, tree, &cells, row);
        for (col, &left) in column_left.iter().enumerate() {
            if grid.columns[col].collapsed {
                continue;
            }
            let slot = row * ncols + col;
            if !matches!(grid.slots.get(slot), Some(Slot::Origin(_))) {
                continue;
            }
            let (Some(fragment), Some(content_height), Some(cell_id)) =
                (fragments[slot].take(), cell_heights[slot], cells[slot])
            else {
                continue;
            };
            // A cell spanning a collapsed track is clipped to the tracks that
            // remain (spec 0009, child 0004, AC-18).
            let rows_spanned = span_rows(&grid, row, col);
            let last_row = (row + rows_spanned).min(nrows);
            let mut last_visible = row;
            let mut collapsed_row = false;
            for r in row..last_row {
                if grid.rows[r].collapsed {
                    collapsed_row = true;
                } else {
                    last_visible = r;
                }
            }
            let span_height = row_bottom[last_visible] - top;
            let cols_spanned = span_cols(&grid, row, col);
            let last_col = (col + cols_spanned).min(ncols);
            let mut collapsed_col = false;
            let mut visible_width = Au::ZERO;
            let mut visible_cols = 0usize;
            for (&width, column) in widths[col..last_col]
                .iter()
                .zip(&grid.columns[col..last_col])
            {
                if column.collapsed {
                    collapsed_col = true;
                } else {
                    visible_width += width;
                    visible_cols += 1;
                }
            }
            if visible_cols > 1 {
                visible_width += spacing_x * (visible_cols - 1) as i32;
            }
            let align = tree
                .get(cell_id)
                .map(|b| b.style.inherited_box.vertical_align)
                .unwrap_or_default();
            let offset = if rows_spanned == 1 && align == VerticalAlign::Baseline {
                match (row_baseline, &*fragment) {
                    (Some(row_baseline), Fragment::Box(cell)) => match cell.baseline {
                        Some(baseline) => (row_baseline - baseline).max(Au::ZERO),
                        None => Au::ZERO,
                    },
                    _ => Au::ZERO,
                }
            } else {
                cell_vertical_offset(align, content_height, span_height)
            };
            let clip = (collapsed_row || collapsed_col).then_some((visible_width, span_height));
            let placed = align_cell(fragment, span_height, offset, clip);
            children.push((Point::new(left, top), placed));
        }
    }

    // Rows past the 1,000,000 slot cap lay out as plain blocks below the
    // grid, with one `warn` (spec 0009, child 0004, AC-15).
    let mut overflow_height = Au::ZERO;
    if !grid.overflow.is_empty() {
        tracing::warn!(
            rows = grid.overflow.len(),
            "a table grid passed 1,000,000 slots; its remaining rows lay out as plain blocks"
        );
        for &row in &grid.overflow {
            if let Some(fragment) = lay_out_box(tree, row, cell_space(table_width), cx) {
                let height = match &*fragment {
                    Fragment::Box(fragment) => fragment.size.height,
                    _ => Au::ZERO,
                };
                children.push((
                    Point::new(Au::ZERO, table_height + overflow_height),
                    fragment,
                ));
                overflow_height += height;
            }
        }
    }
    let full_height = table_height + overflow_height + inset.vertical();

    // The grid was placed from the border box corner; move it inside the
    // table's border and padding.
    let shift = Point::new(inset.left, inset.top);
    for (point, _) in &mut children {
        *point = point.offset_by(shift);
    }
    let size = Size::new(x + inset.horizontal(), full_height);
    // The six background layers paint table, column groups, columns, row
    // groups, rows, cells; the cells paint themselves, so layout hands paint
    // the four middle layers (spec 0009, child 0004, AC-17).
    let mut layers = table_background_layers(
        tree,
        &grid,
        x,
        table_height,
        &column_left,
        &widths,
        &row_top,
        &heights,
    );
    for layer in &mut layers {
        layer.rect.origin = layer.rect.origin.offset_by(shift);
    }
    let background_layers = Some(Rc::from(layers));
    // `border-collapse: collapse` resolves every shared edge once, for paint
    // to draw in one pass after the cell backgrounds (spec 0009, child 0004,
    // AC-17).
    let collapsed_borders = (border_collapse == BorderCollapse::Collapse).then(|| {
        Rc::from(collapsed_edges(
            tree,
            &grid,
            &cells,
            id,
            x,
            table_height,
            &column_left,
            &widths,
            &row_top,
            &heights,
        ))
    });
    Some(Rc::new(BoxFragment {
        box_id: id,
        node,
        size,
        children,
        scrollable_overflow: Rect::default(),
        baseline: None,
        last_baseline: None,
        margins: BlockMargins::default(),
        padding,
        inline_edges: None,
        auto_height: full_height,
        relative_offset: Point::default(),
        sticky: None,
        absolutes: Vec::new(),
        exclusions_out: None,
        user_scrollable: [false, false],
        is_scroll_container: false,
        collapsed_borders,
        background_layers,
    }))
}

/// The column group, column, row group and row backgrounds of a table, in
/// paint order, from its border box top left corner (spec 0009, child 0004,
/// AC-17). A layer only appears when its box paints a color.
#[allow(
    clippy::too_many_arguments,
    reason = "the grid geometry is already spread across the caller"
)]
fn table_background_layers(
    tree: &BoxTree,
    grid: &TableGrid,
    table_width: Au,
    table_height: Au,
    column_left: &[Au],
    widths: &[Au],
    row_top: &[Au],
    heights: &[Au],
) -> Vec<BackgroundLayer> {
    let mut column_groups: Vec<(BoxId, Rect)> = Vec::new();
    let mut columns: Vec<BackgroundLayer> = Vec::new();
    let mut row_groups: Vec<(BoxId, Rect)> = Vec::new();
    let mut rows: Vec<BackgroundLayer> = Vec::new();

    for (col, column) in grid.columns.iter().enumerate() {
        if column.collapsed {
            continue;
        }
        let Some(id) = column.id else {
            continue;
        };
        let Some(column_box) = tree.get(id) else {
            continue;
        };
        let rect = Rect::new(
            Point::new(column_left[col], Au::ZERO),
            Size::new(widths[col], table_height),
        );
        if let Some(color) = background_of(column_box) {
            columns.push(BackgroundLayer { rect, color });
        }
        // A group with `<col>` children paints their union; a group without
        // them is the column itself.
        if column_box.kind == BoxKind::TableColumnGroup {
            merge_layer(&mut column_groups, id, rect);
        } else if let Some(parent) = column_box.parent
            && tree
                .get(parent)
                .is_some_and(|b| b.kind == BoxKind::TableColumnGroup)
        {
            merge_layer(&mut column_groups, parent, rect);
        }
    }

    for (row, row_info) in grid.rows.iter().enumerate() {
        if row_info.collapsed {
            continue;
        }
        let Some(id) = row_info.id else {
            continue;
        };
        let Some(row_box) = tree.get(id) else {
            continue;
        };
        let rect = Rect::new(
            Point::new(Au::ZERO, row_top[row]),
            Size::new(table_width, heights[row]),
        );
        if let Some(color) = background_of(row_box) {
            rows.push(BackgroundLayer { rect, color });
        }
        if let Some(parent) = row_box.parent
            && tree.get(parent).is_some_and(|b| {
                matches!(
                    b.kind,
                    BoxKind::TableRowGroup | BoxKind::TableHeaderGroup | BoxKind::TableFooterGroup
                )
            })
        {
            merge_layer(&mut row_groups, parent, rect);
        }
    }

    let mut layers = Vec::new();
    for (id, rect) in column_groups {
        if let Some(color) = tree.get(id).and_then(background_of) {
            layers.push(BackgroundLayer { rect, color });
        }
    }
    layers.extend(columns);
    for (id, rect) in row_groups {
        if let Some(color) = tree.get(id).and_then(background_of) {
            layers.push(BackgroundLayer { rect, color });
        }
    }
    layers.extend(rows);
    layers
}

/// The box's background color, when it paints one.
fn background_of(layout_box: &crate::box_tree::LayoutBox) -> Option<css::Color> {
    let color = layout_box.style.background.color;
    (color.a > 0.0).then_some(color)
}

/// Grows the layer for `id` to cover `rect`, adding it when new.
fn merge_layer(layers: &mut Vec<(BoxId, Rect)>, id: BoxId, rect: Rect) {
    if let Some((_, existing)) = layers.iter_mut().find(|(layer, _)| *layer == id) {
        let left = existing.origin.x.min(rect.origin.x);
        let top = existing.origin.y.min(rect.origin.y);
        let right = (existing.origin.x + existing.size.width).max(rect.origin.x + rect.size.width);
        let bottom =
            (existing.origin.y + existing.size.height).max(rect.origin.y + rect.size.height);
        *existing = Rect::new(Point::new(left, top), Size::new(right - left, bottom - top));
    } else {
        layers.push((id, rect));
    }
}

/// The row's baseline: the largest first line baseline among its
/// `vertical-align: baseline` cells that span only this row (spec 0009,
/// child 0004, AC-16). `None` when no cell sets one.
fn baseline_of_row(
    grid: &TableGrid,
    fragments: &[Option<Rc<Fragment>>],
    tree: &BoxTree,
    cells: &[Option<BoxId>],
    row: usize,
) -> Option<Au> {
    let ncols = grid.columns.len();
    let mut baseline: Option<Au> = None;
    for col in 0..ncols {
        let slot = row * ncols + col;
        if !matches!(grid.slots.get(slot), Some(Slot::Origin(_))) {
            continue;
        }
        if span_rows(grid, row, col) > 1 {
            continue;
        }
        let Some(cell_id) = cells[slot] else {
            continue;
        };
        if tree
            .get(cell_id)
            .map(|b| b.style.inherited_box.vertical_align)
            .unwrap_or_default()
            != VerticalAlign::Baseline
        {
            continue;
        }
        if let Some(Fragment::Box(cell)) = fragments[slot].as_deref()
            && let Some(cell_baseline) = cell.baseline
        {
            baseline = Some(baseline.map_or(cell_baseline, |b: Au| b.max(cell_baseline)));
        }
    }
    baseline
}

/// Gives a cell fragment the height of its row span and shifts its content
/// down by `offset` for `vertical-align` (spec 0009, child 0004, AC-16).
/// `clip` holds the width and height a cell spanning a collapsed track is cut
/// to (spec 0009, child 0004, AC-18).
fn align_cell(
    fragment: Rc<Fragment>,
    span_height: Au,
    offset: Au,
    clip: Option<(Au, Au)>,
) -> Rc<Fragment> {
    let Fragment::Box(cell) = &*fragment else {
        return fragment;
    };
    let clip_width = clip
        .map(|(width, _)| width)
        .filter(|&width| width < cell.size.width);
    let clip_height = clip
        .map(|(_, height)| height)
        .filter(|&height| height < cell.size.height);
    if offset == Au::ZERO
        && cell.size.height >= span_height
        && clip_width.is_none()
        && clip_height.is_none()
    {
        return fragment;
    }
    let mut cell = cell.clone();
    cell.size.height = cell.size.height.max(span_height);
    if let Some(width) = clip_width {
        cell.size.width = width;
    }
    if let Some(height) = clip_height {
        cell.size.height = height;
    }
    if offset != Au::ZERO {
        for (point, _) in &mut cell.children {
            point.y += offset;
        }
        if let Some(baseline) = &mut cell.baseline {
            *baseline += offset;
        }
        if let Some(baseline) = &mut cell.last_baseline {
            *baseline += offset;
        }
    }
    Rc::new(Fragment::Box(cell))
}

/// The constraint space for a cell: a definite inline size equal to its
/// column span width, and indefinite block size so the cell sizes to its
/// content height (the row then takes the tallest cell). The cell's border
/// box always fills its columns; its own `width` only sized them (CSS
/// Tables 3, cell widths).
fn cell_space(column_width: Au) -> ConstraintSpace {
    ConstraintSpace {
        available_inline: AvailableSize::Definite(column_width),
        available_block: AvailableSize::MaxContent,
        percentage_basis: Size::new(Some(column_width), None),
        writing_mode: WritingMode::default(),
        stretch_inline: Some(column_width),
        stretch_block: None,
        bfc_offset: None,
        exclusions: None,
        clearance: None,
    }
}

/// An empty table fragment.
fn empty_fragment(id: BoxId, node: Option<dom::NodeId>, padding: SideOffsets) -> BoxFragment {
    BoxFragment {
        box_id: id,
        node,
        size: Size::default(),
        children: Vec::new(),
        scrollable_overflow: Rect::default(),
        baseline: None,
        last_baseline: None,
        margins: BlockMargins::default(),
        padding,
        inline_edges: None,
        auto_height: Au::ZERO,
        relative_offset: Point::default(),
        sticky: None,
        absolutes: Vec::new(),
        exclusions_out: None,
        user_scrollable: [false, false],
        is_scroll_container: false,
        collapsed_borders: None,
        background_layers: None,
    }
}
