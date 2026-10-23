//! Spec 0006, AC-4 and AC-9: a long `nowrap` label in a flex row shrinks
//! and ends in an ellipsis only when it clips its overflow.
//!
//! These cases shape real text, so on a machine without system fonts they
//! have nothing to measure and return early.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

mod support;

use std::rc::Rc;

use base::Point;
use layout::{BoxFragment, BoxTree, Fragment};
use support::{Built, build, run};

const TITLE: &str = "Selected tab with a very long title that truncates";

/// The label's box and every text run on its lines, with each run's left
/// edge from the label's content box.
struct Label {
    width: f32,
    runs: Vec<(f32, f32, String)>,
}

fn lay_out_label(label_style: &str) -> Label {
    let Built { mut doc, labels } = build(&format!(
        "
        c display:flex; width:200px
          l {label_style}
            t '{TITLE}'
        "
    ));
    let mut boxes = BoxTree::build(&mut doc);
    let (fragments, _) = run(&mut boxes);
    let mut stack: Vec<Rc<Fragment>> = fragments.root.iter().map(|(_, f)| Rc::clone(f)).collect();
    let mut label: Option<Rc<Fragment>> = None;
    while let Some(fragment) = stack.pop() {
        let is_label = fragment
            .as_box()
            .and_then(|b| b.node)
            .is_some_and(|node| labels.get(&node).is_some_and(|l| l == "l"));
        if is_label {
            label = Some(fragment);
            break;
        }
        stack.extend(fragment.children().iter().map(|(_, c)| Rc::clone(c)));
    }
    let label = label.unwrap();
    let label: &BoxFragment = label.as_box().unwrap();
    let mut runs = Vec::new();
    for (line_at, line) in &label.children {
        for (at, child) in line.children() {
            if let Fragment::Text(text) = &**child {
                let at: Point = line_at.offset_by(*at);
                let x = at.x.to_f32_px();
                runs.push((x, x + text.size.width.to_f32_px(), text.text.clone()));
            }
        }
    }
    Label {
        width: label.size.width.to_f32_px(),
        runs,
    }
}

fn has_fonts(label: &Label) -> bool {
    label.runs.iter().any(|(start, end, _)| end > start)
}

#[test]
fn a_clipping_label_shrinks_and_ends_in_an_ellipsis() {
    let label = lay_out_label("overflow:hidden; white-space:nowrap; text-overflow:ellipsis");
    if !has_fonts(&label) {
        return;
    }
    assert_eq!(label.width, 200.0, "a scroll container's minimum is zero");
    let (_, last_end, last) = label.runs.last().unwrap();
    assert!(
        last == "\u{2026}" || last == "...",
        "the line ends in an ellipsis, not {last:?}"
    );
    assert!(*last_end <= 200.0, "the ellipsis fits: {last_end}");
    let kept: String = label.runs[..label.runs.len() - 1]
        .iter()
        .map(|(_, _, text)| text.as_str())
        .collect();
    assert!(!kept.is_empty() && kept.len() < TITLE.len());
    assert!(
        TITLE.starts_with(&kept),
        "{kept:?} is the start of the title"
    );
}

#[test]
fn a_label_that_does_not_clip_pushes_the_row_wider() {
    let label = lay_out_label("white-space:nowrap; text-overflow:ellipsis");
    if !has_fonts(&label) {
        return;
    }
    assert!(label.width > 200.0, "its minimum is the whole title");
    let text: String = label.runs.iter().map(|(_, _, t)| t.as_str()).collect();
    assert_eq!(text, TITLE, "no ellipsis without clipping");
}

#[test]
fn clip_keeps_the_whole_line() {
    let label = lay_out_label("overflow:hidden; white-space:nowrap");
    if !has_fonts(&label) {
        return;
    }
    assert_eq!(label.width, 200.0);
    let text: String = label.runs.iter().map(|(_, _, t)| t.as_str()).collect();
    assert_eq!(text, TITLE, "clipped by paint, not cut");
}

#[test]
fn lines_that_fit_are_untouched() {
    let Built { mut doc, .. } = build(
        "
        l width:400px; overflow:hidden; white-space:nowrap; text-overflow:ellipsis
          t 'short'
        ",
    );
    let mut boxes = BoxTree::build(&mut doc);
    let (fragments, _) = run(&mut boxes);
    let dump = fragments.dump(&doc);
    assert!(
        !dump.contains('\u{2026}') && !dump.contains("\"...\""),
        "{dump}"
    );
}
