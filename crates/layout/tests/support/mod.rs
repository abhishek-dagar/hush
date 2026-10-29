//! Shared helpers for the layout integration tests: a tree of `div`s
//! written one per line, styled through the UA sheet and the cascade, and
//! the border boxes layout gives each labeled one.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

use std::collections::HashMap;
use std::rc::Rc;

use base::atoms::{LocalName, QualName};
use base::{Au, Point, Size};
use css::{ColorScheme, ComputedStyle, ContainerContext, Device, MatchContext, Stylist};
use dom::{Document, NodeId};
use layout::{BoxTree, Fragment, FragmentTree, LayoutStats, layout};
use text::{FontContext, WebFontLayer};

pub const VIEWPORT: (f32, f32) = (800.0, 600.0);

/// Adds a `name` element under `parent`, sets `attrs`, then styles it by
/// `style` as its `style` attribute.
pub fn add_with_attributes(
    doc: &mut Document,
    stylist: &Stylist,
    parent: NodeId,
    name: &str,
    attrs: &[(&str, &str)],
    style: &str,
) -> NodeId {
    let id = doc.create_element(QualName::html(LocalName::from(name)));
    doc.append_child(parent, id).unwrap();
    for &(attr, value) in attrs {
        doc.set_attribute(id, QualName::attribute(LocalName::from(attr)), value)
            .unwrap();
    }
    let parent_style: Option<Rc<ComputedStyle>> = doc
        .node(parent)
        .and_then(|n| n.as_element()?.style().cloned());
    let declarations = css::parse_declarations(style, None);
    let computed = {
        let element = doc.element_ref(id).unwrap();
        let container_context = ContainerContext {
            size: None,
            names: &[],
        };
        let (computed, _has_subject) = stylist.cascade(
            &element,
            parent_style.as_deref(),
            &declarations,
            Au::from_px(16.0),
            &container_context,
            &mut MatchContext::default(),
        );
        computed
    };
    doc.set_style(id, Some(Rc::new(computed))).unwrap();
    id
}

/// Adds a `name` element under `parent`, styled by `style` as its `style`
/// attribute.
pub fn add(
    doc: &mut Document,
    stylist: &Stylist,
    parent: NodeId,
    name: &str,
    style: &str,
) -> NodeId {
    add_with_attributes(doc, stylist, parent, name, &[], style)
}

/// A case's document, built and styled, with each labeled element's node.
pub struct Built {
    pub doc: Document,
    pub labels: HashMap<NodeId, String>,
}

/// Builds the case `tree`. A line whose style is in single quotes adds
/// that text to its parent instead of an element.
pub fn build(tree: &str) -> Built {
    let (width, height) = VIEWPORT;
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
    let html = add(&mut doc, &stylist, root, "html", "margin:0");
    let body = add(&mut doc, &stylist, html, "body", "margin:0");

    let lines: Vec<&str> = tree.lines().filter(|l| !l.trim().is_empty()).collect();
    let base_indent = lines[0].len() - lines[0].trim_start().len();
    let mut labels: HashMap<NodeId, String> = HashMap::new();
    let mut open: Vec<NodeId> = vec![body];
    for line in lines {
        let depth = (line.len() - line.trim_start().len() - base_indent) / 2;
        open.truncate(depth + 1);
        let parent = *open.last().unwrap();
        let (label, style) = line.trim().split_once(' ').unwrap_or((line.trim(), ""));
        if let Some(text) = style.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
            let id = doc.create_text(text);
            doc.append_child(parent, id).unwrap();
            continue;
        }
        let id = add(&mut doc, &stylist, parent, "div", style);
        labels.insert(id, label.to_owned());
        open.push(id);
    }
    Built { doc, labels }
}

/// Lays out `boxes` at the test viewport.
pub fn run(boxes: &mut BoxTree) -> (FragmentTree, LayoutStats) {
    let (width, height) = VIEWPORT;
    let viewport = Size::new(Au::from_px(width), Au::from_px(height));
    layout(
        boxes,
        viewport,
        &mut FontContext::new(),
        &WebFontLayer::default(),
    )
}

/// Builds the case `tree` and lays it out. Returns every labeled box's
/// rect from the outermost one's border box corner.
pub fn lay_out(tree: &str) -> HashMap<String, (f32, f32, f32, f32)> {
    let Built { mut doc, labels } = build(tree);
    let mut boxes = BoxTree::build(&mut doc);
    rects(&run(&mut boxes).0, &labels)
}

/// Every labeled box's rect from the outermost one's border box corner.
pub fn rects(
    fragments: &FragmentTree,
    labels: &HashMap<NodeId, String>,
) -> HashMap<String, (f32, f32, f32, f32)> {
    let mut found = HashMap::new();
    let mut origin = None;
    let mut stack: Vec<(Point, Rc<Fragment>)> = fragments.root.iter().cloned().collect();
    while let Some((at, fragment)) = stack.pop() {
        if let Some(label) = fragment
            .as_box()
            .and_then(|b| b.node)
            .and_then(|node| labels.get(&node))
        {
            let start: Point = *origin.get_or_insert(at);
            let size = fragment.size();
            found.insert(
                label.clone(),
                (
                    (at.x - start.x).to_f32_px(),
                    (at.y - start.y).to_f32_px(),
                    size.width.to_f32_px(),
                    size.height.to_f32_px(),
                ),
            );
        }
        for (offset, child) in fragment.children().iter().rev() {
            stack.push((at.offset_by(*offset), Rc::clone(child)));
        }
    }
    found
}

/// Lays out `tree` and checks every box against `expected`, to 1 px.
pub fn check(tree: &str, expected: &[(&str, f32, f32, f32, f32)]) {
    let found = lay_out(tree);
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
