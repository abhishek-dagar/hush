//! The mutation API, the only way to change a tree (spec 0002, Mutation API).
//!
//! Every call checks the DOM standard's validity rules and returns a
//! [`DomError`] before touching anything (AC-2). After the checks nothing can
//! fail, so a call either fully happens or leaves the tree as it was. Then,
//! in order: links, `IS_CONNECTED` and `id_index` upkeep, dirty flags (AC-5),
//! a [`MutationRecord`], and the needs frame mark.

use std::panic::Location;

use base::atoms::{Atom, LocalName, QualName};
use base::{local_name, ns};
use css::{ElementChange, PseudoState, RestyleHint};

use crate::node::{
    DoctypeData, ElementData, ElementKind, ElementState, ImageState, Node, NodeData, NodeFlags,
    ShadowMode,
};
use crate::{Attribute, Document, DomError, FrameChange, NodeId};

/// One change, queued for mutation observers and live collections (slice 9).
/// Until they exist the queue is cleared at the end of every task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationRecord {
    /// Children were added to or removed from `target`.
    ChildList {
        /// The parent whose child list changed.
        target: NodeId,
        /// The nodes inserted, in order.
        added: Vec<NodeId>,
        /// The nodes removed.
        removed: Vec<NodeId>,
        /// The sibling before the change.
        previous_sibling: Option<NodeId>,
        /// The sibling after the change.
        next_sibling: Option<NodeId>,
    },
    /// An attribute was set, changed or removed.
    Attribute {
        /// The element.
        target: NodeId,
        /// The attribute's name.
        name: QualName,
        /// The value before, `None` if it was not set.
        old_value: Option<String>,
    },
    /// A text or comment node's text changed.
    CharacterData {
        /// The node.
        target: NodeId,
        /// The text before.
        old_value: String,
    },
}

/// The node kinds the validity rules tell apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Document,
    Doctype,
    Element,
    Text,
    Comment,
    Fragment,
    ShadowRoot,
}

impl Kind {
    fn of(node: &Node) -> Self {
        match node.data {
            NodeData::Document => Self::Document,
            NodeData::Doctype(_) => Self::Doctype,
            NodeData::Element(_) => Self::Element,
            NodeData::Text(_) => Self::Text,
            NodeData::Comment(_) => Self::Comment,
            NodeData::DocumentFragment { .. } => Self::Fragment,
            NodeData::ShadowRoot { .. } => Self::ShadowRoot,
        }
    }
}

/// The dirty flags a disconnected node must not keep.
const DIRTY: NodeFlags = NodeFlags::NEEDS_STYLE
    .union(NodeFlags::DESCENDANT_NEEDS_STYLE)
    .union(NodeFlags::RESTYLE_SUBTREE)
    .union(NodeFlags::NEEDS_BOX_REBUILD)
    .union(NodeFlags::DESCENDANT_NEEDS_BOX_REBUILD);

impl Document {
    /// Creates an element that is not in the tree yet. A `<template>` gets
    /// its inert content fragment here.
    pub fn create_element(&mut self, name: QualName) -> NodeId {
        let is_html = name.ns == ns!("http://www.w3.org/1999/xhtml");
        let is_template = is_html && name.local == local_name!("template");
        let kind = if is_html && name.local == local_name!("iframe") {
            ElementKind::IFrame { frame: None }
        } else if is_html && name.local == local_name!("img") {
            ElementKind::Img {
                image: ImageState::Pending,
            }
        } else {
            ElementKind::Other
        };
        let id = self.create(NodeData::Element(Box::new(ElementData::new(name, kind))));
        if is_template {
            // Owned by the template, so it is no sweep candidate of its own.
            let mut content = Node::new(NodeData::DocumentFragment { host: Some(id) });
            content.flags.insert(NodeFlags::IS_INERT);
            let content = self.nodes.insert(content);
            if let Some(element) = self.nodes.get_mut(id).and_then(Node::as_element_mut) {
                element.kind = ElementKind::Template { content };
            }
        }
        id
    }

    /// Creates a text node that is not in the tree yet.
    pub fn create_text(&mut self, text: impl Into<String>) -> NodeId {
        self.create(NodeData::Text(text.into()))
    }

    /// Creates a comment that is not in the tree yet.
    pub fn create_comment(&mut self, text: impl Into<String>) -> NodeId {
        self.create(NodeData::Comment(text.into()))
    }

    /// Creates an empty document fragment. Inserting it moves its children.
    pub fn create_document_fragment(&mut self) -> NodeId {
        self.create(NodeData::DocumentFragment { host: None })
    }

    /// Creates a doctype that is not in the tree yet.
    pub fn create_doctype(&mut self, doctype: DoctypeData) -> NodeId {
        self.create(NodeData::Doctype(Box::new(doctype)))
    }

    /// Moves `node` to the end of `parent`'s children, taking it out of its
    /// old place first. A fragment moves its children instead.
    #[track_caller]
    pub fn append_child(&mut self, parent: NodeId, node: NodeId) -> Result<(), DomError> {
        self.insert_before(parent, node, None)
    }

    /// Moves `node` into `parent` just before `child`, or to the end when
    /// `child` is `None`. `child` must be a child of `parent`.
    #[track_caller]
    pub fn insert_before(
        &mut self,
        parent: NodeId,
        node: NodeId,
        child: Option<NodeId>,
    ) -> Result<(), DomError> {
        self.ensure_validity(node, parent, child, false, Location::caller())?;
        // Inserting a node before itself means before its next sibling.
        let reference = if child == Some(node) {
            self.next_sibling_of(node)
        } else {
            child
        };
        self.insert(node, parent, reference);
        Ok(())
    }

    /// Takes `child` out of `parent`. The child and its subtree stay alive
    /// until the sweep finds no pin in them.
    #[track_caller]
    pub fn remove_child(&mut self, parent: NodeId, child: NodeId) -> Result<(), DomError> {
        let caller = Location::caller();
        self.live(parent, caller)?;
        if self.live(child, caller)?.parent != Some(parent) {
            return Err(DomError::NotFound);
        }
        self.remove(child);
        Ok(())
    }

    /// Puts `node` where `child` is in `parent`, removing `child`. Queued as
    /// a removal record and an insertion record.
    #[track_caller]
    pub fn replace_child(
        &mut self,
        parent: NodeId,
        node: NodeId,
        child: NodeId,
    ) -> Result<(), DomError> {
        self.ensure_validity(node, parent, Some(child), true, Location::caller())?;
        let mut reference = self.next_sibling_of(child);
        if reference == Some(node) {
            reference = self.next_sibling_of(node);
        }
        self.remove(child);
        self.insert(node, parent, reference);
        Ok(())
    }

    /// Sets the attribute called `name` on `element`, adding it if new.
    #[track_caller]
    pub fn set_attribute(
        &mut self,
        element: NodeId,
        name: QualName,
        value: impl Into<String>,
    ) -> Result<(), DomError> {
        let value = value.into();
        let data = self
            .live_mut(element, Location::caller())?
            .as_element_mut()
            .ok_or(DomError::InvalidState)?;
        let old_value = match data.attrs.iter_mut().find(|attr| attr.name == name) {
            Some(attr) => Some(std::mem::replace(&mut attr.value, value)),
            None => {
                data.attrs.push(Attribute {
                    name: name.clone(),
                    value,
                });
                None
            }
        };
        self.attribute_changed(element, name, old_value);
        Ok(())
    }

    /// Removes the attribute called `name` from `element`. Removing one that
    /// is not set changes nothing.
    #[track_caller]
    pub fn remove_attribute(&mut self, element: NodeId, name: &QualName) -> Result<(), DomError> {
        let data = self
            .live_mut(element, Location::caller())?
            .as_element_mut()
            .ok_or(DomError::InvalidState)?;
        let Some(index) = data.attrs.iter().position(|attr| attr.name == *name) else {
            return Ok(());
        };
        let old_value = data.attrs.remove(index).value;
        self.attribute_changed(element, name.clone(), Some(old_value));
        Ok(())
    }

    /// Adds the `add` states to the element `node` and takes the `remove`
    /// ones away (spec 0006, Interface surface). The only way state bits
    /// change, so a change always restyles what a rule can reach: nothing
    /// when no rule uses the state, else as far as its selectors do
    /// (AC-19). Not an element, or freed: ignored.
    pub fn set_state(&mut self, node: NodeId, add: ElementState, remove: ElementState) {
        let Some(element) = self.nodes.get_mut(node).and_then(Node::as_element_mut) else {
            return;
        };
        let old = element.state;
        element.state.insert(add);
        element.state.remove(remove);
        let new = element.state;
        if old == new || !self.is_connected(node) {
            return;
        }
        let map = self.invalidation.as_deref();
        let hint = self.element_ref(node).map_or(RestyleHint::NONE, |view| {
            [
                PseudoState::Hover,
                PseudoState::Active,
                PseudoState::Focus,
                PseudoState::FocusVisible,
                PseudoState::Disabled,
            ]
            .into_iter()
            .filter(|&state| {
                let bit = ElementState::from(state);
                old.contains(bit) != new.contains(bit)
            })
            .fold(RestyleHint::NONE, |hint, state| {
                hint | css::restyle_hint(map, &view, &ElementChange::State(state))
            })
        });
        if hint != RestyleHint::NONE {
            self.apply_restyle_hint(node, hint);
            self.needs_frame = true;
        }
    }

    /// Replaces a text or comment node's text.
    #[track_caller]
    pub fn set_text(&mut self, node: NodeId, text: impl Into<String>) -> Result<(), DomError> {
        let entry = self.live_mut(node, Location::caller())?;
        let (old_value, is_text) = match &mut entry.data {
            NodeData::Text(current) => (std::mem::replace(current, text.into()), true),
            NodeData::Comment(current) => (std::mem::replace(current, text.into()), false),
            _ => return Err(DomError::InvalidState),
        };
        let parent = entry.parent;
        if is_text && entry.is_connected() {
            if let Some(parent) = parent {
                self.mark_box_rebuild(parent);
                if self.is_style_source(parent) {
                    self.style_sources_changed = true;
                }
            }
            self.needs_frame = true;
        }
        self.mutation_queue.push(MutationRecord::CharacterData {
            target: node,
            old_value,
        });
        Ok(())
    }

    /// Attaches a shadow root to `host` and returns it. From then on the flat
    /// tree shows the shadow root's children in place of the host's own.
    #[track_caller]
    pub fn attach_shadow(&mut self, host: NodeId, mode: ShadowMode) -> Result<NodeId, DomError> {
        let node = self.live(host, Location::caller())?;
        let element = node.as_element().ok_or(DomError::NotSupported)?;
        if element.shadow_root.is_some() || !can_host_shadow(&element.name) {
            return Err(DomError::NotSupported);
        }
        let connected = node.is_connected();

        let mut shadow = Node::new(NodeData::ShadowRoot { host, mode });
        if connected {
            shadow.flags.insert(NodeFlags::IS_CONNECTED);
        }
        let shadow = self.nodes.insert(shadow);
        if let Some(node) = self.nodes.get_mut(host) {
            node.flags.insert(NodeFlags::HAS_SHADOW_ROOT);
            if let Some(element) = node.as_element_mut() {
                element.shadow_root = Some(shadow);
            }
        }
        if connected {
            self.mark_style(host, NodeFlags::NEEDS_STYLE | NodeFlags::RESTYLE_SUBTREE);
            self.mark_box_rebuild(host);
            self.needs_frame = true;
        }
        Ok(shadow)
    }

    fn create(&mut self, data: NodeData) -> NodeId {
        let id = self.nodes.insert(Node::new(data));
        // A node nobody inserts must still be freed.
        self.detached_roots.push(id);
        id
    }

    /// The DOM standard's "ensure pre insert validity" (or, with `replacing`,
    /// the replace checks) of putting `node` into `parent` before or in place
    /// of `child`.
    fn ensure_validity(
        &self,
        node: NodeId,
        parent: NodeId,
        child: Option<NodeId>,
        replacing: bool,
        caller: &Location<'_>,
    ) -> Result<(), DomError> {
        let parent_kind = Kind::of(self.live(parent, caller)?);
        let node_kind = Kind::of(self.live(node, caller)?);
        let child_entry = child.map(|c| self.live(c, caller)).transpose()?;

        if !matches!(
            parent_kind,
            Kind::Document | Kind::Fragment | Kind::ShadowRoot | Kind::Element
        ) {
            return Err(DomError::HierarchyRequest);
        }
        if self.is_host_including_inclusive_ancestor(node, parent) {
            return Err(DomError::HierarchyRequest);
        }
        if child_entry.is_some_and(|c| c.parent != Some(parent)) {
            return Err(DomError::NotFound);
        }
        let allowed = match node_kind {
            Kind::Fragment | Kind::Element | Kind::Comment => true,
            Kind::Text => parent_kind != Kind::Document,
            Kind::Doctype => parent_kind == Kind::Document,
            Kind::Document | Kind::ShadowRoot => false,
        };
        if !allowed {
            return Err(DomError::HierarchyRequest);
        }
        if parent_kind == Kind::Document
            && !self.document_child_allowed(node, node_kind, parent, child, replacing)
        {
            return Err(DomError::HierarchyRequest);
        }
        Ok(())
    }

    /// The document specific rules: at most one element and one doctype,
    /// the doctype first, no text.
    fn document_child_allowed(
        &self,
        node: NodeId,
        node_kind: Kind,
        parent: NodeId,
        child: Option<NodeId>,
        replacing: bool,
    ) -> bool {
        let kind_of = |id: NodeId| self.nodes.get(id).map(Kind::of);
        // When replacing, `child` is going away, so it does not count.
        let has_other = |kind: Kind| {
            self.children(parent)
                .any(|c| kind_of(c) == Some(kind) && !(replacing && Some(c) == child))
        };
        let doctype_after_child = child.is_some_and(|c| {
            std::iter::successors(self.next_sibling_of(c), |&s| self.next_sibling_of(s))
                .any(|s| kind_of(s) == Some(Kind::Doctype))
        });
        let element_before_child = child.is_some_and(|c| {
            std::iter::successors(self.prev_sibling_of(c), |&s| self.prev_sibling_of(s))
                .any(|s| kind_of(s) == Some(Kind::Element))
        });
        let child_is_doctype = !replacing && child.and_then(kind_of) == Some(Kind::Doctype);
        let element_fits = !has_other(Kind::Element) && !child_is_doctype && !doctype_after_child;

        match node_kind {
            Kind::Fragment => {
                let elements = self
                    .children(node)
                    .filter(|&c| kind_of(c) == Some(Kind::Element))
                    .count();
                let has_text = self.children(node).any(|c| kind_of(c) == Some(Kind::Text));
                !has_text && (elements == 0 || (elements == 1 && element_fits))
            }
            Kind::Element => element_fits,
            Kind::Doctype => {
                !has_other(Kind::Doctype)
                    && !element_before_child
                    && !(child.is_none() && has_other(Kind::Element))
            }
            _ => true,
        }
    }

    /// Whether `ancestor` is `node` or above it, crossing from shadow roots
    /// and template content up to their hosts.
    fn is_host_including_inclusive_ancestor(&self, ancestor: NodeId, node: NodeId) -> bool {
        std::iter::successors(Some(node), |&id| self.host_including_parent(id))
            .any(|id| id == ancestor)
    }

    pub(crate) fn host_including_parent(&self, id: NodeId) -> Option<NodeId> {
        let node = self.nodes.get(id)?;
        match node.data {
            NodeData::ShadowRoot { host, .. } => Some(host),
            NodeData::DocumentFragment { host, .. } => host,
            _ => node.parent,
        }
    }

    /// The topmost node above `id`, crossing into hosts.
    pub(crate) fn host_including_root(&self, id: NodeId) -> NodeId {
        std::iter::successors(Some(id), |&n| self.host_including_parent(n))
            .last()
            .unwrap_or(id)
    }

    /// The DOM standard's "insert", after the checks passed.
    fn insert(&mut self, node: NodeId, parent: NodeId, reference: Option<NodeId>) {
        let is_fragment = self.nodes.get(node).map(Kind::of) == Some(Kind::Fragment);
        let nodes: Vec<NodeId> = if is_fragment {
            let children: Vec<NodeId> = self.children(node).collect();
            for &child in &children {
                self.remove(child);
            }
            children
        } else {
            self.remove(node);
            vec![node]
        };
        if nodes.is_empty() {
            return;
        }

        let previous_sibling = match reference {
            Some(reference) => self.prev_sibling_of(reference),
            None => self.nodes.get(parent).and_then(Node::last_child),
        };
        for &inserted in &nodes {
            self.link_before(parent, inserted, reference);
        }
        let connected = self.is_connected(parent);
        if connected {
            let in_document_tree = self.in_document_tree(parent);
            for &inserted in &nodes {
                self.connect_subtree(inserted, in_document_tree);
            }
        }
        self.mutation_queue.push(MutationRecord::ChildList {
            target: parent,
            added: nodes.clone(),
            removed: Vec::new(),
            previous_sibling,
            next_sibling: reference,
        });
        if connected {
            for &inserted in &nodes {
                // Text carries no style of its own; its box comes from the parent's rebuild.
                if self.element(inserted).is_some() {
                    self.mark_style(
                        inserted,
                        NodeFlags::NEEDS_STYLE | NodeFlags::RESTYLE_SUBTREE,
                    );
                }
            }
            self.mark_box_rebuild(parent);
            self.restyle_affected_children(parent);
            // New controls, or a new first `legend`, can change who is
            // disabled (spec 0006, AC-16).
            if self.is_html(parent, local_name!("fieldset")) {
                self.refresh_disabled(parent);
            } else {
                for &inserted in &nodes {
                    self.refresh_disabled(inserted);
                }
            }
            if self.is_style_source(parent)
                || nodes
                    .iter()
                    .any(|&inserted| self.holds_style_source(inserted))
            {
                self.style_sources_changed = true;
            }
            if nodes
                .iter()
                .any(|&inserted| self.holds_image_source(inserted))
            {
                self.image_sources_changed = true;
            }
            self.needs_frame = true;
        }
    }

    /// The DOM standard's "remove", after the checks passed. Does nothing for
    /// a parentless node.
    fn remove(&mut self, node: NodeId) {
        let Some(entry) = self.nodes.get(node) else {
            return;
        };
        let Some(parent) = entry.parent else {
            return;
        };
        let (previous_sibling, next_sibling) = (entry.prev_sibling, entry.next_sibling);
        let connected = self.is_connected(parent);
        let in_document_tree = connected && self.in_document_tree(parent);

        self.unlink(node);
        if connected {
            self.disconnect_subtree(node, in_document_tree);
        }
        self.mutation_queue.push(MutationRecord::ChildList {
            target: parent,
            added: Vec::new(),
            removed: vec![node],
            previous_sibling,
            next_sibling,
        });
        if connected {
            self.mark_box_rebuild(parent);
            self.restyle_affected_children(parent);
            if self.is_html(parent, local_name!("fieldset")) {
                self.refresh_disabled(parent);
            }
            if self.is_style_source(parent) || self.holds_style_source(node) {
                self.style_sources_changed = true;
            }
            if self.holds_image_source(node) {
                self.image_sources_changed = true;
            }
            self.needs_frame = true;
        } else {
            // A pinned node moved out of a detached tree may have been all
            // that kept the rest of that tree alive.
            let old_root = self.host_including_root(parent);
            self.detached_roots.push(old_root);
        }
        self.detached_roots.push(node);
    }

    fn attribute_changed(&mut self, element: NodeId, name: QualName, old_value: Option<String>) {
        let is_id = name.ns == ns!("") && name.local == local_name!("id");
        let is_class = name.ns == ns!("") && name.local == local_name!("class");
        let before = self.element(element);
        let old_id = before.and_then(|e| e.id.clone()).filter(|_| is_id);
        let old_classes = before
            .filter(|_| is_class)
            .map(|e| e.classes.clone())
            .unwrap_or_default();
        if is_id {
            self.refresh_id(element);
        } else if is_class {
            self.refresh_classes(element);
        }
        if self.is_connected(element) {
            let map = self.invalidation.as_deref();
            let hint = match self.element_ref(element) {
                Some(view) if is_id => {
                    let new = css::MatchElement::id(&view);
                    let change = ElementChange::Id {
                        old: old_id.as_ref(),
                        new,
                    };
                    css::restyle_hint(map, &view, &change)
                }
                Some(view) if is_class => {
                    // Only the classes that came or went can change a match.
                    let new = css::MatchElement::classes(&view);
                    let changed: Vec<Atom> = old_classes
                        .iter()
                        .filter(|class| !new.contains(class))
                        .chain(new.iter().filter(|class| !old_classes.contains(class)))
                        .cloned()
                        .collect();
                    css::restyle_hint(map, &view, &ElementChange::Class(&changed))
                }
                Some(view) => css::restyle_hint(map, &view, &ElementChange::Attribute(&name)),
                None => RestyleHint::NONE,
            };
            self.apply_restyle_hint(element, hint);
            if name.ns == ns!("") {
                self.form_attribute_changed(element, &name.local);
            }
            if self.is_style_source(element) {
                self.style_sources_changed = true;
            }
            if self.is_image_source(element) {
                self.image_sources_changed = true;
            }
            self.needs_frame = true;
        }
        self.mutation_queue.push(MutationRecord::Attribute {
            target: element,
            name,
            old_value,
        });
    }

    /// Recaches the element's `id` from its attribute and keeps `id_index` current.
    fn refresh_id(&mut self, element: NodeId) {
        let indexed = self.is_connected(element) && self.in_document_tree(element);
        let Some(data) = self.nodes.get_mut(element).and_then(Node::as_element_mut) else {
            return;
        };
        let new = data
            .attribute(&QualName::attribute(local_name!("id")))
            .filter(|value| !value.is_empty())
            .map(Atom::from);
        let old = std::mem::replace(&mut data.id, new.clone());
        if indexed && old != new {
            if let Some(old) = old {
                self.unindex_id(&old, element);
            }
            if let Some(new) = new {
                self.id_index.entry(new).or_default().push(element);
            }
        }
    }

    /// Recaches the element's class list: split on ASCII whitespace, no repeats.
    fn refresh_classes(&mut self, element: NodeId) {
        let Some(data) = self.nodes.get_mut(element).and_then(Node::as_element_mut) else {
            return;
        };
        let mut classes: Vec<Atom> = Vec::new();
        let value = data
            .attribute(&QualName::attribute(local_name!("class")))
            .unwrap_or_default();
        for class in value.split_ascii_whitespace().map(Atom::from) {
            if !classes.contains(&class) {
                classes.push(class);
            }
        }
        data.classes = classes;
    }

    fn unindex_id(&mut self, id: &Atom, element: NodeId) {
        if let Some(elements) = self.id_index.get_mut(id) {
            elements.retain(|&e| e != element);
            if elements.is_empty() {
                self.id_index.remove(id);
            }
        }
    }

    /// Whether `id` sits in the document's own tree rather than a shadow tree
    /// or a detached one.
    fn in_document_tree(&self, id: NodeId) -> bool {
        std::iter::successors(Some(id), |&n| self.nodes.get(n).and_then(Node::parent)).last()
            == Some(self.root)
    }

    /// Sets `IS_CONNECTED` on `root` and everything below it, shadow trees
    /// included (template content never connects), indexing ids and noting
    /// iframes on the way.
    fn connect_subtree(&mut self, root: NodeId, in_document_tree: bool) {
        let mut stack = vec![(root, in_document_tree)];
        while let Some((id, in_document_tree)) = stack.pop() {
            stack.extend(self.children(id).map(|child| (child, in_document_tree)));
            let Some(node) = self.nodes.get_mut(id) else {
                continue;
            };
            node.flags.insert(NodeFlags::IS_CONNECTED);
            let Some(element) = node.as_element() else {
                continue;
            };
            let (shadow, kind) = (element.shadow_root, element.kind);
            let index = element.id.clone().filter(|_| in_document_tree);
            if let Some(shadow) = shadow {
                stack.push((shadow, false));
            }
            if let Some(atom) = index {
                self.id_index.entry(atom).or_default().push(id);
            }
            if matches!(kind, ElementKind::IFrame { .. }) {
                self.frame_changes.push(FrameChange::Connected(id));
            }
        }
    }

    /// Clears `IS_CONNECTED` and the dirty flags on `root` and everything
    /// below it, drops styles and box ids (the next box rebuild drops the
    /// boxes themselves), takes ids out of `id_index` and lets go of child
    /// frames.
    fn disconnect_subtree(&mut self, root: NodeId, in_document_tree: bool) {
        let mut stack = vec![(root, in_document_tree)];
        while let Some((id, in_document_tree)) = stack.pop() {
            // A focused element that leaves loses focus, with no
            // replacement (spec 0006, AC-13).
            if self.focused == Some(id) {
                self.set_focus(None);
            }
            stack.extend(self.children(id).map(|child| (child, in_document_tree)));
            let Some(node) = self.nodes.get_mut(id) else {
                continue;
            };
            node.flags.remove(NodeFlags::IS_CONNECTED.union(DIRTY));
            node.layout_box = None;
            let Some(element) = node.as_element_mut() else {
                continue;
            };
            element.style = None;
            let mut frame = None;
            if let ElementKind::IFrame { frame: slot } = &mut element.kind {
                frame = slot.take();
            }
            let shadow = element.shadow_root;
            let index = element.id.clone().filter(|_| in_document_tree);
            if let Some(shadow) = shadow {
                stack.push((shadow, false));
            }
            if let Some(atom) = index {
                self.unindex_id(&atom, id);
            }
            if let Some(frame) = frame {
                self.frame_changes.push(FrameChange::Disconnected(frame));
            }
        }
    }

    /// An attribute that form controls read changed on `element`: `disabled`
    /// moves `:disabled` (spec 0006, AC-16), and an `<input>`'s `value`,
    /// `placeholder` and `type` change the text it shows and its
    /// placeholder style (AC-17).
    fn form_attribute_changed(&mut self, element: NodeId, name: &LocalName) {
        if *name == local_name!("disabled") {
            self.refresh_disabled(element);
        }
        let shows_text = matches!(
            *name,
            local_name!("value") | local_name!("placeholder") | local_name!("type")
        );
        if shows_text && self.is_html(element, local_name!("input")) {
            self.mark_style(element, NodeFlags::NEEDS_STYLE);
            self.mark_box_rebuild(element);
        }
    }

    fn apply_restyle_hint(&mut self, element: NodeId, hint: RestyleHint) {
        let mut flags = NodeFlags::empty();
        if hint.contains(RestyleHint::SELF) {
            flags.insert(NodeFlags::NEEDS_STYLE);
        }
        if hint.contains(RestyleHint::SUBTREE) {
            flags.insert(NodeFlags::NEEDS_STYLE | NodeFlags::RESTYLE_SUBTREE);
        }
        if flags != NodeFlags::empty() {
            self.mark_style(element, flags);
        }
        if hint.contains(RestyleHint::LATER_SIBLINGS) {
            let mut current = self.next_sibling_of(element);
            while let Some(id) = current {
                current = self.next_sibling_of(id);
                if self.element(id).is_some() {
                    self.mark_style(id, NodeFlags::NEEDS_STYLE | NodeFlags::RESTYLE_SUBTREE);
                }
            }
        }
        // A `:has()` subject must be restyled when a descendant or a
        // following sibling changes (spec 0010, AC-15). Walk the ancestors
        // and restyle any that are a `:has()` subject.
        let mut current = self.nodes.get(element).and_then(|node| node.parent());
        while let Some(ancestor) = current {
            let has_subject = self
                .nodes
                .get(ancestor)
                .is_some_and(|node| node.flags.contains(NodeFlags::HAS_SUBJECT));
            if has_subject {
                self.mark_subtree_dirty(ancestor);
            }
            current = self.nodes.get(ancestor).and_then(|node| node.parent());
        }
    }

    /// After children were inserted or removed, restyles every child of
    /// `parent` a sibling selector was matched against: `:first-child`,
    /// `:last-child`, `+` and `~` may now answer differently for any of them.
    fn restyle_affected_children(&mut self, parent: NodeId) {
        let affected: Vec<NodeId> = self
            .children(parent)
            .filter(|&id| {
                self.nodes.get(id).is_some_and(|node| {
                    node.flags
                        .contains(NodeFlags::AFFECTED_BY_SIBLING_SELECTORS)
                })
            })
            .collect();
        for id in affected {
            self.mark_style(id, NodeFlags::NEEDS_STYLE | NodeFlags::RESTYLE_SUBTREE);
        }
    }

    /// Whether `root` is a `<style>` or `<link>` or holds one.
    fn holds_style_source(&self, root: NodeId) -> bool {
        self.is_style_source(root) || self.descendants(root).any(|id| self.is_style_source(id))
    }

    /// Whether `id` is an `<img>`.
    fn is_image_source(&self, id: NodeId) -> bool {
        self.element(id)
            .is_some_and(|element| matches!(element.kind, ElementKind::Img { .. }))
    }

    /// Whether `root` is an `<img>` or holds one.
    fn holds_image_source(&self, root: NodeId) -> bool {
        self.is_image_source(root) || self.descendants(root).any(|id| self.is_image_source(id))
    }

    /// Sets style `flags` on a connected node and `DESCENDANT_NEEDS_STYLE` on
    /// every flat tree ancestor.
    pub(crate) fn mark_style(&mut self, id: NodeId, flags: NodeFlags) {
        let Some(node) = self.nodes.get_mut(id) else {
            return;
        };
        if !node.is_connected() {
            return;
        }
        node.flags.insert(flags);
        self.mark_ancestors(id, NodeFlags::DESCENDANT_NEEDS_STYLE);
    }

    /// Marks `id` and its subtree for restyle: used by container query
    /// passes when a container's laid out size changes (spec 0010, AC-18).
    pub fn mark_subtree_dirty(&mut self, id: NodeId) {
        self.mark_style(id, NodeFlags::NEEDS_STYLE | NodeFlags::RESTYLE_SUBTREE);
    }

    /// Sets `NEEDS_BOX_REBUILD` on a connected node (a shadow root's host for
    /// a shadow root) and `DESCENDANT_NEEDS_BOX_REBUILD` up the flat tree.
    pub(crate) fn mark_box_rebuild(&mut self, id: NodeId) {
        let id = self.flat_node(id);
        let Some(node) = self.nodes.get_mut(id) else {
            return;
        };
        if !node.is_connected() {
            return;
        }
        node.flags.insert(NodeFlags::NEEDS_BOX_REBUILD);
        self.mark_ancestors(id, NodeFlags::DESCENDANT_NEEDS_BOX_REBUILD);
    }

    /// Sets `flag` on every flat tree ancestor of `id`, up to the document.
    /// Shadow roots are skipped: in the flat tree their children hang off the
    /// host.
    fn mark_ancestors(&mut self, id: NodeId, flag: NodeFlags) {
        let mut current = self.flat_parent(id);
        while let Some(ancestor) = current {
            if let Some(node) = self.nodes.get_mut(ancestor) {
                node.flags.insert(flag);
            }
            current = self.flat_parent(ancestor);
        }
    }

    /// The parent of `id` in the flat tree: a shadow root's children hang
    /// off its host. `None` for the document, a detached root, or a freed id.
    pub fn flat_parent(&self, id: NodeId) -> Option<NodeId> {
        let parent = self.nodes.get(id)?.parent?;
        Some(self.flat_node(parent))
    }

    /// `id`, or its host if `id` is a shadow root.
    fn flat_node(&self, id: NodeId) -> NodeId {
        match self.nodes.get(id).map(|n| &n.data) {
            Some(NodeData::ShadowRoot { host, .. }) => *host,
            _ => id,
        }
    }

    fn next_sibling_of(&self, id: NodeId) -> Option<NodeId> {
        self.nodes.get(id).and_then(Node::next_sibling)
    }

    fn prev_sibling_of(&self, id: NodeId) -> Option<NodeId> {
        self.nodes.get(id).and_then(Node::prev_sibling)
    }

    /// Takes `id` out of its parent's child list, if it has a parent.
    fn unlink(&mut self, id: NodeId) {
        let Some(node) = self.nodes.get_mut(id) else {
            return;
        };
        let (parent, prev, next) = (
            node.parent.take(),
            node.prev_sibling.take(),
            node.next_sibling.take(),
        );
        let Some(parent) = parent else {
            return;
        };
        match prev.and_then(|p| self.nodes.get_mut(p)) {
            Some(prev_node) => prev_node.next_sibling = next,
            None => {
                if let Some(parent_node) = self.nodes.get_mut(parent) {
                    parent_node.first_child = next;
                }
            }
        }
        match next.and_then(|n| self.nodes.get_mut(n)) {
            Some(next_node) => next_node.prev_sibling = prev,
            None => {
                if let Some(parent_node) = self.nodes.get_mut(parent) {
                    parent_node.last_child = prev;
                }
            }
        }
    }

    /// Links a parentless `child` into `parent` before `reference`, or last.
    fn link_before(&mut self, parent: NodeId, child: NodeId, reference: Option<NodeId>) {
        let prev = match reference {
            Some(reference) => self.prev_sibling_of(reference),
            None => self.nodes.get(parent).and_then(Node::last_child),
        };
        match prev.and_then(|p| self.nodes.get_mut(p)) {
            Some(prev_node) => prev_node.next_sibling = Some(child),
            None => {
                if let Some(parent_node) = self.nodes.get_mut(parent) {
                    parent_node.first_child = Some(child);
                }
            }
        }
        match reference.and_then(|r| self.nodes.get_mut(r)) {
            Some(next_node) => next_node.prev_sibling = Some(child),
            None => {
                if let Some(parent_node) = self.nodes.get_mut(parent) {
                    parent_node.last_child = Some(child);
                }
            }
        }
        if let Some(child_node) = self.nodes.get_mut(child) {
            child_node.parent = Some(parent);
            child_node.prev_sibling = prev;
            child_node.next_sibling = reference;
        }
    }
}

/// The DOM standard's list of elements that may host a shadow root: some
/// HTML elements, and any valid custom element name.
fn can_host_shadow(name: &QualName) -> bool {
    if name.ns != ns!("http://www.w3.org/1999/xhtml") {
        return false;
    }
    let local = &name.local;
    let listed = [
        local_name!("article"),
        local_name!("aside"),
        local_name!("blockquote"),
        local_name!("body"),
        local_name!("div"),
        local_name!("footer"),
        local_name!("h1"),
        local_name!("h2"),
        local_name!("h3"),
        local_name!("h4"),
        local_name!("h5"),
        local_name!("h6"),
        local_name!("header"),
        local_name!("main"),
        local_name!("nav"),
        local_name!("p"),
        local_name!("section"),
        local_name!("span"),
    ];
    listed.contains(local) || is_valid_custom_element_name(local)
}

/// A lowercase ASCII letter first, a hyphen somewhere, no ASCII uppercase,
/// and none of the reserved SVG and MathML names.
fn is_valid_custom_element_name(name: &LocalName) -> bool {
    const RESERVED: [&str; 8] = [
        "annotation-xml",
        "color-profile",
        "font-face",
        "font-face-src",
        "font-face-uri",
        "font-face-format",
        "font-face-name",
        "missing-glyph",
    ];
    let name: &str = name;
    name.starts_with(|c: char| c.is_ascii_lowercase())
        && name.contains('-')
        && !name.chars().any(|c| c.is_ascii_uppercase())
        && !RESERVED.contains(&name)
}
