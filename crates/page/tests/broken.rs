//! AC-17: the broken HTML corpus (`tests/corpus/broken/`) loads through
//! the page the way a real navigation does, in small chunks, and every page
//! parses and renders a frame. The DOM dumps are `insta` snapshots, so a
//! tree builder change that alters a recovered tree shows up in review.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

use std::path::Path;

use display_list::{PageId, SizeF};
use ipc::{DocumentKind, FetchId, NavigationId};
use page::Page;

const NAV: NavigationId = NavigationId(1);

/// Loads `bytes` as a document in 7 byte chunks and draws the first frame.
fn load(bytes: &[u8]) -> Page {
    let viewport = SizeF {
        width: 800.0,
        height: 600.0,
    };
    let mut page = Page::blank(PageId(1), viewport, 1.0, page::ua_stylesheet());
    page.navigate(NAV);
    assert!(page.commit_navigation(NAV, "file:///corpus/", None, &DocumentKind::Document));
    for chunk in bytes.chunks(7) {
        page.fetch_chunk(NAV, FetchId::DOCUMENT, chunk);
    }
    page.fetch_done(NAV, FetchId::DOCUMENT);
    assert!(page.begin_frame(0.0).is_some(), "the page drew no frame");
    page
}

fn corpus(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/corpus/broken")
        .join(name);
    std::fs::read(path).unwrap()
}

#[test]
fn every_broken_page_parses_and_renders() {
    for name in [
        "unclosed.html",
        "misnested.html",
        "stray-table.html",
        "truncated.html",
        "nul-bytes.html",
        "foreign.html",
    ] {
        let page = load(&corpus(name));
        let snapshot = name.trim_end_matches(".html");
        insta::assert_snapshot!(snapshot, page.main_frame().document.dump());
    }
}

#[test]
fn a_page_nested_past_the_cap_stays_within_it() {
    let page = load(&corpus("deep.html"));
    let dump = page.main_frame().document.dump();
    // The dump indents two spaces per level; the document is level 0.
    let deepest = dump
        .lines()
        .map(|line| (line.len() - line.trim_start().len()) / 2)
        .max()
        .unwrap();
    assert!(
        deepest <= html::MAX_DEPTH + 1,
        "nested {deepest} levels deep"
    );
    assert!(
        dump.contains("\"deepest text\""),
        "the deepest text was kept"
    );
}
