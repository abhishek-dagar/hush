//! Grid layout (spec 0010, AC-1 to AC-3): CSS Grid Level 1 for the thin
//! thread. Fixed and `fr` tracks, `repeat(auto-fill, minmax())`, and auto
//! placement in row flow. Every item starts its own formatting context, so
//! no margin collapses with the container or a sibling. Absolute children
//! are handed on to their containing block.

use std::collections::HashSet;
use std::rc::Rc;

use base::{Au, BoxId, Point};
use css::{ComputedStyle, Grid, GridAutoFlow, GridLine, TrackSize};

use crate::block::{GridItems, InProgress};
use crate::box_tree::{BoxTree, LayoutBox};
use crate::fragment::{Fragment, PendingAbsolute};
use crate::{AvailableSize, ConstraintSpace, LayoutContext, lay_out_box};

/// One grid item partway through the algorithm.
struct Item {
    id: BoxId,
    style: Rc<ComputedStyle>,
    /// The cell range the item occupies: ((col_start, row_start), (col_end, row_end)),
    /// with ends exclusive.
    cell: ((usize, usize), (usize, usize)),
    /// The item's border box width and height once placed.
    size: (Au, Au),
    /// The item's laid out fragment.
    fragment: Option<Rc<Fragment>>,
}

/// One resolved track: its start offset, size, and whether it is an `fr`
/// track that can grow to absorb free space.
#[derive(Clone, Copy)]
struct Track {
    start: Au,
    size: Au,
    is_fr: bool,
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

    // Absolute children are not items; their static position is the
    // content box's start corner (spec 0006, Layout rules worth pinning).
    let (absolute_ids, item_ids): (Vec<BoxId>, Vec<BoxId>) = child_ids
        .iter()
        .partition(|&&id| tree.get(id).is_some_and(LayoutBox::is_absolute));

    // Resolve the column and row tracks against the container's content
    // box size. Fixed and percentage tracks resolve now; `fr` tracks are
    // sized afterward from whatever free space remains, minus the total gap
    // so gaps fit inside the content box.
    let gap_column = style
        .alignment
        .column_gap
        .resolve(Some(frame.content_width));
    let gap_row = style.alignment.row_gap.resolve(frame.height);
    let mut columns = resolve_tracks(
        &grid.template_columns,
        Some(frame.content_width),
        gap_column,
    );
    let mut rows = resolve_tracks(&grid.template_rows, frame.height, gap_row);
    let mut items: Vec<Item> = Vec::new();
    let mut next_column = 0usize;
    let mut next_row = 0usize;
    // Cells already taken by explicitly placed items, for dense packing.
    let mut occupied: HashSet<(usize, usize)> = HashSet::new();
    for id in &item_ids {
        let Some(item) = new_item(tree, *id) else {
            continue;
        };
        // Placement: explicit grid lines win, else auto place.
        let cell = if let Some(placement) =
            compute_placement(&item.style.grid, columns.len(), rows.len())
        {
            placement
        } else {
            place_auto(
                &mut next_column,
                &mut next_row,
                &mut columns,
                &mut rows,
                grid.auto_flow,
                &mut occupied,
            )
        };
        // Grow the track lists if placement reached beyond them.
        grow_tracks(&mut columns, &mut rows, cell.1.0 - 1, cell.1.1 - 1);
        items.push(Item {
            id: *id,
            style: Rc::clone(&item.style),
            cell,
            size: (Au::ZERO, Au::ZERO),
            fragment: None,
        });
        // Mark every cell the item occupies, so dense packing skips them.
        for col in cell.0.0..cell.1.0 {
            for row in cell.0.1..cell.1.1 {
                occupied.insert((col, row));
            }
        }
    }

    // Measure every item once to know its content size, then lay it out in
    // its cell range.
    let mut out = GridItems::default();
    for item in &mut items {
        let cell_width: Au = columns[item.cell.0.0..item.cell.1.0]
            .iter()
            .map(|t| t.size)
            .sum();
        let cell_height: Au = rows[item.cell.0.1..item.cell.1.1]
            .iter()
            .map(|t| t.size)
            .sum();
        item.fragment = measure(
            tree,
            item.id,
            &space,
            Some(cell_width),
            Some(cell_height),
            cx,
        );
        item.size = (
            item.fragment.as_ref().map_or(Au::ZERO, |f| f.size().width),
            item.fragment.as_ref().map_or(Au::ZERO, |f| f.size().height),
        );
    }
    // Place items using their cell offsets.
    for item in &items {
        let col_start = item.cell.0.0;
        let row_start = item.cell.0.1;
        let at = Point::new(
            columns[col_start].start + gap_column * (col_start as i32),
            rows[row_start].start + gap_row * (row_start as i32),
        );
        let Some(fragment) = item.fragment.clone() else {
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
    out.absolutes
        .extend(absolute_ids.into_iter().map(|box_id| PendingAbsolute {
            box_id,
            static_position: Point::default(),
            fixed: false,
        }));
    let height: Au =
        rows.iter().map(|t| t.size).sum::<Au>() + gap_row * (rows.len().saturating_sub(1) as i32);
    out.height = height.max(Au::ZERO);
    container.set_grid_items(out, child_ids.len());
}

/// Resolve a track list against the container's size along one axis.
/// Returns tracks with definite sizes; `fr` tracks start at zero and are
/// grown later by [`distribute_free_space`]. `repeat()` is expanded inline.
fn resolve_tracks(template: &[TrackSize], basis: Option<Au>, gap: Au) -> Vec<Track> {
    let mut expanded: Vec<TrackSize> = Vec::new();
    for track in template {
        expand_repeat(track, &mut expanded);
    }
    let mut tracks: Vec<Track> = Vec::new();
    for track in &expanded {
        match track {
            TrackSize::Length(length) => {
                let size = length.resolve(basis);
                tracks.push(Track {
                    start: Au::ZERO,
                    size,
                    is_fr: false,
                });
            }
            TrackSize::Fr(_) => {
                tracks.push(Track {
                    start: Au::ZERO,
                    size: Au::ZERO,
                    is_fr: true,
                });
            }
            TrackSize::Minmax { min, .. } => {
                let min_size = match min.as_ref() {
                    TrackSize::Length(length) => length.resolve(basis),
                    TrackSize::FitContent(length) => length.resolve(basis),
                    _ => Au::ZERO,
                };
                tracks.push(Track {
                    start: Au::ZERO,
                    size: min_size,
                    is_fr: false,
                });
            }
            TrackSize::MinContent => {
                // Intrinsic minimum content size requires measuring content,
                // which is complex; resolve to zero for now (tracks grow
                // implicitly as items are added).
                tracks.push(Track {
                    start: Au::ZERO,
                    size: Au::ZERO,
                    is_fr: false,
                });
            }
            TrackSize::MaxContent => {
                // Intrinsic maximum content size requires measuring content;
                // resolve to zero for now.
                tracks.push(Track {
                    start: Au::ZERO,
                    size: Au::ZERO,
                    is_fr: false,
                });
            }
            TrackSize::FitContent(length) => {
                let size = length.resolve(basis);
                tracks.push(Track {
                    start: Au::ZERO,
                    size,
                    is_fr: false,
                });
            }
            TrackSize::Repeat { .. } => {
                // Expanded above; never reached here.
            }
        }
    }
    // Stagger the starts.
    let mut offset = Au::ZERO;
    for track in &mut tracks {
        track.start = offset;
        offset += track.size;
    }
    // Distribute leftover free space among `fr` tracks.
    distribute_free_space(&mut tracks, basis.unwrap_or(Au::ZERO), gap);
    tracks
}

/// Distribute leftover free space among `fr` tracks in proportion to their
/// flex factors (all `1fr` in this slice). Non-`fr` tracks keep their
/// definite size; `fr` tracks are at least their resolved minimum.
fn distribute_free_space(tracks: &mut [Track], container_size: Au, gap: Au) {
    let fr_count = tracks.iter().filter(|t| t.is_fr).count();
    if fr_count == 0 {
        return;
    }
    let gap_total = gap * (tracks.len().saturating_sub(1) as i32);
    let used: Au = tracks.iter().map(|t| t.size).sum();
    let free = container_size - used - gap_total;
    if free <= Au::ZERO {
        return;
    }
    let share = Au(free.0 / fr_count as i32);
    let mut remainder = free.0 % fr_count as i32;
    for track in tracks.iter_mut() {
        if track.is_fr {
            let extra = if remainder > 0 {
                remainder -= 1;
                share + Au(1)
            } else {
                share
            };
            track.size += extra;
        }
    }
    // Restagger the starts after sizes changed.
    let mut offset = Au::ZERO;
    for track in tracks.iter_mut() {
        track.start = offset;
        offset += track.size;
    }
}

/// Expand a `repeat()` entry into individual track sizes, appending to
/// `out`. Non-`repeat` entries are appended unchanged.
fn expand_repeat(track: &TrackSize, out: &mut Vec<TrackSize>) {
    match track {
        TrackSize::Repeat {
            count,
            track: inner,
        } => {
            let times = match count {
                Some(n) => *n,
                None => {
                    // `auto-fill`/`auto-fit`: fit as many as the container
                    // allows. In this slice we expand a modest fixed number;
                    // the container grows implicit tracks as items are added.
                    12
                }
            };
            for _ in 0..times {
                out.push(*inner.clone());
            }
        }
        _ => out.push(track.clone()),
    }
}

/// Grow the column and row track lists so `cell` fits.
fn grow_tracks(columns: &mut Vec<Track>, rows: &mut Vec<Track>, column: usize, row: usize) {
    while columns.len() <= column {
        let start = columns.last().map_or(Au::ZERO, |t| t.start + t.size);
        columns.push(Track {
            start,
            size: Au::ZERO,
            is_fr: false,
        });
    }
    while rows.len() <= row {
        let start = rows.last().map_or(Au::ZERO, |t| t.start + t.size);
        rows.push(Track {
            start,
            size: Au::ZERO,
            is_fr: false,
        });
    }
}

/// Resolve one side of grid item placement to a 0-based track index.
/// `Line(n)` is 1-indexed from the start; `FromEnd(n)` counts from the end;
/// `Span` and `Auto` are not positions and return `None`.
fn resolve_line(line: Option<&GridLine>, track_count: usize) -> Option<usize> {
    match line {
        Some(GridLine::Line(n)) => {
            let idx = n.saturating_sub(1);
            Some(idx.min(track_count.saturating_sub(1)))
        }
        Some(GridLine::FromEnd(n)) => Some(track_count.saturating_sub(*n)),
        Some(GridLine::Span(_)) | None | Some(GridLine::Auto) => None,
    }
}

/// Compute the cell range for explicitly placed grid lines or named areas.
/// Returns `((col_start, row_start), (col_end, row_end))` with ends exclusive,
/// or `None` when the item should be auto-placed.
fn compute_placement(
    grid: &Grid,
    column_count: usize,
    row_count: usize,
) -> Option<((usize, usize), (usize, usize))> {
    let col_start = resolve_line(grid.column_start.as_ref(), column_count)?;
    let row_start = resolve_line(grid.row_start.as_ref(), row_count)?;
    let col_end = match &grid.column_end {
        Some(GridLine::Line(n)) => (*n).saturating_sub(1).min(column_count),
        Some(GridLine::FromEnd(n)) => column_count.saturating_sub(*n),
        Some(GridLine::Span(n)) => col_start + n,
        _ => col_start + 1,
    };
    let row_end = match &grid.row_end {
        Some(GridLine::Line(n)) => (*n).saturating_sub(1).min(row_count),
        Some(GridLine::FromEnd(n)) => row_count.saturating_sub(*n),
        Some(GridLine::Span(n)) => row_start + n,
        _ => row_start + 1,
    };
    // Named area lookup: if explicit lines weren't set but areas exist,
    // resolve by the item's grid-area name. For a thin thread, area lookup
    // is a no-op without grid-area on the item.
    let _ = &grid.grid_template_areas;
    Some(((col_start, row_start), (col_end, row_end)))
}

/// Place one item automatically, advancing the cursor and growing implicit
/// tracks as needed. Returns `((col_start, row_start), (col_end, row_end))`.
/// In row flow, items go left to right and wrap to the next row.
/// In column flow, items go top to bottom and wrap to the next column.
/// With dense packing, the first empty cell is used; otherwise items keep
/// their order and may leave gaps.
fn place_auto(
    next_column: &mut usize,
    next_row: &mut usize,
    columns: &mut Vec<Track>,
    rows: &mut Vec<Track>,
    flow: GridAutoFlow,
    occupied: &mut HashSet<(usize, usize)>,
) -> ((usize, usize), (usize, usize)) {
    let mut col = *next_column;
    let mut row = *next_row;

    // Grow implicit tracks if the cursor ran past them.
    while row >= rows.len() {
        let start = rows.last().map_or(Au::ZERO, |t| t.start + t.size);
        rows.push(Track {
            start,
            size: Au::ZERO,
            is_fr: false,
        });
    }
    while col >= columns.len() {
        let start = columns.last().map_or(Au::ZERO, |t| t.start + t.size);
        columns.push(Track {
            start,
            size: Au::ZERO,
            is_fr: false,
        });
    }

    if flow == GridAutoFlow::Dense {
        // Find the first empty cell in row-major order.
        'outer: for r in 0..rows.len() {
            for c in 0..columns.len() {
                if !occupied.contains(&(c, r)) {
                    col = c;
                    row = r;
                    break 'outer;
                }
            }
        }
        // If nothing fit, grow a new row at the end.
        if col >= columns.len() {
            col = 0;
            row = rows.len();
            rows.push(Track {
                start: rows.last().map_or(Au::ZERO, |t| t.start + t.size),
                size: Au::ZERO,
                is_fr: false,
            });
        }
        let cell = ((col, row), (col + 1, row + 1));
        *next_column = col + 1;
        *next_row = row;
        cell
    } else {
        // Sparse packing: keep order, wrap at the end of the row/column.
        match flow {
            GridAutoFlow::Column => {
                *next_row += 1;
                if *next_row >= rows.len() {
                    *next_row = 0;
                    *next_column += 1;
                }
                ((col, row), (col + 1, row + 1))
            }
            GridAutoFlow::Row => {
                *next_column += 1;
                if *next_column >= columns.len() {
                    *next_column = 0;
                    *next_row += 1;
                }
                ((col, row), (col + 1, row + 1))
            }
            GridAutoFlow::Dense => unreachable!("dense handled above"),
        }
    }
}

/// An item's style and grid placement, or `None` for a box not in the tree.
fn new_item(tree: &BoxTree, id: BoxId) -> Option<Item> {
    let layout_box = tree.get(id)?;
    let style = Rc::clone(&layout_box.style);
    Some(Item {
        id,
        style,
        cell: ((0, 0), (1, 1)),
        size: (Au::ZERO, Au::ZERO),
        fragment: None,
    })
}

/// Lays out item `id` in its cell's space, with the cell's border box sizes
/// fixed.
fn measure(
    tree: &mut BoxTree,
    id: BoxId,
    space: &ConstraintSpace,
    width: Option<Au>,
    height: Option<Au>,
    cx: &mut LayoutContext<'_>,
) -> Option<Rc<Fragment>> {
    let space = ConstraintSpace {
        available_inline: width.map_or(space.available_inline, AvailableSize::Definite),
        stretch_inline: width,
        stretch_block: height,
        ..space.clone()
    };
    lay_out_box(tree, id, space, cx)
}

#[cfg(test)]
mod tests {
    use base::{Au, atoms::QualName, local_name};
    use css::Length;
    use dom::{Document, NodeId};
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn resolve_tracks_staggers_fixed_tracks() {
        let tracks = resolve_tracks(
            &[TrackSize::Length(Length::px(50.0))],
            Some(Au::from_px(200.0)),
            Au::ZERO,
        );
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].start, Au::ZERO);
        assert_eq!(tracks[0].size, Au::from_px(50.0));
        assert!(!tracks[0].is_fr);
    }

    #[test]
    fn fr_tracks_share_free_space_equally() {
        let tracks = resolve_tracks(
            &[
                TrackSize::Length(Length::px(100.0)),
                TrackSize::Fr(1.0),
                TrackSize::Fr(1.0),
            ],
            Some(Au::from_px(300.0)),
            Au::ZERO,
        );
        assert_eq!(tracks.len(), 3);
        assert_eq!(tracks[0].size, Au::from_px(100.0));
        assert!(!tracks[0].is_fr);
        assert!(tracks[1].is_fr && tracks[2].is_fr);
        // 300 - 100 = 200 free, split as 100 + 100.
        assert_eq!(tracks[1].size, Au::from_px(100.0));
        assert_eq!(tracks[2].size, Au::from_px(100.0));
        // Starts stagger.
        assert_eq!(tracks[0].start, Au::ZERO);
        assert_eq!(tracks[1].start, Au::from_px(100.0));
        assert_eq!(tracks[2].start, Au::from_px(200.0));
    }

    #[test]
    fn repeat_expands_into_individual_tracks() {
        let tracks = resolve_tracks(
            &[TrackSize::Repeat {
                count: Some(3),
                track: Box::new(TrackSize::Length(Length::px(40.0))),
            }],
            Some(Au::from_px(200.0)),
            Au::ZERO,
        );
        assert_eq!(tracks.len(), 3);
        assert_eq!(tracks[0].size, Au::from_px(40.0));
        assert_eq!(tracks[1].start, Au::from_px(40.0));
        assert_eq!(tracks[2].start, Au::from_px(80.0));
    }

    #[test]
    fn distribute_free_space_leaves_non_fr_untouched() {
        let mut tracks = vec![
            Track {
                start: Au::ZERO,
                size: Au::from_px(100.0),
                is_fr: false,
            },
            Track {
                start: Au::from_px(100.0),
                size: Au::ZERO,
                is_fr: true,
            },
            Track {
                start: Au::from_px(200.0),
                size: Au::ZERO,
                is_fr: true,
            },
        ];
        distribute_free_space(&mut tracks, Au::from_px(300.0), Au::ZERO);
        assert_eq!(tracks[0].size, Au::from_px(100.0));
        assert_eq!(tracks[1].size, Au::from_px(100.0));
        assert_eq!(tracks[2].size, Au::from_px(100.0));
    }

    #[test]
    fn distribute_free_space_shrinks_fr_tracks_for_gap() {
        let mut tracks = vec![
            Track {
                start: Au::ZERO,
                size: Au::from_px(100.0),
                is_fr: false,
            },
            Track {
                start: Au::from_px(100.0),
                size: Au::ZERO,
                is_fr: true,
            },
            Track {
                start: Au::from_px(200.0),
                size: Au::ZERO,
                is_fr: true,
            },
        ];
        distribute_free_space(&mut tracks, Au::from_px(300.0), Au::from_px(10.0));
        // 300 - 100 - 10 * 2 = 180 free for 2 fr tracks, 90 each.
        assert_eq!(tracks[1].size, Au::from_px(90.0));
        assert_eq!(tracks[2].size, Au::from_px(90.0));
    }

    #[test]
    fn resolve_tracks_shrinks_fr_tracks_for_gap() {
        let tracks = resolve_tracks(
            &[
                TrackSize::Length(Length::px(100.0)),
                TrackSize::Fr(1.0),
                TrackSize::Fr(1.0),
            ],
            Some(Au::from_px(300.0)),
            Au::from_px(10.0),
        );
        assert_eq!(tracks.len(), 3);
        assert_eq!(tracks[0].size, Au::from_px(100.0));
        // 300 - 100 - 10 * 2 = 180 free, split 90 + 90.
        assert_eq!(tracks[1].size, Au::from_px(90.0));
        assert_eq!(tracks[2].size, Au::from_px(90.0));
    }

    #[test]
    fn resolve_line_maps_positive_number_from_start() {
        let line = GridLine::Line(3);
        assert_eq!(resolve_line(Some(&line), 5), Some(2));
    }

    #[test]
    fn resolve_line_maps_negative_number_from_end() {
        let line = GridLine::FromEnd(1);
        assert_eq!(resolve_line(Some(&line), 5), Some(4));
        let line = GridLine::FromEnd(2);
        assert_eq!(resolve_line(Some(&line), 5), Some(3));
    }

    #[test]
    fn compute_placement_uses_explicit_lines() {
        let grid = Grid {
            template_columns: vec![],
            template_rows: vec![],
            auto_columns: vec![],
            auto_rows: vec![],
            auto_flow: GridAutoFlow::Row,
            column_start: Some(GridLine::Line(2)),
            column_end: Some(GridLine::Line(4)),
            row_start: Some(GridLine::Line(1)),
            row_end: Some(GridLine::Span(2)),
            grid_template_areas: vec![],
        };
        let placement = compute_placement(&grid, 5, 5).unwrap();
        assert_eq!(placement, ((1, 0), (3, 2)));
    }

    #[test]
    fn compute_placement_returns_none_for_auto() {
        let grid = Grid {
            template_columns: vec![],
            template_rows: vec![],
            auto_columns: vec![],
            auto_rows: vec![],
            auto_flow: GridAutoFlow::Row,
            column_start: None,
            column_end: None,
            row_start: None,
            row_end: None,
            grid_template_areas: vec![],
        };
        assert!(compute_placement(&grid, 5, 5).is_none());
    }

    #[test]
    fn place_auto_column_flow_wraps_to_next_column() {
        let mut next_col = 0usize;
        let mut next_row = 0usize;
        let mut columns = vec![
            Track {
                start: Au::ZERO,
                size: Au::from_px(50.0),
                is_fr: false,
            };
            3
        ];
        let mut rows = vec![
            Track {
                start: Au::ZERO,
                size: Au::from_px(50.0),
                is_fr: false,
            };
            2
        ];
        let mut occupied = HashSet::new();
        let cell = place_auto(
            &mut next_col,
            &mut next_row,
            &mut columns,
            &mut rows,
            GridAutoFlow::Column,
            &mut occupied,
        );
        assert_eq!(cell, ((0, 0), (1, 1)));
        assert_eq!(next_row, 1);
        let cell = place_auto(
            &mut next_col,
            &mut next_row,
            &mut columns,
            &mut rows,
            GridAutoFlow::Column,
            &mut occupied,
        );
        assert_eq!(cell, ((0, 1), (1, 2)));
        // Next should wrap to column 1.
        let cell = place_auto(
            &mut next_col,
            &mut next_row,
            &mut columns,
            &mut rows,
            GridAutoFlow::Column,
            &mut occupied,
        );
        assert_eq!(cell, ((1, 0), (2, 1)));
    }

    #[test]
    fn place_auto_dense_fills_earliest_empty_cell() {
        let mut next_col = 0usize;
        let mut next_row = 0usize;
        let mut columns = vec![
            Track {
                start: Au::ZERO,
                size: Au::from_px(50.0),
                is_fr: false,
            };
            2
        ];
        let mut rows = vec![
            Track {
                start: Au::ZERO,
                size: Au::from_px(50.0),
                is_fr: false,
            };
            2
        ];
        let mut occupied = HashSet::new();
        occupied.insert((0, 0));
        let cell = place_auto(
            &mut next_col,
            &mut next_row,
            &mut columns,
            &mut rows,
            GridAutoFlow::Dense,
            &mut occupied,
        );
        assert_eq!(cell, ((1, 0), (2, 1)));
    }

    #[test]
    fn resolve_tracks_min_content_and_max_content_resolve_to_zero() {
        let tracks = resolve_tracks(
            &[TrackSize::MinContent, TrackSize::MaxContent],
            Some(Au::from_px(200.0)),
            Au::ZERO,
        );
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].size, Au::ZERO);
        assert_eq!(tracks[1].size, Au::ZERO);
    }

    #[test]
    fn resolve_tracks_fit_content_resolves_to_limit() {
        let tracks = resolve_tracks(
            &[TrackSize::FitContent(Length::px(75.0))],
            Some(Au::from_px(200.0)),
            Au::ZERO,
        );
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].size, Au::from_px(75.0));
    }

    #[test]
    fn resolve_tracks_fit_content_percent() {
        let tracks = resolve_tracks(
            &[TrackSize::FitContent(Length::Percent(50.0))],
            Some(Au::from_px(200.0)),
            Au::ZERO,
        );
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].size, Au::from_px(100.0));
    }

    #[test]
    fn resolve_tracks_minmax_with_fit_content() {
        let tracks = resolve_tracks(
            &[TrackSize::Minmax {
                min: Box::new(TrackSize::FitContent(Length::px(30.0))),
                max: Box::new(TrackSize::Length(Length::px(100.0))),
            }],
            Some(Au::from_px(200.0)),
            Au::ZERO,
        );
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].size, Au::from_px(30.0));
    }
}
