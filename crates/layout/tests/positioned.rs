//! Spec 0006, AC-5 and AC-7: relative and absolute positioning cases.
//!
//! Cases are written like the flex ones (see `support`). Expected rects
//! are worked out from CSS 2.2 sections 9.3.1, 10.3.7 and 10.6.4 for each
//! tree, in CSS px from the case's outermost box.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

mod support;

use std::collections::HashMap;
use std::rc::Rc;

use dom::NodeId;
use layout::{BoxTree, Fragment, FragmentTree};
use support::{Built, build, check, lay_out, run};

#[test]
fn switch_knob_sits_at_its_insets_inside_the_padding_box() {
    // Spec 0006, Critical test scenarios: Positioning.
    check(
        "
        s position:relative; box-sizing:border-box; width:40px; height:24px; border:1px solid black
          k position:absolute; top:2px; left:2px; width:18px; height:18px
          r position:absolute; top:2px; left:auto; right:2px; width:18px; height:18px
        ",
        &[
            ("s", 0.0, 0.0, 40.0, 24.0),
            // The padding box starts inside the 1px border.
            ("k", 3.0, 3.0, 18.0, 18.0),
            // 1 + 38 - 2 - 18.
            ("r", 19.0, 3.0, 18.0, 18.0),
        ],
    );
}

#[test]
fn absolute_boxes_take_no_space_in_flow() {
    check(
        "
        c width:200px
          a height:10px
          x position:absolute; width:30px; height:30px
          b height:20px
        ",
        &[
            ("c", 0.0, 0.0, 200.0, 30.0),
            // No insets: the static position, right after `a`.
            ("x", 0.0, 10.0, 30.0, 30.0),
            ("b", 0.0, 10.0, 200.0, 20.0),
        ],
    );
}

#[test]
fn both_insets_fill_and_one_inset_shrinks_to_fit() {
    check(
        "
        c position:relative; width:300px; height:100px
          f position:absolute; left:10px; right:20px; top:5px; bottom:15px
          s position:absolute; right:0; bottom:0
            e width:50px; height:10px
        ",
        &[
            ("c", 0.0, 0.0, 300.0, 100.0),
            ("f", 10.0, 5.0, 270.0, 80.0),
            ("s", 250.0, 90.0, 50.0, 10.0),
            ("e", 250.0, 90.0, 50.0, 10.0),
        ],
    );
}

#[test]
fn without_a_positioned_ancestor_the_viewport_is_the_containing_block() {
    // The 800 by 600 viewport: 10% across is 80, 50% down is 300.
    check(
        "
        c width:100px; height:50px; margin-left:30px
          a position:absolute; left:10%; top:50%; width:10px; height:10px
        ",
        &[("c", 0.0, 0.0, 100.0, 50.0), ("a", 50.0, 300.0, 10.0, 10.0)],
    );
}

#[test]
fn auto_margins_center_between_both_insets() {
    check(
        "
        c position:relative; width:300px; height:60px
          m position:absolute; left:0; right:0; top:0; bottom:0; width:100px; height:20px; margin:auto
        ",
        &[
            ("c", 0.0, 0.0, 300.0, 60.0),
            ("m", 100.0, 20.0, 100.0, 20.0),
        ],
    );
}

#[test]
fn relative_offsets_move_only_their_own_box() {
    check(
        "
        c width:100px
          a position:relative; left:5px; right:50px; top:-3px; height:10px
            e height:4px
          b height:10px
        ",
        &[
            ("c", 0.0, 0.0, 100.0, 20.0),
            // `left` wins over `right`, and the children move along.
            ("a", 5.0, -3.0, 100.0, 10.0),
            ("e", 5.0, -3.0, 100.0, 4.0),
            ("b", 0.0, 10.0, 100.0, 10.0),
        ],
    );
}

#[test]
fn an_absolute_box_among_inline_content_starts_where_it_sat_on_the_line() {
    // The inline block sits on the baseline, which depends on the system
    // font; the absolute box's static position is the line's top, just
    // after it.
    let found = lay_out(
        "
        c position:relative; width:200px
          i display:inline-block; width:40px; height:10px
          x position:absolute; width:5px; height:5px
        ",
    );
    assert_eq!(found["i"].0, 0.0);
    assert_eq!(found["x"], (40.0, 0.0, 5.0, 5.0));
}

#[test]
fn flex_absolute_children_are_not_items() {
    // The static position in a flex container is its content box's start.
    check(
        "
        c display:flex; position:relative; width:200px; padding:4px
          a width:50px; height:20px
          x position:absolute; width:10px; height:10px
          b width:50px; height:20px
        ",
        &[
            ("c", 0.0, 0.0, 208.0, 28.0),
            ("a", 4.0, 4.0, 50.0, 20.0),
            ("x", 4.0, 4.0, 10.0, 10.0),
            ("b", 54.0, 4.0, 50.0, 20.0),
        ],
    );
}

/// The label of the box fragment each labeled box's fragment is a child
/// of, `None` for an unlabeled parent.
fn parents(
    fragments: &FragmentTree,
    labels: &HashMap<NodeId, String>,
) -> HashMap<String, Option<String>> {
    let label = |fragment: &Fragment| {
        fragment
            .as_box()
            .and_then(|b| b.node)
            .and_then(|node| labels.get(&node))
            .cloned()
    };
    let mut found = HashMap::new();
    let mut stack: Vec<Rc<Fragment>> = fragments.root.iter().map(|(_, f)| Rc::clone(f)).collect();
    while let Some(fragment) = stack.pop() {
        for (_, child) in fragment.children() {
            if let Some(name) = label(child) {
                found.insert(name, label(&fragment));
            }
            stack.push(Rc::clone(child));
        }
    }
    found
}

#[test]
fn absolute_boxes_hang_off_their_containing_block() {
    // AC-7 follows from this: an `overflow: hidden` box clips its children,
    // so it clips an absolute box only when that box's containing block is
    // the clipping box or inside it.
    let Built { mut doc, labels } = build(
        "
        r position:relative; width:100px; height:100px
          o overflow:hidden; width:50px; height:50px
            x position:absolute; left:60px; width:10px; height:10px
          q overflow:hidden; position:relative; width:50px; height:50px
            y position:absolute; left:60px; width:10px; height:10px
        ",
    );
    let mut boxes = BoxTree::build(&mut doc);
    let (fragments, _) = run(&mut boxes);
    let parents = parents(&fragments, &labels);
    assert_eq!(parents["x"].as_deref(), Some("r"), "hoisted past `o`");
    assert_eq!(parents["y"].as_deref(), Some("q"));
    let root = &fragments.root.as_ref().unwrap().1;
    let r = root.children()[0].1.children()[0].1.as_box().unwrap();
    // `x` reaches 60 + 10 across `r`, inside its 100px.
    assert_eq!(r.size.width.to_f32_px(), 100.0);
    let q = r.children[1].1.as_box().unwrap();
    assert!(q.is_scroll_container);
    assert_eq!(
        q.scrollable_overflow.size.width.to_f32_px(),
        70.0,
        "`y` adds to its containing block's scrollable overflow"
    );
}

#[test]
fn positioned_trees_lay_out_once_and_dump_their_positioning() {
    let Built { mut doc, .. } = build(
        "
        s position:relative; width:40px; height:24px
          k position:absolute; top:2px; left:2px; width:18px; height:18px
          p position:absolute; left:0; right:0
            e height:5px
        ",
    );
    let mut boxes = BoxTree::build(&mut doc);
    assert_eq!(
        boxes.dump(&doc),
        "block html\n  block html>body\n    block html>body>div relative\n      \
         block html>body>div>div[1] absolute\n      \
         block html>body>div>div[2] absolute\n        block html>body>div>div[2]>div\n"
    );
    let (first, stats) = run(&mut boxes);
    assert_eq!(stats.laid_out, 6);
    let (second, stats) = run(&mut boxes);
    assert_eq!(stats.laid_out, 0, "every absolute box is cached too");
    assert!(Rc::ptr_eq(
        &first.root.as_ref().unwrap().1,
        &second.root.as_ref().unwrap().1
    ));
}

#[test]
fn absolute_fragments_follow_the_in_flow_ones_in_the_dump() {
    // Spec 0006, Layout rules worth pinning: Hoisting.
    let Built { mut doc, .. } = build(
        "
        s position:relative; box-sizing:border-box; width:40px; height:24px; border:1px solid black
          k position:absolute; top:2px; left:auto; right:2px; width:18px; height:18px
          a height:4px
        ",
    );
    let mut boxes = BoxTree::build(&mut doc);
    let (fragments, _) = run(&mut boxes);
    assert_eq!(
        fragments.dump(&doc),
        "fragments viewport=800x600\n  box html at=0,0 size=800x24\n    \
         box html>body at=0,0 size=800x24\n      box html>body>div at=0,0 size=40x24\n        \
         box html>body>div>div[2] at=1,1 size=38x4\n        \
         box html>body>div>div[1] at=19,3 size=18x18\n"
    );
}
