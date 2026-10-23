//! AC-23 to AC-25: real pages go through the UA sheet, the cascade, box
//! building and layout, and their fragments land where CSS 2.2 flow layout
//! puts them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

use std::rc::Rc;

use base::atoms::Atom;
use base::{Au, Point, Size};
use display_list::{DisplayList, ItemKind, PageId, SizeF};
use ipc::{DocumentKind, FetchId, NavigationId};
use layout::{Fragment, InlineEdges, LineFragment, TextFragment};
use page::Page;

const NAV: NavigationId = NavigationId(1);
const VIEWPORT: SizeF = SizeF {
    width: 800.0,
    height: 600.0,
};

/// Loads `html` and draws its first frame.
fn load(html: &str) -> (Page, DisplayList) {
    let mut page = Page::blank(PageId(1), VIEWPORT, 1.0, page::ua_stylesheet());
    page.navigate(NAV);
    assert!(page.commit_navigation(
        NAV,
        "http://test/",
        Some("utf-8".into()),
        &DocumentKind::Document
    ));
    page.fetch_chunk(NAV, FetchId::DOCUMENT, html.as_bytes());
    page.fetch_done(NAV, FetchId::DOCUMENT);
    page.end_task();
    let list = page.begin_frame(0.0).expect("no first frame");
    (page, list)
}

/// Every fragment with its top left corner in page coordinates.
fn walk(page: &Page) -> Vec<(Point, Rc<Fragment>)> {
    let tree = page.main_frame().fragments.as_ref().unwrap();
    let mut out = Vec::new();
    let mut stack: Vec<(Point, Rc<Fragment>)> = tree.root.iter().cloned().collect();
    while let Some((at, fragment)) = stack.pop() {
        for (offset, child) in fragment.children().iter().rev() {
            stack.push((at.offset_by(*offset), Rc::clone(child)));
        }
        out.push((at, fragment));
    }
    out
}

fn node(page: &Page, id: &str) -> dom::NodeId {
    page.main_frame()
        .document
        .element_by_id(&Atom::from(id))
        .unwrap_or_else(|| panic!("no #{id}"))
}

/// One box fragment: page position, size and inline edges, in CSS px.
type Found = ((f32, f32), (f32, f32), Option<InlineEdges>);

/// The box fragments of the element with `id` (one per line for an inline
/// element), as page position, size and inline edges, in CSS px.
fn boxes(page: &Page, id: &str) -> Vec<Found> {
    let target = node(page, id);
    walk(page)
        .into_iter()
        .filter_map(|(at, fragment)| {
            let fragment = fragment.as_box()?;
            (fragment.node == Some(target)).then(|| {
                (
                    (at.x.to_f32_px(), at.y.to_f32_px()),
                    (
                        fragment.size.width.to_f32_px(),
                        fragment.size.height.to_f32_px(),
                    ),
                    fragment.inline_edges,
                )
            })
        })
        .collect()
}

/// The one box of `id`: its position and size in CSS px.
fn rect(page: &Page, id: &str) -> (f32, f32, f32, f32) {
    let found = boxes(page, id);
    assert_eq!(found.len(), 1, "#{id} has one box: {found:?}");
    let ((x, y), (w, h), _) = found[0];
    (x, y, w, h)
}

/// Every text fragment with its page position.
fn texts(page: &Page) -> Vec<(Point, Rc<Fragment>)> {
    walk(page)
        .into_iter()
        .filter(|(_, f)| matches!(&**f, Fragment::Text(_)))
        .collect()
}

fn text_of(fragment: &Fragment) -> &TextFragment {
    match fragment {
        Fragment::Text(text) => text,
        _ => panic!("not text"),
    }
}

/// The lines of block `id`, with their page positions.
fn lines(page: &Page, id: &str) -> Vec<(Point, Rc<Fragment>)> {
    let target = node(page, id);
    let mut found = Vec::new();
    for (at, fragment) in walk(page) {
        let Some(block) = fragment.as_box() else {
            continue;
        };
        if block.node != Some(target) {
            continue;
        }
        found.extend(
            block
                .children
                .iter()
                .filter(|(_, c)| matches!(&**c, Fragment::Line(_)))
                .map(|(offset, c)| (at.offset_by(*offset), Rc::clone(c))),
        );
    }
    found
}

fn line_of(fragment: &Fragment) -> &LineFragment {
    match fragment {
        Fragment::Line(line) => line,
        _ => panic!("not a line"),
    }
}

/// Whether the machine has any font to shape with.
fn has_fonts(page: &Page) -> bool {
    !texts(page).is_empty()
}

fn px(au: Au) -> f32 {
    au.to_f32_px()
}

#[test]
fn sibling_margins_collapse_to_the_larger_and_negatives_subtract() {
    // AC-23
    let (page, _) = load(
        "<body style='margin:0'>\
         <div id=a style='height:10px;margin-bottom:20px'></div>\
         <div id=b style='height:10px;margin-top:30px;margin-bottom:20px'></div>\
         <div id=c style='height:10px;margin-top:-5px'></div>",
    );
    let (_, ay, _, _) = rect(&page, "a");
    let (_, by, _, _) = rect(&page, "b");
    let (_, cy, _, _) = rect(&page, "c");
    assert_eq!(by - (ay + 10.0), 30.0, "max(20, 30)");
    assert_eq!(cy - (by + 10.0), 15.0, "20 + (-5)");
}

#[test]
fn a_first_child_margin_collapses_through_its_parent_unless_padding_separates_them() {
    // AC-23
    let (page, _) = load(
        "<body style='margin:0'>\
         <div id=p style='margin-top:10px'><div id=c style='margin-top:25px;height:5px'></div></div>\
         <div id=q style='margin-top:10px;padding-top:1px'>\
           <div id=d style='margin-top:25px;height:5px'></div></div>",
    );
    let (_, py, _, ph) = rect(&page, "p");
    let (_, cy, _, _) = rect(&page, "c");
    assert_eq!(py, 25.0, "the parent moves down by the collapsed margin");
    assert_eq!(cy, py, "the child sits at its parent's top");
    assert_eq!(ph, 5.0);
    let (_, qy, _, _) = rect(&page, "q");
    let (_, dy, _, _) = rect(&page, "d");
    assert_eq!(qy, 30.0 + 10.0);
    assert_eq!(dy, qy + 1.0 + 25.0, "padding keeps the margins apart");
}

#[test]
fn an_empty_block_lets_margins_collapse_through_it_and_a_last_child_through_auto_height() {
    // AC-23
    let (page, _) = load(
        "<body style='margin:0'>\
         <div id=a style='height:10px;margin-bottom:10px'></div>\
         <div id=empty style='margin:15px 0'></div>\
         <div id=b style='height:10px;margin-top:5px'></div>\
         <div id=p><div id=last style='height:10px;margin-bottom:40px'></div></div>\
         <div id=after style='height:10px'></div>",
    );
    let (_, ay, _, _) = rect(&page, "a");
    let (_, by, _, _) = rect(&page, "b");
    assert_eq!(by - (ay + 10.0), 15.0, "10, 15, 15 and 5 make one 15px gap");
    let (_, py, _, ph) = rect(&page, "p");
    let (_, after, _, _) = rect(&page, "after");
    assert_eq!(ph, 10.0, "the margin left through the auto height");
    assert_eq!(after - (py + ph), 40.0);
}

#[test]
fn a_heading_margin_collapses_through_body_into_the_root() {
    // AC-23: body's 8px and the h1's 0.67em (21.44px) collapse into one.
    let (page, _) = load("<h1 id=h>Title</h1>");
    let tree = page.main_frame().fragments.as_ref().unwrap();
    let (_, root) = tree.root.as_ref().unwrap();
    let html = root.as_box().unwrap();
    let (body_at, _) = &html.children[0];
    assert!(
        (px(body_at.y) - 21.44).abs() < 0.02,
        "body at {}",
        px(body_at.y)
    );
    let (_, hy, _, _) = rect(&page, "h");
    assert!((hy - 21.44).abs() < 0.02, "h1 at {hy}");
}

#[test]
fn widths_resolve_center_and_clamp() {
    // AC-23: body content is 800 - 2 * 8 = 784px wide.
    let (page, _) = load(
        "<div id=center style='width:200px;margin:0 auto;height:1px'></div>\
         <div id=border style='width:200px;padding:10px;border:5px solid;\
           box-sizing:border-box;height:50px'></div>\
         <div id=content style='width:200px;padding:10px;border:5px solid;height:50px'></div>\
         <div id=half style='width:50%;height:1px'></div>\
         <div id=max style='max-width:100px;height:1px'></div>\
         <div id=min style='width:10px;min-width:300px;height:1px'></div>\
         <div id=right style='width:100px;margin-left:auto;height:1px'></div>",
    );
    let (x, _, w, _) = rect(&page, "center");
    assert_eq!((x, w), (8.0 + (784.0 - 200.0) / 2.0, 200.0));
    let (_, _, w, h) = rect(&page, "border");
    assert_eq!(
        (w, h),
        (200.0, 50.0),
        "border-box includes padding and border"
    );
    let (_, _, w, h) = rect(&page, "content");
    assert_eq!((w, h), (230.0, 80.0));
    assert_eq!(rect(&page, "half").2, 392.0);
    assert_eq!(rect(&page, "max").2, 100.0);
    assert_eq!(rect(&page, "min").2, 300.0, "min-width wins over width");
    let (x, _, _, _) = rect(&page, "right");
    assert_eq!(x, 8.0 + 784.0 - 100.0);
}

#[test]
fn heights_follow_height_min_and_max_and_percentages_need_a_definite_parent() {
    // AC-23
    let (page, _) = load(
        "<div id=fixed style='height:120px'><div id=pct style='height:50%'></div></div>\
         <div id=auto><div id=lost style='height:50%'>x</div></div>\
         <div id=tall style='min-height:40px'></div>\
         <div id=short style='max-height:5px'><div style='height:50px'></div></div>",
    );
    assert_eq!(rect(&page, "pct").3, 60.0);
    let (_, _, _, lost) = rect(&page, "lost");
    let (_, _, _, auto) = rect(&page, "auto");
    assert_eq!(lost, auto, "a percentage of an auto height is auto");
    assert_eq!(rect(&page, "tall").3, 40.0);
    assert_eq!(rect(&page, "short").3, 5.0);
}

#[test]
fn the_body_background_paints_the_whole_canvas() {
    // AC-23
    let (_, list) = load("<body style='background:#ff0000;margin:50px'><p>short</p>");
    let first = list.items.first().unwrap();
    match first.kind {
        ItemKind::Rect { rect, color, .. } => {
            assert_eq!((rect.origin.x, rect.origin.y), (0.0, 0.0));
            assert_eq!((rect.size.width, rect.size.height), (800.0, 600.0));
            assert_eq!((color.r, color.g, color.b), (1.0, 0.0, 0.0));
        }
        ref other => panic!("{other:?}"),
    }
    let reds = list
        .items
        .iter()
        .filter(|item| matches!(item.kind, ItemKind::Rect { color, .. } if color.r == 1.0 && color.g == 0.0))
        .count();
    assert_eq!(reds, 1, "body does not paint its background twice");

    // The root's own background wins when it has one.
    let (_, list) = load("<html style='background:#0000ff'><body style='background:#ff0000'>");
    match list.items.first().unwrap().kind {
        ItemKind::Rect { color, rect, .. } => {
            assert_eq!(color.b, 1.0);
            assert_eq!(rect.size.height, 600.0);
        }
        ref other => panic!("{other:?}"),
    }
}

#[test]
fn text_align_moves_lines_and_mixed_sizes_share_a_baseline() {
    // AC-24
    let (page, _) = load(
        "<p id=center style='text-align:center'>mid</p>\
         <p id=right style='text-align:right'>end</p>\
         <p id=mixed>small <span id=big style='font-size:40px'>BIG</span> small</p>",
    );
    if !has_fonts(&page) {
        return;
    }
    let texts = texts(&page);
    let find = |s: &str| {
        texts
            .iter()
            .find(|(_, f)| text_of(f).text.trim() == s)
            .map(|(at, f)| (*at, text_of(f)))
            .unwrap_or_else(|| panic!("no {s:?}"))
    };
    let (at, mid) = find("mid");
    let center = at.x + Au(mid.size.width.0 / 2);
    assert!(
        (px(center) - 400.0).abs() <= 1.0,
        "centered at {}",
        px(center)
    );
    let (at, end) = find("end");
    assert!((px(at.x + end.size.width) - 792.0).abs() <= 0.5);

    let line = &lines(&page, "mixed")[0];
    let line_box = line_of(&line.1);
    let baseline = line.0.y + line_box.baseline;
    let (big_at, big) = find("BIG");
    assert_eq!(big_at.y + big.baseline, baseline);
    let small = texts
        .iter()
        .filter(|(_, f)| text_of(f).text.contains("small"))
        .collect::<Vec<_>>();
    assert!(!small.is_empty());
    for (at, f) in small {
        assert_eq!(at.y + text_of(f).baseline, baseline, "one shared baseline");
    }
    assert!(
        px(line_box.size.height) >= 40.0,
        "the big text makes the line taller"
    );
}

#[test]
fn an_inline_element_gets_one_piece_per_line_with_its_edges_on_the_ends() {
    // AC-24
    let (page, list) = load(
        "<div style='width:120px'>aa <span id=s style='background:#00ff00;\
         border-left:3px solid;border-right:4px solid'>bb cc dd ee ff gg hh</span> ii</div>",
    );
    if !has_fonts(&page) {
        return;
    }
    let pieces = boxes(&page, "s");
    assert!(pieces.len() >= 2, "the span wraps: {pieces:?}");
    let edges: Vec<_> = pieces.iter().map(|p| p.2.unwrap()).collect();
    assert_eq!(
        edges.first(),
        Some(&InlineEdges {
            start: true,
            end: false
        })
    );
    assert_eq!(edges.last().map(|e| e.end), Some(true));
    assert!(edges.iter().skip(1).all(|e| !e.start));
    // Each piece paints its own background, and the border only where the
    // piece holds an edge.
    let greens = list
        .items
        .iter()
        .filter(|item| matches!(item.kind, ItemKind::Rect { color, .. } if color.g == 1.0 && color.r == 0.0))
        .count();
    assert_eq!(greens, pieces.len());
    let borders: Vec<[f32; 4]> = list
        .items
        .iter()
        .filter_map(|item| match item.kind {
            ItemKind::Border { widths, .. } => Some(widths),
            _ => None,
        })
        .collect();
    assert_eq!(borders.first().map(|w| (w[1], w[3])), Some((0.0, 3.0)));
    assert_eq!(borders.last().map(|w| (w[1], w[3])), Some((4.0, 0.0)));
}

#[test]
fn inline_blocks_shrink_to_fit_and_images_sit_on_the_baseline() {
    // AC-24, AC-29 (sizes before decoding)
    let (page, _) = load(
        "<p id=p>a <span id=ib style='display:inline-block;padding:2px'>xy</span> \
         <img id=img width=40 height=30> <img id=wide style='width:80px' width=40 height=20>\
         <img id=none></p>",
    );
    let (_, _, img_w, img_h) = rect(&page, "img");
    assert_eq!((img_w, img_h), (40.0, 30.0), "the attributes size it");
    let (_, _, wide_w, wide_h) = rect(&page, "wide");
    assert_eq!((wide_w, wide_h), (80.0, 40.0), "CSS width keeps the ratio");
    assert_eq!(rect(&page, "none").2, 0.0, "no size at all: 0 by 0");
    if !has_fonts(&page) {
        return;
    }
    let (_, _, ib_w, _) = rect(&page, "ib");
    assert!(ib_w < 100.0, "shrinks to its text, not the line: {ib_w}");
    let line = &lines(&page, "p")[0];
    let baseline = px(line.0.y + line_of(&line.1).baseline);
    let (_, img_y, _, _) = rect(&page, "img");
    assert_eq!(
        img_y + img_h,
        baseline,
        "an image's bottom sits on the baseline"
    );
}

#[test]
fn br_and_preformatted_text_break_lines_and_nowrap_does_not_wrap() {
    // AC-24
    let (page, _) = load(
        "<p id=br>one<br>two<br><br>four</p>\
         <pre id=pre>a  b\n\tc\nd</pre>\
         <p id=nowrap style='white-space:nowrap;width:50px'>never wraps at all here</p>\
         <p id=line style='white-space:pre-line'>x   y\n z</p>",
    );
    if !has_fonts(&page) {
        return;
    }
    assert_eq!(
        lines(&page, "br").len(),
        4,
        "an empty line between two breaks"
    );
    assert_eq!(lines(&page, "pre").len(), 3);
    assert_eq!(lines(&page, "nowrap").len(), 1);
    assert_eq!(lines(&page, "line").len(), 2);
    let all = texts(&page);
    let text = |prefix: &str| {
        all.iter()
            .map(|(_, f)| text_of(f).text.clone())
            .find(|t| t.starts_with(prefix))
            .unwrap()
    };
    assert_eq!(text("a "), "a  b", "pre keeps both spaces");
    assert_eq!(text("    "), "        c", "a tab reaches the next stop");
    assert_eq!(text("x"), "x y", "pre-line collapses spaces");
}

#[test]
fn list_items_get_markers_outside_their_first_line() {
    // AC-24
    let (page, _) = load(
        "<ul><li id=disc>apple</li></ul>\
         <ol start=4><li>four</li><li id=n value=9>nine</li><li id=ten>ten</li></ol>\
         <ul><li><ul><li id=circle>inner</li></ul></li></ul>",
    );
    let doc = &page.main_frame().document;
    let dump = page.main_frame().boxes.dump(doc);
    assert!(dump.contains("marker=\"•\""), "{dump}");
    assert!(dump.contains("marker=\"4.\""), "{dump}");
    assert!(dump.contains("marker=\"9.\""), "{dump}");
    assert!(dump.contains("marker=\"10.\""), "{dump}");
    assert!(dump.contains("marker=\"◦\""), "{dump}");
    if !has_fonts(&page) {
        return;
    }
    let li = node(&page, "disc");
    let (li_x, _, _, _) = rect(&page, "disc");
    let marker = texts(&page)
        .into_iter()
        .find(|(_, f)| text_of(f).node == li && text_of(f).text == "•")
        .expect("the marker is drawn");
    let marker_end = px(marker.0.x + text_of(&marker.1).size.width);
    assert!(marker_end < li_x, "outside, to the left of the content");
    let line = &lines(&page, "disc")[0];
    assert_eq!(
        marker.0.y + text_of(&marker.1).baseline,
        line.0.y + line_of(&line.1).baseline,
        "on the first line's baseline"
    );
}

#[test]
fn fonts_follow_the_family_list_and_links_are_underlined() {
    // AC-25
    let (page, list) = load(
        "<p id=serif>Serif text</p><p><code id=code>mono</code></p>\
         <p><b>bold</b></p><p><a id=link href='/x'>link</a></p>",
    );
    if !has_fonts(&page) {
        return;
    }
    let all = texts(&page);
    let font = |s: &str| {
        all.iter()
            .map(|(_, f)| text_of(f))
            .find(|t| t.text == s)
            .unwrap_or_else(|| panic!("no {s:?}"))
    };
    assert_eq!(px(font("Serif text").font_size), 16.0);
    assert_eq!(
        px(font("mono").font_size),
        13.0,
        "monospace defaults to 13px"
    );
    assert_ne!(font("mono").font, font("Serif text").font);
    assert_ne!(font("bold").font, font("Serif text").font);

    let link = font("link");
    let underline = list.items.iter().find_map(|item| match item.kind {
        ItemKind::Rect { rect, color, .. }
            if color.b > 0.9 && color.r < 0.1 && rect.size.height < 4.0 =>
        {
            Some(rect)
        }
        _ => None,
    });
    let underline = underline.expect("the link is underlined");
    assert!((underline.size.width - px(link.size.width)).abs() < 0.1);
    let (at, _) = all.iter().find(|(_, f)| text_of(f).text == "link").unwrap();
    let baseline = px(at.y + link.baseline);
    assert!(underline.origin.y > baseline, "below the baseline");
    assert!(underline.origin.y < baseline + px(link.size.height));
}

#[test]
fn broken_inline_markup_with_a_block_inside_still_lays_out() {
    // CSS 2.2 section 9.2.1.1: the block splits the inline.
    // A `<span>` (not `<a>` in a `<p>`, which the parser would close) keeps
    // the block inside the inline in the DOM.
    let (page, _) = load("<div><span id=a>before<div id=d>inside</div>after</span></div>");
    let doc = &page.main_frame().document;
    assert!(doc.dump().contains("span"), "{}", doc.dump());
    let pieces = boxes(&page, "a");
    if has_fonts(&page) {
        assert!(
            pieces.len() >= 2,
            "the link goes on after the block: {pieces:?}"
        );
    }
    let (_, dy, dw, _) = rect(&page, "d");
    assert!(dw > 700.0 && dy > 0.0);
}

#[test]
fn a_style_change_on_an_inline_element_restyles_its_text() {
    // Text carries its parent's style, so an inline box restyle rebuilds
    // its block (Style damage).
    let (mut page, _) = load("<p>x <span id=s>word</span></p>");
    if !has_fonts(&page) {
        return;
    }
    let s = node(&page, "s");
    page.main_frame_mut()
        .document
        .set_attribute(
            s,
            base::atoms::QualName::attribute(base::local_name!("style")),
            "color:#00ff00",
        )
        .unwrap();
    let list = page.begin_frame(0.0).unwrap();
    let green = list
        .items
        .iter()
        .any(|item| matches!(&item.kind, ItemKind::Text { color, .. } if color.g == 1.0 && color.r == 0.0));
    assert!(green, "the span's text repainted in its new color");
}

#[test]
fn relayout_reuses_clean_boxes_and_keeps_their_collapsed_margins() {
    // AC-9 with margin collapsing: a cached fragment carries its struts.
    let (mut page, _) = load(
        "<div id=a style='margin-bottom:30px'><p>one</p></div>\
         <div id=b style='margin-top:10px'><p>two</p></div>",
    );
    let before = (rect(&page, "a"), rect(&page, "b"));
    page.resize(
        SizeF {
            width: 800.0,
            height: 400.0,
        },
        1.0,
    );
    page.begin_frame(0.0);
    assert_eq!((rect(&page, "a"), rect(&page, "b")), before);
    let size = Size::new(Au::from_px(800.0), Au::from_px(400.0));
    assert_eq!(page.main_frame().fragments.as_ref().unwrap().viewport, size);
}
