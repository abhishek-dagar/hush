use std::fmt::Write as _;
use std::mem::size_of;
use std::rc::Rc;

use base::atoms::{Atom, LocalName, QualName};
use base::local_name;
use css::{ColorScheme, ComputedStyle};

use super::*;

fn element(doc: &mut Document, name: LocalName) -> NodeId {
    doc.create_element(QualName::html(name))
}

fn attr(name: LocalName) -> QualName {
    QualName::attribute(name)
}

/// `<html><body>` in a new document.
fn page() -> (Document, NodeId, NodeId) {
    let mut doc = Document::new("about:blank");
    let html = element(&mut doc, local_name!("html"));
    let body = element(&mut doc, local_name!("body"));
    doc.append_child(doc.root(), html).unwrap();
    doc.append_child(html, body).unwrap();
    (doc, html, body)
}

/// The tree under `id` as text, to check a failed call changed nothing.
fn shape(doc: &Document, id: NodeId) -> String {
    let mut out = String::new();
    let mut stack = vec![(id, 0)];
    while let Some((id, depth)) = stack.pop() {
        let node = doc.node(id).unwrap();
        let label = match node.data() {
            NodeData::Element(e) => e.name().local.to_string(),
            NodeData::Text(t) => format!("#text {t}"),
            other => format!("{other:?}"),
        };
        let _ = writeln!(out, "{:depth$}{label}", "");
        let mut children: Vec<_> = doc.children(id).map(|c| (c, depth + 1)).collect();
        children.reverse();
        stack.extend(children);
    }
    out
}

/// Every element the style pass visits, with whether it restyles.
fn style_visits(doc: &mut Document) -> Vec<(NodeId, bool)> {
    let mut visits = Vec::new();
    doc.restyle_dirty(|_, id, restyle| {
        visits.push((id, restyle));
        false
    });
    visits
}

#[test]
fn nodes_stay_small() {
    // AC-18: 50,000 nodes must fit in about 5 MB.
    assert!(
        size_of::<Node>() <= 96,
        "Node is {} bytes",
        size_of::<Node>()
    );
    assert_eq!(size_of::<Option<NodeId>>(), 8);
}

#[test]
fn append_links_children_in_order() {
    let mut doc = Document::new("about:blank");
    let html = element(&mut doc, local_name!("html"));
    let a = element(&mut doc, local_name!("div"));
    let b = doc.create_text("hi");
    doc.append_child(doc.root(), html).unwrap();
    doc.append_child(html, a).unwrap();
    doc.append_child(html, b).unwrap();

    assert_eq!(doc.flat_children(html).collect::<Vec<_>>(), [a, b]);
    let html_node = doc.node(html).unwrap();
    assert_eq!(
        (html_node.first_child(), html_node.last_child()),
        (Some(a), Some(b))
    );
    assert_eq!(doc.node(b).unwrap().prev_sibling(), Some(a));
    assert_eq!(doc.node(a).unwrap().next_sibling(), Some(b));
    assert_eq!(doc.document_element(), Some(html));
}

#[test]
fn insert_before_replace_and_remove_keep_the_order() {
    let (mut doc, _, body) = page();
    let [a, b, c, d] = [0; 4].map(|_| element(&mut doc, local_name!("p")));
    doc.append_child(body, a).unwrap();
    doc.append_child(body, c).unwrap();
    doc.insert_before(body, b, Some(c)).unwrap();
    assert_eq!(doc.children(body).collect::<Vec<_>>(), [a, b, c]);

    doc.insert_before(body, a, Some(a)).unwrap();
    assert_eq!(
        doc.children(body).collect::<Vec<_>>(),
        [a, b, c],
        "before itself: no move"
    );

    doc.replace_child(body, d, b).unwrap();
    assert_eq!(doc.children(body).collect::<Vec<_>>(), [a, d, c]);
    assert_eq!(doc.node(b).unwrap().parent(), None);
    assert!(!doc.is_connected(b));

    doc.remove_child(body, a).unwrap();
    assert_eq!(doc.children(body).collect::<Vec<_>>(), [d, c]);
    assert_eq!(doc.node(d).unwrap().prev_sibling(), None);
}

#[test]
fn appending_moves_a_node_out_of_its_old_parent() {
    let (mut doc, _, body) = page();
    let (a, b, c) = (
        element(&mut doc, local_name!("div")),
        element(&mut doc, local_name!("div")),
        element(&mut doc, local_name!("p")),
    );
    for id in [a, b] {
        doc.append_child(body, id).unwrap();
    }
    doc.append_child(a, c).unwrap();
    doc.append_child(b, c).unwrap();

    assert_eq!(doc.children(a).count(), 0);
    assert_eq!(doc.node(a).unwrap().last_child(), None);
    assert_eq!(doc.children(b).collect::<Vec<_>>(), [c]);
    assert_eq!(doc.node(c).unwrap().parent(), Some(b));
}

#[test]
fn invalid_mutations_are_rejected_and_change_nothing() {
    // AC-2
    let (mut doc, html, body) = page();
    let div = element(&mut doc, local_name!("div"));
    let text = doc.create_text("x");
    let stray = element(&mut doc, local_name!("p"));
    doc.append_child(body, div).unwrap();
    let before = shape(&doc, doc.root());
    let records = doc.mutation_records().len();

    let second_html = element(&mut doc, local_name!("html"));
    let doctype = doc.create_doctype(DoctypeData {
        name: "html".into(),
        public_id: String::new(),
        system_id: String::new(),
    });
    let fragment = doc.create_document_fragment();
    let (one, two) = (
        element(&mut doc, local_name!("div")),
        element(&mut doc, local_name!("div")),
    );
    doc.append_child(fragment, one).unwrap();
    doc.append_child(fragment, two).unwrap();
    let records = records + doc.mutation_records().len() - records;

    let root = doc.root();
    let cases = [
        (
            doc.append_child(div, html),
            DomError::HierarchyRequest,
            "into own descendant",
        ),
        (
            doc.append_child(div, div),
            DomError::HierarchyRequest,
            "into itself",
        ),
        (
            doc.append_child(root, second_html),
            DomError::HierarchyRequest,
            "second root element",
        ),
        (
            doc.append_child(root, html),
            DomError::HierarchyRequest,
            "root element again",
        ),
        (
            doc.append_child(root, text),
            DomError::HierarchyRequest,
            "text in the document",
        ),
        (
            doc.append_child(root, doctype),
            DomError::HierarchyRequest,
            "doctype after the element",
        ),
        (
            doc.append_child(body, doctype),
            DomError::HierarchyRequest,
            "doctype in an element",
        ),
        (
            doc.append_child(root, fragment),
            DomError::HierarchyRequest,
            "two elements via a fragment",
        ),
        (
            doc.append_child(text, div),
            DomError::HierarchyRequest,
            "into a text node",
        ),
        (
            doc.append_child(div, root),
            DomError::HierarchyRequest,
            "the document itself",
        ),
        (
            doc.remove_child(body, stray),
            DomError::NotFound,
            "remove from a non parent",
        ),
        (
            doc.remove_child(html, div),
            DomError::NotFound,
            "remove from a grandparent",
        ),
        (
            doc.insert_before(body, stray, Some(html)),
            DomError::NotFound,
            "reference not a child",
        ),
        (
            doc.replace_child(body, stray, html),
            DomError::NotFound,
            "replace a non child",
        ),
        (
            doc.set_text(div, "x"),
            DomError::InvalidState,
            "text on an element",
        ),
    ];
    for (result, error, case) in cases {
        assert_eq!(result, Err(error), "{case}");
    }
    assert_eq!(shape(&doc, doc.root()), before, "tree untouched");
    assert_eq!(doc.mutation_records().len(), records, "nothing queued");
    assert_eq!(doc.children(fragment).count(), 2, "fragment untouched");
}

#[test]
fn a_doctype_goes_before_the_element_only() {
    let mut doc = Document::new("about:blank");
    let doctype = doc.create_doctype(DoctypeData {
        name: "html".into(),
        public_id: String::new(),
        system_id: String::new(),
    });
    let html = element(&mut doc, local_name!("html"));
    doc.append_child(doc.root(), html).unwrap();
    doc.insert_before(doc.root(), doctype, Some(html)).unwrap();
    assert_eq!(
        doc.children(doc.root()).collect::<Vec<_>>(),
        [doctype, html]
    );

    let other = element(&mut doc, local_name!("html"));
    assert_eq!(
        doc.insert_before(doc.root(), other, Some(doctype)),
        Err(DomError::HierarchyRequest),
        "an element before the doctype"
    );
    doc.replace_child(doc.root(), other, html).unwrap();
    assert_eq!(
        doc.document_element(),
        Some(other),
        "replacing the one element is fine"
    );
}

#[test]
fn a_fragment_moves_its_children_and_stays_empty() {
    let (mut doc, _, body) = page();
    let fragment = doc.create_document_fragment();
    let a = element(&mut doc, local_name!("p"));
    let b = doc.create_text("b");
    doc.append_child(fragment, a).unwrap();
    doc.append_child(fragment, b).unwrap();

    doc.append_child(body, fragment).unwrap();
    assert_eq!(doc.children(body).collect::<Vec<_>>(), [a, b]);
    assert_eq!(doc.children(fragment).count(), 0);
    assert!(doc.is_connected(a) && doc.is_connected(b));
    assert!(!doc.is_connected(fragment));
}

#[test]
fn connectedness_and_the_id_index_follow_the_tree() {
    let (mut doc, _, body) = page();
    let div = element(&mut doc, local_name!("div"));
    let main = Atom::from("main");
    doc.set_attribute(div, attr(local_name!("id")), "main")
        .unwrap();
    assert_eq!(
        doc.element_by_id(&main),
        None,
        "detached elements are not indexed"
    );

    doc.append_child(body, div).unwrap();
    assert!(doc.is_connected(div));
    assert_eq!(doc.element_by_id(&main), Some(div));

    doc.set_attribute(div, attr(local_name!("id")), "content")
        .unwrap();
    assert_eq!(doc.element_by_id(&main), None);
    assert_eq!(doc.element_by_id(&Atom::from("content")), Some(div));

    // Two elements with one id: the first in tree order wins.
    let first = element(&mut doc, local_name!("p"));
    doc.set_attribute(first, attr(local_name!("id")), "content")
        .unwrap();
    doc.insert_before(body, first, Some(div)).unwrap();
    assert_eq!(doc.element_by_id(&Atom::from("content")), Some(first));

    doc.remove_child(body, first).unwrap();
    doc.remove_child(body, div).unwrap();
    assert!(!doc.is_connected(div));
    assert_eq!(doc.element_by_id(&Atom::from("content")), None);
}

#[test]
fn class_and_attribute_caches_follow_the_attributes() {
    let mut doc = Document::new("about:blank");
    let div = element(&mut doc, local_name!("div"));
    doc.set_attribute(div, attr(local_name!("class")), " a  b a\tc ")
        .unwrap();
    let classes = |doc: &Document| {
        doc.node(div)
            .unwrap()
            .as_element()
            .unwrap()
            .classes()
            .to_vec()
    };
    assert_eq!(classes(&doc), ["a", "b", "c"].map(Atom::from));

    doc.remove_attribute(div, &attr(local_name!("class")))
        .unwrap();
    assert!(classes(&doc).is_empty());
    doc.remove_attribute(div, &attr(local_name!("class")))
        .unwrap();

    doc.set_attribute(div, attr(local_name!("id")), "").unwrap();
    assert_eq!(
        doc.node(div).unwrap().as_element().unwrap().id(),
        None,
        "empty id is no id"
    );
}

#[test]
fn every_mutation_queues_a_record_until_the_task_ends() {
    let (mut doc, _, body) = page();
    doc.end_task();
    let p = element(&mut doc, local_name!("p"));
    let text = doc.create_text("old");
    doc.append_child(body, p).unwrap();
    doc.append_child(p, text).unwrap();
    doc.set_text(text, "new").unwrap();
    doc.set_attribute(p, attr(local_name!("title")), "t")
        .unwrap();
    doc.remove_child(body, p).unwrap();

    assert_eq!(
        doc.mutation_records(),
        [
            MutationRecord::ChildList {
                target: body,
                added: vec![p],
                removed: vec![],
                previous_sibling: None,
                next_sibling: None,
            },
            MutationRecord::ChildList {
                target: p,
                added: vec![text],
                removed: vec![],
                previous_sibling: None,
                next_sibling: None,
            },
            MutationRecord::CharacterData {
                target: text,
                old_value: "old".into(),
            },
            MutationRecord::Attribute {
                target: p,
                name: attr(local_name!("title")),
                old_value: None,
            },
            MutationRecord::ChildList {
                target: body,
                added: vec![],
                removed: vec![p],
                previous_sibling: None,
                next_sibling: None,
            },
        ]
    );
    doc.end_task();
    assert!(doc.mutation_records().is_empty());
}

#[test]
fn one_dirty_leaf_walks_only_its_ancestor_path() {
    // AC-5
    let (mut doc, html, body) = page();
    let mut leaf = body;
    let mut path = vec![html, body];
    for _ in 0..3 {
        // A sibling at every level that must not be visited.
        let sibling = element(&mut doc, local_name!("div"));
        doc.append_child(leaf, sibling).unwrap();
        let next = element(&mut doc, local_name!("div"));
        doc.append_child(leaf, next).unwrap();
        path.push(next);
        leaf = next;
    }
    style_visits(&mut doc);
    assert!(style_visits(&mut doc).is_empty(), "clean after one pass");

    doc.set_attribute(leaf, attr(local_name!("class")), "x")
        .unwrap();
    let ancestors_flagged = path[..path.len() - 1].iter().all(|&a| {
        doc.node(a)
            .unwrap()
            .flags()
            .contains(NodeFlags::DESCENDANT_NEEDS_STYLE)
    });
    assert!(ancestors_flagged);
    assert!(
        doc.node(leaf)
            .unwrap()
            .flags()
            .contains(NodeFlags::NEEDS_STYLE)
    );

    let visits = style_visits(&mut doc);
    let expected: Vec<_> = path.iter().map(|&id| (id, id == leaf)).collect();
    assert_eq!(visits, expected, "the ancestor path, then the leaf");
    assert!(style_visits(&mut doc).is_empty());
}

#[test]
fn insertion_restyles_the_new_subtree_and_rebuilds_the_parents_boxes() {
    let (mut doc, html, body) = page();
    style_visits(&mut doc);
    doc.take_box_rebuild_roots();
    doc.take_needs_frame();

    let section = element(&mut doc, local_name!("section"));
    let p = element(&mut doc, local_name!("p"));
    doc.append_child(section, p).unwrap();
    assert!(
        doc.take_box_rebuild_roots().is_empty(),
        "a detached change marks nothing"
    );
    assert!(!doc.take_needs_frame());

    doc.append_child(body, section).unwrap();
    assert!(doc.take_needs_frame());
    assert_eq!(
        style_visits(&mut doc),
        [(html, false), (body, false), (section, true), (p, true)]
    );
    assert_eq!(doc.take_box_rebuild_roots(), [body]);

    doc.remove_child(body, section).unwrap();
    assert_eq!(doc.take_box_rebuild_roots(), [body]);
}

#[test]
fn sibling_selectors_widen_the_restyle_to_later_siblings() {
    let (mut doc, html, body) = page();
    let [a, b, c] = [0; 3].map(|_| element(&mut doc, local_name!("p")));
    for id in [a, b, c] {
        doc.append_child(body, id).unwrap();
    }
    style_visits(&mut doc);

    doc.mark_affected_by_sibling_selectors(a).unwrap();
    doc.set_attribute(a, attr(local_name!("class")), "x")
        .unwrap();
    assert_eq!(
        style_visits(&mut doc),
        [
            (html, false),
            (body, false),
            (a, true),
            (b, true),
            (c, true)
        ]
    );

    // Removing `a` restyles later siblings a sibling selector matched.
    doc.mark_affected_by_sibling_selectors(c).unwrap();
    doc.remove_child(body, a).unwrap();
    assert_eq!(
        style_visits(&mut doc),
        [(html, false), (body, false), (c, true)]
    );
}

#[test]
fn disconnecting_drops_styles_and_box_ids() {
    let (mut doc, _, body) = page();
    let div = element(&mut doc, local_name!("div"));
    assert_eq!(
        doc.set_style(div, Some(Rc::new(ComputedStyle::default()))),
        Err(DomError::InvalidState),
        "only connected elements carry a style"
    );
    doc.append_child(body, div).unwrap();
    doc.set_style(div, Some(Rc::new(ComputedStyle::default())))
        .unwrap();
    doc.remove_child(body, div).unwrap();
    let node = doc.node(div).unwrap();
    assert!(node.as_element().unwrap().style().is_none());
    assert!(node.layout_box().is_none());
}

#[test]
fn a_pinned_detached_subtree_survives_the_sweep_and_reinserts_intact() {
    // AC-4
    let (mut doc, _, body) = page();
    let section = element(&mut doc, local_name!("section"));
    let p = element(&mut doc, local_name!("p"));
    let text = doc.create_text("kept");
    doc.append_child(body, section).unwrap();
    doc.append_child(section, p).unwrap();
    doc.append_child(p, text).unwrap();
    doc.end_task();

    doc.pin(text).unwrap();
    doc.remove_child(body, section).unwrap();
    let detached = shape(&doc, section);
    assert_eq!(doc.end_task(), 0);
    assert!([section, p, text].iter().all(|&id| doc.is_alive(id)));

    doc.append_child(body, section).unwrap();
    assert_eq!(shape(&doc, section), detached);
    assert!(doc.is_connected(text));
}

#[test]
fn an_unpinned_detached_subtree_is_freed_with_its_template_and_shadow_tree() {
    // AC-4
    let (mut doc, _, body) = page();
    let host = element(&mut doc, local_name!("div"));
    let shadow = doc.attach_shadow(host, ShadowMode::Open).unwrap();
    let in_shadow = element(&mut doc, local_name!("span"));
    doc.append_child(shadow, in_shadow).unwrap();
    let template = element(&mut doc, local_name!("template"));
    let ElementKind::Template { content } =
        doc.node(template).unwrap().as_element().unwrap().kind()
    else {
        panic!("a template has content");
    };
    let in_content = element(&mut doc, local_name!("p"));
    doc.append_child(content, in_content).unwrap();
    doc.append_child(host, template).unwrap();
    doc.append_child(body, host).unwrap();
    doc.end_task();
    let before = doc.node_count();

    doc.pin(in_shadow).unwrap();
    doc.remove_child(body, host).unwrap();
    assert_eq!(doc.end_task(), 0, "a pin in the shadow tree keeps it all");

    doc.unpin(in_shadow).unwrap();
    let owned = [host, shadow, in_shadow, template, content, in_content];
    assert_eq!(doc.end_task(), owned.len());
    assert!(owned.iter().all(|&id| !doc.is_alive(id)));
    assert_eq!(doc.node_count(), before - owned.len());
}

#[test]
fn nodes_never_inserted_are_freed() {
    let mut doc = Document::new("about:blank");
    let div = element(&mut doc, local_name!("div"));
    let text = doc.create_text("x");
    doc.append_child(div, text).unwrap();
    assert_eq!(doc.sweep_candidates(), 1);
    assert_eq!(doc.end_task(), 2);
    assert_eq!(doc.node_count(), 1, "only the document node is left");
}

#[test]
fn moving_a_pinned_node_out_lets_its_old_tree_go() {
    let mut doc = Document::new("about:blank");
    let (html, old_root, pinned) = (
        element(&mut doc, local_name!("html")),
        element(&mut doc, local_name!("div")),
        element(&mut doc, local_name!("p")),
    );
    doc.append_child(doc.root(), html).unwrap();
    doc.append_child(old_root, pinned).unwrap();
    doc.pin(pinned).unwrap();
    doc.end_task();
    assert!(doc.is_alive(old_root), "held by the pin below it");

    doc.append_child(html, pinned).unwrap();
    doc.end_task();
    assert!(!doc.is_alive(old_root), "nothing holds it now");
    assert!(doc.is_alive(pinned));
}

#[test]
fn unpinning_below_zero_is_rejected() {
    let mut doc = Document::new("about:blank");
    let div = element(&mut doc, local_name!("div"));
    assert_eq!(doc.unpin(div), Err(DomError::InvalidState));
}

#[test]
fn freed_slots_come_back_with_a_higher_generation() {
    // AC-3
    let mut doc = Document::new("about:blank");
    let div = element(&mut doc, local_name!("div"));
    doc.end_task();
    assert!(!doc.is_alive(div));
    let reused = element(&mut doc, local_name!("p"));
    assert_eq!(reused.index(), div.index());
    assert!(reused.generation() > div.generation());
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "stale NodeId")]
fn a_stale_id_panics_in_debug_builds() {
    // AC-3
    let mut doc = Document::new("about:blank");
    let div = element(&mut doc, local_name!("div"));
    doc.end_task();
    let _ = doc.node(div);
}

#[test]
#[cfg(not(debug_assertions))]
fn a_stale_id_reads_none_in_release_builds() {
    // AC-3
    let mut doc = Document::new("about:blank");
    let div = element(&mut doc, local_name!("div"));
    doc.end_task();
    assert!(doc.node(div).is_none());
    assert_eq!(doc.append_child(doc.root(), div), Err(DomError::NotFound));
}

#[test]
fn template_content_is_inert_and_never_in_the_flat_tree() {
    let (mut doc, _, body) = page();
    let template = element(&mut doc, local_name!("template"));
    let ElementKind::Template { content } =
        doc.node(template).unwrap().as_element().unwrap().kind()
    else {
        panic!("a template has content");
    };
    let p = element(&mut doc, local_name!("p"));
    doc.append_child(content, p).unwrap();
    doc.append_child(body, template).unwrap();

    assert!(doc.is_connected(template));
    assert!(!doc.is_connected(p), "content never connects");
    assert!(doc.is_inert(p) && !doc.is_inert(template));
    assert_eq!(doc.flat_children(template).count(), 0);
    assert_eq!(
        doc.append_child(content, template),
        Err(DomError::HierarchyRequest),
        "a template inside its own content"
    );
}

#[test]
fn flat_children_shows_the_shadow_tree_of_a_host() {
    // AC-15
    let (mut doc, _, body) = page();
    let host = element(&mut doc, local_name!("div"));
    let light = element(&mut doc, local_name!("p"));
    doc.append_child(body, host).unwrap();
    doc.append_child(host, light).unwrap();
    assert!(
        doc.flat_children(host).eq(doc.children(host)),
        "no shadow root: same list"
    );

    let shadow = doc.attach_shadow(host, ShadowMode::Closed).unwrap();
    let inner = element(&mut doc, local_name!("span"));
    doc.append_child(shadow, inner).unwrap();
    assert_eq!(doc.flat_children(host).collect::<Vec<_>>(), [inner]);
    assert_eq!(doc.children(host).collect::<Vec<_>>(), [light]);
    assert!(doc.is_connected(shadow) && doc.is_connected(inner));
    assert!(
        doc.node(host)
            .unwrap()
            .flags()
            .contains(NodeFlags::HAS_SHADOW_ROOT)
    );

    assert_eq!(
        doc.attach_shadow(host, ShadowMode::Open),
        Err(DomError::NotSupported)
    );
    let img = element(&mut doc, local_name!("img"));
    assert_eq!(
        doc.attach_shadow(img, ShadowMode::Open),
        Err(DomError::NotSupported)
    );
    let custom = doc.create_element(QualName::html(LocalName::from("my-card")));
    assert!(doc.attach_shadow(custom, ShadowMode::Open).is_ok());
    assert_eq!(
        doc.append_child(inner, host),
        Err(DomError::HierarchyRequest),
        "a host inside its own shadow tree"
    );
    assert_eq!(
        doc.append_child(body, shadow),
        Err(DomError::HierarchyRequest)
    );
}

#[test]
fn shadow_tree_changes_dirty_the_host_path() {
    let (mut doc, html, body) = page();
    let host = element(&mut doc, local_name!("div"));
    doc.append_child(body, host).unwrap();
    style_visits(&mut doc);
    doc.take_box_rebuild_roots();
    let shadow = doc.attach_shadow(host, ShadowMode::Open).unwrap();
    style_visits(&mut doc);
    assert_eq!(doc.take_box_rebuild_roots(), [host]);

    let inner = element(&mut doc, local_name!("span"));
    doc.append_child(shadow, inner).unwrap();
    assert_eq!(
        style_visits(&mut doc),
        [(html, false), (body, false), (host, false), (inner, true)]
    );
    assert_eq!(
        doc.take_box_rebuild_roots(),
        [host],
        "the host, not the shadow root"
    );
}

#[test]
fn iframes_report_when_they_join_and_leave_the_document() {
    let (mut doc, _, body) = page();
    let iframe = element(&mut doc, local_name!("iframe"));
    assert!(doc.take_frame_changes().is_empty());
    assert_eq!(
        doc.set_iframe_frame(iframe, FrameId(1)),
        Err(DomError::InvalidState)
    );

    doc.append_child(body, iframe).unwrap();
    assert_eq!(doc.take_frame_changes(), [FrameChange::Connected(iframe)]);
    doc.set_iframe_frame(iframe, FrameId(1)).unwrap();

    doc.remove_child(body, iframe).unwrap();
    assert_eq!(
        doc.take_frame_changes(),
        [FrameChange::Disconnected(FrameId(1))]
    );
    let ElementKind::IFrame { frame } = doc.node(iframe).unwrap().as_element().unwrap().kind()
    else {
        panic!("an iframe element");
    };
    assert_eq!(frame, None);
}

/// Styles `body` and gives it `count` children named `name`, ready for a
/// style pass to share between.
fn siblings(count: usize, name: LocalName) -> (Document, NodeId, Vec<NodeId>) {
    let (mut doc, html, body) = page();
    let root_style = ComputedStyle::initial();
    doc.set_style(html, Some(Rc::clone(&root_style))).unwrap();
    doc.set_style(
        body,
        Some(Rc::new(ComputedStyle::inherit_from(&root_style))),
    )
    .unwrap();
    let children = (0..count)
        .map(|_| {
            let child = element(&mut doc, name.clone());
            doc.append_child(body, child).unwrap();
            doc.set_attribute(child, attr(local_name!("class")), "card")
                .unwrap();
            child
        })
        .collect();
    (doc, body, children)
}

/// Runs a style pass that shares where it can and otherwise gives each
/// element a fresh style inherited from its parent. Returns the sharing hits.
fn style_pass_with_sharing(doc: &mut Document) -> usize {
    let mut hits = 0;
    doc.restyle_dirty(|doc, id, restyle| {
        if !restyle {
            return false;
        }
        let style = match doc.shared_style(id) {
            Some(shared) => {
                hits += 1;
                shared
            }
            None => {
                let parent = doc.flat_parent(id).and_then(|p| {
                    doc.node(p)
                        .and_then(Node::as_element)
                        .and_then(|e| e.style().cloned())
                });
                let parent = parent.unwrap_or_else(ComputedStyle::initial);
                Rc::new(ComputedStyle::inherit_from(&parent))
            }
        };
        doc.set_style(id, Some(style)).unwrap();
        doc.remember_style(id);
        false
    });
    hits
}

#[test]
fn identical_siblings_share_one_style() {
    // AC-7
    let (mut doc, _, cards) = siblings(4, local_name!("div"));
    let hits = style_pass_with_sharing(&mut doc);
    assert_eq!(hits, 3, "every card after the first shares");
    let style = |id| doc.node(id).unwrap().as_element().unwrap().style().cloned();
    let first = style(cards[0]).unwrap();
    for &card in &cards[1..] {
        assert!(Rc::ptr_eq(&first, &style(card).unwrap()));
    }
}

#[test]
fn an_id_a_style_attribute_or_sibling_selectors_stop_sharing() {
    // AC-7
    let (mut doc, _, cards) = siblings(5, local_name!("div"));
    doc.set_attribute(cards[1], attr(local_name!("id")), "unique")
        .unwrap();
    doc.set_attribute(cards[2], attr(local_name!("style")), "color: red")
        .unwrap();
    doc.set_attribute(cards[3], attr(local_name!("class")), "other")
        .unwrap();
    doc.mark_affected_by_sibling_selectors(cards[4]).unwrap();
    assert_eq!(style_pass_with_sharing(&mut doc), 0);
}

#[test]
fn set_style_reports_and_records_its_damage() {
    // AC-8, and the damage reaches the next frame.
    let (mut doc, html, body) = page();
    let mut block = ComputedStyle::default();
    Rc::make_mut(&mut block.display_box).display = css::Display::Block;
    let block = Rc::new(block);
    doc.set_style(html, Some(Rc::clone(&block))).unwrap();
    assert_eq!(
        doc.set_style(body, Some(Rc::clone(&block))),
        Ok(css::StyleDamage::RebuildBoxes),
        "gaining a style gains a box"
    );
    assert!(doc.take_box_rebuild_roots().contains(&html));
    doc.take_needs_frame();

    let same = Rc::clone(&block);
    assert_eq!(doc.set_style(body, Some(same)), Ok(css::StyleDamage::None));
    assert!(!doc.needs_frame());

    let mut red = (*block).clone();
    Rc::make_mut(&mut red.background).color = css::Color::rgb8(255, 0, 0);
    assert_eq!(
        doc.set_style(body, Some(Rc::new(red.clone()))),
        Ok(css::StyleDamage::Repaint)
    );
    Rc::make_mut(&mut red.size).width = Some(css::Length::px(10.0));
    assert_eq!(
        doc.set_style(body, Some(Rc::new(red))),
        Ok(css::StyleDamage::Relayout)
    );
    assert!(doc.needs_frame());
    assert_eq!(
        doc.take_style_damage(),
        [
            (body, css::StyleDamage::Repaint),
            (body, css::StyleDamage::Relayout)
        ]
    );
    assert!(doc.take_box_rebuild_roots().is_empty());
}

/// The invalidation map of an author sheet with `css`.
fn map_of(css: &str) -> Rc<css::InvalidationMap> {
    let sheet = Rc::new(css::parse_stylesheet(css, css::Origin::Author, None));
    let device = css::Device {
        width: 800.0,
        height: 600.0,
        color_scheme: ColorScheme::Light,
        reduced_motion: false,
    };
    css::Stylist::new(&[sheet], device).invalidation_map()
}

#[test]
fn state_changes_restyle_only_what_a_rule_reaches() {
    // Spec 0006, AC-19
    let (mut doc, html, body) = page();
    style_visits(&mut doc);
    doc.take_needs_frame();
    doc.set_invalidation_map(Some(map_of("p { color: red }")));
    doc.set_state(body, ElementState::HOVER, ElementState::empty());
    assert!(
        doc.node(body)
            .unwrap()
            .as_element()
            .unwrap()
            .state()
            .contains(ElementState::HOVER)
    );
    assert!(style_visits(&mut doc).is_empty(), "no rule uses :hover");
    assert!(!doc.take_needs_frame());

    doc.set_invalidation_map(Some(map_of("body:hover { color: red }")));
    doc.set_state(body, ElementState::empty(), ElementState::HOVER);
    assert_eq!(style_visits(&mut doc), [(html, false), (body, true)]);
    assert!(doc.take_needs_frame());

    doc.set_state(body, ElementState::empty(), ElementState::HOVER);
    assert!(style_visits(&mut doc).is_empty(), "no change, no restyle");
}

fn is_disabled(doc: &Document, id: NodeId) -> bool {
    doc.node(id)
        .and_then(Node::as_element)
        .is_some_and(|e| e.state().contains(ElementState::DISABLED))
}

#[test]
fn disabled_follows_the_attribute_and_fieldset_ancestry() {
    // Spec 0006, AC-16
    let (mut doc, _, body) = page();
    let fieldset = element(&mut doc, local_name!("fieldset"));
    let legend = element(&mut doc, local_name!("legend"));
    let in_legend = element(&mut doc, local_name!("button"));
    let outside = element(&mut doc, local_name!("input"));
    let plain = element(&mut doc, local_name!("button"));
    doc.append_child(legend, in_legend).unwrap();
    doc.append_child(fieldset, legend).unwrap();
    doc.append_child(fieldset, outside).unwrap();
    doc.append_child(body, fieldset).unwrap();
    doc.append_child(body, plain).unwrap();
    assert!(!is_disabled(&doc, outside));

    doc.set_attribute(fieldset, attr(local_name!("disabled")), "")
        .unwrap();
    assert!(is_disabled(&doc, outside));
    assert!(
        !is_disabled(&doc, in_legend),
        "the first legend stays enabled"
    );
    assert!(
        !is_disabled(&doc, fieldset),
        "a fieldset is not a control itself"
    );

    // A new first legend takes the old one's place.
    let earlier = element(&mut doc, local_name!("legend"));
    doc.insert_before(fieldset, earlier, Some(legend)).unwrap();
    assert!(is_disabled(&doc, in_legend));

    doc.set_attribute(plain, attr(local_name!("disabled")), "")
        .unwrap();
    assert!(is_disabled(&doc, plain));
    doc.remove_attribute(plain, &attr(local_name!("disabled")))
        .unwrap();
    assert!(!is_disabled(&doc, plain));

    // Inserted already disabled.
    let late = element(&mut doc, local_name!("textarea"));
    doc.set_attribute(late, attr(local_name!("disabled")), "")
        .unwrap();
    doc.append_child(body, late).unwrap();
    assert!(is_disabled(&doc, late));
}

#[test]
fn focus_pins_one_element_and_leaves_with_it() {
    // Spec 0006, AC-13 and Key invariants
    let (mut doc, _, body) = page();
    let a = element(&mut doc, local_name!("button"));
    let b = element(&mut doc, local_name!("button"));
    doc.append_child(body, a).unwrap();
    doc.append_child(body, b).unwrap();

    doc.set_focus(Some(a));
    doc.set_state(a, ElementState::FOCUS, ElementState::empty());
    assert_eq!(doc.focused(), Some(a));
    assert_eq!(doc.node(a).unwrap().pin_count(), 1);

    doc.set_focus(Some(b));
    assert_eq!(doc.focused(), Some(b));
    assert_eq!(doc.node(a).unwrap().pin_count(), 0);
    assert!(
        !doc.node(a)
            .and_then(Node::as_element)
            .unwrap()
            .state()
            .contains(ElementState::FOCUS),
        "the old element loses its focus bit"
    );

    doc.remove_child(body, b).unwrap();
    assert_eq!(
        doc.focused(),
        None,
        "removed: focus goes with no replacement"
    );

    doc.set_focus(Some(a));
    doc.set_attribute(a, attr(local_name!("disabled")), "")
        .unwrap();
    assert_eq!(doc.focused(), None, "disabled: focus goes too");

    let detached = element(&mut doc, local_name!("button"));
    doc.set_focus(Some(detached));
    assert_eq!(doc.focused(), None, "only connected elements take focus");
}
