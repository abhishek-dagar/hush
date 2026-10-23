//! Links: where an activated `<a href>` goes (spec 0008, AC-17). The page
//! only resolves the URL; the browser decides whether it may load
//! (AC-18).

use base::atoms::{LocalName, QualName};
use dom::{Document, NodeId, NodeRef};
use tracing::debug;
use url::Url;

use crate::Page;

impl Page {
    /// The link the status bubble should show, if it changed since the
    /// last call (spec 0008, AC-19): the hovered link, else the focused
    /// one, as its resolved URL; `Some(None)` when there is none any more.
    pub fn take_link_hover(&mut self) -> Option<Option<String>> {
        let doc = &self.main_frame.document;
        let node = self
            .input
            .hovered()
            .filter(|&id| doc.is_alive(id))
            .and_then(|id| link_at(doc, id))
            .or_else(|| doc.focused().and_then(|id| link_at(doc, id)));
        let url = node
            .and_then(|node| {
                self.link_url(NodeRef {
                    frame: dom::FrameId::MAIN,
                    node,
                })
            })
            .map(|url| truncate(url, ipc::MAX_LINK_HOVER_BYTES));
        if url == self.link_hover {
            return None;
        }
        self.link_hover.clone_from(&url);
        Some(url)
    }

    /// The absolute URL the link at or around `node` goes to, resolved
    /// against the document's base URL: the first `<base href>`, else the
    /// document URL. `None` when there is no `<a href>` there, the URL
    /// does not resolve, or it is a `javascript:` URL, which does nothing
    /// (AC-17).
    pub fn link_url(&self, node: NodeRef) -> Option<String> {
        let doc = &self.main_frame.document;
        let link = link_at(doc, node.node)?;
        let href = attribute(doc, link, "href")?.trim();
        let url = base_url(doc)?.join(href).ok()?;
        if url.scheme() == "javascript" {
            debug!("javascript: link ignored");
            return None;
        }
        Some(url.into())
    }
}

/// `text` cut to at most `max` bytes, on a character boundary.
fn truncate(mut text: String, max: usize) -> String {
    if text.len() > max {
        let mut cut = max;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    text
}

/// The nearest inclusive flat tree ancestor of `node` that is an `<a>`
/// with an `href`.
pub(crate) fn link_at(doc: &Document, node: NodeId) -> Option<NodeId> {
    std::iter::successors(Some(node), |&id| doc.flat_parent(id)).find(|&id| {
        doc.is_alive(id)
            && doc.is_html(id, base::local_name!("a"))
            && attribute(doc, id, "href").is_some()
    })
}

/// The document's base URL: the first `<base href>` in tree order,
/// resolved against the document URL, else the document URL.
pub(crate) fn base_url(doc: &Document) -> Option<Url> {
    let document = Url::parse(doc.url()).ok()?;
    let base = LocalName::from("base");
    let mut stack = vec![doc.root()];
    while let Some(id) = stack.pop() {
        if doc.is_html(id, base.clone())
            && let Some(href) = attribute(doc, id, "href")
        {
            return Some(document.join(href.trim()).unwrap_or(document));
        }
        let children: Vec<NodeId> = doc.children(id).collect();
        stack.extend(children.into_iter().rev());
    }
    Some(document)
}

fn attribute<'a>(doc: &'a Document, node: NodeId, name: &str) -> Option<&'a str> {
    doc.node(node)?
        .as_element()?
        .attribute(&QualName::attribute(LocalName::from(name)))
}
