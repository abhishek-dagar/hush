//! AC-36, AC-37: the page tells the browser its title on each navigation,
//! and loads its favicon once parsing ends, without holding `load`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

use display_list::{PageId, SizeF};
use ipc::{Destination, DocumentKind, FetchId, NavigationId, ToBrowser};
use page::Page;

const VIEWPORT: SizeF = SizeF {
    width: 800.0,
    height: 600.0,
};

fn load(page: &mut Page, navigation: NavigationId, url: &str, html: &str) {
    page.navigate(navigation);
    assert!(page.commit_navigation(
        navigation,
        url,
        Some("utf-8".into()),
        &DocumentKind::Document
    ));
    page.fetch_chunk(navigation, FetchId::DOCUMENT, html.as_bytes());
    page.fetch_done(navigation, FetchId::DOCUMENT);
    page.end_task();
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

fn favicon_fetch(page: &mut Page) -> Option<(FetchId, String)> {
    page.take_fetch_messages()
        .into_iter()
        .find_map(|message| match message {
            ToBrowser::Fetch {
                fetch,
                url,
                destination: Destination::Favicon,
                ..
            } => Some((fetch, url)),
            _ => None,
        })
}

#[test]
fn each_navigation_sends_its_title_or_its_url() {
    // AC-36
    let mut page = Page::blank(PageId(1), VIEWPORT, 1.0, page::ua_stylesheet());
    load(
        &mut page,
        NavigationId(1),
        "http://test/a.html",
        "<title>  First \n page </title><p>hi",
    );
    assert_eq!(titles(&mut page), ["First page"]);
    assert!(titles(&mut page).is_empty(), "sent once");
    load(&mut page, NavigationId(2), "http://test/b.html", "<p>none");
    assert_eq!(titles(&mut page), ["http://test/b.html"]);
}

#[test]
fn the_favicon_loads_after_parsing_and_arrives_decoded() {
    // AC-37
    let mut page = Page::blank(PageId(1), VIEWPORT, 1.0, page::ua_stylesheet());
    let nav = NavigationId(1);
    load(&mut page, nav, "http://test/dir/a.html", "<p>no icon link");
    let (fetch, url) = favicon_fetch(&mut page).expect("favicon asked for");
    assert_eq!(url, "http://test/favicon.ico");
    assert!(favicon_fetch(&mut page).is_none(), "asked once");

    let png = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../image/tests/images/two.png"
    ))
    .unwrap();
    page.fetch_response(nav, fetch, 200, &[], &url);
    page.fetch_chunk(nav, fetch, &png);
    page.fetch_done(nav, fetch);
    page.end_task();
    let icons: Vec<_> = page
        .take_head_messages()
        .into_iter()
        .filter_map(|message| match message {
            ToBrowser::FaviconChanged {
                width,
                height,
                rgba,
                ..
            } => Some((width, height, rgba[..4].to_vec())),
            _ => None,
        })
        .collect();
    assert_eq!(icons, [(4, 2, vec![255, 0, 0, 255])]);

    // A `<link rel=icon>` wins; a file page asks for none.
    load(
        &mut page,
        NavigationId(2),
        "http://test/dir/b.html",
        "<link rel=icon href=i.png>",
    );
    assert_eq!(
        favicon_fetch(&mut page).map(|(_, url)| url).as_deref(),
        Some("http://test/dir/i.png")
    );
    load(
        &mut page,
        NavigationId(3),
        "file:///tmp/c.html",
        "<link rel=icon href=i.png>",
    );
    assert!(favicon_fetch(&mut page).is_none());
}
