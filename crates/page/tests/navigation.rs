//! Spec 0008, milestone 4: fragments scroll without a reload (AC-20, AC-21),
//! a restored offset lands after layout (AC-23), and scripts' `location`
//! and `history` reach the browser as requests (AC-27).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

use base::atoms::Atom;
use display_list::{PageId, SizeF};
use dom::FrameId;
use ipc::{
    DocumentKind, FetchId, HistoryInfo, NavigationId, PageNavigationAction, PageNavigationOrigin,
    ScrollTarget, ToBrowser,
};
use page::Page;

const NAV: NavigationId = NavigationId(1);
const URL: &str = "http://test/dir/page.html";

/// A long page: a 2000 px block, then `#later`, then more.
const LONG: &str = "<body style='margin:0'><div style='height:2000px'></div>\
     <h2 id=later style='margin:0;height:40px'>Later</h2><a name=old></a>\
     <div style='height:2000px'></div></body>";

fn load_at(url: &str, html: &str, restore: Option<(f32, f32)>) -> Page {
    let viewport = SizeF {
        width: 800.0,
        height: 600.0,
    };
    let mut page = Page::blank(PageId(1), viewport, 1.0, page::ua_stylesheet());
    page.navigate(NAV);
    assert!(page.commit_navigation(NAV, url, Some("utf-8".into()), &DocumentKind::Document));
    page.restore_scroll(NAV, restore);
    page.end_task();
    page.fetch_chunk(NAV, FetchId::DOCUMENT, html.as_bytes());
    page.fetch_done(NAV, FetchId::DOCUMENT);
    page.end_task();
    settle(&mut page);
    page
}

fn settle(page: &mut Page) {
    page.end_task();
    for _ in 0..8 {
        if page.begin_frame(0.0).is_none() && !page.take_frame_request() {
            break;
        }
    }
}

/// The root viewport's scroll offset.
fn root_scroll(page: &Page) -> f32 {
    let root = page.main_frame().document.document_element().unwrap();
    page.scroll_offset(paint::scroll_id(FrameId::MAIN.0, root))
        .y
}

fn top_of(page: &Page, id: &str) -> f32 {
    let node = page
        .main_frame()
        .document
        .element_by_id(&Atom::from(id))
        .unwrap();
    page.element_boxes()
        .into_iter()
        .find(|(n, _)| *n == node)
        .unwrap()
        .1
        .origin
        .y
}

#[test]
fn a_fragment_move_scrolls_to_its_target_without_a_new_document() {
    // AC-20
    let mut page = load_at(URL, LONG, None);
    assert_eq!(root_scroll(&page), 0.0);
    let history = HistoryInfo {
        length: 2,
        index: 1,
    };
    page.same_document(
        NAV,
        "http://test/dir/page.html#later",
        ScrollTarget::Fragment,
        history,
    );
    settle(&mut page);
    assert_eq!(root_scroll(&page), top_of(&page, "later"));
    assert_eq!(
        page.main_frame().document.url(),
        "http://test/dir/page.html#later"
    );
    assert_eq!(page.history(), history);
    assert!(page.fragment_node().is_some());

    // An `<a name>` counts too, and `#top` with no match goes to the top.
    page.same_document(NAV, &format!("{URL}#old"), ScrollTarget::Fragment, history);
    settle(&mut page);
    assert!(root_scroll(&page) > 2000.0);
    page.same_document(NAV, &format!("{URL}#top"), ScrollTarget::Fragment, history);
    settle(&mut page);
    assert_eq!(root_scroll(&page), 0.0);

    // A saved offset, for back inside the document (AC-21).
    page.same_document(NAV, URL, ScrollTarget::Offset(0.0, 700.0), history);
    settle(&mut page);
    assert_eq!(root_scroll(&page), 700.0);
}

#[test]
fn a_new_document_starts_at_its_fragment_or_its_restored_offset() {
    // AC-23
    let page = load_at(&format!("{URL}#later"), LONG, None);
    assert_eq!(root_scroll(&page), top_of(&page, "later"));
    let page = load_at(URL, LONG, Some((0.0, 1234.0)));
    assert_eq!(root_scroll(&page), 1234.0);
}

#[test]
fn a_user_scroll_ends_a_restore_in_flight() {
    // AC-23: "or you scroll yourself".
    let mut page = load_at(
        URL,
        "<body style='margin:0'><div id=box></div></body>",
        Some((0.0, 900.0)),
    );
    let root = page.main_frame().document.document_element().unwrap();
    let id = paint::scroll_id(FrameId::MAIN.0, root);
    page.scroll_changed(id, 0.0, 10.0);
    page.invalidate();
    settle(&mut page);
    assert_eq!(root_scroll(&page), 10.0, "the user's scroll stays");
}

#[test]
fn location_and_history_become_requests_for_the_browser() {
    // AC-27
    let mut page = load_at(
        URL,
        "<script>location.assign('next.html'); location.replace('/top.html');
         location.reload(); history.back(); history.go(2);</script>",
        None,
    );
    let messages = page.take_navigation_messages();
    let load = |url: &str, action| ToBrowser::NavigateRequest {
        page: PageId(1),
        navigation: NAV,
        url: url.to_owned(),
        origin: PageNavigationOrigin::Script,
        action,
    };
    let go = |delta| ToBrowser::HistoryGo {
        page: PageId(1),
        navigation: NAV,
        delta,
    };
    assert_eq!(
        messages,
        [
            load("http://test/dir/next.html", PageNavigationAction::Push),
            load("http://test/top.html", PageNavigationAction::Replace),
            load(URL, PageNavigationAction::Reload),
            go(-1),
            go(2),
        ]
    );
    assert!(page.take_navigation_messages().is_empty(), "taken once");
}
