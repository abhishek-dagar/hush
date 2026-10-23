//! The DOM: one arena of nodes per document, addressed by [`NodeId`]
//! (spec 0002, DOM).
//!
//! Tree links change only through the [`Document`] mutation API (`mutation`).
//! Detached subtrees live until the end of task sweep finds no pin in them
//! (`lifetime`). Mutations set dirty flags that the style pass and box
//! rebuild walk (`dirty`).

#![forbid(unsafe_code)]

mod dirty;
mod dump;
mod focus;
mod lifetime;
mod mutation;
mod node;
mod style;

use std::collections::HashMap;
use std::panic::Location;
use std::rc::Rc;

use base::atoms::Atom;
use base::{Arena, BoxId, Id};
use css::{InvalidationMap, SharingCache, StyleDamage};
use tracing::warn;

pub use mutation::MutationRecord;
pub use node::{
    Attribute, DoctypeData, ElementData, ElementKind, ElementState, ImageState, Node, NodeData,
    NodeFlags, ShadowMode,
};
pub use style::ElementRef;

/// A node in one document's arena.
pub type NodeId = Id<Node>;

/// One document inside a page: the main frame is 0, each iframe gets the
/// next number. Assigned by the page and never reused (spec 0002, Ids).
///
/// It lives here rather than in `page` because iframe elements and
/// [`NodeRef`] name it, and `dom` cannot depend on `page`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FrameId(pub u32);

impl FrameId {
    /// The page's top level document.
    pub const MAIN: Self = Self(0);
}

/// A node named outside its own document: script wrappers, hit test
/// results, focus, a child frame's iframe element.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeRef {
    /// The document the node lives in.
    pub frame: FrameId,
    /// The node in that document.
    pub node: NodeId,
}

/// A rejected mutation. The tree is left exactly as it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DomError {
    /// The change would break the tree's shape (DOM `HierarchyRequestError`).
    #[error("the node cannot be inserted there")]
    HierarchyRequest,
    /// A node is not where the call expects it, or was freed (DOM `NotFoundError`).
    #[error("the node was not found")]
    NotFound,
    /// The node is the wrong kind for this call (DOM `InvalidStateError`).
    #[error("the node is not in a state that allows this")]
    InvalidState,
    /// The element cannot do this, such as host a shadow root (DOM `NotSupportedError`).
    #[error("the operation is not supported on this node")]
    NotSupported,
}

/// How standards compliant the page asked to be, from its doctype.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum QuirksMode {
    /// Standards mode.
    #[default]
    NoQuirks,
    /// Almost standards mode.
    LimitedQuirks,
    /// Quirks mode.
    Quirks,
}

/// An iframe element joined or left the document. The page acts on these at
/// the end of the task (spec 0002, Frames).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameChange {
    /// The iframe element became connected and needs a child frame.
    Connected(NodeId),
    /// An iframe element left the document; tear down this frame.
    Disconnected(FrameId),
}

/// One document: the arena that owns its nodes, plus document wide state.
pub struct Document {
    nodes: Arena<Node>,
    root: NodeId,
    quirks_mode: QuirksMode,
    url: String,
    encoding: &'static str,
    /// Connected elements in the document tree (not in shadow trees) by `id`.
    id_index: HashMap<Atom, Vec<NodeId>>,
    /// Sweep candidates: roots of trees that may have become unreachable.
    detached_roots: Vec<NodeId>,
    mutation_queue: Vec<MutationRecord>,
    frame_changes: Vec<FrameChange>,
    /// A change the next frame must show happened since the page last looked.
    needs_frame: bool,
    /// Siblings recently styled, for the style pass to share from (AC-7).
    style_sharing: SharingCache<NodeId>,
    /// Elements whose new style needs a repaint or relayout, in the order
    /// they changed. Style changes that rebuild boxes set flags instead.
    style_damage: Vec<(NodeId, StyleDamage)>,
    /// The selector dependencies of the document's rules, so an attribute
    /// change restyles only what a rule can reach (spec 0004, AC-21).
    /// `None` until the page installs one; then every change restyles the
    /// subtree.
    invalidation: Option<Rc<InvalidationMap>>,
    /// A `<style>` or `<link>` joined or left, or its text or attributes
    /// changed, since the page last looked.
    style_sources_changed: bool,
    /// An `<img>` joined or left, or its attributes changed, since the
    /// page last looked.
    image_sources_changed: bool,
    /// The focused element, pinned while it holds focus (spec 0006, Data
    /// model, Focus).
    focused: Option<NodeId>,
}

impl Document {
    /// An empty document at `url`: just the document node.
    pub fn new(url: impl Into<String>) -> Self {
        let mut nodes = Arena::new();
        let mut document = Node::new(NodeData::Document);
        document.flags.insert(NodeFlags::IS_CONNECTED);
        let root = nodes.insert(document);
        Self {
            nodes,
            root,
            quirks_mode: QuirksMode::default(),
            url: url.into(),
            encoding: "UTF-8",
            id_index: HashMap::new(),
            detached_roots: Vec::new(),
            mutation_queue: Vec::new(),
            frame_changes: Vec::new(),
            needs_frame: false,
            style_sharing: SharingCache::new(),
            style_damage: Vec::new(),
            invalidation: None,
            style_sources_changed: false,
            image_sources_changed: false,
            focused: None,
        }
    }

    /// The document node.
    pub fn root(&self) -> NodeId {
        self.root
    }

    /// The quirks mode, set from the doctype by the parser.
    pub fn quirks_mode(&self) -> QuirksMode {
        self.quirks_mode
    }

    /// Sets the quirks mode. The parser calls this from the doctype.
    pub fn set_quirks_mode(&mut self, mode: QuirksMode) {
        self.quirks_mode = mode;
    }

    /// The document's URL.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The character encoding's name.
    pub fn encoding(&self) -> &'static str {
        self.encoding
    }

    /// Sets the encoding's name, once the parser has picked it.
    pub fn set_encoding(&mut self, encoding: &'static str) {
        self.encoding = encoding;
    }

    /// How many nodes the arena holds, connected or not.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Every live node, connected or not, in slot order.
    pub fn node_ids(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.nodes.iter().map(|(id, _)| id)
    }

    /// The node for `id`.
    ///
    /// A freed id is a bug in the caller: a debug build panics with the id and
    /// the slot's current generation, a release build logs and returns `None` (AC-3).
    #[track_caller]
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        let node = self.nodes.get(id);
        if node.is_none() {
            self.report_stale(id, Location::caller());
        }
        node
    }

    /// Whether `id` still names a node. Unlike [`Document::node`], a freed id
    /// is not reported, so use this only where an id may rightly be stale.
    pub fn is_alive(&self, id: NodeId) -> bool {
        self.nodes.contains(id)
    }

    /// Whether `id` is in the document, through shadow roots too.
    pub fn is_connected(&self, id: NodeId) -> bool {
        self.nodes.get(id).is_some_and(Node::is_connected)
    }

    /// Whether `id` is inside a `<template>`'s content, where nothing renders
    /// or runs.
    pub fn is_inert(&self, id: NodeId) -> bool {
        let root =
            std::iter::successors(Some(id), |&n| self.nodes.get(n).and_then(Node::parent)).last();
        root.and_then(|r| self.nodes.get(r))
            .is_some_and(|n| n.flags.contains(NodeFlags::IS_INERT))
    }

    /// The document element (`<html>`), if there is one.
    pub fn document_element(&self) -> Option<NodeId> {
        self.children(self.root)
            .find(|&id| self.nodes.get(id).is_some_and(|n| n.as_element().is_some()))
    }

    /// The plain child list of `id`, ignoring shadow roots.
    pub fn children(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        let first = self.nodes.get(id).and_then(Node::first_child);
        std::iter::successors(first, |&child| {
            self.nodes.get(child).and_then(Node::next_sibling)
        })
    }

    /// The children of `id` in the flat tree, the one traversal style, box
    /// building and hit testing use (AC-15): a shadow host's shadow root
    /// children, otherwise the plain child list. Slot assignment arrives with
    /// the shadow DOM slice, behind this same function.
    pub fn flat_children(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        let shadow = self
            .nodes
            .get(id)
            .and_then(Node::as_element)
            .and_then(ElementData::shadow_root);
        self.children(shadow.unwrap_or(id))
    }

    /// The first connected element in the document tree with this `id`, in
    /// tree order.
    pub fn element_by_id(&self, id: &Atom) -> Option<NodeId> {
        match self.id_index.get(id)?.as_slice() {
            [] => None,
            [only] => Some(*only),
            // Duplicate ids are rare, so walking the tree for them is fine.
            _ => self
                .descendants(self.root)
                .find(|&n| self.element(n).and_then(ElementData::id) == Some(id)),
        }
    }

    /// The changes queued since the last task ended, oldest first.
    pub fn mutation_records(&self) -> &[MutationRecord] {
        &self.mutation_queue
    }

    /// Takes the iframe changes the page must act on.
    pub fn take_frame_changes(&mut self) -> Vec<FrameChange> {
        std::mem::take(&mut self.frame_changes)
    }

    /// Whether a change since the last [`Document::take_needs_frame`] must be
    /// shown in a new frame.
    pub fn needs_frame(&self) -> bool {
        self.needs_frame
    }

    /// Reads and clears the needs frame mark.
    pub fn take_needs_frame(&mut self) -> bool {
        std::mem::take(&mut self.needs_frame)
    }

    /// Records the node's principal box. Called by box building.
    #[track_caller]
    pub fn set_layout_box(
        &mut self,
        id: NodeId,
        layout_box: Option<BoxId>,
    ) -> Result<(), DomError> {
        self.live_mut(id, Location::caller())?.layout_box = layout_box;
        Ok(())
    }

    /// Records the child frame showing a connected iframe's document. Called
    /// by the page after a [`FrameChange::Connected`].
    #[track_caller]
    pub fn set_iframe_frame(&mut self, id: NodeId, frame: FrameId) -> Result<(), DomError> {
        let node = self.live_mut(id, Location::caller())?;
        let connected = node.is_connected();
        match node.as_element_mut().map(|e| &mut e.kind) {
            Some(ElementKind::IFrame { frame: slot }) if connected => {
                *slot = Some(frame);
                Ok(())
            }
            _ => Err(DomError::InvalidState),
        }
    }

    /// Whether an `<img>` joined or left the document, or its attributes
    /// changed, since the last call: the page then looks for new images to
    /// load (spec 0004, child 0005).
    pub fn take_image_sources_changed(&mut self) -> bool {
        std::mem::take(&mut self.image_sources_changed)
    }

    /// What the `<img>` `id` shows, or `None` for any other node.
    pub fn image_state(&self, id: NodeId) -> Option<ImageState> {
        match self.element(id)?.kind {
            ElementKind::Img { image } => Some(image),
            _ => None,
        }
    }

    /// Sets what the `<img>` `id` shows, as its image loads or fails. A
    /// change rebuilds the boxes around it, since its size or its `alt`
    /// text may change with it (AC-29).
    #[track_caller]
    pub fn set_image_state(&mut self, id: NodeId, state: ImageState) -> Result<(), DomError> {
        let node = self.live_mut(id, Location::caller())?;
        let connected = node.is_connected();
        let parent = node.parent;
        let Some(ElementKind::Img { image }) = node.as_element_mut().map(|e| &mut e.kind) else {
            return Err(DomError::InvalidState);
        };
        if *image == state {
            return Ok(());
        }
        *image = state;
        if connected {
            self.mark_box_rebuild(parent.unwrap_or(id));
            self.needs_frame = true;
        }
        Ok(())
    }

    /// Marks that a sibling selector matched against `id`, so its changes
    /// and changes among its siblings restyle it. The style pass calls this
    /// for every element `css::MatchContext` reports.
    #[track_caller]
    pub fn mark_affected_by_sibling_selectors(&mut self, id: NodeId) -> Result<(), DomError> {
        let node = self.live_mut(id, Location::caller())?;
        if node.as_element().is_none() {
            return Err(DomError::InvalidState);
        }
        node.flags.insert(NodeFlags::AFFECTED_BY_SIBLING_SELECTORS);
        Ok(())
    }

    fn element(&self, id: NodeId) -> Option<&ElementData> {
        self.nodes.get(id).and_then(Node::as_element)
    }

    /// Every node below `id`, in tree order, not counting `id` itself or
    /// shadow trees. Uses a heap stack so deep trees cannot overflow.
    fn descendants(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        let mut stack: Vec<NodeId> = self.children_rev(id);
        std::iter::from_fn(move || {
            let next = stack.pop()?;
            stack.extend(self.children_rev(next));
            Some(next)
        })
    }

    fn children_rev(&self, id: NodeId) -> Vec<NodeId> {
        let mut children: Vec<NodeId> = self.children(id).collect();
        children.reverse();
        children
    }

    fn live(&self, id: NodeId, caller: &Location<'_>) -> Result<&Node, DomError> {
        self.nodes.get(id).ok_or_else(|| {
            self.report_stale(id, caller);
            DomError::NotFound
        })
    }

    fn live_mut(&mut self, id: NodeId, caller: &Location<'_>) -> Result<&mut Node, DomError> {
        if !self.nodes.contains(id) {
            self.report_stale(id, caller);
        }
        self.nodes.get_mut(id).ok_or(DomError::NotFound)
    }

    fn report_stale(&self, id: NodeId, caller: &Location<'_>) {
        let current = self.nodes.current_generation(id.index());
        if cfg!(debug_assertions) {
            panic!(
                "stale NodeId {id:?} used at {caller}: the slot's generation is now {current:?}"
            );
        }
        warn!(?id, ?current, %caller, "stale NodeId ignored");
    }
}

#[cfg(test)]
mod tests;
