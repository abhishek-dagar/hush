//! Spec 0006, milestone 5: `@font-face` faces load through the browser the
//! first time text needs them, hide that text for up to 3 s, move on past a
//! bad source, draw once they arrive, and stay inside their document, with
//! no script engine involved (AC-10, AC-11, AC-21).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

use std::time::{Duration, Instant};

use display_list::{DisplayList, FontKey, ItemKind, PageId, SizeF};
use ipc::{Destination, DocumentKind, FetchId, NavigationId, ToBrowser};
use page::{FontContext, Page};

const NAV: NavigationId = NavigationId(1);
const URL: &str = "http://test/dir/page.html";
const VIEWPORT: SizeF = SizeF {
    width: 800.0,
    height: 600.0,
};

/// Every Ahem glyph is a 1em square, so its text is easy to recognize.
fn ahem() -> Vec<u8> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tools/conformance/tests/fixtures/fonts/Ahem.ttf"
    );
    std::fs::read(path).unwrap()
}

fn cantarell() -> Vec<u8> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tools/conformance/fonts/Cantarell-Regular.ttf"
    );
    std::fs::read(path).unwrap()
}

/// A page that committed `html` and parsed it, with Cantarell standing in
/// for every installed font so the fallback is the same everywhere.
fn parsed(html: &str) -> Page {
    let mut page = Page::blank(PageId(1), VIEWPORT, 1.0, page::ua_stylesheet());
    page.use_fonts(FontContext::with_fonts([cantarell()]));
    page.navigate(NAV);
    assert!(page.commit_navigation(NAV, URL, Some("utf-8".into()), &DocumentKind::Document));
    page.end_task();
    page.fetch_chunk(NAV, FetchId::DOCUMENT, html.as_bytes());
    page.end_task();
    page.fetch_done(NAV, FetchId::DOCUMENT);
    page.end_task();
    page
}

/// The font fetches the page asked for, as `(fetch, url)`.
fn font_fetches(page: &mut Page) -> Vec<(FetchId, String)> {
    page.take_fetch_messages()
        .into_iter()
        .filter_map(|message| match message {
            ToBrowser::Fetch {
                fetch,
                url,
                destination: Destination::Font,
                ..
            } => Some((fetch, url)),
            _ => None,
        })
        .collect()
}

fn serve(page: &mut Page, fetch: FetchId, status: u16, body: &[u8]) {
    page.fetch_response(NAV, fetch, status, &[], "http://test/dir/x");
    page.fetch_chunk(NAV, fetch, body);
    page.fetch_done(NAV, fetch);
    page.end_task();
}

/// The next frame, and the fonts registered before it.
fn frame(page: &mut Page) -> (DisplayList, Vec<FontKey>) {
    let list = page.begin_frame(0.0).expect("a frame");
    let (adds, _) = page.take_resource_messages();
    let fonts = adds
        .into_iter()
        .filter_map(|message| match message {
            ToBrowser::AddFont { key, .. } => Some(key),
            _ => None,
        })
        .collect();
    (list, fonts)
}

/// Each `Text` item's font and its glyphs' x positions.
fn texts(list: &DisplayList) -> Vec<(FontKey, Vec<f32>)> {
    list.items
        .iter()
        .filter_map(|item| match &item.kind {
            ItemKind::Text { font, glyphs, .. } => {
                Some((*font, glyphs.iter().map(|g| g.point.x).collect()))
            }
            _ => None,
        })
        .collect()
}

const PAGE: &str = r#"<!DOCTYPE html>
<style>
  @font-face { font-family: X; src: url(missing.ttf), url(good.ttf) }
  body { margin: 0; font-size: 20px }
  p { font-family: X, sans-serif }
</style>
<p>xxx</p>"#;

#[test]
fn a_face_loads_on_first_need_moves_past_a_missing_source_and_then_draws() {
    // AC-10, AC-11, AC-21
    let mut page = parsed(PAGE);
    assert!(
        font_fetches(&mut page).is_empty(),
        "nothing loads before styled text needs it"
    );

    // While it loads, the text is laid out in the fallback but not drawn.
    let (hidden, _) = frame(&mut page);
    assert!(texts(&hidden).is_empty(), "the text waits for its font");
    let fetched = font_fetches(&mut page);
    assert_eq!(fetched.len(), 1);
    assert_eq!(fetched[0].1, "http://test/dir/missing.ttf");

    // A 404 moves on to the next `src`, still hidden.
    serve(&mut page, fetched[0].0, 404, b"not found");
    let fetched = font_fetches(&mut page);
    assert_eq!(fetched.len(), 1);
    assert_eq!(fetched[0].1, "http://test/dir/good.ttf");

    // It arrives: the text lays out again in Ahem, after its `AddFont`.
    serve(&mut page, fetched[0].0, 200, &ahem());
    assert!(page.take_frame_request(), "a loaded font asks for a frame");
    let (drawn, added) = frame(&mut page);
    let texts = texts(&drawn);
    assert_eq!(texts.len(), 1, "{texts:?}");
    let (font, xs) = &texts[0];
    assert!(added.contains(font), "AddFont goes before the list");
    assert_eq!(xs, &[0.0, 20.0, 40.0], "1em Ahem squares");
    assert_eq!(page.next_timer(), None, "no block period left");
}

#[test]
fn a_stalled_font_shows_the_fallback_after_three_seconds() {
    // AC-11
    let mut page = parsed(PAGE);
    let started = Instant::now();
    let (hidden, _) = frame(&mut page);
    assert!(texts(&hidden).is_empty());
    let fetched = font_fetches(&mut page);
    // Let the load settle, so later frames show only what the timer changed.
    while page.take_frame_request() && page.begin_frame(0.0).is_some() {}
    let due = page.next_timer().expect("the block period has a timer");
    assert!(due >= started + page::BLOCK_PERIOD);
    assert!(due <= Instant::now() + page::BLOCK_PERIOD);

    // Before it ends nothing changes.
    page.run_timers(due - Duration::from_millis(100));
    assert!(!page.take_frame_request());
    page.run_timers(due);
    assert!(
        page.take_frame_request(),
        "the block period ending repaints"
    );
    let (fallback, _) = frame(&mut page);
    let shown = texts(&fallback);
    assert_eq!(shown.len(), 1, "drawn in the fallback");
    assert_ne!(shown[0].1, [0.0, 20.0, 40.0], "not Ahem");
    assert_eq!(page.next_timer(), None);

    // The font still swaps in if it ever arrives.
    serve(&mut page, fetched[0].0, 404, b"");
    let next = font_fetches(&mut page);
    serve(&mut page, next[0].0, 200, &ahem());
    let (late, _) = frame(&mut page);
    assert_eq!(texts(&late)[0].1, [0.0, 20.0, 40.0]);
}

#[test]
fn faces_stay_inside_their_document_and_are_released_with_it() {
    // AC-10, Key invariants: a family name only means something where a
    // rule defines it.
    let mut page = parsed(PAGE);
    frame(&mut page);
    let fetched = font_fetches(&mut page);
    serve(&mut page, fetched[0].0, 404, b"");
    let fetched = font_fetches(&mut page);
    serve(&mut page, fetched[0].0, 200, &ahem());
    frame(&mut page);

    // The next document names X without a rule: the fallback, at once.
    page.navigate(NavigationId(2));
    assert!(page.commit_navigation(
        NavigationId(2),
        URL,
        Some("utf-8".into()),
        &DocumentKind::Document
    ));
    page.end_task();
    let html = "<style>body { margin: 0; font-size: 20px } p { font-family: X }</style><p>xxx</p>";
    page.fetch_chunk(NavigationId(2), FetchId::DOCUMENT, html.as_bytes());
    page.fetch_done(NavigationId(2), FetchId::DOCUMENT);
    page.end_task();
    let list = page.begin_frame(0.0).unwrap();
    let (_, mut deletes) = page.take_resource_messages();
    let shown = texts(&list);
    assert_eq!(shown.len(), 1);
    assert_ne!(shown[0].1, [0.0, 20.0, 40.0], "X is not Ahem here");
    // Freed once no list on screen can still name it, two epochs on.
    page.invalidate();
    page.begin_frame(0.0).unwrap();
    deletes.extend(page.take_resource_messages().1);
    assert!(
        deletes
            .iter()
            .any(|message| matches!(message, ToBrowser::DeleteFont { .. })),
        "the old document's face is released"
    );
    assert!(font_fetches(&mut page).is_empty());
}

#[test]
fn a_woff2_file_without_a_hint_fails_the_magic_bytes_and_falls_back() {
    // Spec 0006, Edge cases.
    let mut page = parsed(
        r#"<style>
          @font-face { font-family: X; src: url(icons.woff2) format("woff2"), url(icons) }
          body { margin: 0; font-size: 20px }
          p { font-family: X }
        </style><p>xxx</p>"#,
    );
    frame(&mut page);
    let fetched = font_fetches(&mut page);
    assert_eq!(fetched.len(), 1, "the hinted WOFF2 source is never fetched");
    assert_eq!(fetched[0].1, "http://test/dir/icons");
    serve(&mut page, fetched[0].0, 200, b"wOF2\0\0\0\0compressed");
    assert!(page.take_frame_request());
    let (list, _) = frame(&mut page);
    assert_eq!(texts(&list).len(), 1, "drawn in the fallback for good");
    assert_eq!(page.next_timer(), None);
}
