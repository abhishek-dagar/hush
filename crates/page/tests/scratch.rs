//! Scratch: dump the fragment tree for one WPT page, scripts and all.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

use std::path::{Path, PathBuf};

use display_list::{PageId, SizeF};
use ipc::{DocumentKind, FetchId, NavigationId, ToBrowser};
use page::{FontContext, Page};

const NAV: NavigationId = NavigationId(1);
const ORIGIN: &str = "http://scratch.test/";
const VIEWPORT: SizeF = SizeF {
    width: 800.0,
    height: 600.0,
};

fn root() -> PathBuf {
    PathBuf::from(std::env::var("SCRATCH_PAGE").expect("SCRATCH_PAGE"))
}

fn serve(page: &mut Page, fetch: FetchId, url: &str) {
    let path = url.strip_prefix(ORIGIN).map(|rest| root().join(rest));
    let body = path.and_then(|path| std::fs::read(path).ok());
    let headers = [("content-type".to_owned(), "text/plain".to_owned())];
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

fn settle(page: &mut Page) {
    for _ in 0..32 {
        let fetches: Vec<_> = page
            .take_fetch_messages()
            .into_iter()
            .filter_map(|message| match message {
                ToBrowser::Fetch { fetch, url, .. } => Some((fetch, url)),
                _ => None,
            })
            .collect();
        if fetches.is_empty() {
            let _ = page.begin_frame(0.0);
            if page
                .take_fetch_messages()
                .iter()
                .all(|m| !matches!(m, ToBrowser::Fetch { .. }))
            {
                return;
            }
            continue;
        }
        for (fetch, url) in fetches {
            serve(page, fetch, &url);
        }
    }
    panic!("the page never settled");
}

#[test]
fn dump_one_page() {
    let path = root();
    let name = path.file_name().expect("name").to_string_lossy();
    let bytes = std::fs::read(&path).expect("read");
    let mut page = Page::blank(PageId(1), VIEWPORT, 1.0, page::ua_stylesheet());
    let font = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/fonts/Cantarell-Regular.ttf"),
    )
    .expect("font");
    page.use_fonts(FontContext::with_fonts([font]));
    page.navigate(NAV);
    let url = format!("{ORIGIN}{name}");
    assert!(page.commit_navigation(NAV, &url, None, &DocumentKind::Document));
    page.end_task();
    for chunk in bytes.chunks(64) {
        page.fetch_chunk(NAV, FetchId::DOCUMENT, chunk);
        page.end_task();
    }
    page.fetch_done(NAV, FetchId::DOCUMENT);
    page.end_task();
    settle(&mut page);
    let frame = page.main_frame();
    println!(
        "{}",
        frame
            .fragments
            .as_ref()
            .expect("laid out")
            .dump(&frame.document)
    );
}
