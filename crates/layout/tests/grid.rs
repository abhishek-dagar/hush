//! Spec 0010, AC-1 to AC-5: grid layout cases, each checked against the
//! border boxes the Grid Level 1 algorithms give for the same tree.
//!
//! Cases are written like the flex ones (see `support`): one `div` per
//! line, a label then its `style` attribute, nested by two spaces. Expected
//! rects are in CSS px from the case's outermost box.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

mod support;

use support::check;

#[test]
fn explicit_tracks_place_items_in_row_flow() {
    // AC-1 and AC-2: fixed tracks, row flow auto placement, stretch.
    check(
        "
        g display:grid; width:200px; grid-template-columns:50px 70px; grid-template-rows:20px 30px
          a
          b
          c
        ",
        &[
            ("g", 0.0, 0.0, 200.0, 50.0),
            ("a", 0.0, 0.0, 50.0, 20.0),
            ("b", 50.0, 0.0, 70.0, 20.0),
            ("c", 0.0, 20.0, 50.0, 30.0),
        ],
    );
}

#[test]
fn fr_tracks_share_the_free_space() {
    // AC-1 and AC-3: `fr` tracks divide what the fixed tracks leave.
    check(
        "
        g display:grid; width:300px; height:40px; grid-template-columns:100px 1fr 1fr
          a
          b
          c
        ",
        &[
            ("g", 0.0, 0.0, 300.0, 40.0),
            ("a", 0.0, 0.0, 100.0, 40.0),
            ("b", 100.0, 0.0, 100.0, 40.0),
            ("c", 200.0, 0.0, 100.0, 40.0),
        ],
    );
}

#[test]
fn auto_tracks_stretch_to_free_space() {
    // AC-3: `auto` tracks stretch (Grid 11.8): on a 500px grid with
    // `auto 100px`, an item with no width fills the auto track (400).
    check(
        "
        g display:grid; width:500px; grid-template-columns:auto 100px
          a height:10px
          b height:10px
        ",
        &[
            ("g", 0.0, 0.0, 500.0, 10.0),
            ("a", 0.0, 0.0, 400.0, 10.0),
            ("b", 400.0, 0.0, 100.0, 10.0),
        ],
    );
}

#[test]
fn auto_tracks_take_content_then_stretch() {
    // Auto tracks grow from content, then `justify-content: normal`
    // stretches them to fill the container when items have no definite
    // width. `auto auto` on a 200px grid gives 100 each.
    check(
        "
        g display:grid; width:200px; grid-template-columns:auto auto
          a height:10px
          b height:10px
        ",
        &[
            ("g", 0.0, 0.0, 200.0, 10.0),
            ("a", 0.0, 0.0, 100.0, 10.0),
            ("b", 100.0, 0.0, 100.0, 10.0),
        ],
    );
}

#[test]
fn repeat_auto_fill_makes_as_many_tracks_as_fit() {
    // AC-1: `(200 + 10) / (40 + 10)` is four tracks.
    check(
        "
        g display:grid; width:200px; grid-template-columns:repeat(auto-fill, 40px); column-gap:10px
          a height:10px
          b height:10px
          c height:10px
        ",
        &[
            ("g", 0.0, 0.0, 200.0, 10.0),
            ("a", 0.0, 0.0, 40.0, 10.0),
            ("b", 50.0, 0.0, 40.0, 10.0),
            ("c", 100.0, 0.0, 40.0, 10.0),
        ],
    );
}

#[test]
fn empty_auto_fit_tracks_collapse_before_alignment() {
    // AC-1: with two items, tracks three and four collapse, so end
    // alignment starts at 200 - (40 + 10 + 40) = 110.
    check(
        "
        g display:grid; width:200px; justify-content:end; grid-template-columns:repeat(auto-fit, 40px); column-gap:10px
          a height:10px
          b height:10px
        ",
        &[
            ("a", 110.0, 0.0, 40.0, 10.0),
            ("b", 160.0, 0.0, 40.0, 10.0),
        ],
    );
}

#[test]
fn auto_fit_minmax_fr_shares_free_space() {
    // AC-1: repeat(auto-fit, minmax(100px, 1fr)) in a 500px grid: the
    // empty auto-fit tracks collapse and the two items share 250 each.
    check(
        "
        g display:grid; width:500px; grid-template-columns:repeat(auto-fit, minmax(100px, 1fr))
          a height:10px
          b height:10px
        ",
        &[
            ("g", 0.0, 0.0, 500.0, 10.0),
            ("a", 0.0, 0.0, 250.0, 10.0),
            ("b", 250.0, 0.0, 250.0, 10.0),
        ],
    );
}

#[test]
fn named_lines_and_areas_resolve_to_track_edges() {
    // AC-2: a named line picks a track edge.
    check(
        "
        g display:grid; width:200px; grid-template-columns:[left] 40px [mid] 60px [right] 80px; grid-template-rows:20px
          a grid-column:mid / right
        ",
        &[
            ("g", 0.0, 0.0, 200.0, 20.0),
            ("a", 40.0, 0.0, 60.0, 20.0),
        ],
    );
}

#[test]
fn named_areas_give_both_edges() {
    // AC-2: `grid-template-areas` names an area, and `grid-area` puts an
    // item in it.
    check(
        "
        g display:grid; width:200px; grid-template-columns:40px 60px 80px; grid-template-rows:20px 20px; grid-template-areas:'top top side' 'wide wide wide'
          b grid-column:side; grid-row:top
          c grid-area:wide
        ",
        &[
            ("g", 0.0, 0.0, 200.0, 40.0),
            ("b", 100.0, 0.0, 80.0, 20.0),
            ("c", 0.0, 20.0, 180.0, 20.0),
        ],
    );
}

#[test]
fn grid_template_with_areas_sizes_the_grid() {
    // AC-1: grid-template with named areas creates rows and columns from the
    // string form, and the item in area c lands at 50x30, y=20.
    check(
        "
        g display:grid; width:200px; grid-template:\"a b\" 20px \"c d\" 30px / 50px 1fr
          c grid-area:c
        ",
        &[("g", 0.0, 0.0, 200.0, 50.0), ("c", 0.0, 20.0, 50.0, 30.0)],
    );
}

#[test]
fn negative_lines_count_from_the_end_and_spans_fill_tracks() {
    // AC-2: line -1 is the last line; a span reaches back two tracks.
    check(
        "
        g display:grid; width:200px; grid-template-columns:40px 60px 80px
          a grid-column:span 2 / -1; height:10px
        ",
        &[("g", 0.0, 0.0, 200.0, 10.0), ("a", 40.0, 0.0, 140.0, 10.0)],
    );
}

#[test]
fn column_flow_and_gaps() {
    // AC-2 and AC-4: column flow fills rows first; gaps add between tracks.
    check(
        "
        g display:grid; width:200px; grid-template-columns:20px 20px; grid-template-rows:20px 20px; grid-auto-flow:column; column-gap:10px; row-gap:5px
          a
          b
          c
        ",
        &[
            ("a", 0.0, 0.0, 20.0, 20.0),
            ("b", 0.0, 25.0, 20.0, 20.0),
            ("c", 30.0, 0.0, 20.0, 20.0),
        ],
    );
}

#[test]
fn grid_auto_flow_short_hand_makes_new_rows() {
    // AC-1: `grid:auto-flow 25px / 100px 200px` puts the third item on a
    // new row at x=0, y=25, not a new column at x=300.
    check(
        "
        g display:grid; width:400px; grid:auto-flow 25px / 100px 200px
          a height:10px
          b height:10px
          c height:10px
        ",
        &[
            ("g", 0.0, 0.0, 400.0, 50.0),
            ("a", 0.0, 0.0, 100.0, 10.0),
            ("b", 100.0, 0.0, 200.0, 10.0),
            ("c", 0.0, 25.0, 100.0, 10.0),
        ],
    );
}

#[test]
fn dense_flow_backfills_earlier_holes() {
    // AC-2: sparse placement leaves (2, 0) empty; dense backfills it.
    check(
        "
        g display:grid; width:150px; grid-template-columns:repeat(3, 50px); grid-auto-flow:row dense
          a grid-column:span 2; height:10px
          b grid-column:span 2; height:10px
          c height:10px
        ",
        &[
            ("a", 0.0, 0.0, 100.0, 10.0),
            ("b", 0.0, 10.0, 100.0, 10.0),
            ("c", 100.0, 0.0, 50.0, 10.0),
        ],
    );
}

#[test]
fn order_changes_placement_order() {
    // AC-2: `order` sorts items before auto placement.
    check(
        "
        g display:grid; width:100px; grid-template-columns:50px 50px; grid-template-rows:10px
          a order:2
          b order:1
        ",
        &[("b", 0.0, 0.0, 50.0, 10.0), ("a", 50.0, 0.0, 50.0, 10.0)],
    );
}

#[test]
fn content_alignment_positions_the_whole_grid() {
    // AC-4: `justify-content: center` and `align-content: end`.
    check(
        "
        g display:grid; width:200px; height:100px; grid-template-columns:40px 40px; grid-template-rows:20px; justify-content:center; align-content:end
          a
          b
        ",
        &[
            ("a", 60.0, 80.0, 40.0, 20.0),
            ("b", 100.0, 80.0, 40.0, 20.0),
        ],
    );
}

#[test]
fn self_alignment_places_an_item_inside_its_area() {
    // AC-4: `justify-self: center` and `align-self: end`.
    check(
        "
        g display:grid; width:200px; height:100px; grid-template-columns:100px; grid-template-rows:100px
          a width:40px; height:20px; justify-self:center; align-self:end
        ",
        &[("a", 30.0, 80.0, 40.0, 20.0)],
    );
}

#[test]
fn implicit_tracks_use_grid_auto_columns_and_rows() {
    // AC-1: a column-flow grid beyond its template takes implicit tracks.
    check(
        "
        g display:grid; width:200px; grid-template-columns:50px; grid-template-rows:20px; grid-auto-flow:column; grid-auto-columns:30px
          a
          b
          c
        ",
        &[
            ("a", 0.0, 0.0, 50.0, 20.0),
            ("b", 50.0, 0.0, 30.0, 20.0),
            ("c", 80.0, 0.0, 30.0, 20.0),
        ],
    );
}

#[test]
fn absolutely_positioned_child_with_placement_uses_its_area() {
    // AC-5: a definite grid area is the containing block; no insets puts
    // the box at the area's start corner.
    check(
        "
        c width:200px
          g display:grid; grid-template-columns:50px 70px; grid-template-rows:20px 30px
            x position:absolute; grid-column:2; grid-row:2; width:10px; height:10px
        ",
        &[("x", 50.0, 20.0, 10.0, 10.0)],
    );
}

#[test]
fn absolutely_positioned_child_without_placement_uses_the_padding_box() {
    // AC-5: the grid container itself is the containing block.
    check(
        "
        g display:grid; width:200px; height:80px; padding:10px
          x position:absolute; left:5px; top:5px; width:10px; height:10px
        ",
        &[("x", 5.0, 5.0, 10.0, 10.0)],
    );
}

#[test]
fn inline_grid_shrinks_to_its_tracks() {
    // AC-3: an inline grid reports its max content width and sits inline.
    check(
        "
        g display:inline-grid; grid-template-columns:auto auto
          a width:30px; height:10px
          b width:40px; height:10px
        ",
        &[
            ("g", 0.0, 0.0, 70.0, 10.0),
            ("a", 0.0, 0.0, 30.0, 10.0),
            ("b", 30.0, 0.0, 40.0, 10.0),
        ],
    );
}

#[test]
fn text_directly_inside_becomes_an_anonymous_item() {
    // AC-1: a run of text is wrapped in an anonymous grid item.
    check(
        "
        g display:grid; width:100px; grid-template-columns:50px 50px; grid-template-rows:10px
          a
          'text'
        ",
        &[("a", 0.0, 0.0, 50.0, 10.0)],
    );
}

#[test]
fn bare_fr_row_takes_its_content_height_in_an_auto_height_grid() {
    // A bare `1fr` has an `auto` minimum (Grid §7.2.4), so under an auto
    // height it grows to its content instead of collapsing to zero.
    check(
        "
        g display:grid; width:100px; grid-template-columns:100px; grid-template-rows:min-content 1fr min-content
          a height:10px
          b height:30px
          c height:20px
        ",
        &[
            ("g", 0.0, 0.0, 100.0, 60.0),
            ("a", 0.0, 0.0, 100.0, 10.0),
            ("b", 0.0, 10.0, 100.0, 30.0),
            ("c", 0.0, 40.0, 100.0, 20.0),
        ],
    );
}

#[test]
fn minmax_track_grows_to_its_maximum_from_free_space() {
    // Grid §12.6: free space grows a `minmax(0, 150px)` track up to its
    // growth limit even when no item asks for that width.
    check(
        "
        g display:grid; width:300px; grid-template-columns:minmax(0, 150px) 50px
          a height:10px
          b height:10px
        ",
        &[
            ("g", 0.0, 0.0, 300.0, 10.0),
            ("a", 0.0, 0.0, 150.0, 10.0),
            ("b", 150.0, 0.0, 50.0, 10.0),
        ],
    );
}

#[test]
fn grid_item_in_auto_column_reports_its_content_width() {
    // AC-3: an inner grid in an auto column contributes its min/max content
    // width (80 from its two fixed 40px columns) instead of 0 when the outer
    // width is indefinite.
    check(
        "
        g display:inline-grid; grid-template-columns:auto 100px
          i display:grid; grid-template-columns:40px 40px
            a height:10px
            b height:10px
        ",
        &[
            ("g", 0.0, 0.0, 180.0, 10.0),
            ("i", 0.0, 0.0, 80.0, 10.0),
            ("a", 0.0, 0.0, 40.0, 10.0),
            ("b", 40.0, 0.0, 40.0, 10.0),
        ],
    );
}

#[test]
fn aspect_ratio_item_gets_a_height_in_a_column() {
    // AC-7: a grid item with aspect-ratio:3/1 in a 150px column gets
    // height 50, not 0, when alignment does not stretch it.
    check(
        "
        g display:grid; width:300px; grid-template-columns:150px 150px; align-items:start
          a aspect-ratio:3 / 1
        ",
        &[("g", 0.0, 0.0, 300.0, 50.0), ("a", 0.0, 0.0, 150.0, 50.0)],
    );
}
