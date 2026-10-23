//! The parser's tree sink over the DOM (spec 0004, child 0002, `TreeSink`).
//!
//! Every change goes through the DOM's mutation API, so dirty flags, the id
//! index and mutation records stay right while the page loads. The parser
//! only passes nodes this sink created, so a failed call is a bug in the
//! parser, logged and skipped rather than a crash.

use base::atoms::{LocalName, QualName};
use dom::{DoctypeData, Document, ElementKind, NodeData, NodeId, QuirksMode};
use html::{Attribute, CreateFlags, NodeOrText, TreeSink};
use tracing::trace;

/// Builds the parser's tree in `doc`.
pub struct DomSink<'a> {
    doc: &'a mut Document,
    /// What `element_name` answers for a node that is not an element.
    no_name: QualName,
}

impl<'a> DomSink<'a> {
    /// A sink that builds into `doc`.
    pub fn new(doc: &'a mut Document) -> Self {
        Self {
            doc,
            no_name: QualName::html(LocalName::from("")),
        }
    }

    /// The text node a run of text joins, if `node` is one.
    fn text_node(&self, node: Option<NodeId>) -> Option<(NodeId, String)> {
        let node = node?;
        match self.doc.node(node)?.data() {
            NodeData::Text(text) => Some((node, text.clone())),
            _ => None,
        }
    }

    /// Makes `child` a node, joining text to `neighbor` when that is a text
    /// node. `None` when the text was joined and nothing needs inserting.
    fn node_for(
        &mut self,
        child: NodeOrText<'_, NodeId>,
        neighbor: Option<NodeId>,
    ) -> Option<NodeId> {
        match child {
            NodeOrText::Node(node) => Some(node),
            NodeOrText::Text(text) => {
                if let Some((node, mut existing)) = self.text_node(neighbor) {
                    existing.push_str(text);
                    log(self.doc.set_text(node, existing));
                    return None;
                }
                Some(self.doc.create_text(text))
            }
        }
    }
}

fn log(result: Result<(), dom::DomError>) {
    if let Err(err) = result {
        trace!(%err, "parser tree call refused");
    }
}

impl TreeSink for DomSink<'_> {
    type Handle = NodeId;

    fn document(&self) -> NodeId {
        self.doc.root()
    }

    fn create_element(&mut self, name: QualName, attrs: Vec<Attribute>, _: CreateFlags) -> NodeId {
        // The DOM gives a `<template>` its content fragment itself.
        let element = self.doc.create_element(name);
        for attr in attrs {
            log(self.doc.set_attribute(element, attr.name, attr.value));
        }
        element
    }

    fn create_comment(&mut self, text: &str) -> NodeId {
        self.doc.create_comment(text)
    }

    fn append(&mut self, parent: NodeId, child: NodeOrText<'_, NodeId>) {
        let last = self.doc.node(parent).and_then(|n| n.last_child());
        if let Some(node) = self.node_for(child, last) {
            log(self.doc.append_child(parent, node));
        }
    }

    fn insert_before(&mut self, sibling: NodeId, child: NodeOrText<'_, NodeId>) {
        let Some(entry) = self.doc.node(sibling) else {
            return;
        };
        let (parent, previous) = (entry.parent(), entry.prev_sibling());
        let Some(parent) = parent else {
            return;
        };
        if let Some(node) = self.node_for(child, previous) {
            log(self.doc.insert_before(parent, node, Some(sibling)));
        }
    }

    fn append_doctype(&mut self, name: &str, public_id: &str, system_id: &str) {
        let doctype = self.doc.create_doctype(DoctypeData {
            name: name.to_owned(),
            public_id: public_id.to_owned(),
            system_id: system_id.to_owned(),
        });
        let root = self.doc.root();
        log(self.doc.append_child(root, doctype));
    }

    fn add_attrs_if_missing(&mut self, target: NodeId, attrs: Vec<Attribute>) {
        for attr in attrs {
            let missing = self
                .doc
                .node(target)
                .and_then(|n| n.as_element())
                .is_some_and(|element| element.attribute(&attr.name).is_none());
            if missing {
                log(self.doc.set_attribute(target, attr.name, attr.value));
            }
        }
    }

    fn remove_from_parent(&mut self, target: NodeId) {
        if let Some(parent) = self.doc.node(target).and_then(|n| n.parent()) {
            log(self.doc.remove_child(parent, target));
        }
    }

    fn reparent_children(&mut self, from: NodeId, to: NodeId) {
        let children: Vec<NodeId> = self.doc.children(from).collect();
        for child in children {
            log(self.doc.append_child(to, child));
        }
    }

    fn template_contents(&self, template: NodeId) -> NodeId {
        match self
            .doc
            .node(template)
            .and_then(|n| n.as_element())
            .map(|e| e.kind())
        {
            Some(ElementKind::Template { content }) => content,
            _ => template,
        }
    }

    fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.doc.node(node).and_then(|n| n.parent())
    }

    fn set_quirks_mode(&mut self, mode: html::QuirksMode) {
        self.doc.set_quirks_mode(match mode {
            html::QuirksMode::NoQuirks => QuirksMode::NoQuirks,
            html::QuirksMode::LimitedQuirks => QuirksMode::LimitedQuirks,
            html::QuirksMode::Quirks => QuirksMode::Quirks,
        });
    }

    fn element_name(&self, node: NodeId) -> &QualName {
        self.doc
            .node(node)
            .and_then(|n| n.as_element())
            .map_or(&self.no_name, |element| element.name())
    }
}

#[cfg(test)]
mod tests {
    use html::{ParseStep, Parser, ParserOptions};

    use super::*;

    #[test]
    fn the_parser_builds_the_dom_through_the_mutation_api() {
        let mut doc = Document::new("http://example.test/");
        let mut parser = Parser::new(ParserOptions::default());
        let mut sink = DomSink::new(&mut doc);
        parser.feed(&mut sink, b"<!DOCTYPE html><p id=a class='x y'>one ");
        parser.feed(&mut sink, b"two<!-- c --></p>");
        assert_eq!(parser.end(&mut sink), ParseStep::Done);

        assert_eq!(
            doc.dump(),
            "#document\n  #doctype html\n  html\n    head\n    body\n      \
             p id=\"a\" class=\"x y\"\n        \"one two\"\n        <!-- c -->\n"
        );
        let p = doc.element_by_id(&base::atoms::Atom::from("a"));
        assert!(p.is_some(), "the id index saw the parsed element");
        assert!(doc.needs_frame(), "parsing marked the document for a frame");
    }
}
