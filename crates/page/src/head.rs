//! What the window shows for a page (spec 0004, Value sourcing, AC-36,
//! AC-37): its title and its favicon.
//!
//! The title is the first `<title>`'s text with whitespace collapsed, or
//! the document's URL when there is none; it is sent whenever it changes,
//! so each navigation sends one. The favicon comes from the first
//! `<link rel~=icon href>`, else `/favicon.ico` on the document's origin,
//! for `http(s)` documents only. It is asked for once parsing ends, does
//! not hold the `load` event, and is decoded here on the page thread.

use base::atoms::{LocalName, QualName};
use base::ns;
use display_list::PageId;
use dom::{Document, NodeData, NodeId};
use ipc::{FetchId, MAX_TITLE_CHARS, SharedBytes, ToBrowser};
use tracing::{debug, warn};

use crate::FetchIds;

#[derive(Debug, Default)]
enum Favicon {
    /// Parsing has not ended yet.
    #[default]
    Unasked,
    Loading {
        fetch: FetchId,
        url: String,
        bytes: Vec<u8>,
        ok: bool,
    },
    /// Shown, failed, or not wanted.
    Settled,
}

/// The committed document's title and favicon, and the messages for them.
#[derive(Debug, Default)]
pub(crate) struct Head {
    /// The last title sent.
    title: Option<String>,
    favicon: Favicon,
    messages: Vec<ToBrowser>,
}

impl Head {
    /// Takes the `TitleChanged` and `FaviconChanged` messages to send.
    pub(crate) fn take_messages(&mut self) -> Vec<ToBrowser> {
        std::mem::take(&mut self.messages)
    }

    /// Whether a title went out for this document yet.
    pub(crate) fn title_sent(&self) -> bool {
        self.title.is_some()
    }

    /// Sends the title again if it changed. `parsed` says whether the
    /// parser finished: until then, or until it reaches `<body>`, a
    /// `<title>` may still come, so the URL is not sent yet.
    pub(crate) fn refresh_title(&mut self, page: PageId, doc: &Document, parsed: bool) {
        let title = match document_title(doc) {
            Some(title) => title,
            None if parsed || has_body(doc) => doc.url().to_owned(),
            None => return,
        };
        let title: String = title.chars().take(MAX_TITLE_CHARS).collect();
        if self.title.as_ref() == Some(&title) {
            return;
        }
        self.title = Some(title.clone());
        self.messages.push(ToBrowser::TitleChanged { page, title });
    }

    /// Once parsing ended: the favicon fetch to send, if the document
    /// wants one.
    pub(crate) fn request_favicon(
        &mut self,
        doc: &Document,
        ids: &mut FetchIds,
    ) -> Option<(FetchId, String)> {
        if !matches!(self.favicon, Favicon::Unasked) {
            return None;
        }
        self.favicon = Favicon::Settled;
        let url = favicon_url(doc)?;
        let fetch = ids.next_id();
        debug!(url, fetch = fetch.0, "favicon requested");
        self.favicon = Favicon::Loading {
            fetch,
            url: url.clone(),
            bytes: Vec::new(),
            ok: true,
        };
        Some((fetch, url))
    }

    /// Whether `fetch` loads the favicon.
    pub(crate) fn owns(&self, fetch: FetchId) -> bool {
        matches!(self.favicon, Favicon::Loading { fetch: f, .. } if f == fetch)
    }

    /// The favicon's headers: only a 2xx answer is used.
    pub(crate) fn fetch_response(&mut self, status: u16) {
        if let Favicon::Loading { ok, .. } = &mut self.favicon {
            *ok = (200..300).contains(&status);
        }
    }

    /// Body bytes of the favicon.
    pub(crate) fn fetch_chunk(&mut self, data: &[u8]) {
        if let Favicon::Loading {
            bytes, ok: true, ..
        } = &mut self.favicon
        {
            bytes.extend_from_slice(data);
        }
    }

    /// The favicon's body ended: decode it and send it.
    pub(crate) fn fetch_done(&mut self, page: PageId) {
        let Favicon::Loading { url, bytes, ok, .. } = std::mem::take(&mut self.favicon) else {
            return;
        };
        self.favicon = Favicon::Settled;
        if !ok {
            debug!(url, "no favicon");
            return;
        }
        match image::decode_icon(&bytes, &image::Limits::default()) {
            Ok(icon) => self.messages.push(ToBrowser::FaviconChanged {
                page,
                width: icon.width,
                height: icon.height,
                rgba: SharedBytes::from_bytes(&icon.rgba),
            }),
            Err(err) => warn!(url, %err, "favicon did not decode"),
        }
    }

    /// The favicon failed to load: the window keeps its icon.
    pub(crate) fn fetch_failed(&mut self) {
        self.favicon = Favicon::Settled;
    }
}

fn is_html(doc: &Document, node: NodeId, local: &str) -> bool {
    doc.node(node)
        .and_then(|n| n.as_element())
        .is_some_and(|element| {
            let name = element.name();
            name.ns == ns!("http://www.w3.org/1999/xhtml") && &*name.local == local
        })
}

fn attribute<'a>(doc: &'a Document, node: NodeId, name: &str) -> Option<&'a str> {
    doc.node(node)?
        .as_element()?
        .attribute(&QualName::attribute(LocalName::from(name)))
}

/// The first HTML element named `local` in tree order for which `test`
/// holds.
fn first(doc: &Document, local: &str, test: impl Fn(NodeId) -> bool) -> Option<NodeId> {
    let mut stack = vec![doc.root()];
    while let Some(node) = stack.pop() {
        if is_html(doc, node, local) && test(node) {
            return Some(node);
        }
        let children: Vec<NodeId> = doc.children(node).collect();
        stack.extend(children.into_iter().rev());
    }
    None
}

/// Whether the root element has a `<body>` child yet.
fn has_body(doc: &Document) -> bool {
    doc.document_element()
        .is_some_and(|html| doc.children(html).any(|child| is_html(doc, child, "body")))
}

/// The first `<title>`'s text, ASCII whitespace stripped and collapsed;
/// `None` when there is no title or it is blank.
fn document_title(doc: &Document) -> Option<String> {
    let title = first(doc, "title", |_| true)?;
    let text: String = doc
        .children(title)
        .filter_map(|child| match doc.node(child)?.data() {
            NodeData::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let collapsed = text.split_ascii_whitespace().collect::<Vec<_>>().join(" ");
    (!collapsed.is_empty()).then_some(collapsed)
}

/// Where the favicon comes from, for an `http(s)` document.
fn favicon_url(doc: &Document) -> Option<String> {
    let base = url::Url::parse(doc.url()).ok()?;
    if !matches!(base.scheme(), "http" | "https") {
        return None;
    }
    let href = |node| {
        let href = attribute(doc, node, "href")?.trim();
        (!href.is_empty()).then(|| base.join(href).ok()).flatten()
    };
    let link = first(doc, "link", |node| {
        let is_icon = attribute(doc, node, "rel").is_some_and(|rel| {
            rel.split_ascii_whitespace()
                .any(|word| word.eq_ignore_ascii_case("icon"))
        });
        is_icon && href(node).is_some()
    });
    let url = match link {
        Some(node) => href(node)?,
        None => base.join("/favicon.ico").ok()?,
    };
    Some(url.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(url: &str, html: &str) -> Document {
        let mut doc = Document::new(url);
        let mut parser = html::Parser::new(html::ParserOptions::default());
        let mut step = parser.feed(&mut crate::DomSink::new(&mut doc), html.as_bytes());
        loop {
            step = match step {
                html::ParseStep::RunScript(_) => parser.resume(&mut crate::DomSink::new(&mut doc)),
                html::ParseStep::Continue => parser.end(&mut crate::DomSink::new(&mut doc)),
                html::ParseStep::Done => return doc,
            };
        }
    }

    #[test]
    fn the_title_is_collapsed_text_or_the_url() {
        let doc = parsed(
            "http://a/",
            "<title>\n  Hello \t  there </title><title>second</title>",
        );
        assert_eq!(document_title(&doc).as_deref(), Some("Hello there"));
        let doc = parsed(
            "http://a/",
            "<title>   </title><svg><title>no</title></svg>",
        );
        assert_eq!(document_title(&doc), None);

        let mut head = Head::default();
        let empty = Document::new("http://a/");
        head.refresh_title(PageId(1), &empty, false);
        assert!(head.take_messages().is_empty(), "a title may still come");
        head.refresh_title(PageId(1), &doc, false);
        head.refresh_title(PageId(1), &doc, true);
        assert_eq!(
            head.take_messages(),
            [ToBrowser::TitleChanged {
                page: PageId(1),
                title: "http://a/".into()
            }],
            "the URL, and only once"
        );
        let long = parsed("http://a/", &format!("<title>{}</title>", "x".repeat(5000)));
        head.refresh_title(PageId(1), &long, true);
        let [ToBrowser::TitleChanged { title, .. }] = &head.take_messages()[..] else {
            panic!("one title");
        };
        assert_eq!(title.chars().count(), MAX_TITLE_CHARS);
    }

    #[test]
    fn the_favicon_is_the_first_icon_link_else_favicon_ico_on_the_web_only() {
        let doc = parsed(
            "http://a/dir/page.html",
            "<link rel=stylesheet href=s.css><link rel=icon><link rel='Shortcut ICON' href=i.png><link rel=icon href=late.png>",
        );
        assert_eq!(favicon_url(&doc).as_deref(), Some("http://a/dir/i.png"));
        let doc = parsed("https://a/dir/page.html", "<p>no links");
        assert_eq!(favicon_url(&doc).as_deref(), Some("https://a/favicon.ico"));
        let doc = parsed("file:///tmp/page.html", "<link rel=icon href=i.png>");
        assert_eq!(favicon_url(&doc), None);
    }
}
