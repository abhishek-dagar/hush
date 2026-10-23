//! AC-39: every page in the checked in corpus (`tests/corpus/`) loads the
//! way a real navigation does, its style sheets, scripts and images served
//! from its folder, and its DOM, computed style, fragment tree and display
//! list dumps are `insta` snapshots. Text is shaped with the bundled
//! Cantarell face only, so the dumps match on every machine.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use display_list::{DisplayList, PageId, SizeF};
use ipc::{Destination, DocumentKind, FetchId, NavigationId, ToBrowser};
use page::{FontContext, Page};

const NAV: NavigationId = NavigationId(1);
const ORIGIN: &str = "http://corpus.test/";
const VIEWPORT: SizeF = SizeF {
    width: 800.0,
    height: 600.0,
};

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus")
}

/// Answers one fetch from the corpus folder: the file, or a 404.
fn serve(page: &mut Page, fetch: FetchId, url: &str, destination: Destination) {
    let path = url.strip_prefix(ORIGIN).map(|rest| corpus().join(rest));
    let body = path.and_then(|path| std::fs::read(path).ok());
    let content_type = match destination {
        Destination::Style => "text/css",
        Destination::Script => "text/javascript",
        Destination::Image | Destination::Favicon | Destination::Font => "application/octet-stream",
    };
    let headers = [("content-type".to_owned(), content_type.to_owned())];
    match body {
        Some(body) => {
            page.fetch_response(NAV, fetch, 200, &headers, url);
            page.fetch_chunk(NAV, fetch, &body);
        }
        None => page.fetch_response(NAV, fetch, 404, &headers, url),
    }
    page.fetch_done(NAV, fetch);
    page.end_task();
}

/// Serves every fetch and draws frames until the page asks for nothing
/// more. Returns the last list drawn.
fn settle(page: &mut Page, mut last: Option<DisplayList>) -> DisplayList {
    for _ in 0..32 {
        let fetches: Vec<_> = page
            .take_fetch_messages()
            .into_iter()
            .filter_map(|message| match message {
                ToBrowser::Fetch {
                    fetch,
                    url,
                    destination,
                    ..
                } => Some((fetch, url, destination)),
                _ => None,
            })
            .collect();
        if fetches.is_empty() {
            if let Some(list) = page.begin_frame(0.0) {
                last = Some(list);
            }
            // The style pass may have found background images.
            if page
                .take_fetch_messages()
                .iter()
                .all(|m| !matches!(m, ToBrowser::Fetch { .. }))
            {
                return last.expect("the page drew no frame");
            }
            continue;
        }
        for (fetch, url, destination) in fetches {
            serve(page, fetch, &url, destination);
        }
    }
    panic!("the page never settled");
}

/// Loads `dir/name` in 64 byte chunks. A `header-<label>.html` page is
/// committed with `<label>` as its `Content-Type` charset.
fn load(dir: &str, name: &str) -> (Page, DisplayList) {
    let bytes = std::fs::read(corpus().join(dir).join(name)).unwrap();
    let charset = name
        .strip_prefix("header-")
        .and_then(|rest| rest.strip_suffix(".html"))
        .map(str::to_owned);
    let mut page = Page::blank(PageId(1), VIEWPORT, 1.0, page::ua_stylesheet());
    let font = std::fs::read(corpus().join("fonts/Cantarell-Regular.ttf")).unwrap();
    page.use_fonts(FontContext::with_fonts([font]));
    page.navigate(NAV);
    let url = format!("{ORIGIN}{dir}/{name}");
    assert!(page.commit_navigation(NAV, &url, charset, &DocumentKind::Document));
    page.end_task();
    for chunk in bytes.chunks(64) {
        page.fetch_chunk(NAV, FetchId::DOCUMENT, chunk);
        page.end_task();
    }
    page.fetch_done(NAV, FetchId::DOCUMENT);
    page.end_task();
    let list = settle(&mut page, None);
    (page, list)
}

/// The four dumps, one section each.
fn dumps(page: &Page, list: &DisplayList) -> String {
    let frame = page.main_frame();
    let doc = &frame.document;
    format!(
        "== dom ==\n{}\n== computed style ==\n{}\n== fragments ==\n{}\n== display list ==\n{}",
        doc.dump(),
        doc.dump_styles(),
        frame.fragments.as_ref().expect("laid out").dump(doc),
        list.dump(),
    )
}

fn check(dir: &str, names: &[&str]) {
    for name in names {
        let (page, list) = load(dir, name);
        let snapshot = format!("{dir}-{}", name.trim_end_matches(".html"));
        insta::assert_snapshot!(snapshot, dumps(&page, &list));
    }
}

#[test]
fn reading_pages_match_their_snapshots() {
    check(
        "pages",
        &[
            "article.html",
            "margins.html",
            "inline.html",
            "cascade.html",
            "vars.html",
        ],
    );
}

#[test]
fn each_image_format_matches_its_snapshot() {
    check(
        "pages",
        &[
            "image-png.html",
            "image-jpg.html",
            "image-gif.html",
            "image-webp.html",
        ],
    );
}

#[test]
fn each_encoding_case_matches_its_snapshot() {
    check(
        "encodings",
        &[
            "bom-utf16le.html",
            "header-koi8-r.html",
            "meta-iso-8859-2.html",
            "default-windows-1252.html",
        ],
    );
}

#[test]
fn the_broken_set_matches_its_snapshots() {
    check(
        "broken",
        &[
            "unclosed.html",
            "misnested.html",
            "stray-table.html",
            "truncated.html",
            "nul-bytes.html",
            "foreign.html",
        ],
    );
}

#[test]
fn a_script_edits_the_page_on_dom_content_loaded_and_on_a_timer() {
    // AC-32, AC-33: the list after each edit shows it.
    let (mut page, list) = load("pages", "script.html");
    let loaded = dumps(&page, &list);
    assert!(loaded.contains("\"Loaded\""), "the DOMContentLoaded edit");
    insta::assert_snapshot!("pages-script", loaded);

    page.run_timers(Instant::now() + Duration::from_millis(200));
    let list = settle(&mut page, None);
    let fired = dumps(&page, &list);
    assert!(fired.contains("\"Timer fired\""), "the timer edit");
    assert_ne!(
        list.dump(),
        loaded.split("== display list ==\n").nth(1).unwrap(),
        "the timer's edit reached the display list"
    );
    insta::assert_snapshot!("pages-script-after-timer", fired);
}
