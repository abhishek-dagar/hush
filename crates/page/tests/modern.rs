//! AC-1, AC-4, AC-6, AC-8, AC-9, AC-19, AC-23, AC-27, AC-28: a card grid from
//! a `@layer` sheet that scales on hover through a bound transform.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

use std::path::Path;

use display_list::{PageId, SizeF, SpatialKind};
use ipc::{DocumentKind, FetchId, NavigationId};
use layout::BoxKind;
use page::Page;

const NAV: NavigationId = NavigationId(1);
const VIEWPORT: SizeF = SizeF {
    width: 800.0,
    height: 600.0,
};

/// Loads `bytes` as a document and draws frames until it settles, returning
/// the page and the last display list.
fn load(bytes: &[u8]) -> (Page, Option<display_list::DisplayList>) {
    let mut page = Page::blank(PageId(1), VIEWPORT, 1.0, page::ua_stylesheet());
    page.navigate(NAV);
    assert!(page.commit_navigation(NAV, "file:///modern/", None, &DocumentKind::Document));
    for chunk in bytes.chunks(7) {
        page.fetch_chunk(NAV, FetchId::DOCUMENT, chunk);
    }
    page.fetch_done(NAV, FetchId::DOCUMENT);
    page.end_task();
    // Draw frames until the page settles (no more NeedsFrame).
    let mut list = None;
    for _ in 0..10 {
        match page.begin_frame(0.0) {
            Some(l) => list = Some(l),
            None => break,
        }
    }
    (page, list)
}

fn thin_thread() -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/modern/thin-thread.html");
    std::fs::read(path).unwrap()
}

/// The grid container box exists.
#[test]
fn thin_thread_grid_container_exists() {
    let (page, _) = load(&thin_thread());
    let boxes = &page.main_frame().boxes;
    assert!(
        boxes.iter().any(|b| b.kind == BoxKind::GridContainer),
        "no grid container box"
    );
}

/// Grid container boxes have child items placed in cells.
#[test]
fn thin_thread_grid_container_has_items() {
    let (page, _) = load(&thin_thread());
    let boxes = &page.main_frame().boxes;
    let container = boxes
        .iter()
        .find(|b| b.kind == BoxKind::GridContainer)
        .expect("no grid container box");
    assert!(
        !container.children.is_empty(),
        "grid container has no child items"
    );
}

/// Transforms create reference frames in the display list (static bindings
/// for the initial scale; bound bindings once a transition runs).
#[test]
fn thin_thread_transform_reference_frames() {
    let (_page, list) = load(&thin_thread());
    let Some(list) = list else {
        panic!("no display list");
    };
    let has_reference_frame = list
        .spatial
        .iter()
        .any(|node| matches!(node.kind, SpatialKind::ReferenceFrame { .. }));
    assert!(
        has_reference_frame,
        "no reference frame spatial node in the display list"
    );
}
