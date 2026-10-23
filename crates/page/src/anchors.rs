//! Where the root viewport scrolls when a document is entered (spec 0008,
//! AC-20, AC-23): the element a URL's fragment names, or the offset an
//! entry was left at. The browser decides; the page finds the target and
//! scrolls.
//!
//! A target is applied after each layout until it fits the scroll range,
//! the `load` event fires, or the user scrolls, whichever comes first, so
//! a long page lands right while its images arrive.

use base::local_name;
use dom::{Document, FrameId, NodeId, NodeRef};
use ipc::{HistoryInfo, NavigationId, ScrollTarget};
use tracing::debug;
use url::Url;

use crate::Page;

/// A root scroll the page still owes (spec 0008, AC-23).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum PendingScroll {
    /// The document URL's fragment, else the top.
    Fragment,
    /// A saved offset in CSS px.
    Offset(f32, f32),
}

impl From<ScrollTarget> for PendingScroll {
    fn from(target: ScrollTarget) -> Self {
        match target {
            ScrollTarget::Fragment => Self::Fragment,
            ScrollTarget::Offset(x, y) => Self::Offset(x, y),
        }
    }
}

impl Page {
    /// The browser moved inside the current document (`SameDocument`,
    /// spec 0008, AC-20, AC-21): the document takes `url`, and the root
    /// scrolls to `scroll`. Nothing reloads, so fetches in flight go on.
    /// A stale navigation is dropped.
    pub fn same_document(
        &mut self,
        navigation: NavigationId,
        url: &str,
        scroll: ScrollTarget,
        history: HistoryInfo,
    ) {
        if !self.is_current(navigation) || self.committed != navigation {
            debug!(?navigation, "stale same document move dropped");
            return;
        }
        self.main_frame.document.set_url(url);
        self.history = history;
        self.pending_scroll = Some(scroll.into());
        self.needs_update = true;
    }

    /// The offset to restore for the document just committed (spec 0008,
    /// AC-23), from `CommitNavigation`; without one it starts at its
    /// fragment, or the top.
    pub fn restore_scroll(&mut self, navigation: NavigationId, offset: Option<(f32, f32)>) {
        if self.committed == navigation
            && let Some((x, y)) = offset
        {
            self.pending_scroll = Some(PendingScroll::Offset(x, y));
            self.needs_update = true;
        }
    }

    /// The session history as scripts see it (`history.length`).
    pub fn set_history(&mut self, history: HistoryInfo) {
        self.history = history;
        if let Some(script) = &self.script {
            script.host.borrow_mut().history_length = history.length;
        }
    }

    /// The `NavigateRequest` and `HistoryGo` messages for what scripts
    /// asked since the last call (spec 0008, AC-27), under the committed
    /// navigation. A URL over the page limit is dropped (AC-18).
    pub fn take_navigation_messages(&mut self) -> Vec<ipc::ToBrowser> {
        let Some(script) = &self.script else {
            return Vec::new();
        };
        let asked = std::mem::take(&mut script.host.borrow_mut().navigations);
        let (page, navigation) = (self.id, self.committed);
        asked
            .into_iter()
            .filter_map(|asked| match asked {
                crate::script_host::ScriptNavigation::Load(url, action) => {
                    if url.len() > ipc::MAX_PAGE_URL_BYTES {
                        tracing::warn!("script navigation to a URL over 2 MB dropped");
                        return None;
                    }
                    let action = match action {
                        script::LocationAction::Push => ipc::PageNavigationAction::Push,
                        script::LocationAction::Replace => ipc::PageNavigationAction::Replace,
                        script::LocationAction::Reload => ipc::PageNavigationAction::Reload,
                    };
                    Some(ipc::ToBrowser::NavigateRequest {
                        page,
                        navigation,
                        url,
                        origin: ipc::PageNavigationOrigin::Script,
                        action,
                    })
                }
                crate::script_host::ScriptNavigation::Go(delta) => {
                    Some(ipc::ToBrowser::HistoryGo {
                        page,
                        navigation,
                        delta,
                    })
                }
            })
            .collect()
    }

    /// The session history the browser last told the page about.
    pub fn history(&self) -> HistoryInfo {
        self.history
    }

    /// The root scroll offset the pending target asks for, after this
    /// frame's layout; `None` when nothing is pending. A fragment that
    /// names nothing (or is empty, or `top`) is the top.
    pub(crate) fn pending_scroll_offset(&self) -> Option<(f32, f32)> {
        match self.pending_scroll? {
            PendingScroll::Offset(x, y) => Some((x, y)),
            PendingScroll::Fragment => {
                let doc = &self.main_frame.document;
                let Some(target) = fragment_target(doc) else {
                    return Some((0.0, 0.0));
                };
                // An empty `<a name>` has no box: the next element in tree
                // order that has one stands for it.
                let boxes = self.element_boxes();
                let y = tree_order_from(doc, target)
                    .find_map(|node| boxes.iter().find(|(n, _)| *n == node))
                    .map_or(0.0, |(_, rect)| rect.origin.y);
                Some((0.0, y))
            }
        }
    }

    /// Applies the pending target to the root scroll frame, in the scroll
    /// table paint reads, with a new generation so the render side takes
    /// it over its own (spec 0002, Scrolling). No new frame is asked for.
    pub(crate) fn apply_pending_scroll(&mut self) {
        let Some((x, y)) = self.pending_scroll_offset() else {
            return;
        };
        let Some(root) = self.main_frame.document.document_element() else {
            return;
        };
        let scroll_id = paint::scroll_id(FrameId::MAIN.0, root);
        let entry = self.scroll.entry(scroll_id).or_default();
        if entry.x != x || entry.y != y {
            *entry = display_list::ScrollOffset {
                x: x.max(0.0),
                y: y.max(0.0),
                generation: entry.generation.wrapping_add(1),
            };
        }
    }

    /// After a frame painted `list`: the target is done once its offset
    /// fits the root's scroll range, or the document's `load` fired.
    pub(crate) fn settle_pending_scroll(&mut self, list: &display_list::DisplayList) {
        let Some((x, y)) = self.pending_scroll_offset() else {
            return;
        };
        let Some(root) = self.main_frame.document.document_element() else {
            return;
        };
        let root_id = paint::scroll_id(FrameId::MAIN.0, root);
        let fits = list.spatial.iter().any(|node| match &node.kind {
            display_list::SpatialKind::ScrollFrame {
                scroll_id,
                frame_rect,
                content_size,
                ..
            } if *scroll_id == root_id => {
                let max_x = (content_size.width - frame_rect.size.width).max(0.0);
                let max_y = (content_size.height - frame_rect.size.height).max(0.0);
                x <= max_x + 0.5 && y <= max_y + 0.5
            }
            _ => false,
        });
        if fits || self.lifecycle.loaded {
            self.pending_scroll = None;
        }
    }

    /// The user scrolled `scroll_id`: a restore in flight on the root
    /// gives way (AC-23).
    pub(crate) fn user_scrolled(&mut self, scroll_id: display_list::ScrollId) {
        let root = self
            .main_frame
            .document
            .document_element()
            .map(|root| paint::scroll_id(FrameId::MAIN.0, root));
        if root == Some(scroll_id) {
            self.pending_scroll = None;
        }
    }

    /// The node the document URL's fragment names, if any (AC-20).
    pub fn fragment_node(&self) -> Option<NodeRef> {
        fragment_target(&self.main_frame.document).map(|node| NodeRef {
            frame: FrameId::MAIN,
            node,
        })
    }
}

/// The element the document URL's fragment names (AC-20): the first
/// element whose `id` is the percent decoded fragment, else the first
/// `<a name>` with it. `None` without a fragment, for an empty one, or
/// when nothing matches (`top` included): those scroll to the top.
pub(crate) fn fragment_target(doc: &Document) -> Option<NodeId> {
    let url = Url::parse(doc.url()).ok()?;
    let fragment = percent_decode(url.fragment()?);
    if fragment.is_empty() {
        return None;
    }
    if let Some(node) = doc.element_by_id(&base::atoms::Atom::from(fragment.as_str())) {
        return Some(node);
    }
    let mut stack = vec![doc.root()];
    while let Some(id) = stack.pop() {
        if doc.is_html(id, local_name!("a"))
            && doc.node(id).and_then(|n| n.as_element()).and_then(|e| {
                e.attribute(&base::atoms::QualName::attribute(
                    base::atoms::LocalName::from("name"),
                ))
            }) == Some(fragment.as_str())
        {
            return Some(id);
        }
        let children: Vec<NodeId> = doc.children(id).collect();
        stack.extend(children.into_iter().rev());
    }
    None
}

/// `node` and every node after it in tree order.
fn tree_order_from(doc: &Document, node: NodeId) -> impl Iterator<Item = NodeId> + '_ {
    std::iter::successors(Some(node), move |&id| {
        doc.children(id).next().or_else(|| {
            std::iter::successors(Some(id), |&up| doc.node(up).and_then(|n| n.parent()))
                .find_map(|up| doc.node(up).and_then(|n| n.next_sibling()))
        })
    })
}

/// `%XX` escapes decoded, as UTF 8 with bad sequences replaced.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| char::from(b).to_digit(16);
        if bytes[i] == b'%'
            && let (Some(high), Some(low)) = (
                bytes.get(i + 1).copied().and_then(hex),
                bytes.get(i + 2).copied().and_then(hex),
            )
        {
            // Two hex digits make one byte.
            out.push((high * 16 + low) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragments_are_percent_decoded() {
        assert_eq!(percent_decode("caf%C3%A9"), "café");
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("100%"), "100%", "a lone % stays");
    }
}
