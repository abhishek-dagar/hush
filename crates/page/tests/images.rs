//! AC-29: images load through the browser, decode on the page thread,
//! reach the renderer with `AddImage`, and size and draw their elements; a
//! broken one shows its `alt` text.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

use display_list::{DisplayList, ImageKey, ItemKind, PageId, RectF, SizeF};
use ipc::{Destination, DocumentKind, FetchId, NavigationId, ToBrowser};
use page::Page;

const NAV: NavigationId = NavigationId(1);
const URL: &str = "http://test/dir/page.html";
const VIEWPORT: SizeF = SizeF {
    width: 800.0,
    height: 600.0,
};

/// The 4 by 2 PNG the `image` crate's tests decode: red, then blue.
fn png() -> Vec<u8> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../image/tests/images/two.png");
    std::fs::read(path).unwrap()
}

/// A page that committed `html` and parsed it all, one task per message.
fn parsed(html: &str) -> Page {
    let mut page = Page::blank(PageId(1), VIEWPORT, 1.0, page::ua_stylesheet());
    page.navigate(NAV);
    assert!(page.commit_navigation(NAV, URL, Some("utf-8".into()), &DocumentKind::Document));
    page.end_task();
    page.fetch_chunk(NAV, FetchId::DOCUMENT, html.as_bytes());
    page.end_task();
    page.fetch_done(NAV, FetchId::DOCUMENT);
    page.end_task();
    page
}

/// The image fetches the page asked for, as `(fetch, url)`.
fn image_fetches(page: &mut Page) -> Vec<(FetchId, String)> {
    page.take_fetch_messages()
        .into_iter()
        .filter_map(|message| match message {
            ToBrowser::Fetch {
                fetch,
                url,
                destination: Destination::Image,
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

/// An image `AddImage` registered: its key and size in pixels.
type Added = (ImageKey, (u32, u32));

/// The next frame, and the images registered before it.
fn frame(page: &mut Page) -> (DisplayList, Vec<Added>) {
    let list = page.begin_frame(0.0).expect("a frame");
    let (adds, _) = page.take_resource_messages();
    let images = adds
        .into_iter()
        .filter_map(|message| match message {
            ToBrowser::AddImage { key, size, .. } => Some((key, size)),
            _ => None,
        })
        .collect();
    (list, images)
}

fn image_items(list: &DisplayList) -> Vec<(ImageKey, RectF)> {
    list.items
        .iter()
        .filter_map(|item| match item.kind {
            ItemKind::Image { key, rect } => Some((key, rect)),
            _ => None,
        })
        .collect()
}

fn has_text(list: &DisplayList) -> bool {
    list.items
        .iter()
        .any(|item| matches!(item.kind, ItemKind::Text { .. }))
}

#[test]
fn an_img_loads_decodes_and_draws_at_its_natural_size() {
    let mut page = parsed(r#"<body style="margin:0"><img src="two.png"></body>"#);
    let asked = image_fetches(&mut page);
    assert_eq!(asked, [(FetchId(1), "http://test/dir/two.png".to_owned())]);

    // Loading, with no width or height: 0 by 0 and nothing drawn.
    let (list, images) = frame(&mut page);
    assert!(images.is_empty());
    assert!(image_items(&list).is_empty());

    serve(&mut page, FetchId(1), 200, &png());
    assert!(page.take_frame_request(), "the decode asks for a frame");
    let (list, images) = frame(&mut page);
    assert_eq!(images.len(), 1, "AddImage goes out before the list");
    let (key, size) = images[0];
    assert_eq!(size, (4, 2));
    let drawn = image_items(&list);
    assert_eq!(drawn.len(), 1);
    assert_eq!(drawn[0].0, key);
    assert_eq!(
        (drawn[0].1.size.width, drawn[0].1.size.height),
        (4.0, 2.0),
        "relaid out to the picture's size"
    );
}

#[test]
fn width_and_height_attributes_reserve_space_and_size_the_picture() {
    let mut page =
        parsed(r#"<body style="margin:0"><img src="two.png" width="40"><p id=after>x</p></body>"#);
    let (fetch, _) = image_fetches(&mut page)[0].clone();
    serve(&mut page, fetch, 200, &png());
    let (list, _) = frame(&mut page);
    let drawn = image_items(&list);
    // 40 wide keeps the 2:1 picture's ratio: 20 high.
    assert_eq!(
        (drawn[0].1.size.width, drawn[0].1.size.height),
        (40.0, 20.0)
    );
}

#[test]
fn a_broken_image_shows_its_alt_text_or_nothing() {
    let mut page = parsed(
        r#"<body><img id=a src="gone.png" alt="a missing picture"><img id=b src="junk.png"><img id=c></body>"#,
    );
    let asked = image_fetches(&mut page);
    assert_eq!(asked.len(), 2, "no src, no fetch");
    let (list, _) = frame(&mut page);
    assert!(!has_text(&list), "nothing shows while loading");

    serve(&mut page, asked[0].0, 404, b"not found");
    serve(&mut page, asked[1].0, 200, b"<html>not an image</html>");
    let (list, images) = frame(&mut page);
    assert!(images.is_empty());
    assert!(image_items(&list).is_empty());
    let frame = page.main_frame();
    let doc = &frame.document;
    let [a, b] = ["a", "b"].map(|id| doc.element_by_id(&id.into()).unwrap());
    assert_eq!(doc.image_state(a), Some(dom::ImageState::Failed));
    assert_eq!(doc.image_state(b), Some(dom::ImageState::Failed));
    // #a lays out its alt text in its place.
    let alt_texts: Vec<(dom::NodeId, String)> = frame
        .boxes
        .iter()
        .flat_map(|b| &b.inline)
        .filter_map(|item| match item {
            layout::InlineItem::Text { node, text, .. } => Some((*node, text.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(alt_texts, [(a, "a missing picture".to_owned())]);
    // #b and #c, with no alt text, stay empty boxes that draw nothing.
    let replaced = frame
        .boxes
        .iter()
        .filter(|b| b.kind == layout::BoxKind::Replaced)
        .count();
    assert_eq!(replaced, 2);
}

#[test]
fn a_background_image_loads_once_styled_and_draws_cut_to_its_box() {
    let mut page = parsed(
        r#"<body style="margin:0"><div style="background-image:url(bg.png); width:3px; height:10px; padding:1px"></div></body>"#,
    );
    assert!(
        image_fetches(&mut page).is_empty(),
        "known after the style pass"
    );
    let (list, _) = frame(&mut page);
    assert!(image_items(&list).is_empty());
    let asked = image_fetches(&mut page);
    let urls: Vec<&str> = asked.iter().map(|(_, url)| url.as_str()).collect();
    assert_eq!(urls, ["http://test/dir/bg.png"]);

    serve(&mut page, asked[0].0, 200, &png());
    let (list, images) = frame(&mut page);
    assert_eq!(images.len(), 1);
    let item = list
        .items
        .iter()
        .find(|item| matches!(item.kind, ItemKind::Image { .. }))
        .unwrap();
    let ItemKind::Image { rect, .. } = item.kind else {
        unreachable!()
    };
    // At the padding box's top left, at natural size.
    assert_eq!((rect.origin.x, rect.origin.y), (0.0, 0.0));
    assert_eq!((rect.size.width, rect.size.height), (4.0, 2.0));
    // Cut to the 5 by 12 border box.
    let clip = &list.clips[item.clip.unwrap().0 as usize];
    assert_eq!((clip.rect.size.width, clip.rect.size.height), (5.0, 12.0));
}

#[test]
fn removing_a_loading_img_cancels_its_fetch_and_frees_a_loaded_one() {
    let mut page = parsed(r#"<body><img id=a src="a.png"><img id=b src="b.png"></body>"#);
    let asked = image_fetches(&mut page);
    serve(&mut page, asked[1].0, 200, &png());
    let (_, images) = frame(&mut page);
    let (key, _) = images[0];

    let doc = &mut page.main_frame_mut().document;
    for id in ["a", "b"] {
        let node = doc.element_by_id(&id.into()).unwrap();
        let parent = doc.node(node).unwrap().parent().unwrap();
        doc.remove_child(parent, node).unwrap();
    }
    page.end_task();
    let cancels: Vec<FetchId> = page
        .take_fetch_messages()
        .into_iter()
        .filter_map(|message| match message {
            ToBrowser::CancelFetch { fetch, .. } => Some(fetch),
            _ => None,
        })
        .collect();
    assert_eq!(cancels, [asked[0].0]);

    // The key goes one epoch after the last list that drew it.
    let mut freed = Vec::new();
    for _ in 0..3 {
        page.invalidate();
        page.begin_frame(0.0).unwrap();
        let (_, deletes) = page.take_resource_messages();
        freed.extend(deletes.into_iter().filter_map(|message| match message {
            ToBrowser::DeleteImage { key, .. } => Some(key),
            _ => None,
        }));
    }
    assert_eq!(freed, [key]);
}
