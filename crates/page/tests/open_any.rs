//! Built in documents for responses that are not HTML (spec 0035): text
//! and JSON pages, the image page with its zoom, the "can't show" page,
//! and that none of them runs script or fetches anything.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

use base::atoms::LocalName;
use display_list::{DisplayList, ItemKind, PageId, RectF, SizeF};
use dom::{Document, NodeData, NodeId};
use ipc::{CursorKind, DocumentKind, FetchId, InputEvent, NavigationId, PointerButton, ToBrowser};
use page::Page;

const NAV: NavigationId = NavigationId(1);

fn png() -> Vec<u8> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../image/tests/images/two.png");
    std::fs::read(path).unwrap()
}

/// A page that committed `kind` at `url` in a `width` by `height`
/// viewport and got `body` in full.
fn loaded(url: &str, kind: DocumentKind, body: &[u8], width: f32, height: f32) -> Page {
    let viewport = SizeF { width, height };
    let mut page = Page::blank(PageId(1), viewport, 1.0, page::ua_stylesheet());
    page.navigate(NAV);
    assert!(page.commit_navigation(NAV, url, None, &kind));
    page.end_task();
    if !body.is_empty() {
        page.fetch_chunk(NAV, FetchId::DOCUMENT, body);
        page.end_task();
    }
    page.fetch_done(NAV, FetchId::DOCUMENT);
    page.end_task();
    page
}

fn frame(page: &mut Page) -> DisplayList {
    let list = page.begin_frame(0.0).expect("a frame");
    let _ = page.take_resource_messages();
    list
}

fn find(doc: &Document, local: &str) -> Option<NodeId> {
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

fn text(doc: &Document, node: NodeId) -> String {
    let mut out = String::new();
    let mut stack = vec![node];
    while let Some(id) = stack.pop() {
        if let Some(NodeData::Text(t)) = doc.node(id).map(|n| n.data()) {
            out.push_str(t);
        }
        let children: Vec<NodeId> = doc.children(id).collect();
        stack.extend(children.into_iter().rev());
    }
    out
}

fn rect_of(page: &Page, node: NodeId) -> RectF {
    page.element_boxes()
        .into_iter()
        .find(|(n, _)| *n == node)
        .map(|(_, rect)| rect)
        .expect("a box")
}

fn titles(page: &mut Page) -> Vec<String> {
    page.take_head_messages()
        .into_iter()
        .filter_map(|message| match message {
            ToBrowser::TitleChanged { title, .. } => Some(title),
            _ => None,
        })
        .collect()
}

fn click(page: &mut Page, x: f32, y: f32) {
    let button = PointerButton::Primary;
    page.input(InputEvent::PointerDown { x, y, button });
    page.input(InputEvent::PointerUp { x, y, button });
}

fn image_rects(list: &DisplayList) -> Vec<RectF> {
    list.items
        .iter()
        .filter_map(|item| match item.kind {
            ItemKind::Image { rect, .. } => Some(rect),
            _ => None,
        })
        .collect()
}

#[test]
fn text_pages_wrap_even_one_long_word() {
    // AC-1, AC-3: a minified line wider than the window wraps.
    let long = "x".repeat(400);
    for kind in [DocumentKind::PlainText, DocumentKind::Json] {
        let body = format!("\"{long}\"");
        let mut page = loaded("http://a/t", kind.clone(), body.as_bytes(), 200.0, 600.0);
        frame(&mut page);
        let doc = &page.main_frame().document;
        let pre = find(doc, "pre").unwrap();
        assert_eq!(text(doc, pre), body, "{kind:?}: bytes shown exactly");
        let rect = rect_of(&page, pre);
        assert!(rect.size.width <= 200.0, "{kind:?}: {rect:?}");
        assert!(rect.size.height > 60.0, "{kind:?}: many lines, {rect:?}");
    }
}

#[test]
fn json_pretty_prints_on_a_tick_and_back_on_another() {
    // AC-2
    let raw = r#"{"b":1.50,"a":[1,"x"]}"#;
    let mut page = loaded(
        "http://a/api",
        DocumentKind::Json,
        raw.as_bytes(),
        800.0,
        600.0,
    );
    frame(&mut page);
    let doc = &page.main_frame().document;
    let pre = find(doc, "pre").unwrap();
    let toggle = find(doc, "input").expect("a checkbox for valid JSON");
    let label = find(doc, "label").unwrap();
    assert!(text(doc, label).contains("Pretty print"));
    assert_eq!(text(doc, pre), raw, "unchecked shows the raw text");

    let at = rect_of(&page, toggle);
    let (x, y) = (at.origin.x + 2.0, at.origin.y + 2.0);
    click(&mut page, x, y);
    let pretty = "{\n  \"b\": 1.50,\n  \"a\": [\n    1,\n    \"x\"\n  ]\n}";
    assert_eq!(text(&page.main_frame().document, pre), pretty);
    frame(&mut page);
    click(&mut page, x, y);
    assert_eq!(text(&page.main_frame().document, pre), raw);
}

#[test]
fn invalid_json_shows_raw_text_with_no_checkbox() {
    // AC-2
    let page = loaded("http://a/api", DocumentKind::Json, b"{\"a\":", 800.0, 600.0);
    let doc = &page.main_frame().document;
    assert!(find(doc, "input").is_none());
    assert_eq!(text(doc, find(doc, "pre").unwrap()), "{\"a\":");
}

#[test]
fn json_without_a_charset_is_utf8() {
    // AC-1
    let body = "{\"name\":\"\u{e9}t\u{e9}\"}";
    let page = loaded(
        "http://a/api",
        DocumentKind::Json,
        body.as_bytes(),
        800.0,
        600.0,
    );
    let doc = &page.main_frame().document;
    assert_eq!(text(doc, find(doc, "pre").unwrap()), body);
}

fn image(file_name: &str) -> DocumentKind {
    DocumentKind::Image {
        file_name: file_name.into(),
        mime: "image/png".into(),
    }
}

#[test]
fn an_image_page_shows_the_body_centered_with_its_size_in_the_title() {
    // AC-4, AC-6, AC-12
    let mut page = loaded("http://a/two.png", image("two.png"), &png(), 800.0, 600.0);
    let list = frame(&mut page);
    assert_eq!(titles(&mut page).last().unwrap(), "two.png (4 \u{d7} 2)");
    assert!(
        page.take_fetch_messages().is_empty(),
        "no image fetch, no favicon"
    );
    assert_eq!(page.script_heap_bytes(), None, "no script engine");
    let drawn = image_rects(&list);
    assert_eq!(drawn.len(), 1);
    let rect = drawn[0];
    assert_eq!(
        (rect.size.width, rect.size.height),
        (4.0, 2.0),
        "never scaled up"
    );
    assert_eq!((rect.origin.x, rect.origin.y), (398.0, 299.0), "centered");
    let backdrop = list.items.iter().any(|item| {
        matches!(item.kind, ItemKind::Rect { color, .. }
            if (color.r, color.g, color.b) == (14.0 / 255.0, 14.0 / 255.0, 14.0 / 255.0))
    });
    assert!(backdrop, "the #0e0e0e backdrop");
}

#[test]
fn a_large_image_fits_then_zooms_to_actual_size_on_a_click() {
    // AC-4, AC-5: the 4 by 2 picture in a 2 by 1 window.
    let mut page = loaded("http://a/two.png", image("two.png"), &png(), 2.0, 1.0);
    let list = frame(&mut page);
    let fit = image_rects(&list)[0];
    assert_eq!(
        (fit.size.width, fit.size.height),
        (2.0, 1.0),
        "scaled to fit"
    );
    page.input(InputEvent::PointerMove { x: 1.0, y: 0.5 });
    frame(&mut page);
    assert_eq!(page.take_cursor_change(), Some(CursorKind::ZoomIn));

    click(&mut page, 1.0, 0.5);
    let list = frame(&mut page);
    let actual = image_rects(&list)[0];
    assert_eq!(
        (actual.size.width, actual.size.height),
        (4.0, 2.0),
        "one image pixel per CSS pixel"
    );
    let root = page.main_frame().document.document_element().unwrap();
    let scroll = page.scroll_offset(paint::scroll_id(0, root));
    assert_eq!(
        (scroll.x, scroll.y),
        (1.0, 0.5),
        "the clicked point stays under the pointer"
    );
    assert_eq!(
        page.take_cursor_change(),
        Some(CursorKind::ZoomOut),
        "the frame after the click restyles the cursor"
    );

    // A resize that makes it fit returns to the fit view.
    page.resize(
        SizeF {
            width: 800.0,
            height: 600.0,
        },
        1.0,
    );
    let list = frame(&mut page);
    let rect = image_rects(&list)[0];
    assert_eq!((rect.origin.x, rect.origin.y), (398.0, 299.0));
    click(&mut page, 399.0, 300.0);
    let list = frame(&mut page);
    assert_eq!(
        image_rects(&list)[0],
        rect,
        "a fitting image ignores clicks"
    );
}

#[test]
fn bytes_that_dont_decode_become_the_cant_show_page() {
    // AC-7, AC-9
    let mut page = loaded(
        "http://a/cat.png",
        image("cat.png"),
        b"\x89PNG broken",
        800.0,
        600.0,
    );
    frame(&mut page);
    let doc = &page.main_frame().document;
    assert_eq!(doc.url(), "http://a/cat.png", "same URL, same entry");
    let body = text(doc, find(doc, "body").unwrap());
    assert!(body.contains("cat.png"), "{body}");
    assert!(body.contains("image/png \u{b7} 11 bytes"), "{body}");
    assert!(body.contains("can't show this file"), "{body}");
    assert_eq!(titles(&mut page).last().unwrap(), "cat.png");
    assert!(page.take_fetch_messages().is_empty());
}

#[test]
fn the_cant_show_page_escapes_what_the_server_named() {
    // AC-8, AC-12
    let kind = DocumentKind::Unviewable {
        file_name: "<script>alert(1)</script>.bin".into(),
        mime: "application/octet-stream".into(),
        size: Some(50 * 1024 * 1024),
    };
    let mut page = loaded("http://a/x", kind, b"", 800.0, 600.0);
    frame(&mut page);
    let doc = &page.main_frame().document;
    assert!(find(doc, "script").is_none(), "no markup injected");
    let heading = find(doc, "h1").unwrap();
    assert_eq!(text(doc, heading), "<script>alert(1)</script>.bin");
    let body = text(doc, find(doc, "body").unwrap());
    assert!(
        body.contains("application/octet-stream \u{b7} 50.0 MB"),
        "{body}"
    );
    assert!(find(doc, "button").is_none(), "no buttons");
    assert_eq!(
        titles(&mut page).last().unwrap(),
        "<script>alert(1)</script>.bin"
    );
    assert_eq!(page.script_heap_bytes(), None);
    assert!(page.take_fetch_messages().is_empty());
}

#[test]
fn text_pages_run_no_script_and_fetch_no_favicon() {
    // AC-12, AC-13: an HTML page still gets both.
    let mut text_page = loaded(
        "http://a/t.txt",
        DocumentKind::PlainText,
        b"hi",
        800.0,
        600.0,
    );
    assert_eq!(text_page.script_heap_bytes(), None);
    assert!(text_page.take_fetch_messages().is_empty());
    let mut html = loaded("http://a/", DocumentKind::Document, b"<p>hi", 800.0, 600.0);
    assert!(html.script_heap_bytes().is_some());
    assert!(!html.take_fetch_messages().is_empty(), "the favicon");
}

#[test]
fn text_pages_follow_the_dark_theme_from_the_first_frame() {
    // A page committed after the OS went dark starts dark: the canvas is
    // dark and the text light, so it stays readable.
    let mut page = Page::blank(
        PageId(1),
        SizeF {
            width: 800.0,
            height: 600.0,
        },
        1.0,
        page::ua_stylesheet(),
    );
    page.set_preferences(css::ColorScheme::Dark, false);
    page.navigate(NAV);
    assert!(page.commit_navigation(NAV, "http://a/t.txt", None, &DocumentKind::PlainText));
    page.fetch_chunk(NAV, FetchId::DOCUMENT, b"hello");
    page.fetch_done(NAV, FetchId::DOCUMENT);
    page.end_task();
    let list = frame(&mut page);
    let dark = 0x12 as f32 / 255.0;
    let canvas = list.items.iter().any(|item| {
        matches!(item.kind, ItemKind::Rect { color, .. } if (color.r, color.g, color.b) == (dark, dark, dark))
    });
    assert!(canvas, "the #121212 canvas");
    let light_text = list.items.iter().any(
        |item| matches!(item.kind, ItemKind::Text { color, .. } if color.r > 0.8 && color.g > 0.8),
    );
    assert!(light_text, "light text");
}
