//! Stable text dumps of a document, and the tree position paths every dump
//! labels nodes with (spec 0002, Value sourcing: Dumps, AC-17).
//!
//! Paths such as `html>body>div[2]` never contain raw ids, so the same
//! document always dumps to the same bytes. Walks use explicit stacks, so a
//! very deep document cannot overflow the thread stack.

use std::fmt::Write;

use crate::{Document, Node, NodeData, NodeId, ShadowMode};

impl Document {
    /// The tree position path of `id`: one label per level from the top,
    /// joined by `>`. An element is labeled by its local name, other nodes
    /// by `#text`, `#comment`, `#doctype`, `#fragment`, `#content` (template
    /// content) or `#shadow-root`. A label shared with other siblings gets
    /// its 1 based position among them, as in `div[2]`. Shadow roots and
    /// template content continue from their host; a detached subtree starts
    /// with `(detached)`. A freed id gives `(freed)`.
    pub fn path(&self, id: NodeId) -> String {
        let mut labels = Vec::new();
        let mut current = id;
        // Bounded by the node count: each step moves to a parent or a host.
        for _ in 0..=self.nodes.len() {
            let Some(node) = self.nodes.get(current) else {
                return "(freed)".into();
            };
            if matches!(node.data, NodeData::Document) {
                break;
            }
            labels.push(self.label_with_position(current, node));
            current = match (node.parent, &node.data) {
                (Some(parent), _) => parent,
                (None, NodeData::ShadowRoot { host, .. }) => *host,
                (None, NodeData::DocumentFragment { host: Some(host) }) => *host,
                (None, _) => {
                    labels.push("(detached)".into());
                    break;
                }
            };
        }
        labels.reverse();
        labels.join(">")
    }

    /// The whole document tree, one node per line, indented by depth.
    /// Shadow roots and template content print under their host, before
    /// its children.
    pub fn dump(&self) -> String {
        let mut out = String::new();
        let mut stack = vec![(self.root, 0usize)];
        while let Some((id, depth)) = stack.pop() {
            let Some(node) = self.nodes.get(id) else {
                continue;
            };
            let _ = writeln!(out, "{:indent$}{}", "", describe(node), indent = depth * 2);
            let mut below: Vec<NodeId> = Vec::new();
            if let Some(element) = node.as_element() {
                below.extend(element.shadow_root());
                if let crate::ElementKind::Template { content } = element.kind() {
                    below.push(content);
                }
            }
            below.extend(self.children(id));
            stack.extend(below.into_iter().rev().map(|child| (child, depth + 1)));
        }
        out
    }

    /// Each connected element in flat tree order, as its path and the
    /// summary of its computed style, one per line.
    pub fn dump_styles(&self) -> String {
        let mut out = String::new();
        let mut stack: Vec<NodeId> = self.document_element().into_iter().collect();
        while let Some(id) = stack.pop() {
            let Some(element) = self.nodes.get(id).and_then(Node::as_element) else {
                continue;
            };
            let style = element
                .style()
                .map_or_else(|| "(unstyled)".into(), |style| style.dump());
            let _ = writeln!(out, "{}: {style}", self.path(id));
            let children: Vec<NodeId> = self.flat_children(id).collect();
            stack.extend(children.into_iter().rev());
        }
        out
    }

    /// `node`'s label, with its position among same labeled siblings when
    /// it has any.
    fn label_with_position(&self, id: NodeId, node: &Node) -> String {
        let own = label(node);
        let Some(parent) = node.parent else {
            return own;
        };
        let mut position = 0;
        let mut count = 0;
        for sibling in self.children(parent) {
            let Some(other) = self.nodes.get(sibling) else {
                continue;
            };
            if label(other) == own {
                count += 1;
                if sibling == id {
                    position = count;
                }
            }
        }
        if count > 1 {
            format!("{own}[{position}]")
        } else {
            own
        }
    }
}

fn label(node: &Node) -> String {
    match &node.data {
        NodeData::Element(element) => element.name().local.to_string(),
        NodeData::Text(_) => "#text".into(),
        NodeData::Comment(_) => "#comment".into(),
        NodeData::Doctype(_) => "#doctype".into(),
        NodeData::Document => "#document".into(),
        NodeData::DocumentFragment { host: Some(_) } => "#content".into(),
        NodeData::DocumentFragment { host: None } => "#fragment".into(),
        NodeData::ShadowRoot { .. } => "#shadow-root".into(),
    }
}

/// One dump line: the label plus what the node carries.
fn describe(node: &Node) -> String {
    match &node.data {
        NodeData::Element(element) => {
            let mut line = match &element.name().prefix {
                Some(prefix) => format!("{prefix}:{}", element.name().local),
                None => element.name().local.to_string(),
            };
            for attr in element.attributes() {
                let _ = write!(line, " {}={:?}", attr.name.local, attr.value);
            }
            line
        }
        NodeData::Text(text) => format!("{text:?}"),
        NodeData::Comment(text) => format!("<!--{text}-->"),
        NodeData::Doctype(doctype) => format!("#doctype {}", doctype.name),
        NodeData::ShadowRoot { mode, .. } => match mode {
            ShadowMode::Open => "#shadow-root (open)".into(),
            ShadowMode::Closed => "#shadow-root (closed)".into(),
        },
        _ => label(node),
    }
}

#[cfg(test)]
mod tests {
    use base::atoms::QualName;
    use base::local_name;

    use super::*;

    fn element(doc: &mut Document, parent: NodeId, name: base::atoms::LocalName) -> NodeId {
        let id = doc.create_element(QualName::html(name));
        doc.append_child(parent, id).unwrap();
        id
    }

    #[test]
    fn paths_number_only_siblings_that_share_a_label() {
        let mut doc = Document::new("about:blank");
        let root = doc.root();
        let html = element(&mut doc, root, local_name!("html"));
        let body = element(&mut doc, html, local_name!("body"));
        let first = element(&mut doc, body, local_name!("div"));
        let p = element(&mut doc, body, local_name!("p"));
        let second = element(&mut doc, body, local_name!("div"));
        let text = doc.create_text("hi");
        doc.append_child(second, text).unwrap();

        assert_eq!(doc.path(html), "html");
        assert_eq!(doc.path(first), "html>body>div[1]");
        assert_eq!(doc.path(p), "html>body>p");
        assert_eq!(doc.path(text), "html>body>div[2]>#text");

        doc.remove_child(body, second).unwrap();
        assert_eq!(doc.path(text), "(detached)>div>#text");
    }

    #[test]
    fn shadow_and_template_content_continue_from_their_host() {
        let mut doc = Document::new("about:blank");
        let root = doc.root();
        let html = element(&mut doc, root, local_name!("html"));
        let host = element(&mut doc, html, local_name!("div"));
        let shadow = doc.attach_shadow(host, ShadowMode::Open).unwrap();
        let inner = element(&mut doc, shadow, local_name!("span"));
        let template = element(&mut doc, html, local_name!("template"));
        let crate::ElementKind::Template { content } =
            doc.node(template).unwrap().as_element().unwrap().kind()
        else {
            panic!("a template has content");
        };
        let inert = element(&mut doc, content, local_name!("p"));

        assert_eq!(doc.path(inner), "html>div>#shadow-root>span");
        assert_eq!(doc.path(inert), "html>template>#content>p");
        let dump = doc.dump();
        assert_eq!(
            dump,
            "#document\n  html\n    div\n      #shadow-root (open)\n        span\n    \
             template\n      #content\n        p\n"
        );
        assert_eq!(doc.dump(), dump, "the same document dumps the same bytes");
    }
}
