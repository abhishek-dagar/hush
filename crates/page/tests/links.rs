//! Spec 0008, milestone 1: an activated link resolves to an absolute URL
//! the browser can check and load (AC-17), and the page reports its `load`
//! event (AC-25).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

use display_list::{PageId, SizeF};
use ipc::{DocumentKind, FetchId, InputEvent, NavigationId, PointerButton, ToBrowser};
use page::Page;

const NAV: NavigationId = NavigationId(1);

/// A committed, parsed page at `url` that drew its first frame.
fn load(url: &str, html: &str) -> Page {
    let viewport = SizeF {
        width: 800.0,
        height: 600.0,
    };
    let mut page = Page::blank(PageId(1), viewport, 1.0, page::ua_stylesheet());
    page.navigate(NAV);
    assert!(page.commit_navigation(NAV, url, Some("utf-8".into()), &DocumentKind::Document));
    page.end_task();
    page.fetch_chunk(NAV, FetchId::DOCUMENT, html.as_bytes());
    page.end_task();
    page.fetch_done(NAV, FetchId::DOCUMENT);
    page.end_task();
    assert!(page.begin_frame(0.0).is_some(), "no first frame");
    page
}

const LINKS: &str = "<!doctype html><style>
    body { margin: 0 } a { display: block; height: 20px }
    </style>
    <a id=rel href='next.html'><span>Next</span></a>
    <a id=js href='javascript:alert(1)'>Script</a>
    <a id=none>No href</a>";

/// Clicks with `button` at `y` and returns the URL the activation goes to.
fn click(page: &mut Page, y: f32, button: PointerButton) -> Option<String> {
    page.input(InputEvent::PointerDown { x: 5.0, y, button });
    let outcome = page.input(InputEvent::PointerUp { x: 5.0, y, button });
    outcome.activated.and_then(|node| page.link_url(node))
}

#[test]
fn a_click_on_a_link_or_its_text_resolves_against_the_document() {
    // AC-17
    let mut page = load("http://test/dir/page.html", LINKS);
    assert_eq!(
        click(&mut page, 5.0, PointerButton::Primary).as_deref(),
        Some("http://test/dir/next.html")
    );
}

#[test]
fn a_middle_click_follows_the_link_too() {
    // AC-17
    let mut page = load("http://test/dir/page.html", LINKS);
    assert_eq!(
        click(&mut page, 5.0, PointerButton::Middle).as_deref(),
        Some("http://test/dir/next.html")
    );
    assert_eq!(
        click(&mut page, 45.0, PointerButton::Middle),
        None,
        "no href"
    );
}

#[test]
fn the_first_base_href_wins() {
    // AC-17
    let html = format!(
        "<head><base href='https://cdn.test/root/'><base href='https://other.test/'></head>{LINKS}"
    );
    let mut page = load("http://test/dir/page.html", &html);
    assert_eq!(
        click(&mut page, 5.0, PointerButton::Primary).as_deref(),
        Some("https://cdn.test/root/next.html")
    );
}

#[test]
fn javascript_links_and_anchors_without_href_go_nowhere() {
    // AC-17
    let mut page = load("http://test/page.html", LINKS);
    assert_eq!(click(&mut page, 25.0, PointerButton::Primary), None);
    assert_eq!(click(&mut page, 45.0, PointerButton::Primary), None);
}

#[test]
fn load_complete_is_reported_once_for_the_committed_navigation() {
    // AC-25
    let mut page = load("http://test/page.html", LINKS);
    assert_eq!(
        page.take_load_message(),
        Some(ToBrowser::LoadComplete {
            page: PageId(1),
            navigation: NAV
        })
    );
    assert_eq!(page.take_load_message(), None);
}
