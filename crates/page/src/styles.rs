//! A document's style sheets and the style pass (spec 0004, child 0003,
//! Sources, Render blocking, AC-18 to AC-22).
//!
//! Sheets come from the UA sheet, `<style>` and `<link rel=stylesheet>`,
//! kept in tree order whatever order they load in. Linked sheets are
//! fetched through the browser like every other load; the page only asks.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use base::atoms::{LocalName, QualName};
use base::{Au, Size};
use base::{local_name, ns};
use css::{
    ColorScheme, ComputedStyle, ContainerType, Device, MatchContext, MediaList, Origin, Stylesheet,
    Stylist,
};
use dom::{Document, NodeData, NodeId};
use ipc::FetchId;
use tracing::{debug, warn};

use crate::FetchIds;

/// How long the first paint waits for the sheets linked in `<head>`, from
/// the commit (AC-22).
pub const RENDER_BLOCKING_CAP: Duration = Duration::from_secs(3);

/// What the page asks the browser for on behalf of its sheets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StyleRequest {
    /// Fetch a linked sheet.
    Fetch {
        /// The page's id for the fetch.
        fetch: FetchId,
        /// The absolute URL.
        url: String,
    },
    /// The `<link>` went away or changed: stop loading it.
    Cancel {
        /// The fetch to stop.
        fetch: FetchId,
    },
}

/// What one style pass did, for the `style` trace event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StyleStats {
    /// Elements the walk visited.
    pub visited: usize,
    /// Elements whose style was computed or shared.
    pub styled: usize,
    /// Of those, the ones the sharing cache answered.
    pub shared: usize,
}

#[derive(Debug)]
enum LinkState {
    Loading { bytes: Vec<u8> },
    Loaded(Rc<Stylesheet>),
    Failed,
}

#[derive(Debug)]
struct Link {
    url: String,
    fetch: FetchId,
    state: LinkState,
    /// It was found in `<head>` while the parser ran, so it holds the first
    /// paint (AC-22).
    blocking: bool,
    /// The response's charset and final URL, once the headers arrive.
    charset: Option<String>,
    final_url: Option<String>,
}

#[derive(Debug)]
enum Kind {
    Inline { text: String, sheet: Rc<Stylesheet> },
    Link(Link),
}

#[derive(Debug)]
struct Source {
    node: NodeId,
    media: Option<String>,
    kind: Kind,
}

impl Source {
    /// The sheet, with its `media` attribute, once it has one.
    fn sheet(&self) -> Option<Rc<Stylesheet>> {
        let sheet = match &self.kind {
            Kind::Inline { sheet, .. } => sheet,
            Kind::Link(Link {
                state: LinkState::Loaded(sheet),
                ..
            }) => sheet,
            Kind::Link(_) => return None,
        };
        Some(Rc::clone(sheet))
    }
}

/// Container border box sizes from the previous layout pass, keyed by the
/// element that is a query container (spec 0010, AC-17, AC-18). The next
/// style pass resolves `cq*` units and `@container` conditions against it.
pub type ContainerSizes = HashMap<NodeId, Size>;

/// The main document's sheets, its `Stylist`, and the fetches for them.
pub struct Styles {
    ua: Rc<Stylesheet>,
    /// A built in document's own user agent rules, after the UA sheet
    /// (spec 0035, `text.css`).
    builtin: Option<Rc<Stylesheet>>,
    sources: Vec<Source>,
    stylist: Stylist,
    requests: Vec<StyleRequest>,
    committed_at: Instant,
}

impl Styles {
    /// Styles for a document committed at `committed_at`, with only the UA
    /// sheet so far.
    pub fn new(ua: Rc<Stylesheet>, device: Device, committed_at: Instant) -> Self {
        let stylist = Stylist::new(std::slice::from_ref(&ua), device);
        Self {
            ua,
            builtin: None,
            sources: Vec::new(),
            stylist,
            requests: Vec::new(),
            committed_at,
        }
    }

    /// Adds a built in document's rules after the UA sheet (spec 0035,
    /// `text.css`). Called once, before the document has any nodes.
    pub fn with_builtin(mut self, sheet: Rc<Stylesheet>) -> Self {
        self.builtin = Some(sheet);
        self.stylist = Stylist::new(&self.sheets(), self.stylist.device());
        self
    }

    /// The rules in use.
    pub fn stylist(&self) -> &Stylist {
        &self.stylist
    }

    /// Takes the fetches and cancels to send.
    pub fn take_requests(&mut self) -> Vec<StyleRequest> {
        std::mem::take(&mut self.requests)
    }

    /// Finds the document's `<style>` and `<link rel=stylesheet>` elements
    /// again, in tree order. New links start fetching, with ids from `ids`;
    /// links that left are cancelled. `parsing` says whether the parser is
    /// still running, which makes a link in `<head>` hold the first paint.
    /// When the set of sheets changed, rebuilds the rules, installs their
    /// invalidation map and restyles the document.
    pub fn refresh(&mut self, doc: &mut Document, parsing: bool, ids: &mut FetchIds) {
        let before: Vec<*const Stylesheet> = self.sheets().iter().map(Rc::as_ptr).collect();
        let mut old = std::mem::take(&mut self.sources);
        for (node, in_head) in style_elements(doc) {
            let media = attribute(doc, node, "media");
            let source = if is_named(doc, node, "style") {
                let text = text_content(doc, node);
                let reused = old.iter().position(|s| {
                    s.node == node
                        && s.media == media
                        && matches!(&s.kind, Kind::Inline { text: t, .. } if *t == text)
                });
                match reused {
                    Some(index) => old.swap_remove(index),
                    None => {
                        let mut sheet =
                            css::parse_stylesheet(&text, Origin::Author, Some(doc.url()));
                        sheet.media = media.as_deref().map(|m| Rc::new(MediaList::parse(m)));
                        Source {
                            node,
                            media,
                            kind: Kind::Inline {
                                text,
                                sheet: Rc::new(sheet),
                            },
                        }
                    }
                }
            } else {
                let Some(url) = stylesheet_url(doc, node) else {
                    continue;
                };
                let reused = old.iter().position(|s| {
                    s.node == node && matches!(&s.kind, Kind::Link(link) if link.url == url)
                });
                match reused {
                    Some(index) => {
                        let mut source = old.swap_remove(index);
                        if source.media != media {
                            source.media = media;
                            if let Kind::Link(Link {
                                state: LinkState::Loaded(sheet),
                                ..
                            }) = &mut source.kind
                            {
                                *sheet = with_media(sheet, source.media.as_deref());
                            }
                        }
                        source
                    }
                    None => {
                        let fetch = ids.next_id();
                        debug!(url, fetch = fetch.0, "stylesheet requested");
                        self.requests.push(StyleRequest::Fetch {
                            fetch,
                            url: url.clone(),
                        });
                        Source {
                            node,
                            media,
                            kind: Kind::Link(Link {
                                url,
                                fetch,
                                state: LinkState::Loading { bytes: Vec::new() },
                                blocking: parsing && in_head,
                                charset: None,
                                final_url: None,
                            }),
                        }
                    }
                }
            };
            self.sources.push(source);
        }
        for gone in old {
            if let Kind::Link(Link {
                fetch,
                state: LinkState::Loading { .. },
                ..
            }) = gone.kind
            {
                self.requests.push(StyleRequest::Cancel { fetch });
            }
        }
        let after: Vec<*const Stylesheet> = self.sheets().iter().map(Rc::as_ptr).collect();
        if before != after {
            self.rebuild(doc);
        }
    }

    /// Every sheet in cascade order: the UA sheet, a built in document's
    /// rules, then the document's in tree order.
    fn sheets(&self) -> Vec<Rc<Stylesheet>> {
        std::iter::once(Rc::clone(&self.ua))
            .chain(self.builtin.clone())
            .chain(self.sources.iter().filter_map(Source::sheet))
            .collect()
    }

    fn rebuild(&mut self, doc: &mut Document) {
        self.stylist = Stylist::new(&self.sheets(), self.stylist.device());
        doc.set_invalidation_map(Some(self.stylist.invalidation_map()));
        doc.restyle_all();
    }

    fn link_mut(&mut self, fetch: FetchId) -> Option<&mut Link> {
        self.sources
            .iter_mut()
            .find_map(|source| match &mut source.kind {
                Kind::Link(link) if link.fetch == fetch => Some(link),
                _ => None,
            })
    }

    /// A linked sheet's headers arrived. Only a 2xx answer is used.
    pub fn fetch_response(
        &mut self,
        fetch: FetchId,
        status: u16,
        headers: &[(String, String)],
        final_url: &str,
    ) {
        let Some(link) = self.link_mut(fetch) else {
            return;
        };
        if !(200..300).contains(&status) {
            warn!(url = link.url, status, "stylesheet skipped");
            link.state = LinkState::Failed;
            return;
        }
        link.charset = headers
            .iter()
            .find(|(name, _)| name == "content-type")
            .and_then(|(_, value)| charset(value));
        link.final_url = Some(final_url.to_owned());
    }

    /// Body bytes of a linked sheet. Returns whether `fetch` was one.
    pub fn fetch_chunk(&mut self, fetch: FetchId, data: &[u8]) -> bool {
        match self.link_mut(fetch) {
            Some(Link {
                state: LinkState::Loading { bytes },
                ..
            }) => {
                bytes.extend_from_slice(data);
                true
            }
            Some(_) => true,
            None => false,
        }
    }

    /// A linked sheet's body ended: decode and parse it, then restyle.
    pub fn fetch_done(&mut self, doc: &mut Document, fetch: FetchId) {
        let media = self
            .sources
            .iter()
            .find(|s| matches!(&s.kind, Kind::Link(link) if link.fetch == fetch))
            .and_then(|s| s.media.clone());
        let encoding = doc.encoding();
        let Some(link) = self.link_mut(fetch) else {
            return;
        };
        let LinkState::Loading { bytes } = &link.state else {
            return;
        };
        let text = css::decode_stylesheet(bytes, link.charset.as_deref(), encoding);
        let base = link.final_url.as_deref().unwrap_or(&link.url);
        let mut sheet = css::parse_stylesheet(&text, Origin::Author, Some(base));
        sheet.media = media.as_deref().map(|m| Rc::new(MediaList::parse(m)));
        debug!(url = link.url, rules = sheet.len(), "stylesheet loaded");
        link.state = LinkState::Loaded(Rc::new(sheet));
        self.rebuild(doc);
    }

    /// A linked sheet failed: it is skipped, and stops holding the paint.
    /// Returns whether `fetch` was one.
    pub fn fetch_failed(&mut self, fetch: FetchId) -> bool {
        match self.link_mut(fetch) {
            Some(link) => {
                link.state = LinkState::Failed;
                true
            }
            None => false,
        }
    }

    /// Whether a linked sheet is still loading: a parser blocking script
    /// waits for it (spec 0004, AC-31), and so does `load`.
    pub fn loading(&self) -> bool {
        self.sources.iter().any(|source| {
            matches!(
                &source.kind,
                Kind::Link(Link {
                    state: LinkState::Loading { .. },
                    ..
                })
            )
        })
    }

    /// Whether the first paint must still wait at `now`: a sheet linked in
    /// `<head>` is loading and fewer than 3 s passed since the commit
    /// (AC-22).
    pub fn render_blocked(&self, now: Instant) -> bool {
        now.duration_since(self.committed_at) < RENDER_BLOCKING_CAP
            && self.sources.iter().any(|source| {
                matches!(
                    &source.kind,
                    Kind::Link(Link {
                        blocking: true,
                        state: LinkState::Loading { .. },
                        ..
                    })
                )
            })
    }

    /// The viewport changed. Restyles the document when a media query
    /// result flipped or a value uses viewport units (AC-19).
    pub fn set_viewport(&mut self, doc: &mut Document, width: Au, height: Au) {
        let device = Device {
            width: width.to_f32_px(),
            height: height.to_f32_px(),
            color_scheme: self.stylist.device().color_scheme,
            reduced_motion: self.stylist.device().reduced_motion,
        };
        if self.stylist.set_device(device) {
            doc.restyle_all();
        }
    }

    /// The OS color scheme or reduced motion setting changed. Restyles the
    /// document when a media query result flipped (spec 0010, AC-13).
    pub fn set_preferences(
        &mut self,
        doc: &mut Document,
        color_scheme: ColorScheme,
        reduced_motion: bool,
    ) {
        let mut device = self.stylist.device();
        device.color_scheme = color_scheme;
        device.reduced_motion = reduced_motion;
        if self.stylist.set_device(device) {
            doc.restyle_all();
        }
    }

    /// The style pass: every element on a dirty path whose style must be
    /// recomputed takes it from the sharing cache, else from the cascade
    /// (AC-18, AC-21). An element whose style did not change keeps its old
    /// `Rc`, and its children are only restyled when it did change.
    pub fn style_pass(&self, doc: &mut Document, container_sizes: &ContainerSizes) -> StyleStats {
        let mut stats = StyleStats::default();
        let base = doc.url().to_owned();
        let style_name = QualName::attribute(LocalName::from("style"));
        doc.restyle_dirty(|doc, node, needs_style| {
            stats.visited += 1;
            if !needs_style {
                return false;
            }
            stats.styled += 1;
            let root_font_size = doc
                .document_element()
                .and_then(|root| element_style(doc, root))
                .map_or(Au::from_px(16.0), |style| style.font.size);
            if let Some(shared) = doc.shared_style(node) {
                stats.shared += 1;
                let changed = set_style(doc, node, shared);
                self.placeholder_pass(doc, node, root_font_size);
                return changed;
            }
            let parent_style = doc
                .flat_parent(node)
                .and_then(|parent| element_style(doc, parent));
            // The nearest ancestor container's size, and the container names
            // in effect, for `cq*` units and `@container name` gating
            // (spec 0010, AC-17). Ancestors are styled before this element,
            // so their sizes from the previous layout pass are available.
            let (container_size, container_names) = {
                let mut size = None;
                let mut names = Vec::new();
                let mut current = doc.flat_parent(node);
                while let Some(ancestor) = current {
                    if let Some(ancestor_style) = element_style(&*doc, ancestor)
                        && ancestor_style.size.container_type != ContainerType::Normal
                    {
                        if size.is_none() {
                            size = container_sizes.get(&ancestor).copied();
                        }
                        names.extend(ancestor_style.size.container_name.0.iter().cloned());
                    }
                    current = doc.flat_parent(ancestor);
                }
                (size, names)
            };
            let container_context = css::ContainerContext {
                size: container_size,
                names: &container_names,
            };
            // Matching borrows the document; the results come out as ids.
            let (style, affected, has_subject) = {
                let Some(element) = doc.element_ref(node) else {
                    return false;
                };
                let style_attribute = css::MatchElement::attribute(&element, &style_name)
                    .map(|text| css::parse_declarations(text, Some(&base)))
                    .unwrap_or_default();
                let mut context = MatchContext::default();
                let (style, subject) = self.stylist.cascade(
                    &element,
                    parent_style.as_deref(),
                    &style_attribute,
                    root_font_size,
                    &container_context,
                    &mut context,
                );
                let affected: Vec<NodeId> =
                    context.sibling_dependent.iter().map(|e| e.id()).collect();
                (style, affected, subject)
            };
            for id in affected {
                let _ = doc.mark_affected_by_sibling_selectors(id);
            }
            let changed = set_style(doc, node, Rc::new(style));
            let _ = doc.set_has_subject(node, has_subject);
            doc.remember_style(node);
            self.placeholder_pass(doc, node, root_font_size);
            changed
        });
        stats
    }

    /// Gives an `<input>` that shows its placeholder (a `placeholder`
    /// attribute and an empty `value`) its `::placeholder` style, and takes
    /// it from any other element (spec 0006, AC-17).
    fn placeholder_pass(&self, doc: &mut Document, node: NodeId, root_font_size: Au) {
        let attribute = |doc: &Document, name: LocalName| {
            doc.element_ref(node).and_then(|element| {
                css::MatchElement::attribute(&element, &QualName::attribute(name))
                    .map(str::to_owned)
            })
        };
        let shows_placeholder = doc.is_text_field(node)
            && attribute(doc, local_name!("placeholder")).is_some_and(|p| !p.is_empty())
            && attribute(doc, local_name!("value")).is_none_or(|v| v.is_empty());
        let style = shows_placeholder
            .then(|| {
                let input_style = element_style(doc, node)?;
                let element = doc.element_ref(node)?;
                Some(
                    self.stylist
                        .compute_placeholder(&element, &input_style, root_font_size),
                )
            })
            .flatten();
        let unchanged = doc
            .node(node)
            .and_then(|n| n.as_element())
            .is_some_and(|e| e.placeholder_style().map(|s| &**s) == style.as_deref());
        if !unchanged {
            let _ = doc.set_placeholder_style(node, style);
        }
    }
}

/// Gives `node` `style`, unless its current style is equal, which it keeps
/// so nothing downstream sees a change. Returns whether it changed.
fn set_style(doc: &mut Document, node: NodeId, style: Rc<ComputedStyle>) -> bool {
    if element_style(doc, node).is_some_and(|old| *old == *style) {
        return false;
    }
    let _ = doc.set_style(node, Some(style));
    true
}

fn element_style(doc: &Document, node: NodeId) -> Option<Rc<ComputedStyle>> {
    doc.node(node)?.as_element()?.style().cloned()
}

fn with_media(sheet: &Rc<Stylesheet>, media: Option<&str>) -> Rc<Stylesheet> {
    let mut copy = (**sheet).clone();
    copy.media = media.map(|m| Rc::new(MediaList::parse(m)));
    Rc::new(copy)
}

/// Every HTML `<style>` and `<link>` in the document tree, in tree order,
/// with whether it sits inside `<head>`.
fn style_elements(doc: &Document) -> Vec<(NodeId, bool)> {
    let mut found = Vec::new();
    let mut stack: Vec<(NodeId, bool)> = vec![(doc.root(), false)];
    while let Some((id, in_head)) = stack.pop() {
        let in_head = in_head || is_named(doc, id, "head");
        if is_named(doc, id, "style") || is_named(doc, id, "link") {
            found.push((id, in_head));
        }
        let children: Vec<NodeId> = doc.children(id).collect();
        stack.extend(children.into_iter().rev().map(|child| (child, in_head)));
    }
    found
}

fn is_named(doc: &Document, node: NodeId, local: &str) -> bool {
    doc.node(node)
        .and_then(|n| n.as_element())
        .is_some_and(|element| {
            let name = element.name();
            name.ns == ns!("http://www.w3.org/1999/xhtml") && &*name.local == local
        })
}

fn attribute(doc: &Document, node: NodeId, name: &str) -> Option<String> {
    let element = doc.node(node)?.as_element()?;
    element
        .attribute(&QualName::attribute(LocalName::from(name)))
        .map(str::to_owned)
}

/// The text of `node`'s text children, joined: a `<style>` sheet.
fn text_content(doc: &Document, node: NodeId) -> String {
    let text: String = doc
        .children(node)
        .filter_map(|child| match doc.node(child)?.data() {
            NodeData::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    // An `.xht` style element wraps its sheet in `<![CDATA[ ... ]]>`, which
    // an XHTML parser strips (spec 0005, AC-18).
    crate::strip_cdata(&text).to_owned()
}

/// The absolute URL a `<link>` loads as a style sheet: `rel` holds
/// `stylesheet` but not `alternate`, and `href` is set and resolves.
fn stylesheet_url(doc: &Document, node: NodeId) -> Option<String> {
    let rel = attribute(doc, node, "rel")?.to_ascii_lowercase();
    let mut words = rel.split_ascii_whitespace();
    let words: Vec<&str> = words.by_ref().collect();
    if !words.contains(&"stylesheet") || words.contains(&"alternate") {
        return None;
    }
    let href = attribute(doc, node, "href")?;
    let href = href.trim();
    if href.is_empty() {
        return None;
    }
    let base = url::Url::parse(doc.url()).ok()?;
    Some(base.join(href).ok()?.into())
}

/// The `charset` parameter of a `Content-Type` value.
fn charset(content_type: &str) -> Option<String> {
    content_type.split(';').skip(1).find_map(|param| {
        let (name, value) = param.split_once('=')?;
        name.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| value.trim().trim_matches('"').to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charset_comes_from_the_content_type_parameter() {
        assert_eq!(
            charset("text/css; charset=\"ISO-8859-1\"").as_deref(),
            Some("ISO-8859-1")
        );
        assert_eq!(charset("text/css"), None);
    }
}
