//! Spec 0009, child 0001: floats, `clear`, block formatting contexts,
//! the exclusion space and how a BFC root grows to hold its floats.
//!
//! Cases are worked out from CSS 2.2 section 9.5 in CSS px from the case's
//! outermost box. Floats blockify (CSS 2.2 section 9.7), take no room in
//! flow, and sit at the first opportunity the exclusion space offers. A
//! `clear` pushes a box below the matching floats. A BFC root's `height:
//! auto` grows to hold its floats (CSS 2.2 section 10.6.7).
//!
//! Notes on coverage: clearance, BFC-root growth and width-`auto` floats are
//! exercised below. Packing floats side by side and placing `float: right`
//! at the right edge (CSS 2.2 section 9.5.1 rules 1-9) are covered by
//! `two_left_floats_sit_side_by_side_when_there_is_room` and
//! `a_right_float_sits_at_the_top_right_of_its_container`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

mod support;

use base::{Au, Point, Size};
use layout::{BoxTree, Fragment, layout};
use support::{Built, build, check, lay_out};
use text::{FontContext, WebFontLayer};

#[test]
fn a_left_float_sits_at_the_top_left_of_its_container() {
    // AC-1: a float is placed as high as it may go, then as far left as it
    // may go. Floats take no room in flow, so the container's `height: auto`
    // stays 0 unless it is a BFC root (see `a_bfc_root_grows_to_hold_its_float`).
    check(
        "
        c width:400px
          f float:left; width:100px; height:50px
        ",
        &[("c", 0.0, 0.0, 400.0, 0.0), ("f", 0.0, 0.0, 100.0, 50.0)],
    );
}

#[test]
fn clear_both_pushes_a_block_below_all_earlier_floats() {
    // AC-2: clearance moves the box below the matching floats, stopping
    // margins collapsing through it.
    check(
        "
        c width:400px
          f1 float:left; width:100px; height:50px
          f2 float:right; width:100px; height:30px
          b clear:both; height:20px
        ",
        &[
            ("c", 0.0, 0.0, 400.0, 70.0),
            ("f1", 0.0, 0.0, 100.0, 50.0),
            ("f2", 300.0, 0.0, 100.0, 30.0),
            ("b", 0.0, 50.0, 400.0, 20.0),
        ],
    );
}

#[test]
fn a_bfc_root_with_height_auto_grows_to_hold_its_float() {
    // AC-3: a block formatting context root with `height: auto` grows to
    // hold its floats. `overflow: hidden` starts a formatting context.
    check(
        "
        c width:400px; overflow:hidden
          f float:left; width:100px; height:50px
        ",
        &[("c", 0.0, 0.0, 400.0, 50.0), ("f", 0.0, 0.0, 100.0, 50.0)],
    );
}

#[test]
fn a_float_width_auto_shrinks_to_fit_its_content() {
    // AC-1: a `width: auto` float takes its content's max content width
    // when that fits (CSS 2.2 section 10.3.5). Wikipedia's thumbnails.
    let found = lay_out(
        "
        c width:400px
          f float:right; padding:2px
            i width:150px; height:50px
        ",
    );
    assert_eq!(found["f"], (246.0, 0.0, 154.0, 54.0));
}

#[test]
fn a_float_width_auto_never_shrinks_below_its_min_content() {
    // Content that cannot break and is wider than the container: the float
    // takes its min content width and overflows (CSS 2.2 section 10.3.5).
    let found = lay_out(
        "
        c width:400px
          f float:left
            i width:600px; height:50px
        ",
    );
    assert_eq!(found["f"].2, 600.0);
}

#[test]
fn layout_is_stable_across_passes() {
    // AC-5, first part: floats keep layout incremental. Laying the same tree
    // twice reuses every cached fragment.
    let first = lay_out(
        "
        c width:400px
          f float:left; width:100px; height:50px
          b height:20px
        ",
    );
    let second = lay_out(
        "
        c width:400px
          f float:left; width:100px; height:50px
          b height:20px
        ",
    );
    assert_eq!(first, second, "the layout is stable across passes");
    assert_eq!(first["f"], (0.0, 0.0, 100.0, 50.0));
    // `b` has no `clear`, so it sits at the top beside the float at full
    // width (block boxes beside floats are not narrowed yet: task 7).
    assert_eq!(first["b"], (0.0, 0.0, 400.0, 20.0));
}

#[test]
fn two_left_floats_sit_side_by_side_when_there_is_room() {
    // AC-1: each float is as far left as it may go given earlier floats, so
    // two 100px left floats in a 400px container sit side by side.
    check(
        "
        c width:400px
          f1 float:left; width:100px; height:50px
          f2 float:left; width:100px; height:30px
        ",
        &[
            ("c", 0.0, 0.0, 400.0, 0.0),
            ("f1", 0.0, 0.0, 100.0, 50.0),
            ("f2", 100.0, 0.0, 100.0, 30.0),
        ],
    );
}

#[test]
fn a_right_float_sits_at_the_top_right_of_its_container() {
    // AC-1: a right float is as far right as it may go.
    check(
        "
        c width:400px
          f float:right; width:100px; height:50px
        ",
        &[("c", 0.0, 0.0, 400.0, 0.0), ("f", 300.0, 0.0, 100.0, 50.0)],
    );
}

/// Text laid out beside a float must start at the free space's inner edge,
/// not paint over the float (spec 0009, child 0001).
#[test]
fn text_beside_a_left_float_starts_at_its_right_edge() {
    let ahem = include_bytes!("../../../tools/conformance/tests/fixtures/fonts/Ahem.ttf").to_vec();
    let mut fonts = FontContext::with_families(ahem, []);
    let Built { mut doc, labels: _ } = build(
        "
        c width:400px
          f float:left; width:100px; height:50px
          t 'Hello world'
        ",
    );
    let mut boxes = BoxTree::build(&mut doc);
    let viewport = Size::new(Au::from_px(800.0), Au::from_px(600.0));
    let (tree, _) = layout(&mut boxes, viewport, &mut fonts, &WebFontLayer::default());

    let mut text_x = Vec::new();
    if let Some((offset, fragment)) = &tree.root {
        collect_text_x(fragment, Point::default().offset_by(*offset), &mut text_x);
    }

    assert!(
        !text_x.is_empty(),
        "expected a text fragment to be laid out beside the float"
    );
    // The float is 100 px wide at the left edge, so text must start at its
    // right edge (x = 100), not at x = 0 where the float paints.
    for x in &text_x {
        assert!(
            *x >= Au::from_px(100.0),
            "text at {x:?}px overlaps the left float"
        );
    }
}

/// Walks the fragment tree, recording the accumulated x position of every
/// text fragment.
fn collect_text_x(fragment: &Fragment, at: Point, out: &mut Vec<Au>) {
    match fragment {
        Fragment::Text(_) => out.push(at.x),
        Fragment::Box(_) | Fragment::Line(_) => {
            for (offset, child) in fragment.children() {
                collect_text_x(child, at.offset_by(*offset), out);
            }
        }
    }
}

/// Every text fragment's left and right edge, from the case's root.
fn text_edges(tree: &str) -> Vec<(f32, f32)> {
    let ahem = include_bytes!("../../../tools/conformance/tests/fixtures/fonts/Ahem.ttf").to_vec();
    let mut fonts = FontContext::with_families(ahem, []);
    let Built { mut doc, labels: _ } = build(tree);
    let mut boxes = BoxTree::build(&mut doc);
    let viewport = Size::new(Au::from_px(800.0), Au::from_px(600.0));
    let (fragments, _) = layout(&mut boxes, viewport, &mut fonts, &WebFontLayer::default());
    let mut edges = Vec::new();
    let mut stack: Vec<(Point, &Fragment)> = fragments
        .root
        .iter()
        .map(|(offset, fragment)| (Point::default().offset_by(*offset), &**fragment))
        .collect();
    while let Some((at, fragment)) = stack.pop() {
        if let Fragment::Text(_) = fragment {
            let right = at.x + fragment.size().width;
            edges.push((at.x.to_f32_px(), right.to_f32_px()));
        }
        for (offset, child) in fragment.children() {
            stack.push((at.offset_by(*offset), child));
        }
    }
    assert!(!edges.is_empty(), "expected text to be laid out");
    edges
}

/// A float placed below earlier content narrows the next block's first
/// line too: the block sees the float from its own top, not its parent's
/// (spec 0009, child 0001). Wikipedia's hatnote, infobox, paragraph.
#[test]
fn a_float_after_earlier_content_narrows_the_next_blocks_first_line() {
    let edges = text_edges(
        "
        c width:400px
          n height:20px
          f float:right; width:100px; height:200px
          p margin-top:8px
            t 'aaaa aaaa aaaa aaaa aaaa aaaa aaaa aaaa aaaa'
        ",
    );
    for (_, right) in edges {
        assert!(right <= 300.0, "text reaching {right}px overlaps the float");
    }
}

/// A float inside one block still narrows the lines of the blocks after
/// it in the same BFC: the parent takes the child's floats back (CSS 2.2
/// section 9.5). Wikipedia's infobox in one `section`, text in the next.
#[test]
fn a_float_inside_a_block_narrows_the_next_blocks_lines() {
    let edges = text_edges(
        "
        c width:400px
          s1
            f float:right; width:100px; height:200px
          s2 padding-top:10px
            t 'aaaa aaaa aaaa aaaa aaaa aaaa aaaa aaaa aaaa'
        ",
    );
    for (_, right) in edges {
        assert!(right <= 300.0, "text reaching {right}px overlaps the float");
    }
}

/// Lines in a padded block start at the float's edge, not the float's
/// width past the block's own content edge (spec 0009, child 0001).
#[test]
fn text_in_a_padded_block_starts_at_the_float_edge() {
    let edges = text_edges(
        "
        c width:400px
          f float:left; width:100px; height:50px
          p padding-left:20px
            t 'Hello world'
        ",
    );
    for (left, _) in edges {
        assert_eq!(left, 100.0, "text should start at the float's right edge");
    }
}

#[test]
fn a_float_sits_inside_its_margins() {
    // CSS 2.2 section 9.5.1: the margin box is placed, so the border box sits
    // inset by the left and top margins on either side.
    check(
        "
        c width:400px
          l float:left; width:100px; height:50px; margin:10px 0 0 20px
          r float:right; width:100px; height:50px; margin:10px 30px 0 0
        ",
        &[
            ("c", 0.0, 0.0, 400.0, 0.0),
            ("l", 20.0, 10.0, 100.0, 50.0),
            ("r", 270.0, 10.0, 100.0, 50.0),
        ],
    );
}

#[test]
fn a_new_formatting_context_sits_beside_the_floats() {
    // CSS 2.2 section 9.5: a box that starts its own block formatting
    // context (`flow-root`, `overflow: hidden`) must not overlap a float, so
    // it narrows into the band beside it. A plain block still runs under it.
    check(
        "
        c width:400px
          f float:left; width:100px; height:100px
          r display:flow-root; height:20px
          o overflow:hidden; height:20px
          p height:20px
        ",
        &[
            ("f", 0.0, 0.0, 100.0, 100.0),
            ("r", 100.0, 0.0, 300.0, 20.0),
            ("o", 100.0, 20.0, 300.0, 20.0),
            ("p", 0.0, 40.0, 400.0, 20.0),
        ],
    );
}
