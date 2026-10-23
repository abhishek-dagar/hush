//! AC-17: every tree prints a stable text dump, compared with checked in
//! `insta` snapshots. The sample page goes through the whole pipeline once.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

use display_list::{PageId, SizeF};
use page::Page;

/// The five dumps of the sample page after its first frame: DOM, computed
/// styles, box tree, fragment tree and display list.
fn dumps() -> [String; 5] {
    let viewport = SizeF {
        width: 800.0,
        height: 600.0,
    };
    let mut page = Page::new(PageId(1), page::sample::document(), viewport, 1.0);
    let list = page.begin_frame(0.0).unwrap();
    let frame = page.main_frame();
    let doc = &frame.document;
    [
        doc.dump(),
        doc.dump_styles(),
        frame.boxes.dump(doc),
        frame.fragments.as_ref().unwrap().dump(doc),
        list.dump(),
    ]
}

#[test]
fn every_tree_dumps_the_same_bytes_on_every_run() {
    assert_eq!(dumps(), dumps());
}

#[test]
fn dumps_match_their_snapshots() {
    let [dom, styles, boxes, fragments, list] = dumps();
    insta::assert_snapshot!("dom", dom);
    insta::assert_snapshot!("styles", styles);
    insta::assert_snapshot!("boxes", boxes);
    insta::assert_snapshot!("fragments", fragments);
    insta::assert_snapshot!("display_list", list);
}
