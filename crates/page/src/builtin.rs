//! Built in documents for responses that are not HTML (spec 0035): text
//! and JSON pages, the image page, and the "can't show" page.
//!
//! `net` decides the kind; the page builds each document itself from a
//! fixed template or the body as text, and runs it through the normal
//! style, layout and paint path. No script engine starts for them and they
//! fetch nothing: the image page decodes the navigation's own body (AC-6,
//! AC-12). Every server controlled value is escaped into the templates.

use std::rc::Rc;

use base::atoms::{LocalName, QualName};
use css::{Origin, Stylesheet};
use dom::{Document, ElementState, NodeData, NodeId};
use html::{Parser, ParserOptions};
use ipc::{DocumentKind, FetchId};
use tracing::{debug, info};

use crate::anchors::PendingScroll;
use crate::{DomSink, Page};

/// The largest JSON body the pretty print checkbox is offered for, in
/// bytes (spec 0035, AC-2).
pub(crate) const JSON_PRETTY_MAX_BYTES: usize = 10 * 1024 * 1024;

const TEXT_CSS: &str = include_str!("builtin/text.css");
const IMAGE_HTML: &str = include_str!("builtin/image.html");
const UNVIEWABLE_HTML: &str = include_str!("builtin/unviewable.html");

/// The built in document a page shows, with its own state.
#[derive(Debug)]
pub(crate) enum Builtin {
    /// A text page.
    Text,
    /// A JSON page.
    Json(JsonView),
    /// An image page.
    Image(ImageView),
    /// The "can't show" page.
    Unviewable,
}

impl Builtin {
    /// The built in document for `kind`, or `None` for HTML and the error
    /// page.
    pub(crate) fn for_kind(kind: &DocumentKind) -> Option<Self> {
        match kind {
            DocumentKind::Document | DocumentKind::Error { .. } => None,
            DocumentKind::PlainText => Some(Self::Text),
            DocumentKind::Json => Some(Self::Json(JsonView::default())),
            DocumentKind::Image { file_name, mime } => Some(Self::Image(ImageView {
                file_name: file_name.clone(),
                mime: mime.clone(),
                img: None,
                natural: None,
                actual: false,
                zoomable: false,
            })),
            DocumentKind::Unviewable { .. } => Some(Self::Unviewable),
        }
    }

    /// Whether the document's body is parsed as text.
    pub(crate) fn is_text(&self) -> bool {
        matches!(self, Self::Text | Self::Json(_))
    }
}

/// The image page's state (spec 0035, State transitions).
#[derive(Debug)]
pub(crate) struct ImageView {
    file_name: String,
    mime: String,
    /// The `<img>` the template made.
    img: Option<NodeId>,
    /// The decoded size, once decoded.
    natural: Option<(u32, u32)>,
    /// Shown at actual size rather than fitted.
    actual: bool,
    /// Larger than the viewport, so a click zooms (AC-5).
    zoomable: bool,
}

/// The JSON page's state (spec 0035, State transitions).
#[derive(Debug, Default)]
pub(crate) struct JsonView {
    /// The `<pre>` holding the text.
    pre: Option<NodeId>,
    /// The checkbox, once the body is in and valid.
    toggle: Option<NodeId>,
    raw: String,
    pretty: String,
    /// The text shown is the pretty one.
    checked: bool,
}

/// The rules text and JSON documents add after the UA sheet.
pub(crate) fn text_sheet() -> Rc<Stylesheet> {
    Rc::new(css::parse_stylesheet(TEXT_CSS, Origin::Ua, None))
}

impl Page {
    /// Builds the image page or the "can't show" page for a just
    /// committed `kind` from its template. Other kinds parse their body.
    pub(crate) fn build_builtin(&mut self, url: &str, kind: &DocumentKind) {
        match kind {
            DocumentKind::Image { file_name, .. } => {
                let html = fill(IMAGE_HTML, &[("title", file_name), ("src", url)]);
                if let Some(images) = &mut self.images {
                    images.adopt_document(url);
                }
                self.parse_builtin(&html);
                if let Some(Builtin::Image(view)) = &mut self.builtin {
                    view.img = find_element(&self.main_frame.document, "img");
                }
            }
            DocumentKind::Unviewable {
                file_name,
                mime,
                size,
            } => {
                let details = match size {
                    Some(bytes) => format!("{mime} \u{b7} {}", format_size(*bytes)),
                    None => mime.clone(),
                };
                let html = fill(
                    UNVIEWABLE_HTML,
                    &[("title", file_name), ("details", &details)],
                );
                self.parse_builtin(&html);
            }
            _ => {}
        }
    }

    /// Parses a whole template at once, with script off.
    fn parse_builtin(&mut self, html: &str) {
        let mut parser = Parser::new(ParserOptions {
            encoding_hint: Some("utf-8".into()),
            scripting: false,
            plain_text: false,
        });
        let document = &mut self.main_frame.document;
        let step = parser.feed(&mut DomSink::new(document), html.as_bytes());
        self.parser = Some(parser);
        self.pump(step);
        if let Some(parser) = &mut self.parser {
            let step = parser.end(&mut DomSink::new(&mut self.main_frame.document));
            self.pump(step);
        }
    }

    /// Whether the image page is waiting for the navigation's body.
    pub(crate) fn image_body_pending(&self) -> bool {
        matches!(&self.builtin, Some(Builtin::Image(view)) if view.natural.is_none())
            && self
                .images
                .as_ref()
                .is_some_and(|i| i.owns(FetchId::DOCUMENT))
    }

    /// The image page's body ended: decode it. A picture gives the title
    /// its size (AC-4); bytes that don't decode turn the page into the
    /// "can't show" page in the same history entry (AC-7).
    pub(crate) fn image_body_done(&mut self) {
        let Some(images) = &mut self.images else {
            return;
        };
        let received = images.received(FetchId::DOCUMENT).unwrap_or_default();
        images.fetch_done(
            &mut self.main_frame.document,
            FetchId::DOCUMENT,
            &mut self.resources,
        );
        self.needs_update = true;
        let natural = images.natural_size(FetchId::DOCUMENT);
        let Some(Builtin::Image(view)) = &mut self.builtin else {
            return;
        };
        let Some((width, height)) = natural else {
            info!(
                received,
                "image page did not decode; showing the can't show page"
            );
            let kind = DocumentKind::Unviewable {
                file_name: view.file_name.clone(),
                mime: view.mime.clone(),
                size: Some(received as u64),
            };
            let url = self.main_frame.document.url().to_owned();
            self.commit_navigation(self.committed, &url, None, &kind);
            return;
        };
        view.natural = Some((width, height));
        let title = format!("{} ({width} \u{d7} {height})", view.file_name);
        set_title(&mut self.main_frame.document, &title);
        self.image_viewport_changed();
    }

    /// The viewport changed, or the image decoded: an image larger than
    /// the viewport zooms on a click; one that fits again leaves the
    /// actual size view (AC-5).
    pub(crate) fn image_viewport_changed(&mut self) {
        let viewport = self.main_frame.viewport;
        let Some(Builtin::Image(view)) = &mut self.builtin else {
            return;
        };
        let Some((width, height)) = view.natural else {
            return;
        };
        view.zoomable = width as f32 > viewport.width.to_f32_px()
            || height as f32 > viewport.height.to_f32_px();
        if !view.zoomable && view.actual {
            view.actual = false;
            self.pending_scroll = Some(PendingScroll::Offset(0.0, 0.0));
        }
        self.apply_image_class();
    }

    /// Gives the `<img>` the class its view asks for.
    fn apply_image_class(&mut self) {
        let Some(Builtin::Image(view)) = &self.builtin else {
            return;
        };
        let Some(img) = view.img else {
            return;
        };
        let class = match (view.actual, view.zoomable) {
            (true, _) => "actual",
            (false, true) => "zoomable",
            (false, false) => "",
        };
        let doc = &mut self.main_frame.document;
        let name = QualName::attribute(LocalName::from("class"));
        let _ = doc.set_attribute(img, name, class);
        self.needs_update = true;
    }

    /// A primary click on `node` at `x`, `y` (viewport CSS px): on a
    /// zoomable image it flips between the fit and actual size views,
    /// keeping the clicked image point under the pointer (AC-5).
    pub(crate) fn image_click(&mut self, node: Option<NodeId>, x: f32, y: f32) {
        let Some(Builtin::Image(view)) = &self.builtin else {
            return;
        };
        let (Some(img), Some((width, height))) = (view.img, view.natural) else {
            return;
        };
        if node != Some(img) || !view.zoomable {
            return;
        }
        let to_actual = !view.actual;
        let offset = if to_actual {
            let Some((_, rect)) = self.element_boxes().into_iter().find(|(n, _)| *n == img) else {
                return;
            };
            let viewport = self.main_frame.viewport;
            let (view_w, view_h) = (viewport.width.to_f32_px(), viewport.height.to_f32_px());
            let (width, height) = (width as f32, height as f32);
            // The image point under the pointer, in image pixels.
            let point_x = (x - rect.origin.x) * width / rect.size.width.max(1.0);
            let point_y = (y - rect.origin.y) * height / rect.size.height.max(1.0);
            // Where the image sits at actual size: centered on an axis it
            // fits, else at the start.
            let origin_x = ((view_w - width) / 2.0).max(0.0);
            let origin_y = ((view_h - height) / 2.0).max(0.0);
            let scroll_x = (origin_x + point_x - x).clamp(0.0, (width - view_w).max(0.0));
            let scroll_y = (origin_y + point_y - y).clamp(0.0, (height - view_h).max(0.0));
            (scroll_x, scroll_y)
        } else {
            (0.0, 0.0)
        };
        debug!(to_actual, ?offset, "image zoom");
        if let Some(Builtin::Image(view)) = &mut self.builtin {
            view.actual = to_actual;
        }
        self.pending_scroll = Some(PendingScroll::Offset(offset.0, offset.1));
        self.apply_image_class();
    }

    /// The JSON page's body ended: when it is valid and small enough, the
    /// "Pretty print" checkbox goes above the text, unchecked (AC-2).
    pub(crate) fn json_body_done(&mut self) {
        let Some(Builtin::Json(view)) = &mut self.builtin else {
            return;
        };
        let doc = &mut self.main_frame.document;
        let Some(pre) = find_element(doc, "pre") else {
            return;
        };
        let raw = text_content(doc, pre);
        if raw.len() > JSON_PRETTY_MAX_BYTES {
            info!(bytes = raw.len(), "JSON too large to pretty print");
            return;
        }
        let Some(pretty) = json_pretty(&raw) else {
            debug!("not valid JSON; no pretty print");
            return;
        };
        let Some(parent) = doc.node(pre).and_then(|n| n.parent()) else {
            return;
        };
        let label = doc.create_element(QualName::html(LocalName::from("label")));
        let class = QualName::attribute(LocalName::from("class"));
        let _ = doc.set_attribute(label, class, "pretty-print");
        let toggle = doc.create_element(QualName::html(LocalName::from("input")));
        let kind = QualName::attribute(LocalName::from("type"));
        let _ = doc.set_attribute(toggle, kind, "checkbox");
        let text = doc.create_text("Pretty print");
        let inserted = doc
            .append_child(label, toggle)
            .and_then(|()| doc.append_child(label, text))
            .and_then(|()| doc.insert_before(parent, label, Some(pre)));
        if inserted.is_err() {
            return;
        }
        *view = JsonView {
            pre: Some(pre),
            toggle: Some(toggle),
            raw,
            pretty,
            checked: false,
        };
        self.needs_update = true;
    }

    /// After an input event: when the checkbox's checkedness changed, the
    /// text swaps between raw and pretty (AC-2).
    pub(crate) fn json_sync_toggle(&mut self) {
        let Some(Builtin::Json(view)) = &mut self.builtin else {
            return;
        };
        let (Some(pre), Some(toggle)) = (view.pre, view.toggle) else {
            return;
        };
        let doc = &mut self.main_frame.document;
        let checked = doc
            .node(toggle)
            .and_then(|n| n.as_element())
            .is_some_and(|e| e.state().contains(ElementState::CHECKED));
        if checked == view.checked {
            return;
        }
        view.checked = checked;
        let text = if checked { &view.pretty } else { &view.raw };
        replace_text(doc, pre, text);
        self.needs_update = true;
    }
}

/// Fills `{{name}}` slots in one pass, each value HTML escaped, so a value
/// that itself looks like a slot stays literal (AC-12).
fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            rest = &rest[start..];
            break;
        };
        let name = &after[..end];
        match values.iter().find(|(slot, _)| *slot == name) {
            Some((_, value)) => out.push_str(&escape(value)),
            None => out.push_str(&rest[start..start + 2 + end + 2]),
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

/// Escapes text for HTML text and quoted attribute values (the same
/// escaping as `net`'s built in pages).
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// A size in binary units with one decimal (spec 0035, Value sourcing):
/// bytes below 1 KB, else KB, MB or GB at 1,024.
fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 3] = ["KB", "MB", "GB"];
    if bytes < 1024 {
        return if bytes == 1 {
            "1 byte".into()
        } else {
            format!("{bytes} bytes")
        };
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// The first HTML element named `local` in tree order.
fn find_element(doc: &Document, local: &str) -> Option<NodeId> {
    let local = LocalName::from(local);
    let mut stack = vec![doc.root()];
    while let Some(node) = stack.pop() {
        if doc.is_html(node, local.clone()) {
            return Some(node);
        }
        let children: Vec<NodeId> = doc.children(node).collect();
        stack.extend(children.into_iter().rev());
    }
    None
}

/// The text of `node`'s text children.
fn text_content(doc: &Document, node: NodeId) -> String {
    doc.children(node)
        .filter_map(|child| match doc.node(child)?.data() {
            NodeData::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// Replaces `node`'s children with one text node.
fn replace_text(doc: &mut Document, node: NodeId, text: &str) {
    let children: Vec<NodeId> = doc.children(node).collect();
    for child in children {
        let _ = doc.remove_child(node, child);
    }
    let text = doc.create_text(text);
    let _ = doc.append_child(node, text);
}

/// Sets the first `<title>`'s text.
fn set_title(doc: &mut Document, title: &str) {
    if let Some(element) = find_element(doc, "title") {
        replace_text(doc, element, title);
    }
}

/// Reindents JSON with 2 spaces (spec 0035, AC-2), keeping key order and
/// the exact text of every number and string; `None` when `raw` is not
/// valid JSON. It works token by token with an explicit stack, so deep
/// nesting can't overflow the thread's stack.
pub(crate) fn json_pretty(raw: &str) -> Option<String> {
    #[derive(Clone, Copy)]
    enum Next {
        Value,
        Key,
        After,
    }
    let bytes = raw.as_bytes();
    let mut out = String::with_capacity(raw.len() + raw.len() / 2);
    // The open containers; `true` for an object.
    let mut stack: Vec<bool> = Vec::new();
    let mut at = skip_space(bytes, 0);
    let mut next = Next::Value;
    loop {
        match next {
            Next::Value => {
                let start = at;
                match *bytes.get(at)? {
                    open @ (b'{' | b'[') => {
                        let object = open == b'{';
                        let close = if object { b'}' } else { b']' };
                        at = skip_space(bytes, at + 1);
                        if bytes.get(at) == Some(&close) {
                            out.push_str(if object { "{}" } else { "[]" });
                            at += 1;
                            next = Next::After;
                        } else {
                            out.push(char::from(open));
                            stack.push(object);
                            new_line(&mut out, stack.len());
                            next = if object { Next::Key } else { Next::Value };
                        }
                        continue;
                    }
                    b'"' => at = string_end(bytes, at)?,
                    b't' => at = literal_end(bytes, at, b"true")?,
                    b'f' => at = literal_end(bytes, at, b"false")?,
                    b'n' => at = literal_end(bytes, at, b"null")?,
                    b'-' | b'0'..=b'9' => at = number_end(bytes, at)?,
                    _ => return None,
                }
                out.push_str(raw.get(start..at)?);
                next = Next::After;
            }
            Next::Key => {
                if bytes.get(at) != Some(&b'"') {
                    return None;
                }
                let end = string_end(bytes, at)?;
                out.push_str(raw.get(at..end)?);
                at = skip_space(bytes, end);
                if bytes.get(at) != Some(&b':') {
                    return None;
                }
                out.push_str(": ");
                at = skip_space(bytes, at + 1);
                next = Next::Value;
            }
            Next::After => {
                at = skip_space(bytes, at);
                let Some(&object) = stack.last() else {
                    return (at == bytes.len()).then_some(out);
                };
                match *bytes.get(at)? {
                    b',' => {
                        out.push(',');
                        new_line(&mut out, stack.len());
                        at = skip_space(bytes, at + 1);
                        next = if object { Next::Key } else { Next::Value };
                    }
                    close @ (b'}' | b']') if (close == b'}') == object => {
                        stack.pop();
                        new_line(&mut out, stack.len());
                        out.push(char::from(close));
                        at += 1;
                    }
                    _ => return None,
                }
            }
        }
    }
}

fn new_line(out: &mut String, depth: usize) {
    out.push('\n');
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn skip_space(bytes: &[u8], mut at: usize) -> usize {
    while matches!(bytes.get(at), Some(b' ' | b'\t' | b'\n' | b'\r')) {
        at += 1;
    }
    at
}

fn literal_end(bytes: &[u8], at: usize, literal: &[u8]) -> Option<usize> {
    bytes[at..]
        .starts_with(literal)
        .then_some(at + literal.len())
}

/// The end of the string starting at the quote at `at`, past its closing
/// quote; `None` for an unterminated string, a bad escape or a raw
/// control character.
fn string_end(bytes: &[u8], at: usize) -> Option<usize> {
    let mut i = at + 1;
    loop {
        match *bytes.get(i)? {
            b'"' => return Some(i + 1),
            b'\\' => match *bytes.get(i + 1)? {
                b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => i += 2,
                b'u' => {
                    let hex = bytes.get(i + 2..i + 6)?;
                    if !hex.iter().all(u8::is_ascii_hexdigit) {
                        return None;
                    }
                    i += 6;
                }
                _ => return None,
            },
            0x00..=0x1F => return None,
            _ => i += 1,
        }
    }
}

/// The end of the number starting at `at`: `-? (0 | [1-9][0-9]*)
/// (. [0-9]+)? ([eE] [+-]? [0-9]+)?`.
fn number_end(bytes: &[u8], at: usize) -> Option<usize> {
    let digits = |from: usize| {
        let mut end = from;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        (end > from).then_some(end)
    };
    let mut i = at;
    if bytes.get(i) == Some(&b'-') {
        i += 1;
    }
    i = match bytes.get(i)? {
        b'0' => i + 1,
        b'1'..=b'9' => digits(i)?,
        _ => return None,
    };
    if bytes.get(i) == Some(&b'.') {
        i = digits(i + 1)?;
    }
    if matches!(bytes.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(bytes.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        i = digits(i)?;
    }
    Some(i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pretty_print_reindents_and_keeps_text_exact() {
        // AC-2: key order and number and string text kept.
        let raw = r#"{"b":1.50,"a":[1,-2e+10,{"s":"x\"\u00e9"}],"e":{},"f":[],"t":true,"n":null}"#;
        let pretty = json_pretty(raw).unwrap();
        assert_eq!(
            pretty,
            "{\n  \"b\": 1.50,\n  \"a\": [\n    1,\n    -2e+10,\n    {\n      \"s\": \"x\\\"\\u00e9\"\n    }\n  ],\n  \"e\": {},\n  \"f\": [],\n  \"t\": true,\n  \"n\": null\n}"
        );
        assert_eq!(json_pretty(" 42 \n"), Some("42".into()));
        assert_eq!(json_pretty("[ ]"), Some("[]".into()));
    }

    #[test]
    fn invalid_json_gets_no_pretty_print() {
        // AC-2
        for raw in [
            "",
            "{",
            "[1,]",
            "{\"a\" 1}",
            "{a:1}",
            "01",
            "1.",
            "-",
            "\"\t\"",
            "\"\\x\"",
            "[1] 2",
            "tru",
            "{\"a\":1,}",
            "[}",
            "NaN",
        ] {
            assert_eq!(json_pretty(raw), None, "{raw:?}");
        }
    }

    #[test]
    fn deep_nesting_does_not_overflow_the_stack() {
        let raw = "[".repeat(100_000) + &"]".repeat(100_000);
        assert!(json_pretty(&raw).is_some());
    }

    #[test]
    fn templates_escape_every_value_once() {
        // AC-12
        let page = fill(
            "<title>{{title}}</title><p>{{details}}</p>{{other}}",
            &[
                ("title", "<script>alert(1)</script>{{details}}"),
                ("details", "a&b"),
            ],
        );
        assert_eq!(
            page,
            "<title>&lt;script&gt;alert(1)&lt;/script&gt;{{details}}</title><p>a&amp;b</p>{{other}}"
        );
        assert!(fill(IMAGE_HTML, &[("src", "x\" onerror=\"y")]).contains("x&quot; onerror"));
    }

    #[test]
    fn sizes_use_binary_units_with_one_decimal() {
        // Spec 0035, Value sourcing, size.
        assert_eq!(format_size(0), "0 bytes");
        assert_eq!(format_size(1), "1 byte");
        assert_eq!(format_size(1023), "1023 bytes");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(50 * 1024 * 1024), "50.0 MB");
        assert_eq!(format_size(3 * 1024 * 1024 * 1024), "3.0 GB");
    }
}
