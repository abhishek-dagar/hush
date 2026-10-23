//! Styles on elements: setting them with their damage, and the style
//! sharing cache the style pass draws on (spec 0002, Computed style).

use std::panic::Location;
use std::rc::Rc;

use base::atoms::{Atom, QualName};
use base::{local_name, ns};
use css::{ComputedStyle, InvalidationMap, StyleDamage};

use crate::node::{ElementData, ElementState, Node, NodeFlags};
use crate::{Document, DomError, NodeId};

impl Document {
    /// Sets an element's computed style and returns what the change costs
    /// (AC-8). The style pass calls this; until the cascade exists, styles
    /// are assigned directly. Only connected elements carry a style.
    ///
    /// The damage is recorded for the next frame: `RebuildBoxes` (including
    /// gaining or losing a style) marks the flat parent's boxes for rebuild,
    /// and `Repaint` or `Relayout` is queued for [`Document::take_style_damage`].
    #[track_caller]
    pub fn set_style(
        &mut self,
        id: NodeId,
        style: Option<Rc<ComputedStyle>>,
    ) -> Result<StyleDamage, DomError> {
        let node = self.live_mut(id, Location::caller())?;
        if !node.is_connected() && style.is_some() {
            return Err(DomError::InvalidState);
        }
        let element = node.as_element_mut().ok_or(DomError::InvalidState)?;
        let old = std::mem::replace(&mut element.style, style);
        let damage = match (&old, &element.style) {
            (Some(old), Some(new)) => StyleDamage::compute(old, new),
            (None, None) => StyleDamage::None,
            // The element gains or loses its box.
            _ => StyleDamage::RebuildBoxes,
        };
        match damage {
            StyleDamage::None => {}
            StyleDamage::RebuildBoxes => {
                // The element's own box changes, so its parent's boxes are rebuilt.
                if let Some(parent) = self.flat_parent(id) {
                    self.mark_box_rebuild(parent);
                }
                self.needs_frame = true;
            }
            StyleDamage::Repaint | StyleDamage::Relayout => {
                self.style_damage.push((id, damage));
                self.needs_frame = true;
            }
        }
        Ok(damage)
    }

    /// Sets the `::placeholder` style of the `<input>` `id` (spec 0006,
    /// AC-17). A change rebuilds the input's boxes, whose text draws with
    /// it.
    #[track_caller]
    pub fn set_placeholder_style(
        &mut self,
        id: NodeId,
        style: Option<Rc<ComputedStyle>>,
    ) -> Result<(), DomError> {
        let element = self
            .live_mut(id, Location::caller())?
            .as_element_mut()
            .ok_or(DomError::InvalidState)?;
        if element.placeholder == style {
            return Ok(());
        }
        element.placeholder = style;
        self.mark_box_rebuild(id);
        self.needs_frame = true;
        Ok(())
    }

    /// Takes the elements whose style changed in a way that needs a repaint
    /// or relayout, oldest first. An element may have been removed or freed
    /// since, so check it before use.
    pub fn take_style_damage(&mut self) -> Vec<(NodeId, StyleDamage)> {
        std::mem::take(&mut self.style_damage)
    }

    /// A view of the element `id` for selector matching, if it is a live element.
    pub fn element_ref(&self, id: NodeId) -> Option<ElementRef<'_>> {
        let node = self.nodes.get(id)?;
        let data = node.as_element()?;
        Some(ElementRef {
            doc: self,
            id,
            node,
            data,
        })
    }

    /// Installs the selector dependencies of the document's rules, so later
    /// attribute changes restyle only what a rule can reach (spec 0004,
    /// AC-21). The page installs a new map whenever its rules change.
    pub fn set_invalidation_map(&mut self, map: Option<Rc<InvalidationMap>>) {
        self.invalidation = map;
    }

    /// Whether a `<style>` or `<link>` element joined or left the document,
    /// or its text or attributes changed, since the last call.
    pub fn take_style_sources_changed(&mut self) -> bool {
        std::mem::take(&mut self.style_sources_changed)
    }

    /// Marks every element for restyle: the rules changed, or a media query
    /// result or the viewport did.
    pub fn restyle_all(&mut self) {
        if let Some(root) = self.document_element() {
            self.mark_style(root, NodeFlags::NEEDS_STYLE | NodeFlags::RESTYLE_SUBTREE);
            self.needs_frame = true;
        }
    }

    /// Whether `id` is an HTML `<style>` or `<link>`, which carry style sheets.
    pub(crate) fn is_style_source(&self, id: NodeId) -> bool {
        self.element(id).is_some_and(|element| {
            let name = element.name();
            name.ns == ns!("http://www.w3.org/1999/xhtml")
                && (name.local == local_name!("style") || name.local == local_name!("link"))
        })
    }

    /// A style the element `id` can share with a sibling styled just before
    /// it, from the style sharing cache (AC-7). The style pass tries this
    /// before running the cascade for an element.
    pub fn shared_style(&mut self, id: NodeId) -> Option<Rc<ComputedStyle>> {
        let parent = self.flat_parent(id)?;
        // The cache resolves its keys through the document, so it steps out
        // of the document while it looks.
        let mut cache = std::mem::take(&mut self.style_sharing);
        let found = self.element_ref(id).and_then(|element| {
            let parent_style = self.element(parent).and_then(ElementData::style);
            cache.lookup(parent, parent_style, &element, |key| self.element_ref(key))
        });
        self.style_sharing = cache;
        found
    }

    /// Offers the element `id`'s current style to later siblings through the
    /// style sharing cache. The style pass calls this after styling it.
    pub fn remember_style(&mut self, id: NodeId) {
        let Some(parent) = self.flat_parent(id) else {
            return;
        };
        let mut cache = std::mem::take(&mut self.style_sharing);
        if let Some(element) = self.element_ref(id)
            && let Some(style) = element.data.style()
        {
            let parent_style = self.element(parent).and_then(ElementData::style);
            cache.insert(parent, parent_style, id, &element, Rc::clone(style));
        }
        self.style_sharing = cache;
    }

    /// Forgets every sharing candidate. The style pass starts from an empty
    /// cache each time.
    pub(crate) fn clear_style_sharing(&mut self) {
        self.style_sharing.clear();
    }
}

/// An element seen through `css::MatchElement`, so `css` never names a `dom` type.
#[derive(Clone, Copy)]
pub struct ElementRef<'a> {
    doc: &'a Document,
    id: NodeId,
    node: &'a Node,
    data: &'a ElementData,
}

impl ElementRef<'_> {
    /// The element's id in its document.
    pub fn id(&self) -> NodeId {
        self.id
    }
}

impl css::MatchElement for ElementRef<'_> {
    fn name(&self) -> &QualName {
        self.data.name()
    }

    fn id(&self) -> Option<&Atom> {
        self.data.id()
    }

    fn classes(&self) -> &[Atom] {
        self.data.classes()
    }

    fn attribute(&self, name: &QualName) -> Option<&str> {
        self.data.attribute(name)
    }

    fn parent_element(&self) -> Option<Self> {
        self.doc.element_ref(self.node.parent()?)
    }

    fn prev_sibling_element(&self) -> Option<Self> {
        let nodes = &self.doc.nodes;
        std::iter::successors(self.node.prev_sibling(), |&id| {
            nodes.get(id).and_then(Node::prev_sibling)
        })
        .find_map(|id| self.doc.element_ref(id))
    }

    fn next_sibling_element(&self) -> Option<Self> {
        let nodes = &self.doc.nodes;
        std::iter::successors(self.node.next_sibling(), |&id| {
            nodes.get(id).and_then(Node::next_sibling)
        })
        .find_map(|id| self.doc.element_ref(id))
    }

    fn is_root(&self) -> bool {
        self.node.parent() == Some(self.doc.root)
    }

    fn same_attributes(&self, other: &Self) -> bool {
        self.data.attributes() == other.data.attributes()
    }

    fn same_state(&self, other: &Self) -> bool {
        self.data.state() == other.data.state()
    }

    fn is_in_state(&self, state: css::PseudoState) -> bool {
        self.data.state().contains(ElementState::from(state))
    }

    fn has_shadow_root(&self) -> bool {
        self.data.shadow_root().is_some()
    }

    fn is_affected_by_sibling_selectors(&self) -> bool {
        self.node
            .flags
            .contains(NodeFlags::AFFECTED_BY_SIBLING_SELECTORS)
    }
}
