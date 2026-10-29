//! Grid layout (spec 0010, AC-1 to AC-5): CSS Grid Level 1.
//!
//! The container expands its explicit grid, places every item, sizes the
//! columns and then the rows with the Grid §11 algorithm, aligns the tracks
//! inside the container, and lays each item out in its area. Every item
//! starts its own formatting context, so no margin collapses with the
//! container or a sibling. Absolutely positioned children are not items:
//! one with a definite grid area uses that area as its containing block and
//! the rest use the container's padding box (AC-5).

use std::collections::HashSet;
use std::rc::Rc;

use base::atoms::Atom;
use base::{Au, BoxId, Point, Rect, Size};
use css::{
    AlignItems, Breadth, ComputedStyle, Display, GridArea, GridAutoFlow, GridLine, JustifyContent,
    Length, RepeatCount, Sizes, TrackEntry, TrackList, TrackSize,
};

use crate::block::{GridItems, InProgress, resolve};
use crate::box_tree::{BoxTree, LayoutBox};
use crate::fragment::{Fragment, PendingAbsolute};
use crate::{AvailableSize, ConstraintSpace, LayoutContext, intrinsic, lay_out_box};

/// An item's cell range, ends exclusive.
type Cell = ((usize, usize), (usize, usize));

/// One side of an item's placement: a definite track index, or `None` for
/// auto placement, and how many tracks it spans.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Axis {
    start: Option<usize>,
    span: usize,
}

impl Axis {
    fn auto() -> Self {
        Self {
            start: None,
            span: 1,
        }
    }
}

/// One grid item partway through the algorithm.
struct Item {
    id: BoxId,
    style: Rc<ComputedStyle>,
    col: Axis,
    row: Axis,
    cell: Cell,
    /// It has been given a cell.
    placed: bool,
}

/// One resolved track.
#[derive(Clone)]
struct Track {
    /// The minimum track sizing function.
    min: Breadth,
    /// The maximum track sizing function.
    max: Breadth,
    /// The base size (Grid §11).
    base: Au,
    /// The growth limit; `None` means infinite.
    limit: Option<Au>,
    /// `fit-content()`: the limit is the smaller of its argument and the
    /// content's max content size.
    fit_content: bool,
    /// `fr` factor; zero for a non flexible track.
    flex: f32,
    /// The maximum is `auto`, so `align-content: stretch` may grow it.
    auto_max: bool,
    /// The track came from `repeat(auto-fit, ...)` and collapses when empty.
    auto_fit: bool,
    /// It was empty and clamped to zero (Grid §7.2.3.2).
    collapsed: bool,
}

impl Track {
    fn new(size: &TrackSize, auto_fit: bool) -> Self {
        let (min, max, fit_content) = match size {
            // A bare `fr` has an `auto` minimum (Grid §7.2.4).
            TrackSize::Breadth(Breadth::Fr(fr)) => (Breadth::Auto, Breadth::Fr(*fr), false),
            TrackSize::Breadth(breadth) => (breadth.clone(), breadth.clone(), false),
            TrackSize::MinMax(min, max) => (min.clone(), max.clone(), false),
            TrackSize::FitContent(length) => (Breadth::Auto, Breadth::Length(length.clone()), true),
        };
        let flex = match &max {
            Breadth::Fr(fr) => *fr,
            _ => 0.0,
        };
        let auto_max = matches!(max, Breadth::Auto);
        Self {
            min,
            max,
            base: Au::ZERO,
            limit: None,
            fit_content,
            flex,
            auto_max,
            auto_fit,
            collapsed: false,
        }
    }

    /// Resolves the fixed parts of the track against `basis`.
    fn init(&mut self, basis: Option<Au>) {
        self.base = match &self.min {
            Breadth::Length(length) => length.resolve(basis),
            _ => Au::ZERO,
        };
        self.limit = match &self.max {
            Breadth::Length(length) => Some(length.resolve(basis)),
            // `fr`, `auto`, `min-content` and `max-content` start unlimited.
            Breadth::Fr(_) | Breadth::Auto | Breadth::MinContent | Breadth::MaxContent => None,
        };
        if let Some(limit) = self.limit
            && limit < self.base
        {
            self.limit = Some(self.base);
        }
    }

    fn is_flexible(&self) -> bool {
        self.flex > 0.0
    }

    /// Whether its minimum grows from content.
    fn intrinsic_min(&self) -> bool {
        matches!(
            self.min,
            Breadth::Auto | Breadth::MinContent | Breadth::MaxContent
        )
    }

    /// Whether its maximum grows from content.
    fn intrinsic_max(&self) -> bool {
        matches!(
            self.max,
            Breadth::Auto | Breadth::MinContent | Breadth::MaxContent
        ) || self.fit_content
    }
}

/// The expanded explicit grid: its tracks and the names on every line.
struct ExplicitGrid {
    tracks: Vec<Track>,
    /// `lines[i]` names the line before track `i`; the last names the end.
    lines: Vec<Vec<Atom>>,
}

/// Lays out the grid container `container` and hands its items to it, placed.
pub(crate) fn lay_out(tree: &mut BoxTree, container: &mut InProgress, cx: &mut LayoutContext<'_>) {
    let Some(layout_box) = tree.get(container.id) else {
        return;
    };
    let style = Rc::clone(&layout_box.style);
    let child_ids = layout_box.children.clone();
    let frame = container.grid_frame();
    let space = container.child_space();
    let grid = &style.grid;

    // Absolute and fixed children are not items. Their static position is
    // the content box's start corner unless they have a definite area.
    let (absolute_ids, item_ids): (Vec<BoxId>, Vec<BoxId>) = child_ids.iter().partition(|&&id| {
        tree.get(id)
            .is_some_and(|b| b.is_absolute() || b.is_fixed())
    });

    let column_gap = style
        .alignment
        .column_gap
        .resolve(Some(frame.content_width));
    let row_gap = style.alignment.row_gap.resolve(frame.height);

    // Steps 1 and 2: expand the explicit grid and place every item.
    let mut columns = expand_grid(
        &grid.template_columns,
        Some(frame.content_width),
        column_gap,
    );
    let mut rows = expand_grid(&grid.template_rows, frame.height, row_gap);
    let mut items: Vec<Item> = item_ids
        .iter()
        .filter_map(|&id| {
            let style = tree.get(id).map(|b| Rc::clone(&b.style))?;
            Some(Item {
                id,
                style,
                col: Axis::auto(),
                row: Axis::auto(),
                cell: ((0, 0), (1, 1)),
                placed: false,
            })
        })
        .collect();
    // `order` modified document order (Grid §5.4).
    items.sort_by_key(|item| item.style.flex.order);
    for item in &mut items {
        let item_grid = &item.style.grid;
        let (col_start, col_span) = resolve_axis(
            &item_grid.column_start,
            &item_grid.column_end,
            &columns.lines,
            &grid.template_areas,
            false,
        );
        let (row_start, row_span) = resolve_axis(
            &item_grid.row_start,
            &item_grid.row_end,
            &rows.lines,
            &grid.template_areas,
            true,
        );
        item.col = Axis {
            start: col_start,
            span: col_span,
        };
        item.row = Axis {
            start: row_start,
            span: row_span,
        };
    }
    let (col_count, row_count) = place_items(
        &mut items,
        grid.auto_flow,
        columns.tracks.len(),
        rows.tracks.len(),
    );
    grow_tracks(
        &mut columns,
        col_count,
        &grid.auto_columns,
        Some(frame.content_width),
    );
    grow_tracks(&mut rows, row_count, &grid.auto_rows, frame.height);

    // Step 3: every empty `auto-fit` track collapses to zero.
    collapse_auto_fit(&mut columns.tracks, &items, true);
    collapse_auto_fit(&mut rows.tracks, &items, false);

    // Step 4: size the columns, then the rows.
    size_columns(
        tree,
        &items,
        &mut columns.tracks,
        frame.content_width,
        column_gap,
        style.alignment.justify_content,
        cx,
    );
    size_rows(
        tree,
        &items,
        &columns.tracks,
        &mut rows.tracks,
        frame.height,
        column_gap,
        row_gap,
        style.alignment.align_content,
        &space,
        cx,
    );
    if columns.tracks.is_empty() {
        columns
            .tracks
            .push(Track::new(&TrackSize::Breadth(Breadth::Auto), false));
        columns.lines.push(Vec::new());
    }
    if rows.tracks.is_empty() {
        rows.tracks
            .push(Track::new(&TrackSize::Breadth(Breadth::Auto), false));
        rows.lines.push(Vec::new());
    }

    // Step 7: align the grid's tracks inside the container.
    let column_space = grid_length(&columns.tracks, column_gap);
    let row_space = grid_length(&rows.tracks, row_gap);
    let (col_offset, col_extra) = grid_alignment(
        style.alignment.justify_content,
        frame.content_width - column_space,
        visible_count(&columns.tracks),
    );
    let (row_offset, row_extra) = grid_alignment(
        style.alignment.align_content,
        frame.height.map_or(Au::ZERO, |height| height - row_space),
        visible_count(&rows.tracks),
    );
    let col_gap = column_gap + col_extra;
    let row_gap = row_gap + row_extra;
    let col_starts = track_starts(&columns.tracks, column_gap, col_extra, col_offset);
    let row_starts = track_starts(&rows.tracks, row_gap - row_extra, row_extra, row_offset);

    // Steps 6 and 8: lay the items out in their areas and hand the absolute
    // children their containing blocks.
    let mut out = GridItems::default();
    for item in &items {
        let area = area_rect(
            item.cell,
            &col_starts,
            &row_starts,
            &columns.tracks,
            &rows.tracks,
            col_gap,
            row_gap,
        );
        let Some((at, fragment)) = place_item(tree, item, area, &style, &space, cx) else {
            continue;
        };
        let at = at.offset_by(
            fragment
                .as_box()
                .map_or(Point::default(), |child| child.relative_offset),
        );
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
    for &box_id in &absolute_ids {
        let fixed = tree.get(box_id).is_some_and(LayoutBox::is_fixed);
        let Some(layout_box) = tree.get(box_id) else {
            continue;
        };
        let item_grid = &layout_box.style.grid;
        let (col, col_span) = resolve_axis(
            &item_grid.column_start,
            &item_grid.column_end,
            &columns.lines,
            &grid.template_areas,
            false,
        );
        let (row, row_span) = resolve_axis(
            &item_grid.row_start,
            &item_grid.row_end,
            &rows.lines,
            &grid.template_areas,
            true,
        );
        let (containing_block, static_position) = match (col, row) {
            (Some(col), Some(row)) => {
                let cell = ((col, row), (col + col_span, row + row_span));
                let area = area_rect(
                    cell,
                    &col_starts,
                    &row_starts,
                    &columns.tracks,
                    &rows.tracks,
                    col_gap,
                    row_gap,
                );
                (Some(area), area.origin)
            }
            _ => (None, Point::default()),
        };
        // The static start is the area's start corner so the box lands at the
        // area when `lay_out_absolute` resolves `static_position - cb.origin`.
        out.absolutes.push(PendingAbsolute {
            box_id,
            static_position,
            fixed,
            containing_block,
        });
    }
    out.height = (row_offset + grid_length(&rows.tracks, row_gap)).max(Au::ZERO);
    container.set_grid_items(out, child_ids.len());
}

/// Resolves one axis of an item's placement against the line names and
/// named areas (Grid §8.3 and §8.4).
fn resolve_axis(
    start: &GridLine,
    end: &GridLine,
    lines: &[Vec<Atom>],
    areas: &[GridArea],
    rows: bool,
) -> (Option<usize>, usize) {
    let start_pos = area_line(start, areas, rows, false).or_else(|| resolve_line(start, lines));
    let end_pos = area_line(end, areas, rows, true).or_else(|| resolve_line(end, lines));
    let start_span = span_of(start);
    let end_span = span_of(end);
    match (start_pos, end_pos) {
        (Some(start), Some(end)) if end >= start => (Some(start), (end - start).max(1)),
        (Some(start), Some(end)) => (Some(end), (start - end).max(1)),
        (Some(start), None) => (Some(start), if is_span(end) { end_span } else { 1 }),
        (None, Some(end)) => {
            let span = if is_span(start) { start_span } else { 1 };
            (Some(end.saturating_sub(span)), span)
        }
        (None, None) => (None, start_span.max(end_span).max(1)),
    }
}

fn is_span(line: &GridLine) -> bool {
    matches!(line, GridLine::Span { .. })
}

fn span_of(line: &GridLine) -> usize {
    match line {
        GridLine::Span { count, .. } => (*count as usize).max(1),
        _ => 1,
    }
}

/// A line named after a grid area resolves to that area's start or end.
fn area_line(line: &GridLine, areas: &[GridArea], rows: bool, end: bool) -> Option<usize> {
    let GridLine::Line {
        index,
        name: Some(name),
    } = line
    else {
        return None;
    };
    if *index != 1 {
        return None;
    }
    let area = areas.iter().find(|area| &area.name == name)?;
    Some(if rows {
        if end { area.row_end } else { area.row_start }
    } else if end {
        area.col_end
    } else {
        area.col_start
    })
}

/// Resolves a definite line to a 0-based line index. Negative numbers count
/// from the end of the explicit grid (Grid §8.3); names count matches.
fn resolve_line(line: &GridLine, lines: &[Vec<Atom>]) -> Option<usize> {
    let GridLine::Line { index, name } = line else {
        return None;
    };
    if *index == 0 {
        return None;
    }
    if let Some(name) = name {
        let matches: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, names)| names.contains(name))
            .map(|(index, _)| index)
            .collect();
        if matches.is_empty() {
            return None;
        }
        let nth = if *index > 0 {
            (*index as usize).saturating_sub(1)
        } else {
            matches.len().saturating_sub((-*index) as usize)
        };
        return matches.get(nth).copied();
    }
    let count = lines.len() as i32;
    let resolved = if *index > 0 {
        *index - 1
    } else {
        (count + *index).max(0)
    };
    Some(resolved.max(0) as usize)
}

/// Places every item, in the three passes of Grid §8.5, and returns the
/// grid's size in tracks: the explicit count, grown by any implicit track
/// an item needs.
fn place_items(
    items: &mut [Item],
    flow: GridAutoFlow,
    explicit_columns: usize,
    explicit_rows: usize,
) -> (usize, usize) {
    let mut occupied: HashSet<(usize, usize)> = HashSet::new();
    let mut col_count = explicit_columns.max(1);
    let mut row_count = explicit_rows.max(1);

    // Definite in both axes.
    for item in items.iter_mut() {
        if let (Some(col), Some(row)) = (item.col.start, item.row.start) {
            let cell = cell(col, row, item.col.span, item.row.span);
            item.cell = cell;
            item.placed = true;
            mark(&mut occupied, cell);
            col_count = col_count.max(cell.1.0);
            row_count = row_count.max(cell.1.1);
        }
    }
    // Locked to a track in the flow direction.
    for item in items.iter_mut() {
        if item.placed {
            continue;
        }
        if !flow.column && item.row.start.is_some() && item.col.start.is_none() {
            let row = item.row.start.unwrap_or(0);
            let col = first_fit_in_row(&occupied, row, item.col.span, item.row.span, 0);
            item.cell = cell(col, row, item.col.span, item.row.span);
            item.placed = true;
            mark(&mut occupied, item.cell);
            col_count = col_count.max(item.cell.1.0);
            row_count = row_count.max(item.cell.1.1);
        } else if flow.column && item.col.start.is_some() && item.row.start.is_none() {
            let col = item.col.start.unwrap_or(0);
            let row = first_fit_in_column(&occupied, col, item.col.span, item.row.span, 0);
            item.cell = cell(col, row, item.col.span, item.row.span);
            item.placed = true;
            mark(&mut occupied, item.cell);
            col_count = col_count.max(item.cell.1.0);
            row_count = row_count.max(item.cell.1.1);
        }
    }
    // The flow axis has a fixed count: the explicit tracks, grown to the
    // widest span so a wide item can still be placed.
    let widest_col = items.iter().map(|item| item.col.span).max().unwrap_or(1);
    let widest_row = items.iter().map(|item| item.row.span).max().unwrap_or(1);
    let mut col_limit = col_count.max(widest_col).max(1);
    let mut row_limit = row_count.max(widest_row).max(1);

    // Everything else, in the auto placement cursor.
    let (mut cursor_col, mut cursor_row) = (0usize, 0usize);
    for item in items.iter_mut() {
        if item.placed {
            continue;
        }
        let dense = flow.dense;
        if !flow.column {
            if let Some(col) = item.col.start {
                let from = if dense { 0 } else { cursor_row };
                let row = first_fit_in_column(&occupied, col, item.col.span, item.row.span, from);
                item.cell = cell(col, row, item.col.span, item.row.span);
                cursor_col = col;
                cursor_row = row + item.row.span;
            } else {
                let (col, row) = auto_place_row(
                    &occupied,
                    item.col.span,
                    item.row.span,
                    cursor_col,
                    cursor_row,
                    dense,
                    col_limit,
                );
                item.cell = cell(col, row, item.col.span, item.row.span);
                cursor_col = col + item.col.span;
                cursor_row = row;
                if cursor_col >= col_limit {
                    cursor_col = 0;
                    cursor_row += 1;
                }
            }
        } else if let Some(row) = item.row.start {
            let from = if dense { 0 } else { cursor_col };
            let col = first_fit_in_row(&occupied, row, item.col.span, item.row.span, from);
            item.cell = cell(col, row, item.col.span, item.row.span);
            cursor_col = col + item.col.span;
            cursor_row = row;
        } else {
            let (col, row) = auto_place_column(
                &occupied,
                item.col.span,
                item.row.span,
                cursor_col,
                cursor_row,
                dense,
                row_limit,
            );
            item.cell = cell(col, row, item.col.span, item.row.span);
            cursor_col = col;
            cursor_row = row + item.row.span;
            if cursor_row >= row_limit {
                cursor_row = 0;
                cursor_col += 1;
            }
        }
        item.placed = true;
        mark(&mut occupied, item.cell);
        col_count = col_count.max(item.cell.1.0);
        row_count = row_count.max(item.cell.1.1);
        col_limit = col_limit.max(item.cell.1.0);
        row_limit = row_limit.max(item.cell.1.1);
    }
    (col_count, row_count)
}

fn cell(col: usize, row: usize, col_span: usize, row_span: usize) -> Cell {
    ((col, row), (col + col_span.max(1), row + row_span.max(1)))
}

fn mark(occupied: &mut HashSet<(usize, usize)>, cell: Cell) {
    for col in cell.0.0..cell.1.0 {
        for row in cell.0.1..cell.1.1 {
            occupied.insert((col, row));
        }
    }
}

fn fits(
    occupied: &HashSet<(usize, usize)>,
    col: usize,
    row: usize,
    col_span: usize,
    row_span: usize,
) -> bool {
    for c in col..col + col_span.max(1) {
        for r in row..row + row_span.max(1) {
            if occupied.contains(&(c, r)) {
                return false;
            }
        }
    }
    true
}

/// The first column in `row` where the item fits, starting at `from`.
fn first_fit_in_row(
    occupied: &HashSet<(usize, usize)>,
    row: usize,
    col_span: usize,
    row_span: usize,
    from: usize,
) -> usize {
    let mut col = from;
    while !fits(occupied, col, row, col_span, row_span) {
        col += 1;
    }
    col
}

/// The first row in `col` where the item fits, starting at `from`.
fn first_fit_in_column(
    occupied: &HashSet<(usize, usize)>,
    col: usize,
    col_span: usize,
    row_span: usize,
    from: usize,
) -> usize {
    let mut row = from;
    while !fits(occupied, col, row, col_span, row_span) {
        row += 1;
    }
    row
}

/// Row flow auto placement: scan cells row by row within `col_limit`.
fn auto_place_row(
    occupied: &HashSet<(usize, usize)>,
    col_span: usize,
    row_span: usize,
    cursor_col: usize,
    cursor_row: usize,
    dense: bool,
    col_limit: usize,
) -> (usize, usize) {
    let limit = col_limit.max(col_span).max(1);
    let (mut col, mut row) = if dense {
        (0, 0)
    } else {
        (cursor_col, cursor_row)
    };
    loop {
        if col + col_span > limit {
            col = 0;
            row += 1;
            continue;
        }
        if fits(occupied, col, row, col_span, row_span) {
            return (col, row);
        }
        col += 1;
    }
}

/// Column flow auto placement: scan cells column by column within
/// `row_limit`.
fn auto_place_column(
    occupied: &HashSet<(usize, usize)>,
    col_span: usize,
    row_span: usize,
    cursor_col: usize,
    cursor_row: usize,
    dense: bool,
    row_limit: usize,
) -> (usize, usize) {
    let limit = row_limit.max(row_span).max(1);
    let (mut col, mut row) = if dense {
        (0, 0)
    } else {
        (cursor_col, cursor_row)
    };
    loop {
        if row + row_span > limit {
            row = 0;
            col += 1;
            continue;
        }
        if fits(occupied, col, row, col_span, row_span) {
            return (col, row);
        }
        row += 1;
    }
}

/// Adds implicit tracks up to `count`, sized by `grid-auto-columns` /
/// `grid-auto-rows` in order (Grid §7.5; spec 0010, AC-1).
fn grow_tracks(grid: &mut ExplicitGrid, count: usize, implicit: &[TrackSize], _basis: Option<Au>) {
    let explicit = grid.tracks.len();
    while grid.tracks.len() < count {
        let index = grid.tracks.len() - explicit;
        let size = implicit
            .get(index % implicit.len().max(1))
            .cloned()
            .unwrap_or(TrackSize::Breadth(Breadth::Auto));
        grid.tracks.push(Track::new(&size, false));
        grid.lines.push(Vec::new());
    }
}

/// Clamps empty `auto-fit` tracks to zero (Grid §7.2.3.2).
fn collapse_auto_fit(tracks: &mut [Track], items: &[Item], columns: bool) {
    for (index, track) in tracks.iter_mut().enumerate() {
        if !track.auto_fit {
            continue;
        }
        let occupied = items.iter().any(|item| {
            let (start, end) = if columns {
                (item.cell.0.0, item.cell.1.0)
            } else {
                (item.cell.0.1, item.cell.1.1)
            };
            index >= start && index < end
        });
        if !occupied {
            track.collapsed = true;
        }
    }
}

/// Expands a template into tracks and line names, resolving `repeat()`.
fn expand_grid(list: &TrackList, basis: Option<Au>, gap: Au) -> ExplicitGrid {
    let mut grid = ExplicitGrid {
        tracks: Vec::new(),
        lines: vec![Vec::new()],
    };
    expand_entries(&list.entries, &mut grid, basis, gap, false);
    if grid.lines.len() != grid.tracks.len() + 1 {
        grid.lines.resize(grid.tracks.len() + 1, Vec::new());
    }
    grid
}

fn expand_entries(
    entries: &[TrackEntry],
    grid: &mut ExplicitGrid,
    basis: Option<Au>,
    gap: Au,
    auto_fit: bool,
) {
    for entry in entries {
        match entry {
            TrackEntry::LineNames(names) => {
                if let Some(line) = grid.lines.last_mut() {
                    line.extend(names.iter().cloned());
                }
            }
            TrackEntry::Size(size) => {
                grid.tracks.push(Track::new(size, auto_fit));
                grid.lines.push(Vec::new());
            }
            TrackEntry::Repeat { count, tracks } => {
                let times = match count {
                    RepeatCount::Int(count) => *count as usize,
                    RepeatCount::AutoFill | RepeatCount::AutoFit => {
                        auto_repeat_count(tracks, basis, gap)
                    }
                };
                let fit = auto_fit || matches!(count, RepeatCount::AutoFit);
                for _ in 0..times {
                    expand_entries(tracks, grid, basis, gap, fit);
                }
            }
        }
    }
}

/// How many repetitions of `tracks` fit in `basis`, at least one.
fn auto_repeat_count(tracks: &[TrackEntry], basis: Option<Au>, gap: Au) -> usize {
    let Some(basis) = basis else {
        return 1;
    };
    let (size, count) = repeat_estimate(tracks, Some(basis));
    if count == 0 {
        return 1;
    }
    let denominator = size.0 + gap.0 * count as i32;
    if denominator <= 0 {
        return 1;
    }
    let numerator = basis.0 + gap.0;
    (numerator / denominator).max(1) as usize
}

/// The fixed size of a `repeat()` body and how many tracks it holds.
fn repeat_estimate(entries: &[TrackEntry], basis: Option<Au>) -> (Au, usize) {
    let mut size = Au::ZERO;
    let mut count = 0usize;
    for entry in entries {
        match entry {
            TrackEntry::Size(track) => {
                size += match track {
                    TrackSize::Breadth(breadth) => estimate_breadth(breadth, basis),
                    TrackSize::MinMax(min, _) => estimate_breadth(min, basis),
                    TrackSize::FitContent(_) => Au::ZERO,
                };
                count += 1;
            }
            TrackEntry::Repeat {
                count: RepeatCount::Int(times),
                tracks,
            } => {
                let (inner, inner_count) = repeat_estimate(tracks, basis);
                size += inner * *times as i32;
                count += inner_count * *times as usize;
            }
            TrackEntry::Repeat { .. } | TrackEntry::LineNames(_) => {}
        }
    }
    (size, count)
}

fn estimate_breadth(breadth: &Breadth, basis: Option<Au>) -> Au {
    match breadth {
        Breadth::Length(length) => length.resolve(basis),
        _ => Au::ZERO,
    }
}

/// Step 4 for columns: contributions come from item intrinsic widths.
fn size_columns(
    tree: &BoxTree,
    items: &[Item],
    tracks: &mut [Track],
    available: Au,
    gap: Au,
    align: JustifyContent,
    cx: &mut LayoutContext<'_>,
) {
    for track in tracks.iter_mut() {
        track.init(Some(available));
    }
    let contributions: Vec<(usize, usize, Au, Au)> = items
        .iter()
        .map(|item| {
            let (min, max) =
                intrinsic::contribution(tree, item.id, cx).unwrap_or((Au::ZERO, Au::ZERO));
            (item.cell.0.0, item.cell.1.0 - item.cell.0.0, min, max)
        })
        .collect();
    apply_contributions(tracks, &contributions);
    maximize(tracks, available, gap, align);
}

/// Step 4 for rows: contributions are the item heights at their column
/// spans.
#[allow(clippy::too_many_arguments, reason = "the grid axes are explicit")]
fn size_rows(
    tree: &mut BoxTree,
    items: &[Item],
    columns: &[Track],
    rows: &mut [Track],
    available: Option<Au>,
    column_gap: Au,
    row_gap: Au,
    align: JustifyContent,
    space: &ConstraintSpace,
    cx: &mut LayoutContext<'_>,
) {
    for track in rows.iter_mut() {
        track.init(available);
    }
    let mut contributions = Vec::with_capacity(items.len());
    for item in items {
        let width = span_size(columns, item.cell.0.0, item.cell.1.0, column_gap);
        let height = measure_height(tree, item, width, space, cx);
        contributions.push((item.cell.0.1, item.cell.1.1 - item.cell.0.1, height, height));
    }
    apply_contributions(rows, &contributions);
    if let Some(available) = available {
        maximize(rows, available, row_gap, align);
    } else {
        // An `fr` row under indefinite space takes its content size.
        for track in rows.iter_mut() {
            if track.is_flexible()
                && let Some(limit) = track.limit
            {
                track.base = track.base.max(limit);
            }
        }
    }
}

/// Applies intrinsic min and max contributions to the tracks they span.
fn apply_contributions(tracks: &mut [Track], contributions: &[(usize, usize, Au, Au)]) {
    for &(start, span, min, max) in contributions {
        if tracks.is_empty() {
            continue;
        }
        let start = start.min(tracks.len() - 1);
        if span <= 1 {
            let track = &mut tracks[start];
            if track.collapsed {
                continue;
            }
            match &track.min {
                Breadth::Auto | Breadth::MinContent => track.base = track.base.max(min),
                Breadth::MaxContent => track.base = track.base.max(max),
                Breadth::Length(_) | Breadth::Fr(_) => {}
            }
            if track.fit_content
                && let Some(limit) = track.limit
            {
                track.limit = Some(limit.min(max));
            }
            match &track.max {
                Breadth::Auto | Breadth::MaxContent => {
                    track.limit = Some(track.limit.unwrap_or(Au::ZERO).max(max));
                }
                Breadth::MinContent => {
                    track.limit = Some(track.limit.unwrap_or(Au::ZERO).max(min));
                }
                Breadth::Length(_) | Breadth::Fr(_) => {}
            }
            if let Some(limit) = track.limit
                && limit < track.base
            {
                track.limit = Some(track.base);
            }
        } else {
            distribute_base(tracks, start, span, min);
            distribute_limit(tracks, start, span, max);
        }
    }
}

/// Makes the spanned tracks' base sizes sum to at least `target`.
fn distribute_base(tracks: &mut [Track], start: usize, span: usize, target: Au) {
    let end = (start + span).min(tracks.len());
    let mut indices: Vec<usize> = (start..end)
        .filter(|&index| !tracks[index].collapsed && tracks[index].intrinsic_min())
        .collect();
    if indices.is_empty() {
        indices = (start..end)
            .filter(|&index| !tracks[index].collapsed)
            .collect();
    }
    let sum: Au = indices.iter().map(|&index| tracks[index].base).sum();
    let extra = target - sum;
    if extra <= Au::ZERO || indices.is_empty() {
        return;
    }
    let share = extra.0 / indices.len() as i32;
    let mut remainder = extra.0 % indices.len() as i32;
    for index in indices {
        let add = if remainder > 0 {
            remainder -= 1;
            share + 1
        } else {
            share
        };
        tracks[index].base += Au(add);
    }
}

/// Raises the spanned tracks' growth limits toward `target`.
fn distribute_limit(tracks: &mut [Track], start: usize, span: usize, target: Au) {
    let end = (start + span).min(tracks.len());
    let candidates: Vec<usize> = (start..end)
        .filter(|&index| {
            !tracks[index].collapsed
                && tracks[index].limit.is_some()
                && tracks[index].intrinsic_max()
        })
        .collect();
    if candidates.is_empty() {
        return;
    }
    let sum: Au = candidates
        .iter()
        .map(|&index| tracks[index].limit.unwrap_or(Au::ZERO))
        .sum();
    let extra = target - sum;
    if extra <= Au::ZERO {
        return;
    }
    let share = extra.0 / candidates.len() as i32;
    let mut remainder = extra.0 % candidates.len() as i32;
    for index in candidates {
        let add = if remainder > 0 {
            remainder -= 1;
            share + 1
        } else {
            share
        };
        let limit = tracks[index].limit.unwrap_or(Au::ZERO) + Au(add);
        tracks[index].limit = Some(limit);
    }
}

/// Grows base sizes toward their growth limits (Grid §12.6), then flexible
/// tracks into the free space, else stretches `auto` tracks when the content
/// alignment asks for it.
fn maximize(tracks: &mut [Track], available: Au, gap: Au, align: JustifyContent) {
    let used = grid_length(tracks, gap);
    if available <= used {
        return;
    }
    let free = grow_to_limits(tracks, available - used);
    let factors: f32 = tracks
        .iter()
        .filter(|track| !track.collapsed)
        .map(|track| track.flex)
        .sum();
    if factors > 0.0 {
        let non_flex = grid_length_without_flex(tracks, gap);
        let space = (available - non_flex).0;
        let unit = f64::from(space) / f64::from(factors);
        for track in tracks.iter_mut() {
            if track.collapsed || !track.is_flexible() {
                continue;
            }
            let size = Au((unit * f64::from(track.flex)).round() as i32);
            track.base = track.base.max(size);
        }
        return;
    }
    if aligns_stretch(align) {
        let indices: Vec<usize> = tracks
            .iter()
            .enumerate()
            .filter(|(_, track)| !track.collapsed && track.auto_max)
            .map(|(index, _)| index)
            .collect();
        if !indices.is_empty() {
            let share = free.0 / indices.len() as i32;
            let mut remainder = free.0 % indices.len() as i32;
            for index in indices {
                let add = if remainder > 0 {
                    remainder -= 1;
                    share + 1
                } else {
                    share
                };
                tracks[index].base += Au(add);
            }
        }
    }
}

/// Shares `free` equally among the non flexible tracks whose base size is
/// below a finite growth limit, freezing each at its limit (Grid §12.6).
/// Returns the space left over.
fn grow_to_limits(tracks: &mut [Track], mut free: Au) -> Au {
    loop {
        let open: Vec<usize> = tracks
            .iter()
            .enumerate()
            .filter(|(_, track)| {
                !track.collapsed
                    && !track.is_flexible()
                    && track.limit.is_some_and(|limit| limit > track.base)
            })
            .map(|(index, _)| index)
            .collect();
        if open.is_empty() || free <= Au::ZERO {
            return free;
        }
        // At least one unit each, so the loop always makes progress.
        let share = Au((free.0 / open.len() as i32).max(1));
        for index in open {
            if free <= Au::ZERO {
                break;
            }
            let track = &mut tracks[index];
            let room = track.limit.unwrap_or(track.base) - track.base;
            let add = share.min(room).min(free);
            track.base += add;
            free -= add;
        }
    }
}

fn aligns_stretch(align: JustifyContent) -> bool {
    matches!(align, JustifyContent::Normal | JustifyContent::Stretch)
}

/// The total size of the visible tracks plus the gaps between them.
fn grid_length(tracks: &[Track], gap: Au) -> Au {
    let mut total = Au::ZERO;
    let mut seen = false;
    for track in tracks {
        if track.collapsed {
            continue;
        }
        if seen {
            total += gap;
        }
        total += track.base;
        seen = true;
    }
    total
}

/// Like [`grid_length`], but flexible tracks count as zero.
fn grid_length_without_flex(tracks: &[Track], gap: Au) -> Au {
    let mut total = Au::ZERO;
    let mut seen = false;
    for track in tracks {
        if track.collapsed {
            continue;
        }
        if seen {
            total += gap;
        }
        if !track.is_flexible() {
            total += track.base;
        }
        seen = true;
    }
    total
}

fn visible_count(tracks: &[Track]) -> i32 {
    tracks.iter().filter(|track| !track.collapsed).count() as i32
}

/// Where every track starts, including the alignment offset and extra space
/// between tracks (`space-between` and friends).
fn track_starts(tracks: &[Track], gap: Au, extra: Au, offset: Au) -> Vec<Au> {
    let mut starts = Vec::with_capacity(tracks.len());
    let mut at = offset;
    let mut seen = false;
    for track in tracks {
        if track.collapsed {
            starts.push(at);
            continue;
        }
        if seen {
            at += gap + extra;
        }
        starts.push(at);
        at += track.base;
        seen = true;
    }
    starts
}

/// The offset before the first track and extra space between tracks for
/// `free` space over `count` tracks (CSS Box Alignment §5).
fn grid_alignment(align: JustifyContent, free: Au, count: i32) -> (Au, Au) {
    let count = count.max(1);
    match align {
        JustifyContent::FlexStart
        | JustifyContent::Normal
        | JustifyContent::Start
        | JustifyContent::Stretch => (Au::ZERO, Au::ZERO),
        JustifyContent::FlexEnd | JustifyContent::End => (free.max(Au::ZERO), Au::ZERO),
        JustifyContent::Center => (Au(free.0 / 2), Au::ZERO),
        JustifyContent::SpaceBetween if free > Au::ZERO && count > 1 => {
            (Au::ZERO, Au(free.0 / (count - 1)))
        }
        JustifyContent::SpaceAround if free > Au::ZERO => {
            let each = Au(free.0 / count);
            (Au(each.0 / 2), each)
        }
        JustifyContent::SpaceEvenly if free > Au::ZERO => {
            let each = Au(free.0 / (count + 1));
            (each, each)
        }
        JustifyContent::SpaceBetween
        | JustifyContent::SpaceAround
        | JustifyContent::SpaceEvenly => (Au::ZERO, Au::ZERO),
    }
}

/// The rectangle an item's cell covers, from the container's content box.
fn area_rect(
    cell: Cell,
    col_starts: &[Au],
    row_starts: &[Au],
    columns: &[Track],
    rows: &[Track],
    column_gap: Au,
    row_gap: Au,
) -> Rect {
    let x = col_starts.get(cell.0.0).copied().unwrap_or(Au::ZERO);
    let y = row_starts.get(cell.0.1).copied().unwrap_or(Au::ZERO);
    let width = span_size(columns, cell.0.0, cell.1.0, column_gap);
    let height = span_size(rows, cell.0.1, cell.1.1, row_gap);
    Rect::new(Point::new(x, y), Size::new(width, height))
}

/// The size of a span of tracks, gaps included.
fn span_size(tracks: &[Track], start: usize, end: usize, gap: Au) -> Au {
    let mut size = Au::ZERO;
    let mut seen = false;
    for track in tracks.iter().take(end).skip(start) {
        if track.collapsed {
            continue;
        }
        if seen {
            size += gap;
        }
        size += track.base;
        seen = true;
    }
    size
}

/// Lays out one item in `area`, applying its self alignment, and returns its
/// fragment.
fn place_item(
    tree: &mut BoxTree,
    item: &Item,
    area: Rect,
    container: &ComputedStyle,
    space: &ConstraintSpace,
    cx: &mut LayoutContext<'_>,
) -> Option<(Point, Rc<Fragment>)> {
    let style = &item.style;
    let alignment = &style.alignment;
    let justify = alignment
        .justify_self
        .unwrap_or(container.alignment.justify_items);
    let align = alignment
        .align_self
        .unwrap_or(container.alignment.align_items);
    let margin = |length: &Option<Length>| {
        length
            .as_ref()
            .map(|length| length.resolve(Some(space.percentage_basis.width.unwrap_or(Au::ZERO))))
    };
    let margins = [
        margin(&style.margin.top),
        margin(&style.margin.right),
        margin(&style.margin.bottom),
        margin(&style.margin.left),
    ];
    let (margin_x, margin_y) = (
        margins[3].unwrap_or(Au::ZERO) + margins[1].unwrap_or(Au::ZERO),
        margins[0].unwrap_or(Au::ZERO) + margins[2].unwrap_or(Au::ZERO),
    );
    let auto_x = margins[3].is_none() && margins[1].is_none();
    let auto_y = margins[0].is_none() && margins[2].is_none();
    let width_auto = style.size.width.is_none();
    let height_auto = style.size.height.is_none();
    let stretch_x =
        width_auto && !auto_x && matches!(justify, AlignItems::Stretch | AlignItems::Normal);
    let stretch_y =
        height_auto && !auto_y && matches!(align, AlignItems::Stretch | AlignItems::Normal);
    let content_width = (area.size.width - margin_x).max(Au::ZERO);

    // A non-stretched `auto` width shrinks to fit its content, capped by the
    // area. A stretched one fills the area, both through the space.
    let stretch_inline = if stretch_x {
        Some((area.size.width - margin_x).max(Au::ZERO))
    } else if width_auto
        && style.display_box.display != Display::Grid
        && style.display_box.display != Display::InlineGrid
        && tree
            .get(item.id)
            .is_some_and(|b| b.kind != crate::box_tree::BoxKind::Replaced)
    {
        let (_, max) = intrinsic::content_sizes(tree, item.id, cx);
        Some((max + margin_x).min(area.size.width).max(Au::ZERO) - margin_x)
    } else {
        None
    };
    let width_definite = !width_auto || stretch_x;
    let stretch_block = if height_auto && !stretch_y && width_definite {
        // The width is definite (the column) but the height is automatic:
        // derive it from `aspect-ratio` (spec 0010, AC-7).
        apply_aspect_ratio(&style.size, content_width, true, Some(area.size.height))
    } else {
        stretch_y.then_some((area.size.height - margin_y).max(Au::ZERO))
    };
    let item_space = ConstraintSpace {
        available_inline: AvailableSize::Definite(content_width.max(Au::ZERO)),
        available_block: AvailableSize::MaxContent,
        percentage_basis: Size::new(Some(area.size.width), Some(area.size.height)),
        stretch_inline,
        stretch_block,
        ..space.clone()
    };
    let fragment = lay_out_box(tree, item.id, item_space, cx)?;
    let size = fragment.size();
    let free_x = area.size.width - size.width - margin_x;
    let free_y = area.size.height - size.height - margin_y;
    let x = area.origin.x + item_offset(justify, free_x, margins[3], margins[1]);
    let y = area.origin.y + item_offset(align, free_y, margins[0], margins[2]);
    Some((Point::new(x, y), fragment))
}

/// Where an item's border box starts inside its area on one axis.
fn item_offset(
    align: AlignItems,
    free: Au,
    start_margin: Option<Au>,
    end_margin: Option<Au>,
) -> Au {
    if start_margin.is_none() || end_margin.is_none() {
        if free <= Au::ZERO {
            return start_margin.unwrap_or(Au::ZERO);
        }
        return match (start_margin, end_margin) {
            (None, None) => Au(free.0 / 2),
            (None, Some(_)) => free,
            _ => Au::ZERO,
        } + start_margin.unwrap_or(Au::ZERO);
    }
    start_margin.unwrap_or(Au::ZERO)
        + match align {
            AlignItems::Stretch
            | AlignItems::Normal
            | AlignItems::Start
            | AlignItems::FlexStart
            | AlignItems::Baseline => Au::ZERO,
            AlignItems::End | AlignItems::FlexEnd => free,
            AlignItems::Center => Au(free.0 / 2),
        }
}

/// Derives the dependent axis size from `aspect-ratio` when the other axis is
/// definite (spec 0010, AC-7). `known` is the definite content-box size on the
/// known axis and `width_known` says whether `known` is the width. Returns the
/// derived content-box size on the other axis, clamped by that axis's min/max.
fn apply_aspect_ratio(
    style: &Sizes,
    known: Au,
    width_known: bool,
    basis: Option<Au>,
) -> Option<Au> {
    let (rw, rh) = style
        .aspect_ratio
        .ratio
        .filter(|&(w, h)| w > 0.0 && h > 0.0)?;
    let scale = rw / rh;
    let mut derived = if width_known {
        // Height is automatic: height = width / ratio.
        Au::from_px(known.to_f32_px() / scale)
    } else {
        // Width is automatic: width = height * ratio.
        Au::from_px(known.to_f32_px() * scale)
    };
    if width_known {
        let min = resolve(style.min_height.clone(), basis).unwrap_or(Au::ZERO);
        let max = resolve(style.max_height.clone(), basis);
        derived = derived.clamp(min, max.unwrap_or(Au::MAX));
    } else {
        let min = resolve(style.min_width.clone(), basis).unwrap_or(Au::ZERO);
        let max = resolve(style.max_width.clone(), basis);
        derived = derived.clamp(min, max.unwrap_or(Au::MAX));
    }
    Some(derived.max(Au::ZERO))
}

/// Lays out an item at its column span to find its row contribution: its
/// border box height plus its vertical margins.
fn measure_height(
    tree: &mut BoxTree,
    item: &Item,
    width: Au,
    space: &ConstraintSpace,
    cx: &mut LayoutContext<'_>,
) -> Au {
    let style = &item.style;
    let margin = |length: &Option<Length>| {
        length
            .as_ref()
            .map(|length| length.resolve(Some(space.percentage_basis.width.unwrap_or(Au::ZERO))))
            .unwrap_or(Au::ZERO)
    };
    let margin_x = margin(&style.margin.left) + margin(&style.margin.right);
    let margin_y = margin(&style.margin.top) + margin(&style.margin.bottom);
    // The width is definite (the column). If the height is automatic and an
    // `aspect-ratio` is set, derive the height from the ratio (spec 0010,
    // AC-7), so row tracks grow to fit instead of collapsing to zero.
    let height_auto = style.size.height.is_none();
    let stretch_block = if height_auto {
        apply_aspect_ratio(&style.size, width, true, None)
    } else {
        None
    };
    let item_space = ConstraintSpace {
        available_inline: AvailableSize::Definite(width),
        available_block: AvailableSize::MaxContent,
        percentage_basis: Size::new(Some(width), None),
        stretch_inline: Some((width - margin_x).max(Au::ZERO)),
        stretch_block,
        ..space.clone()
    };
    lay_out_box(tree, item.id, item_space, cx)
        .map(|fragment| fragment.size().height)
        .unwrap_or(Au::ZERO)
        + margin_y
}

/// The container's min and max content widths, for its own sizing as a flex
/// or grid item, an inline grid, or a shrink to fit box (AC-3).
pub(crate) fn intrinsic_sizes(
    tree: &BoxTree,
    id: BoxId,
    layout_box: &LayoutBox,
    cx: &mut LayoutContext<'_>,
) -> (Au, Au) {
    let style = &layout_box.style;
    let gap = style.alignment.column_gap.resolve(None);
    let grid = expand_grid(&style.grid.template_columns, None, gap);
    let mut tracks = grid.tracks;
    if tracks.is_empty() {
        tracks.push(Track::new(&TrackSize::Breadth(Breadth::Auto), false));
    }
    for track in tracks.iter_mut() {
        track.init(None);
    }
    let count = tracks.len();
    let children: Vec<_> = layout_box
        .children
        .iter()
        .copied()
        .filter(|&child| {
            tree.get(child)
                .is_some_and(|b| !b.is_absolute() && !b.is_fixed())
        })
        .collect();
    let mut mins = vec![Au::ZERO; count];
    let mut maxes = vec![Au::ZERO; count];
    for (index, child) in children.into_iter().enumerate() {
        let Some((min, max)) = intrinsic::contribution(tree, child, cx) else {
            continue;
        };
        let slot = index % count;
        mins[slot] = mins[slot].max(min);
        maxes[slot] = maxes[slot].max(max);
    }
    let mut min_width = Au::ZERO;
    let mut max_width = Au::ZERO;
    for (index, track) in tracks.iter().enumerate() {
        if track.collapsed {
            continue;
        }
        if index > 0 {
            min_width += gap;
            max_width += gap;
        }
        min_width += match &track.min {
            Breadth::Length(length) => length.resolve(None),
            _ => mins[index],
        };
        max_width += match &track.max {
            Breadth::Length(length) => length.resolve(None),
            _ => maxes[index],
        };
    }
    let _ = id;
    (min_width, max_width)
}

#[cfg(test)]
mod tests {
    use base::atoms::Atom;
    use css::{Breadth, GridLine, RepeatCount, TrackEntry, TrackList, TrackSize};

    use super::*;

    fn px(px: f32) -> Au {
        Au::from_px(px)
    }

    fn size(px: f32) -> TrackSize {
        TrackSize::Breadth(Breadth::Length(Length::px(px)))
    }

    #[test]
    fn fixed_tracks_stagger_with_gaps() {
        let mut grid = expand_grid(
            &TrackList {
                entries: vec![TrackEntry::Size(size(50.0)), TrackEntry::Size(size(70.0))],
            },
            Some(px(200.0)),
            px(10.0),
        );
        for track in grid.tracks.iter_mut() {
            track.init(Some(px(200.0)));
        }
        assert_eq!(grid.tracks.len(), 2);
        assert_eq!(grid.tracks[0].base, px(50.0));
        assert_eq!(grid.tracks[1].base, px(70.0));
        assert_eq!(grid_length(&grid.tracks, px(10.0)), px(130.0));
    }

    #[test]
    fn fr_tracks_share_free_space() {
        let mut grid = expand_grid(
            &TrackList {
                entries: vec![
                    TrackEntry::Size(size(100.0)),
                    TrackEntry::Size(TrackSize::Breadth(Breadth::Fr(1.0))),
                    TrackEntry::Size(TrackSize::Breadth(Breadth::Fr(1.0))),
                ],
            },
            Some(px(300.0)),
            Au::ZERO,
        );
        for track in grid.tracks.iter_mut() {
            track.init(Some(px(300.0)));
        }
        maximize(
            &mut grid.tracks,
            px(300.0),
            Au::ZERO,
            JustifyContent::Normal,
        );
        assert_eq!(grid.tracks[0].base, px(100.0));
        assert_eq!(grid.tracks[1].base, px(100.0));
        assert_eq!(grid.tracks[2].base, px(100.0));
    }

    #[test]
    fn repeat_expands_into_tracks() {
        let grid = expand_grid(
            &TrackList {
                entries: vec![TrackEntry::Repeat {
                    count: RepeatCount::Int(3),
                    tracks: vec![TrackEntry::Size(size(40.0))],
                }],
            },
            Some(px(200.0)),
            Au::ZERO,
        );
        assert_eq!(grid.tracks.len(), 3);
    }

    #[test]
    fn auto_fill_fits_as_many_as_the_container_allows() {
        let grid = expand_grid(
            &TrackList {
                entries: vec![TrackEntry::Repeat {
                    count: RepeatCount::AutoFill,
                    tracks: vec![TrackEntry::Size(TrackSize::MinMax(
                        Breadth::Length(Length::px(40.0)),
                        Breadth::Fr(1.0),
                    ))],
                }],
            },
            Some(px(200.0)),
            px(10.0),
        );
        // (200 + 10) / (40 + 10) = 4.
        assert_eq!(grid.tracks.len(), 4);
    }

    #[test]
    fn auto_fit_tracks_collapse_when_empty() {
        let mut grid = expand_grid(
            &TrackList {
                entries: vec![TrackEntry::Repeat {
                    count: RepeatCount::AutoFit,
                    tracks: vec![TrackEntry::Size(size(40.0))],
                }],
            },
            Some(px(200.0)),
            Au::ZERO,
        );
        let mut items = Vec::new();
        collapse_auto_fit(&mut grid.tracks, &items, true);
        assert!(grid.tracks.iter().all(|track| track.collapsed));
        items.clear();
    }

    #[test]
    fn resolve_line_counts_negative_numbers_from_the_end() {
        let lines = vec![vec![], vec![], vec![], vec![], vec![]];
        assert_eq!(
            resolve_line(
                &GridLine::Line {
                    index: 1,
                    name: None
                },
                &lines
            ),
            Some(0)
        );
        assert_eq!(
            resolve_line(
                &GridLine::Line {
                    index: -1,
                    name: None
                },
                &lines
            ),
            Some(4)
        );
        assert_eq!(
            resolve_line(
                &GridLine::Line {
                    index: -2,
                    name: None
                },
                &lines
            ),
            Some(3)
        );
    }

    #[test]
    fn named_lines_resolve_by_occurrence() {
        let lines = vec![
            vec![],
            vec![Atom::from("a")],
            vec![],
            vec![Atom::from("a")],
            vec![],
        ];
        let line = |index| GridLine::Line {
            index,
            name: Some(Atom::from("a")),
        };
        assert_eq!(resolve_line(&line(1), &lines), Some(1));
        assert_eq!(resolve_line(&line(2), &lines), Some(3));
        assert_eq!(resolve_line(&line(-1), &lines), Some(3));
    }

    #[test]
    fn span_resolves_before_or_after_a_definite_line() {
        let lines = vec![vec![], vec![], vec![], vec![]];
        let (start, span) = resolve_axis(
            &GridLine::Line {
                index: 2,
                name: None,
            },
            &GridLine::Span {
                count: 2,
                name: None,
            },
            &lines,
            &[],
            false,
        );
        assert_eq!((start, span), (Some(1), 2));

        let (start, span) = resolve_axis(
            &GridLine::Span {
                count: 2,
                name: None,
            },
            &GridLine::Line {
                index: 4,
                name: None,
            },
            &lines,
            &[],
            false,
        );
        assert_eq!((start, span), (Some(1), 2));
    }

    #[test]
    fn named_areas_give_both_edges() {
        let areas = vec![GridArea {
            name: Atom::from("main"),
            row_start: 1,
            row_end: 3,
            col_start: 0,
            col_end: 2,
        }];
        let line = GridLine::Line {
            index: 1,
            name: Some(Atom::from("main")),
        };
        assert_eq!(area_line(&line, &areas, false, false), Some(0));
        assert_eq!(area_line(&line, &areas, false, true), Some(2));
        assert_eq!(area_line(&line, &areas, true, false), Some(1));
        assert_eq!(area_line(&line, &areas, true, true), Some(3));
    }

    #[test]
    fn grid_alignment_falls_back_from_spacing_keywords() {
        assert_eq!(
            grid_alignment(JustifyContent::SpaceBetween, px(-10.0), 3),
            (Au::ZERO, Au::ZERO)
        );
        assert_eq!(
            grid_alignment(JustifyContent::SpaceEvenly, px(30.0), 2),
            (px(10.0), px(10.0))
        );
    }
}
