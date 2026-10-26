//! Spec 0009, child 0004, AC-15 and AC-16: table layout cases, checked
//! against the boxes Chrome 155 gives for the same markup, to 1 px.
//!
//! A case is a tree written one per line: a label, then the element tag
//! after a `:`, then its `style` attribute, nested by two spaces. A line
//! whose style is in single quotes adds that text. A line without a tag is
//! a `div`. Every case runs inside an `html` and `body` with no margins at
//! an 800 by 600 viewport, the way the expected values were taken.
//!
//! Expected rects are the label, then x, y, width and height in CSS px
//! from the case's outermost box.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

mod support;

use std::collections::HashMap;

use css::{Device, Stylist};
use dom::{Document, NodeId};
use layout::{BoxTree, FragmentTree};
use support::{VIEWPORT, add, add_with_attributes, rects, run};

/// A built case: its document and each labeled element's node.
struct Built {
    doc: Document,
    labels: HashMap<NodeId, String>,
}

/// Builds the case `tree` with real table elements.
fn build(tree: &str) -> Built {
    let (width, height) = VIEWPORT;
    let stylist = Stylist::new(&[css::ua_stylesheet()], Device { width, height });
    let mut doc = Document::new("about:blank");
    let root = doc.root();
    let html = add(&mut doc, &stylist, root, "html", "margin:0");
    let body = add(&mut doc, &stylist, html, "body", "margin:0");

    let lines: Vec<&str> = tree.lines().filter(|l| !l.trim().is_empty()).collect();
    let base_indent = lines[0].len() - lines[0].trim_start().len();
    let mut labels: HashMap<NodeId, String> = HashMap::new();
    let mut open: Vec<NodeId> = vec![body];
    for line in lines {
        let depth = (line.len() - line.trim_start().len() - base_indent) / 2;
        open.truncate(depth + 1);
        let parent = *open.last().expect("the body is always open");
        let (head, style) = line.trim().split_once(' ').unwrap_or((line.trim(), ""));
        if let Some(text) = style.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
            let id = doc.create_text(text);
            doc.append_child(parent, id).unwrap();
            continue;
        }
        let (label, rest) = head.split_once(':').unwrap_or((head, "div"));
        let (tag, attrs) = match rest.split_once('[') {
            Some((tag, tail)) => {
                let inner = tail.strip_suffix(']').unwrap_or(tail);
                let attrs: Vec<(&str, &str)> = inner
                    .split(';')
                    .filter(|pair| !pair.is_empty())
                    .map(|pair| pair.split_once('=').unwrap_or((pair, "")))
                    .collect();
                (tag, attrs)
            }
            None => (rest, Vec::new()),
        };
        let id = add_with_attributes(&mut doc, &stylist, parent, tag, &attrs, style);
        labels.insert(id, label.to_owned());
        open.push(id);
    }
    Built { doc, labels }
}

/// Lays out `tree` and checks every box against `expected`, to 1 px.
fn check(tree: &str, expected: &[(&str, f32, f32, f32, f32)]) {
    let Built { mut doc, labels } = build(tree);
    let mut boxes = BoxTree::build(&mut doc);
    let (fragments, _) = run(&mut boxes);
    let found = rects(&fragments, &labels);
    let mut wrong = Vec::new();
    for &(label, x, y, width, height) in expected {
        let want = (x, y, width, height);
        let Some(&got) = found.get(label) else {
            wrong.push(format!("{label}: no box, want {want:?}"));
            continue;
        };
        let close = |a: f32, b: f32| (a - b).abs() <= 1.0;
        if !(close(got.0, x) && close(got.1, y) && close(got.2, width) && close(got.3, height)) {
            wrong.push(format!("{label}: got {got:?}, want {want:?}"));
        }
    }
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}

/// Every labeled box's rect from the outermost one's border box corner.
fn lay_out(tree: &str) -> HashMap<String, (f32, f32, f32, f32)> {
    let Built { mut doc, labels } = build(tree);
    let mut boxes = BoxTree::build(&mut doc);
    let (fragments, _): (FragmentTree, _) = run(&mut boxes);
    rects(&fragments, &labels)
}

#[test]
fn colspan_cell_takes_two_columns() {
    // AC-15: a cell with `colspan:2` fills both columns of its row.
    check(
        "
        t:table width:100px; border-spacing:0
          r:tr
            a:td[colspan=2] height:20px; padding:0
          r2:tr
            b:td height:20px; padding:0
            c:td height:20px; padding:0
        ",
        &[
            ("t", 0.0, 0.0, 100.0, 40.0),
            ("a", 0.0, 0.0, 100.0, 20.0),
            ("b", 0.0, 20.0, 50.0, 20.0),
            ("c", 50.0, 20.0, 50.0, 20.0),
        ],
    );
}

#[test]
fn tallest_cell_sets_the_row_height() {
    // AC-16: a row's height is the tallest single row cell, and the shorter
    // cell stretches to it.
    check(
        "
        t:table width:100px; border-spacing:0
          r:tr
            a:td height:20px; padding:0
            b:td height:40px; padding:0
          r2:tr
            c:td height:10px; padding:0
        ",
        &[
            ("t", 0.0, 0.0, 100.0, 50.0),
            ("a", 0.0, 0.0, 50.0, 40.0),
            ("b", 50.0, 0.0, 50.0, 40.0),
            ("c", 0.0, 40.0, 50.0, 10.0),
        ],
    );
}

#[test]
fn rowspan_cell_fills_the_rows_it_spans() {
    // AC-16: a `rowspan:2` cell 60 px tall spreads its height over its two
    // rows, so both rows grow and the cell fills both.
    check(
        "
        t:table width:100px; border-spacing:0
          r:tr
            a:td[rowspan=2] height:60px; padding:0
            b:td height:10px; padding:0
          r2:tr
            c:td height:10px; padding:0
        ",
        &[
            ("t", 0.0, 0.0, 100.0, 60.0),
            ("a", 0.0, 0.0, 50.0, 60.0),
            ("b", 50.0, 0.0, 50.0, 30.0),
            ("c", 50.0, 30.0, 50.0, 30.0),
        ],
    );
}

#[test]
fn middle_alignment_centres_the_cell_content() {
    // AC-16: `vertical-align: middle` centres a short cell's content in the
    // taller row.
    let found = lay_out(
        "
        t:table width:100px; border-spacing:0
          r:tr
            a:td height:10px; vertical-align:middle; padding:0
              i:div height:4px
            b:td height:40px; padding:0
        ",
    );
    let a = found["a"];
    let i = found["i"];
    assert!((a.3 - 40.0).abs() <= 1.0, "the cell stretches to the row");
    assert!((i.1 - a.1 - 15.0).abs() <= 1.0, "content sits 15 px down");
}

#[test]
fn bottom_alignment_lowers_the_cell_content() {
    // AC-16: `vertical-align: bottom` pushes a short cell's content down.
    let found = lay_out(
        "
        t:table width:100px; border-spacing:0
          r:tr
            a:td height:10px; vertical-align:bottom; padding:0
              i:div height:4px
            b:td height:40px; padding:0
        ",
    );
    let a = found["a"];
    let i = found["i"];
    assert!((i.1 - a.1 - 30.0).abs() <= 1.0, "content sits 30 px down");
}

#[test]
fn border_spacing_sits_around_and_between_cells() {
    // AC-17: separate borders put `border-spacing` around and between the
    // grid, and the columns share the width left over.
    check(
        "
        t:table width:120px; border-spacing:10px
          r:tr
            a:td padding:0; height:10px
            b:td padding:0; height:10px
        ",
        &[
            ("t", 0.0, 0.0, 120.0, 30.0),
            ("a", 10.0, 10.0, 45.0, 10.0),
            ("b", 65.0, 10.0, 45.0, 10.0),
        ],
    );
}

#[test]
fn collapsed_borders_have_no_spacing() {
    // AC-17: `border-collapse: collapse` ignores `border-spacing`.
    check(
        "
        t:table width:100px; border-spacing:10px; border-collapse:collapse
          r:tr
            a:td padding:0; height:10px
            b:td padding:0; height:10px
        ",
        &[
            ("t", 0.0, 0.0, 100.0, 10.0),
            ("a", 0.0, 0.0, 50.0, 10.0),
            ("b", 50.0, 0.0, 50.0, 10.0),
        ],
    );
}

#[test]
fn collapsed_row_takes_no_height_and_paints_nothing() {
    // AC-18: `visibility: collapse` on a row removes it from the table's
    // height and painting, and the rows below move up.
    let found = lay_out(
        "
        t:table width:100px; border-spacing:0
          r:tr
            a:td height:10px; padding:0
          r2:tr visibility:collapse
            b:td height:20px; padding:0
          r3:tr
            c:td height:10px; padding:0
        ",
    );
    let t = found["t"];
    let a = found["a"];
    let c = found["c"];
    assert!(
        (t.3 - 20.0).abs() <= 1.0,
        "the collapsed row adds no height"
    );
    assert!((a.1 - 0.0).abs() <= 1.0);
    assert!((c.1 - 10.0).abs() <= 1.0, "the row below moves up");
    assert!(!found.contains_key("b"), "the collapsed row paints nothing");
}

#[test]
fn collapsed_column_gives_up_its_width() {
    // AC-18: `visibility: collapse` on a `<col>` removes the column's width
    // and painting, and the other columns keep the sizes computed first.
    let found = lay_out(
        "
        t:table width:200px; border-spacing:0
          g:colgroup
            x:col
            y:col visibility:collapse
          r:tr
            a:td height:10px; padding:0
            b:td height:10px; padding:0
        ",
    );
    let t = found["t"];
    let a = found["a"];
    assert!((t.2 - 100.0).abs() <= 1.0, "the table loses the column");
    assert!(
        (a.2 - 100.0).abs() <= 1.0,
        "the first column keeps its size"
    );
    assert!(
        !found.contains_key("b"),
        "the collapsed column paints nothing"
    );
}

#[test]
fn col_width_sizes_a_fixed_layout_column() {
    // AC-16: `table-layout: fixed` sizes a column from its `<col>` width and
    // shares the rest equally.
    let found = lay_out(
        "
        t:table width:200px; border-spacing:0; table-layout:fixed
          g:colgroup
            x:col width:60px
            y:col
          r:tr
            a:td height:10px; padding:0
            b:td height:10px; padding:0
        ",
    );
    let a = found["a"];
    let b = found["b"];
    assert!((a.2 - 60.0).abs() <= 1.0, "the col width wins");
    assert!((b.1 - 0.0).abs() <= 1.0);
    assert!((b.0 - 60.0).abs() <= 1.0, "the second column follows");
    assert!((b.2 - 140.0).abs() <= 1.0, "the rest is shared");
}

#[test]
fn rowspan_zero_reaches_the_end_of_its_row_group() {
    // AC-15: a `rowspan="0"` cell spans to the end of its own row group and
    // no further, so the next group starts below it.
    let found = lay_out(
        "
        t:table width:100px; border-spacing:0
          g:tbody
            r1:tr
              a:td[rowspan=0] height:40px; padding:0
            r2:tr
              b:td height:10px; padding:0
          g2:tbody
            r3:tr
              d:td height:10px; padding:0
        ",
    );
    let a = found["a"];
    let b = found["b"];
    let d = found["d"];
    assert!(
        (b.0 - 50.0).abs() <= 1.0,
        "the cell spans both group rows, pushing b to the second column: b={b:?}"
    );
    assert!(
        (d.1 - (a.1 + a.3)).abs() <= 1.0,
        "the next group starts below the span: a={a:?} d={d:?}"
    );
    assert!((d.0 - 0.0).abs() <= 1.0, "and in the first column");
}

#[test]
fn caption_sits_above_the_table_and_shares_its_width() {
    // AC-14: the wrapper holds the caption above the table, and both take the
    // table's width.
    check(
        "
        t:table width:100px; border-spacing:0
          c:caption height:20px
          r:tr
            a:td height:10px; padding:0
        ",
        &[
            ("c", 0.0, 0.0, 100.0, 20.0),
            ("t", 0.0, 20.0, 100.0, 10.0),
            ("a", 0.0, 20.0, 100.0, 10.0),
        ],
    );
}

#[test]
fn caption_side_bottom_puts_the_caption_under_the_table() {
    // AC-14: `caption-side: bottom` moves the caption below the table.
    check(
        "
        t:table width:100px; border-spacing:0; caption-side:bottom
          c:caption height:20px
          r:tr
            a:td height:10px; padding:0
        ",
        &[("t", 0.0, 0.0, 100.0, 10.0), ("c", 0.0, 10.0, 100.0, 20.0)],
    );
}

#[test]
fn inline_table_sits_on_its_line_as_one_piece() {
    // AC-14: two `inline-table` boxes share a line, one after the other.
    let found = lay_out(
        "
        d:div
          t1:table width:60px; display:inline-table; border-spacing:0
            r:tr
              a:td height:10px; padding:0
          t2:table width:60px; display:inline-table; border-spacing:0
            r:tr
              b:td height:10px; padding:0
        ",
    );
    let t1 = found["t1"];
    let t2 = found["t2"];
    let d = found["d"];
    assert!((t1.1 - t2.1).abs() <= 1.0, "both sit on the same line");
    assert!(
        (t2.0 - (t1.0 + 60.0)).abs() <= 1.0,
        "the second follows the first: {t1:?} {t2:?}"
    );
    assert!((t1.2 - 60.0).abs() <= 1.0);
    assert!(d.3 >= 10.0, "the line holds the table: {d:?}");
}

#[test]
fn table_width_is_the_sum_of_its_columns() {
    // AC-16: the table's border box is the sum of its column widths.
    let found = lay_out(
        "
        t:table width:100px; border-spacing:0
          r:tr
            a:td padding:0
            b:td padding:0
        ",
    );
    let t = found["t"];
    let a = found["a"];
    let b = found["b"];
    assert!((t.2 - (a.2 + b.2)).abs() <= 1.0, "columns fill the table");
    assert!((t.2 - 100.0).abs() <= 1.0, "the table takes its width");
}
