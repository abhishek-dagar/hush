//! The page's side of the script boundary (spec 0004, child 0006): every
//! DOM operation the bindings ask for, over the main document, and the
//! page's timers.
//!
//! The page lends its document here for each call into the engine and
//! takes it back right after, so the engine's callbacks reach the DOM
//! through this shared host, and no borrow is ever held across a call
//! into the engine.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use base::atoms::{LocalName, QualName};
use base::ns;
use css::MatchContext;
use dom::{Document, FrameId, NodeData, NodeId, NodeRef};
use script::{
    ConsoleLevel, DomException, LocationAction, NodeKind, ReadyState, ScriptHost, TimerId,
};
use tracing::{debug, error, info, warn};

/// Timer nesting past which short delays are raised (HTML, timer
/// initialization steps).
const NESTING_CLAMP_LEVEL: u32 = 5;
/// The shortest delay a deeply nested timer gets.
const NESTED_MIN_DELAY_MS: f64 = 4.0;
/// The longest delay, as browsers cap it (2^31 - 1 ms).
const MAX_DELAY_MS: f64 = 2_147_483_647.0;

/// A navigation a script asked for (spec 0008, AC-27), for the browser.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ScriptNavigation {
    /// `location`: load this absolute URL.
    Load(String, LocationAction),
    /// `history.back`, `forward` or `go`.
    Go(i32),
}

#[derive(Debug, Clone, Copy)]
struct Timer {
    due: Instant,
    /// Its timer nesting level.
    nesting: u32,
}

/// The DOM and timers the script engine works on.
pub(crate) struct PageHost {
    /// The main document while the engine runs; an empty stand in while
    /// the page holds it.
    pub(crate) document: Document,
    /// `document.readyState`.
    pub(crate) ready_state: ReadyState,
    timers: BTreeMap<TimerId, Timer>,
    next_timer: u32,
    /// The nesting level of the timer whose callback is running, 0 outside
    /// one.
    pub(crate) running_nesting: u32,
    /// What scripts asked to load since the page last took it.
    pub(crate) navigations: Vec<ScriptNavigation>,
    /// `history.length`, as the browser last said.
    pub(crate) history_length: u32,
}

impl PageHost {
    pub(crate) fn new() -> Self {
        Self {
            document: Document::new("about:blank"),
            ready_state: ReadyState::Loading,
            timers: BTreeMap::new(),
            next_timer: 1,
            running_nesting: 0,
            navigations: Vec::new(),
            history_length: 1,
        }
    }

    /// When the next timer is due.
    pub(crate) fn next_due(&self) -> Option<Instant> {
        self.timers.values().map(|timer| timer.due).min()
    }

    /// The earliest timer due by `now`, with its nesting level, taken off
    /// the list.
    pub(crate) fn take_due(&mut self, now: Instant) -> Option<(TimerId, u32)> {
        let (&id, timer) = self
            .timers
            .iter()
            .filter(|(_, timer)| timer.due <= now)
            .min_by_key(|(id, timer)| (timer.due, **id))?;
        let nesting = timer.nesting;
        self.timers.remove(&id);
        Some((id, nesting))
    }

    /// The node `node` names in the main document, if it is alive.
    fn local(&self, node: NodeRef) -> Option<NodeId> {
        (node.frame == FrameId::MAIN && self.document.is_alive(node.node)).then_some(node.node)
    }

    fn reference(&self, node: Option<NodeId>) -> Option<NodeRef> {
        node.map(|node| NodeRef {
            frame: FrameId::MAIN,
            node,
        })
    }

    /// The `<html>` element's first child element called `name`.
    fn html_child(&self, name: &str) -> Option<NodeRef> {
        let html = self.document.document_element()?;
        let child = self.document.children(html).find(|&child| {
            self.document
                .node(child)
                .and_then(|n| n.as_element())
                .is_some_and(|e| {
                    e.name().ns == ns!("http://www.w3.org/1999/xhtml") && &*e.name().local == name
                })
        });
        self.reference(child)
    }

    /// The nodes below `root`, in tree order.
    fn descendants(&self, root: NodeId) -> Vec<NodeId> {
        let mut found = Vec::new();
        let mut stack: Vec<NodeId> = self.document.children(root).collect();
        stack.reverse();
        while let Some(node) = stack.pop() {
            found.push(node);
            let children: Vec<NodeId> = self.document.children(node).collect();
            stack.extend(children.into_iter().rev());
        }
        found
    }

    fn is_element(&self, node: NodeId) -> bool {
        self.document
            .node(node)
            .and_then(|n| n.as_element())
            .is_some()
    }
}

/// Whether `name` can name an element or attribute: an XML `Name`, cut to
/// what HTML documents need (no colon tricks).
fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let starts = |c: char| c.is_alphabetic() || c == '_' || c == ':';
    starts(first) && chars.all(|c| starts(c) || c.is_alphanumeric() || matches!(c, '-' | '.'))
}

impl ScriptHost for PageHost {
    fn document(&self) -> NodeRef {
        NodeRef {
            frame: FrameId::MAIN,
            node: self.document.root(),
        }
    }

    fn document_element(&self) -> Option<NodeRef> {
        self.reference(self.document.document_element())
    }

    fn body(&self) -> Option<NodeRef> {
        self.html_child("body")
    }

    fn head(&self) -> Option<NodeRef> {
        self.html_child("head")
    }

    fn ready_state(&self) -> ReadyState {
        self.ready_state
    }

    fn element_by_id(&self, id: &str) -> Option<NodeRef> {
        self.reference(self.document.element_by_id(&id.into()))
    }

    fn query_selector_all(
        &self,
        scope: NodeRef,
        selectors: &str,
    ) -> Result<Vec<NodeRef>, DomException> {
        let selectors = css::parse_selectors(selectors).ok_or(DomException::Syntax)?;
        let Some(scope) = self.local(scope) else {
            return Ok(Vec::new());
        };
        let found = self
            .descendants(scope)
            .into_iter()
            .filter(|&node| {
                self.document.element_ref(node).is_some_and(|element| {
                    selectors
                        .iter()
                        .any(|s| s.matches(&element, &mut MatchContext::default()))
                })
            })
            .map(|node| NodeRef {
                frame: FrameId::MAIN,
                node,
            })
            .collect();
        Ok(found)
    }

    fn create_element(&mut self, local_name: &str) -> Result<NodeRef, DomException> {
        if !is_valid_name(local_name) {
            return Err(DomException::InvalidCharacter);
        }
        // An HTML document lowercases the names it creates.
        let name = QualName::html(LocalName::from(local_name.to_ascii_lowercase()));
        let node = self.document.create_element(name);
        Ok(NodeRef {
            frame: FrameId::MAIN,
            node,
        })
    }

    fn create_text_node(&mut self, data: &str) -> NodeRef {
        NodeRef {
            frame: FrameId::MAIN,
            node: self.document.create_text(data),
        }
    }

    fn node_kind(&self, node: NodeRef) -> Option<NodeKind> {
        let node = self.document.node(self.local(node)?)?;
        Some(match node.data() {
            NodeData::Document => NodeKind::Document,
            NodeData::Doctype(_) => NodeKind::Doctype,
            NodeData::Element(_) => NodeKind::Element,
            NodeData::Text(_) => NodeKind::Text,
            NodeData::Comment(_) => NodeKind::Comment,
            NodeData::DocumentFragment { .. } | NodeData::ShadowRoot { .. } => NodeKind::Fragment,
        })
    }

    fn node_name(&self, node: NodeRef) -> Option<String> {
        let node = self.document.node(self.local(node)?)?;
        Some(match node.data() {
            NodeData::Document => "#document".into(),
            NodeData::Doctype(doctype) => doctype.name.clone(),
            NodeData::Element(element) => {
                let name = element.name();
                if name.ns == ns!("http://www.w3.org/1999/xhtml") {
                    name.local.to_ascii_uppercase().to_string()
                } else {
                    name.local.to_string()
                }
            }
            NodeData::Text(_) => "#text".into(),
            NodeData::Comment(_) => "#comment".into(),
            NodeData::DocumentFragment { .. } | NodeData::ShadowRoot { .. } => {
                "#document-fragment".into()
            }
        })
    }

    fn parent(&self, node: NodeRef) -> Option<NodeRef> {
        let parent = self.document.node(self.local(node)?)?.parent();
        self.reference(parent)
    }

    fn first_child(&self, node: NodeRef) -> Option<NodeRef> {
        let child = self.document.node(self.local(node)?)?.first_child();
        self.reference(child)
    }

    fn last_child(&self, node: NodeRef) -> Option<NodeRef> {
        let child = self.document.node(self.local(node)?)?.last_child();
        self.reference(child)
    }

    fn next_sibling(&self, node: NodeRef) -> Option<NodeRef> {
        let sibling = self.document.node(self.local(node)?)?.next_sibling();
        self.reference(sibling)
    }

    fn previous_sibling(&self, node: NodeRef) -> Option<NodeRef> {
        let sibling = self.document.node(self.local(node)?)?.prev_sibling();
        self.reference(sibling)
    }

    fn child_nodes(&self, node: NodeRef) -> Vec<NodeRef> {
        let Some(node) = self.local(node) else {
            return Vec::new();
        };
        self.document
            .children(node)
            .map(|node| NodeRef {
                frame: FrameId::MAIN,
                node,
            })
            .collect()
    }

    fn text_content(&self, node: NodeRef) -> Option<String> {
        let id = self.local(node)?;
        match self.document.node(id)?.data() {
            NodeData::Document | NodeData::Doctype(_) => None,
            NodeData::Text(text) | NodeData::Comment(text) => Some(text.clone()),
            NodeData::Element(_)
            | NodeData::DocumentFragment { .. }
            | NodeData::ShadowRoot { .. } => {
                let text = self
                    .descendants(id)
                    .into_iter()
                    .filter_map(|node| match self.document.node(node)?.data() {
                        NodeData::Text(text) => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                Some(text)
            }
        }
    }

    fn set_text_content(&mut self, node: NodeRef, text: &str) -> Result<(), DomException> {
        let id = self.local(node).ok_or(DomException::NotFound)?;
        let is_container = match self.document.node(id).map(|n| n.data()) {
            Some(NodeData::Text(_) | NodeData::Comment(_)) => false,
            Some(NodeData::Document | NodeData::Doctype(_)) | None => return Ok(()),
            Some(_) => true,
        };
        if !is_container {
            return Ok(self.document.set_text(id, text)?);
        }
        for child in self.document.children(id).collect::<Vec<_>>() {
            self.document.remove_child(id, child)?;
        }
        if !text.is_empty() {
            let child = self.document.create_text(text);
            self.document.append_child(id, child)?;
        }
        Ok(())
    }

    fn insert_before(
        &mut self,
        parent: NodeRef,
        child: NodeRef,
        reference: Option<NodeRef>,
    ) -> Result<(), DomException> {
        let parent = self.local(parent).ok_or(DomException::NotFound)?;
        let child = self.local(child).ok_or(DomException::NotFound)?;
        let reference = match reference {
            Some(reference) => Some(self.local(reference).ok_or(DomException::NotFound)?),
            None => None,
        };
        Ok(self.document.insert_before(parent, child, reference)?)
    }

    fn remove_child(&mut self, parent: NodeRef, child: NodeRef) -> Result<(), DomException> {
        let parent = self.local(parent).ok_or(DomException::NotFound)?;
        let child = self.local(child).ok_or(DomException::NotFound)?;
        Ok(self.document.remove_child(parent, child)?)
    }

    fn replace_child(
        &mut self,
        parent: NodeRef,
        new_child: NodeRef,
        old_child: NodeRef,
    ) -> Result<(), DomException> {
        let parent = self.local(parent).ok_or(DomException::NotFound)?;
        let new_child = self.local(new_child).ok_or(DomException::NotFound)?;
        let old_child = self.local(old_child).ok_or(DomException::NotFound)?;
        Ok(self.document.replace_child(parent, new_child, old_child)?)
    }

    fn attribute(&self, element: NodeRef, name: &str) -> Option<String> {
        let element = self.document.node(self.local(element)?)?.as_element()?;
        let name = QualName::attribute(LocalName::from(name.to_ascii_lowercase()));
        element.attribute(&name).map(str::to_owned)
    }

    fn set_attribute(
        &mut self,
        element: NodeRef,
        name: &str,
        value: &str,
    ) -> Result<(), DomException> {
        if !is_valid_name(name) {
            return Err(DomException::InvalidCharacter);
        }
        let element = self.local(element).ok_or(DomException::NotFound)?;
        if !self.is_element(element) {
            return Err(DomException::InvalidState);
        }
        let name = QualName::attribute(LocalName::from(name.to_ascii_lowercase()));
        Ok(self.document.set_attribute(element, name, value)?)
    }

    fn remove_attribute(&mut self, element: NodeRef, name: &str) -> Result<(), DomException> {
        let element = self.local(element).ok_or(DomException::NotFound)?;
        if !self.is_element(element) {
            return Err(DomException::InvalidState);
        }
        let name = QualName::attribute(LocalName::from(name.to_ascii_lowercase()));
        Ok(self.document.remove_attribute(element, &name)?)
    }

    fn pin(&mut self, node: NodeRef) {
        if let Some(node) = self.local(node) {
            let _ = self.document.pin(node);
        }
    }

    fn unpin(&mut self, node: NodeRef) {
        if let Some(node) = self.local(node) {
            let _ = self.document.unpin(node);
        }
    }

    /// The HTML standard's timer steps: a negative or invalid delay is 0, a
    /// timer nested more than 5 levels deep waits at least 4 ms (AC-33).
    fn set_timeout(&mut self, delay_ms: f64) -> TimerId {
        let mut delay = if delay_ms.is_finite() {
            delay_ms.clamp(0.0, MAX_DELAY_MS)
        } else {
            0.0
        };
        let nesting = self.running_nesting;
        if nesting > NESTING_CLAMP_LEVEL && delay < NESTED_MIN_DELAY_MS {
            delay = NESTED_MIN_DELAY_MS;
        }
        let id = TimerId(self.next_timer);
        self.next_timer = self.next_timer.wrapping_add(1).max(1);
        self.timers.insert(
            id,
            Timer {
                due: Instant::now() + Duration::from_secs_f64(delay / 1000.0),
                nesting: nesting + 1,
            },
        );
        id
    }

    fn clear_timeout(&mut self, id: TimerId) {
        self.timers.remove(&id);
    }

    fn console(&mut self, level: ConsoleLevel, message: &str) {
        match level {
            ConsoleLevel::Debug => debug!(target: "console", "{message}"),
            ConsoleLevel::Log | ConsoleLevel::Info => info!(target: "console", "{message}"),
            ConsoleLevel::Warn => warn!(target: "console", "{message}"),
            ConsoleLevel::Error => error!(target: "console", "{message}"),
        }
    }

    fn location(&self) -> String {
        self.document.url().to_owned()
    }

    fn navigate(&mut self, url: &str, action: LocationAction) -> Result<(), DomException> {
        let url = match action {
            LocationAction::Reload => self.document.url().to_owned(),
            LocationAction::Push | LocationAction::Replace => {
                crate::links::base_url(&self.document)
                    .and_then(|base| base.join(url.trim()).ok())
                    .ok_or(DomException::Syntax)?
                    .into()
            }
        };
        self.navigations.push(ScriptNavigation::Load(url, action));
        Ok(())
    }

    fn history_go(&mut self, delta: i32) {
        self.navigations.push(ScriptNavigation::Go(delta));
    }

    fn history_length(&self) -> u32 {
        self.history_length
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deeply_nested_timers_wait_at_least_4_ms() {
        // AC-33
        let mut host = PageHost::new();
        let start = Instant::now();
        let shallow = host.set_timeout(0.0);
        host.running_nesting = 6;
        let deep = host.set_timeout(0.0);
        let negative = {
            host.running_nesting = 0;
            host.set_timeout(-50.0)
        };
        let due = |host: &PageHost, id| host.timers[&id].due;
        assert!(due(&host, shallow) < start + Duration::from_millis(4));
        assert!(due(&host, deep) >= start + Duration::from_millis(4));
        assert!(due(&host, negative) < start + Duration::from_millis(4));
        assert_eq!(host.timers[&deep].nesting, 7);

        // Due timers come out earliest first, ties by id.
        let later = Instant::now() + Duration::from_millis(10);
        assert_eq!(host.take_due(later).map(|(id, _)| id), Some(shallow));
        assert_eq!(host.take_due(later).map(|(id, _)| id), Some(negative));
        assert_eq!(host.take_due(later).map(|(id, _)| id), Some(deep));
        assert_eq!(host.take_due(later), None);
    }

    #[test]
    fn names_follow_the_xml_name_rules() {
        assert!(is_valid_name("div"));
        assert!(is_valid_name("my-element"));
        assert!(is_valid_name("x:y"));
        for bad in ["", "1a", "a b", "a<b", "-x"] {
            assert!(!is_valid_name(bad), "{bad}");
        }
    }
}
