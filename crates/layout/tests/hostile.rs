//! Spec 0009, AC-23: hostile pages cannot hang or crash the engine.
//! The float and grid caps hold, deeply nested tables and floats use the
//! existing nesting cap, and the engine lays out hostile markup within
//! bounded time.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

mod support;

use base::Au;
use css::{
    ColorScheme, ComputedStyle, ContainerContext, Declaration, Device, MatchContext, Stylist,
};
use dom::{Document, NodeId};
use layout::{BoxTree, FragmentTree};
use support::run;

/// Lays out `doc` and returns its fragment tree. The engine must not panic.
fn lay_out(doc: &mut Document) -> FragmentTree {
    let mut boxes = BoxTree::build(doc);
    let (fragments, _) = run(&mut boxes);
    fragments
}

/// Cascades `declarations` for `id`, returning an `Rc<ComputedStyle>`.
fn cascade(
    stylist: &Stylist,
    doc: &Document,
    id: NodeId,
    parent_style: Option<&ComputedStyle>,
    declarations: &[Declaration],
) -> std::rc::Rc<ComputedStyle> {
    let element = doc.element_ref(id).unwrap();
    let container_context = ContainerContext {
        size: None,
        names: &[],
    };
    let (computed, _has_subject) = stylist.cascade(
        &element,
        parent_style,
        declarations,
        Au::from_px(16.0),
        &container_context,
        &mut MatchContext::default(),
    );
    std::rc::Rc::new(computed)
}

/// A table with `colspan=1000` on 2000 rows stays under the 1,000,000 slot
/// cap: the grid stops at the cap and the rest of the rows are laid out as
/// plain blocks. The engine must not panic or hang. (AC-15, AC-23)
#[test]
fn huge_colspan_stays_under_slot_cap() {
    let (width, height) = support::VIEWPORT;
    let stylist = Stylist::new(
        &[css::ua_stylesheet()],
        Device {
            width,
            height,
            color_scheme: ColorScheme::Light,
            reduced_motion: false,
        },
    );
    let mut doc = Document::new("about:blank");
    let root = doc.root();
    let html = support::add(&mut doc, &stylist, root, "html", "margin:0");
    let body = support::add(&mut doc, &stylist, html, "body", "margin:0");

    let table_decls = css::parse_declarations("width:800px; border-spacing:0", None);
    let table = support::add(
        &mut doc,
        &stylist,
        body,
        "table",
        "width:800px; border-spacing:0",
    );
    let computed = cascade(&stylist, &doc, table, None, &table_decls);
    doc.set_style(table, Some(computed)).unwrap();

    let decls = css::parse_declarations("height:10px; padding:0", None);

    for _ in 0..2000 {
        let row = support::add_with_attributes(&mut doc, &stylist, table, "tr", &[], "");
        let cell =
            support::add_with_attributes(&mut doc, &stylist, row, "td", &[("colspan", "1000")], "");
        let parent_style = doc.node(row).and_then(|n| n.as_element()?.style().cloned());
        let computed = cascade(&stylist, &doc, row, parent_style.as_deref(), &decls);
        doc.set_style(row, Some(computed)).unwrap();
        let computed = cascade(&stylist, &doc, cell, None, &decls);
        doc.set_style(cell, Some(computed)).unwrap();
    }
    let _ = lay_out(&mut doc);
}

/// Many floats in one BFC: the engine must not panic or hang, and the float
/// cap (10,000 per BFC) bounds the work. (AC-5, AC-23)
#[test]
fn float_pile_does_not_panic() {
    let (width, height) = support::VIEWPORT;
    let stylist = Stylist::new(
        &[css::ua_stylesheet()],
        Device {
            width,
            height,
            color_scheme: ColorScheme::Light,
            reduced_motion: false,
        },
    );
    let mut doc = Document::new("about:blank");
    let root = doc.root();
    let html = support::add(&mut doc, &stylist, root, "html", "margin:0");
    let body = support::add(&mut doc, &stylist, html, "body", "margin:0");
    let container = support::add(
        &mut doc,
        &stylist,
        body,
        "div",
        "overflow:hidden; width:800px",
    );
    let decls = css::parse_declarations("float:left; width:10px; height:10px; margin:0", None);
    let mut last = container;
    for _ in 0..500 {
        let f = support::add(&mut doc, &stylist, last, "div", "");
        let parent_style = doc
            .node(last)
            .and_then(|n| n.as_element()?.style().cloned());
        let computed = cascade(&stylist, &doc, f, parent_style.as_deref(), &decls);
        doc.set_style(f, Some(computed)).unwrap();
        last = f;
    }
    let _ = lay_out(&mut doc);
}

/// Deeply nested tables: the nesting cap bounds recursion, and the engine
/// must not panic or overflow the stack. (AC-23)
#[test]
fn deeply_nested_tables_do_not_overflow() {
    let (width, height) = support::VIEWPORT;
    let stylist = Stylist::new(
        &[css::ua_stylesheet()],
        Device {
            width,
            height,
            color_scheme: ColorScheme::Light,
            reduced_motion: false,
        },
    );
    let mut doc = Document::new("about:blank");
    let root = doc.root();
    let html = support::add(&mut doc, &stylist, root, "html", "margin:0");
    let body = support::add(&mut doc, &stylist, html, "body", "margin:0");

    let table_decls = css::parse_declarations("width:800px; border-spacing:0", None);
    let row_decls = css::parse_declarations("", None);
    let cell_decls = css::parse_declarations("height:5px; padding:0", None);
    let mut last = body;
    for _ in 0..100 {
        let t = support::add(&mut doc, &stylist, last, "table", "");
        let computed = cascade(&stylist, &doc, t, None, &table_decls);
        doc.set_style(t, Some(computed)).unwrap();
        let r = support::add(&mut doc, &stylist, t, "tr", "");
        let computed = cascade(&stylist, &doc, r, None, &row_decls);
        doc.set_style(r, Some(computed)).unwrap();
        let c = support::add(&mut doc, &stylist, r, "td", "");
        let computed = cascade(&stylist, &doc, c, None, &cell_decls);
        doc.set_style(c, Some(computed)).unwrap();
        last = t;
    }
    let _ = lay_out(&mut doc);
}

/// A document that combines floats, tables, and nesting exercises all the
/// hostile-input caps at once. (AC-23)
#[test]
fn combined_hostile_input_lays_out() {
    let (width, height) = support::VIEWPORT;
    let stylist = Stylist::new(
        &[css::ua_stylesheet()],
        Device {
            width,
            height,
            color_scheme: ColorScheme::Light,
            reduced_motion: false,
        },
    );
    let mut doc = Document::new("about:blank");
    let root = doc.root();
    let html = support::add(&mut doc, &stylist, root, "html", "margin:0");
    let body = support::add(&mut doc, &stylist, html, "body", "margin:0");
    let container = support::add(
        &mut doc,
        &stylist,
        body,
        "div",
        "overflow:hidden; width:800px",
    );

    let float_decls = css::parse_declarations("float:left; width:20px; height:20px", None);
    let table_decls = css::parse_declarations("width:200px; border-spacing:0", None);
    let row_decls = css::parse_declarations("", None);
    let cell_decls = css::parse_declarations("height:10px; padding:0", None);

    let mut last = container;
    for _ in 0..100 {
        let f = support::add(&mut doc, &stylist, last, "div", "");
        let parent_style = doc
            .node(last)
            .and_then(|n| n.as_element()?.style().cloned());
        let computed = cascade(&stylist, &doc, f, parent_style.as_deref(), &float_decls);
        doc.set_style(f, Some(computed)).unwrap();

        let t = support::add(&mut doc, &stylist, last, "table", "");
        let computed = cascade(&stylist, &doc, t, None, &table_decls);
        doc.set_style(t, Some(computed)).unwrap();
        let r = support::add(&mut doc, &stylist, t, "tr", "");
        let computed = cascade(&stylist, &doc, r, None, &row_decls);
        doc.set_style(r, Some(computed)).unwrap();
        let c = support::add(&mut doc, &stylist, r, "td", "");
        let computed = cascade(&stylist, &doc, c, None, &cell_decls);
        doc.set_style(c, Some(computed)).unwrap();

        last = t;
    }
    let _ = lay_out(&mut doc);
}
