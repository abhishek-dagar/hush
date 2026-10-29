//! Spec 0006, AC-3 and AC-4: flex layout cases, each checked against the
//! border boxes Chrome 155 gives for the same tree, to 1 px. Firefox (Gecko,
//! as Zen 1.22b) gave the same boxes to 1 px on every case.
//!
//! A case is a tree of `div`s, one per line: a label, then its `style`
//! attribute, nested by two spaces. Each one is styled through the UA
//! sheet and the cascade, inside an `html` and `body` with no margins at
//! an 800 by 600 viewport, the way the expected values were taken.
//! Expected rects are the label, then x, y, width and height in CSS px
//! from the case's outermost box.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

mod support;

use std::rc::Rc;

use layout::BoxTree;
use support::{Built, build, check, lay_out, rects, run};

#[test]
fn text_directly_inside_becomes_an_anonymous_item() {
    // AC-3: a run of text is an item of its own, sized to its content.
    let Built { mut doc, labels } = build(
        "
        c display:flex; width:400px
          t 'two words'
          a width:50px; height:10px
          t 'more'
        ",
    );
    let mut boxes = BoxTree::build(&mut doc);
    assert_eq!(
        boxes.dump(&doc),
        "block html\n  block html>body\n    flex html>body>div\n      \
         block (anonymous)\n        text html>body>div>#text[1] \"two words\"\n      \
         block html>body>div>div\n      block (anonymous)\n        \
         text html>body>div>#text[2] \"more\"\n"
    );
    let (fragments, _) = run(&mut boxes);
    let found = rects(&fragments, &labels);
    let a = found["a"];
    let root = fragments.root.as_ref().unwrap().1.clone();
    let flex = root.children()[0].1.children()[0].1.clone();
    let items: Vec<(f32, f32)> = flex
        .children()
        .iter()
        .map(|(at, item)| (at.x.to_f32_px(), item.size().width.to_f32_px()))
        .collect();
    assert_eq!(items.len(), 3);
    if items[0].1 == 0.0 {
        // No system font on this machine: the text has no width.
        return;
    }
    assert_eq!(items[0].0, 0.0);
    assert!((a.0 - items[0].1).abs() < 0.02, "the box follows the text");
    assert!((items[2].0 - (a.0 + 50.0)).abs() < 0.02);
    assert!(items[2].1 > 0.0 && items[2].1 < items[0].1);
}

#[test]
fn a_second_pass_reuses_every_measure() {
    // Spec 0006, Layout rules worth pinning: stretched and unstretched
    // layouts are cached apart, so nothing runs again on a clean tree.
    let Built { mut doc, .. } = build(
        "
        c display:flex; flex-wrap:wrap; width:300px
          a flex:1 1 100px
            e height:10px
          b width:150px; height:40px
          d display:flex; flex-direction:column; width:200px
            f flex:1
            g height:20px
        ",
    );
    let mut boxes = BoxTree::build(&mut doc);
    let (first, stats) = run(&mut boxes);
    assert!(stats.laid_out > 0);
    let (second, stats) = run(&mut boxes);
    assert_eq!(stats.laid_out, 0);
    assert!(Rc::ptr_eq(
        &first.root.as_ref().unwrap().1,
        &second.root.as_ref().unwrap().1
    ));
}

#[test]
fn deep_flex_nesting_stays_on_the_stack() {
    // Past the nesting cap a flex container stacks its items like blocks,
    // so a hostile depth neither overflows the stack nor drops content.
    let mut tree = String::from("\n");
    for depth in 0..200 {
        let indent = "  ".repeat(depth);
        tree.push_str(&format!(
            "{indent}n{depth} display:flex; padding-left:1px\n"
        ));
    }
    let leaf = "  ".repeat(200);
    tree.push_str(&format!("{leaf}leaf width:10px; height:10px\n"));
    let found = lay_out(&tree);
    let leaf = found["leaf"];
    assert_eq!((leaf.2, leaf.3), (10.0, 10.0));
    assert_eq!(leaf.0, 200.0, "every level keeps its padding");
}

#[test]
fn grow_equal_with_gap() {
    check(
        "
        c display:flex; width:300px; gap:8px
          a flex:1 1 0; height:20px
          b flex:1 1 0; height:20px
          d flex:1 1 0; height:20px
        ",
        &[
            ("c", 0.0, 0.0, 300.0, 20.0),
            ("a", 0.0, 0.0, 94.66, 20.0),
            ("b", 102.66, 0.0, 94.67, 20.0),
            ("d", 205.33, 0.0, 94.67, 20.0),
        ],
    );
}

#[test]
fn wrap_to_a_second_line() {
    check(
        "
        c display:flex; flex-wrap:wrap; width:300px
          a width:120px; height:20px
          b width:120px; height:30px
          d width:120px; height:20px
        ",
        &[
            ("c", 0.0, 0.0, 300.0, 50.0),
            ("a", 0.0, 0.0, 120.0, 20.0),
            ("b", 120.0, 0.0, 120.0, 30.0),
            ("d", 0.0, 30.0, 120.0, 20.0),
        ],
    );
}

#[test]
fn shrink_by_scaled_factor() {
    check(
        "
        c display:flex; width:300px
          a width:200px; height:10px
          b width:200px; height:10px; flex-shrink:2
        ",
        &[
            ("c", 0.0, 0.0, 300.0, 10.0),
            ("a", 0.0, 0.0, 166.67, 10.0),
            ("b", 166.67, 0.0, 133.33, 10.0),
        ],
    );
}

#[test]
fn grow_respects_min_and_max() {
    check(
        "
        c display:flex; width:400px
          a flex:1 1 0; max-width:50px; height:10px
          b flex:1 1 0; height:10px
          d flex:1 1 0; min-width:200px; height:10px
        ",
        &[
            ("c", 0.0, 0.0, 400.0, 10.0),
            ("a", 0.0, 0.0, 50.0, 10.0),
            ("b", 50.0, 0.0, 150.0, 10.0),
            ("d", 200.0, 0.0, 200.0, 10.0),
        ],
    );
}

#[test]
fn grow_by_factor_from_basis() {
    check(
        "
        c display:flex; width:500px
          a flex:1 1 100px; height:10px
          b flex:3 1 50px; height:10px
        ",
        &[
            ("c", 0.0, 0.0, 500.0, 10.0),
            ("a", 0.0, 0.0, 187.5, 10.0),
            ("b", 187.5, 0.0, 312.5, 10.0),
        ],
    );
}

#[test]
fn justify_center() {
    check(
        "
        c display:flex; width:300px; justify-content:center
          a width:50px; height:10px
          b width:50px; height:10px
        ",
        &[
            ("c", 0.0, 0.0, 300.0, 10.0),
            ("a", 100.0, 0.0, 50.0, 10.0),
            ("b", 150.0, 0.0, 50.0, 10.0),
        ],
    );
}

#[test]
fn justify_flex_end() {
    check(
        "
        c display:flex; width:300px; justify-content:flex-end
          a width:50px; height:10px
          b width:50px; height:10px
        ",
        &[
            ("c", 0.0, 0.0, 300.0, 10.0),
            ("a", 200.0, 0.0, 50.0, 10.0),
            ("b", 250.0, 0.0, 50.0, 10.0),
        ],
    );
}

#[test]
fn justify_space_between() {
    check(
        "
        c display:flex; width:300px; justify-content:space-between
          a width:50px; height:10px
          b width:50px; height:10px
          d width:50px; height:10px
        ",
        &[
            ("c", 0.0, 0.0, 300.0, 10.0),
            ("a", 0.0, 0.0, 50.0, 10.0),
            ("b", 125.0, 0.0, 50.0, 10.0),
            ("d", 250.0, 0.0, 50.0, 10.0),
        ],
    );
}

#[test]
fn justify_space_around() {
    check(
        "
        c display:flex; width:300px; justify-content:space-around
          a width:50px; height:10px
          b width:50px; height:10px
          d width:50px; height:10px
        ",
        &[
            ("c", 0.0, 0.0, 300.0, 10.0),
            ("a", 25.0, 0.0, 50.0, 10.0),
            ("b", 125.0, 0.0, 50.0, 10.0),
            ("d", 225.0, 0.0, 50.0, 10.0),
        ],
    );
}

#[test]
fn justify_space_evenly() {
    check(
        "
        c display:flex; width:300px; justify-content:space-evenly
          a width:50px; height:10px
          b width:50px; height:10px
          d width:50px; height:10px
        ",
        &[
            ("c", 0.0, 0.0, 300.0, 10.0),
            ("a", 37.5, 0.0, 50.0, 10.0),
            ("b", 125.0, 0.0, 50.0, 10.0),
            ("d", 212.5, 0.0, 50.0, 10.0),
        ],
    );
}

#[test]
fn align_items_center_and_end() {
    check(
        "
        c display:flex; width:300px; height:100px; align-items:center
          a width:50px; height:20px
          b width:50px; height:40px; align-self:flex-end
          d width:50px; height:30px; align-self:flex-start
        ",
        &[
            ("c", 0.0, 0.0, 300.0, 100.0),
            ("a", 0.0, 40.0, 50.0, 20.0),
            ("b", 50.0, 60.0, 50.0, 40.0),
            ("d", 100.0, 0.0, 50.0, 30.0),
        ],
    );
}

#[test]
fn stretch_to_definite_height() {
    check(
        "
        c display:flex; width:300px; height:100px
          a width:50px
          b width:50px; height:40px
          d width:50px; max-height:70px
        ",
        &[
            ("c", 0.0, 0.0, 300.0, 100.0),
            ("a", 0.0, 0.0, 50.0, 100.0),
            ("b", 50.0, 0.0, 50.0, 40.0),
            ("d", 100.0, 0.0, 50.0, 70.0),
        ],
    );
}

#[test]
fn stretch_to_tallest_item() {
    check(
        "
        c display:flex; width:300px
          a width:50px; height:50px
          b width:50px
            e height:10px
          d width:50px; margin-top:5px; margin-bottom:5px
        ",
        &[
            ("c", 0.0, 0.0, 300.0, 50.0),
            ("a", 0.0, 0.0, 50.0, 50.0),
            ("b", 50.0, 0.0, 50.0, 50.0),
            ("e", 50.0, 0.0, 50.0, 10.0),
            ("d", 100.0, 5.0, 50.0, 40.0),
        ],
    );
}

#[test]
fn min_height_line_centers() {
    check(
        "
        c display:flex; width:300px; min-height:60px; align-items:center
          a width:50px; height:20px
        ",
        &[("c", 0.0, 0.0, 300.0, 60.0), ("a", 0.0, 20.0, 50.0, 20.0)],
    );
}

#[test]
fn column_grows_in_definite_height() {
    check(
        "
        c display:flex; flex-direction:column; width:200px; height:300px
          a flex:1 1 0
          b height:50px; flex:none
          d flex:2 1 0
        ",
        &[
            ("c", 0.0, 0.0, 200.0, 300.0),
            ("a", 0.0, 0.0, 200.0, 83.33),
            ("b", 0.0, 83.33, 200.0, 50.0),
            ("d", 0.0, 133.33, 200.0, 166.67),
        ],
    );
}

#[test]
fn column_auto_height_stretches_width() {
    check(
        "
        c display:flex; flex-direction:column; width:200px; row-gap:10px
          a height:30px
          b height:40px; width:50px
          d height:20px; margin-left:10px; margin-right:30px
        ",
        &[
            ("c", 0.0, 0.0, 200.0, 110.0),
            ("a", 0.0, 0.0, 200.0, 30.0),
            ("b", 0.0, 40.0, 50.0, 40.0),
            ("d", 10.0, 90.0, 160.0, 20.0),
        ],
    );
}

#[test]
fn column_align_center() {
    check(
        "
        c display:flex; flex-direction:column; width:200px; align-items:center
          a height:30px; width:50px
          b height:40px; width:80px; align-self:flex-end
        ",
        &[
            ("c", 0.0, 0.0, 200.0, 70.0),
            ("a", 75.0, 0.0, 50.0, 30.0),
            ("b", 120.0, 30.0, 80.0, 40.0),
        ],
    );
}

#[test]
fn column_min_height_grows() {
    check(
        "
        c display:flex; flex-direction:column; width:100px; min-height:200px
          a height:20px; flex-grow:1
          b height:20px
        ",
        &[
            ("c", 0.0, 0.0, 100.0, 200.0),
            ("a", 0.0, 0.0, 100.0, 180.0),
            ("b", 0.0, 180.0, 100.0, 20.0),
        ],
    );
}

#[test]
fn column_items_shrink_to_content() {
    check(
        "
        c display:flex; flex-direction:column; width:100px; height:100px
          a height:80px
          b height:80px
        ",
        &[
            ("c", 0.0, 0.0, 100.0, 100.0),
            ("a", 0.0, 0.0, 100.0, 50.0),
            ("b", 0.0, 50.0, 100.0, 50.0),
        ],
    );
}

#[test]
fn column_items_keep_content_minimum() {
    check(
        "
        c display:flex; flex-direction:column; width:100px; height:100px
          a height:80px
            e height:70px
          b height:80px
        ",
        &[
            ("c", 0.0, 0.0, 100.0, 100.0),
            ("a", 0.0, 0.0, 100.0, 70.0),
            ("e", 0.0, 0.0, 100.0, 70.0),
            ("b", 0.0, 70.0, 100.0, 30.0),
        ],
    );
}

#[test]
fn order_reorders() {
    check(
        "
        c display:flex; width:300px
          a width:50px; height:10px; order:2
          b width:50px; height:10px; order:1
          d width:50px; height:10px
        ",
        &[
            ("c", 0.0, 0.0, 300.0, 10.0),
            ("a", 100.0, 0.0, 50.0, 10.0),
            ("b", 50.0, 0.0, 50.0, 10.0),
            ("d", 0.0, 0.0, 50.0, 10.0),
        ],
    );
}

#[test]
fn auto_margin_pushes_to_the_end() {
    check(
        "
        c display:flex; width:300px
          a width:50px; height:10px
          b width:50px; height:10px; margin-left:auto
        ",
        &[
            ("c", 0.0, 0.0, 300.0, 10.0),
            ("a", 0.0, 0.0, 50.0, 10.0),
            ("b", 250.0, 0.0, 50.0, 10.0),
        ],
    );
}

#[test]
fn auto_margins_center_both_ways() {
    check(
        "
        c display:flex; width:300px; height:100px
          a width:50px; height:20px; margin:auto
        ",
        &[
            ("c", 0.0, 0.0, 300.0, 100.0),
            ("a", 125.0, 40.0, 50.0, 20.0),
        ],
    );
}

#[test]
fn wrapped_lines_stretch_in_definite_height() {
    check(
        "
        c display:flex; flex-wrap:wrap; width:300px; height:200px
          a width:120px; height:20px
          b width:120px; height:20px
          d width:120px
          f width:120px; height:30px
        ",
        &[
            ("c", 0.0, 0.0, 300.0, 200.0),
            ("a", 0.0, 0.0, 120.0, 20.0),
            ("b", 120.0, 0.0, 120.0, 20.0),
            ("d", 0.0, 95.0, 120.0, 105.0),
            ("f", 120.0, 95.0, 120.0, 30.0),
        ],
    );
}

#[test]
fn wrapped_lines_with_row_gap() {
    check(
        "
        c display:flex; flex-wrap:wrap; width:250px; row-gap:10px; column-gap:10px
          a width:120px; height:20px
          b width:120px; height:30px
          d width:120px; height:20px
          f flex:1 0 100px; height:25px
        ",
        &[
            ("c", 0.0, 0.0, 250.0, 65.0),
            ("a", 0.0, 0.0, 120.0, 20.0),
            ("b", 130.0, 0.0, 120.0, 30.0),
            ("d", 0.0, 40.0, 120.0, 20.0),
            ("f", 130.0, 40.0, 120.0, 25.0),
        ],
    );
}

#[test]
fn automatic_minimum_keeps_content() {
    check(
        "
        c display:flex; width:200px
          a flex:1 1 0; height:20px
            e width:300px; height:10px
          b flex:1 1 0; height:20px
        ",
        &[
            ("c", 0.0, 0.0, 200.0, 20.0),
            ("a", 0.0, 0.0, 300.0, 20.0),
            ("e", 0.0, 0.0, 300.0, 10.0),
            ("b", 300.0, 0.0, 0.0, 20.0),
        ],
    );
}

#[test]
fn scroll_container_shrinks_past_content() {
    check(
        "
        c display:flex; width:200px
          a flex:1 1 0; height:20px; overflow:hidden
            e width:300px; height:10px
          b flex:1 1 0; height:20px
        ",
        &[
            ("c", 0.0, 0.0, 200.0, 20.0),
            ("a", 0.0, 0.0, 100.0, 20.0),
            ("e", 0.0, 0.0, 300.0, 10.0),
            ("b", 100.0, 0.0, 100.0, 20.0),
        ],
    );
}

#[test]
fn min_width_zero_shrinks_past_content() {
    check(
        "
        c display:flex; width:200px
          a width:300px; height:20px; min-width:0
          b width:100px; height:20px
        ",
        &[
            ("c", 0.0, 0.0, 200.0, 20.0),
            ("a", 0.0, 0.0, 150.0, 20.0),
            ("b", 150.0, 0.0, 50.0, 20.0),
        ],
    );
}

#[test]
fn automatic_minimum_shrinks_to_content() {
    check(
        "
        c display:flex; width:200px
          a width:300px; height:20px
            e width:120px; height:10px
          b width:100px; height:20px
        ",
        &[
            ("c", 0.0, 0.0, 200.0, 20.0),
            ("a", 0.0, 0.0, 150.0, 20.0),
            ("e", 0.0, 0.0, 120.0, 10.0),
            ("b", 150.0, 0.0, 50.0, 20.0),
        ],
    );
}

#[test]
fn box_sizing_and_edges() {
    check(
        "
        c display:flex; width:400px
          a flex:0 0 100px; height:20px; padding:10px; border:2px solid black
          b flex:0 0 100px; height:20px; padding:10px; border:2px solid black; box-sizing:border-box
          d flex:1 1 0; height:20px; padding:0 5px; margin:0 7px
        ",
        &[
            ("c", 0.0, 0.0, 400.0, 44.0),
            ("a", 0.0, 0.0, 124.0, 44.0),
            ("b", 124.0, 0.0, 100.0, 24.0),
            ("d", 231.0, 0.0, 162.0, 20.0),
        ],
    );
}

#[test]
fn percent_basis_and_gap() {
    check(
        "
        c display:flex; width:300px; column-gap:10%
          a flex:0 0 50%; height:10px
          b flex:1 1 0; height:10px
        ",
        &[
            ("c", 0.0, 0.0, 300.0, 10.0),
            ("a", 0.0, 0.0, 150.0, 10.0),
            ("b", 180.0, 0.0, 120.0, 10.0),
        ],
    );
}

#[test]
fn nested_row_in_column() {
    check(
        "
        c display:flex; flex-direction:column; width:300px; height:200px
          a display:flex; flex:1 1 0; gap:10px
            e flex:1 1 0
            g width:40px
          b height:40px
        ",
        &[
            ("c", 0.0, 0.0, 300.0, 200.0),
            ("a", 0.0, 0.0, 300.0, 160.0),
            ("e", 0.0, 0.0, 250.0, 160.0),
            ("g", 260.0, 0.0, 40.0, 160.0),
            ("b", 0.0, 160.0, 300.0, 40.0),
        ],
    );
}

#[test]
fn nested_column_in_row_stretches() {
    check(
        "
        c display:flex; width:300px
          a display:flex; flex-direction:column; width:100px
            e flex:1 1 0
            g height:20px
          b width:50px; height:80px
        ",
        &[
            ("c", 0.0, 0.0, 300.0, 80.0),
            ("a", 0.0, 0.0, 100.0, 80.0),
            ("e", 0.0, 0.0, 100.0, 60.0),
            ("g", 0.0, 60.0, 100.0, 20.0),
            ("b", 100.0, 0.0, 50.0, 80.0),
        ],
    );
}

#[test]
fn max_content_basis_from_children() {
    check(
        "
        c display:flex; width:400px
          a height:10px
            e width:60px; height:10px
          b height:10px; display:flex
            g width:30px; height:10px
            h width:45px; height:10px
        ",
        &[
            ("c", 0.0, 0.0, 400.0, 10.0),
            ("a", 0.0, 0.0, 60.0, 10.0),
            ("e", 0.0, 0.0, 60.0, 10.0),
            ("b", 60.0, 0.0, 75.0, 10.0),
            ("g", 60.0, 0.0, 30.0, 10.0),
            ("h", 90.0, 0.0, 45.0, 10.0),
        ],
    );
}

#[test]
fn shrink_with_wrap_and_max_content() {
    check(
        "
        c display:flex; flex-wrap:wrap; width:150px
          a height:10px; display:flex; flex-wrap:wrap
            e width:100px; height:10px
            g width:100px; height:10px
          b width:100px; height:10px
        ",
        &[
            ("c", 0.0, 0.0, 150.0, 20.0),
            ("a", 0.0, 0.0, 150.0, 10.0),
            ("e", 0.0, 0.0, 100.0, 10.0),
            ("g", 0.0, 10.0, 100.0, 10.0),
            ("b", 0.0, 10.0, 100.0, 10.0),
        ],
    );
}

#[test]
fn gap_shorthand_with_two_calc_values() {
    // AC-4: `gap:calc(10px + 5px) calc(2 * 10px)` keeps both gaps, 15 row
    // and 20 column, instead of dropping them to zero.
    check(
        "
        c display:flex; width:300px; gap:calc(10px + 5px) calc(2 * 10px)
          a width:50px; height:10px
          b width:50px; height:10px
        ",
        &[
            ("c", 0.0, 0.0, 300.0, 10.0),
            ("a", 0.0, 0.0, 50.0, 10.0),
            ("b", 70.0, 0.0, 50.0, 10.0),
        ],
    );
}

#[test]
fn align_content_center_offsets_the_first_line() {
    // AC-6: `flex-wrap` with `align-content:center` offsets every line,
    // including the first. Two 150x20 items in a 200x100 container land at
    // y=30 and y=50.
    check(
        "
        c display:flex; flex-wrap:wrap; width:200px; height:100px; align-content:center
          a width:150px; height:20px
          b width:150px; height:20px
        ",
        &[
            ("c", 0.0, 0.0, 200.0, 100.0),
            ("a", 0.0, 30.0, 150.0, 20.0),
            ("b", 0.0, 50.0, 150.0, 20.0),
        ],
    );
}
