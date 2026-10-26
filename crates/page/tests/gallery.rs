//! Spec 0006, milestone 6: the control gallery, opened as a page by file
//! URL with its three sheets and `icons.ttf`, lays out without overlaps or
//! wrong clipping at 1280 and at 640 CSS px (the 200% case), draws its icons
//! from `@font-face`, and restyles a hover in budget (AC-19, AC-20, AC-21).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use base::atoms::{LocalName, QualName};
use base::{Au, Point};
use display_list::{DisplayList, FontKey, ItemKind, PageId, SizeF};
use dom::NodeId;
use ipc::{DocumentKind, FetchId, InputEvent, NavigationId, ToBrowser};
use layout::Fragment;
use page::{FontContext, Page};

const NAV: NavigationId = NavigationId(1);

fn assets() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../ui/assets")
}

fn cantarell() -> Vec<u8> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tools/conformance/fonts/Cantarell-Regular.ttf"
    );
    std::fs::read(path).unwrap()
}

/// What loading the gallery gave: the page, and the key `icons.ttf` was
/// registered under.
struct Gallery {
    page: Page,
    icons: Option<FontKey>,
    last: DisplayList,
}

/// Opens `gallery.html` by file URL in a `width` CSS px wide page, answering
/// every fetch from the assets folder, until nothing is left to load or
/// draw. Cantarell stands in for every installed font, so text measures the
/// same on every machine.
fn open(width: f32, height: f32) -> Gallery {
    let folder = assets().canonicalize().unwrap();
    let url = url::Url::from_file_path(folder.join("gallery.html")).unwrap();
    let viewport = SizeF { width, height };
    let mut page = Page::blank(PageId(1), viewport, 1.0, page::ua_stylesheet());
    page.use_fonts(FontContext::with_fonts([cantarell()]));
    page.navigate(NAV);
    assert!(page.commit_navigation(NAV, url.as_str(), None, &DocumentKind::Document));
    page.end_task();
    let html = std::fs::read(folder.join("gallery.html")).unwrap();
    page.fetch_chunk(NAV, FetchId::DOCUMENT, &html);
    page.end_task();
    page.fetch_done(NAV, FetchId::DOCUMENT);
    page.end_task();

    let icons_len = std::fs::read(folder.join("icons.ttf")).unwrap().len();
    let mut icons = None;
    let mut last = None;
    for _ in 0..50 {
        let mut served = false;
        for message in page.take_fetch_messages() {
            let ToBrowser::Fetch { fetch, url, .. } = message else {
                continue;
            };
            let path = url::Url::parse(&url).unwrap().to_file_path().unwrap();
            let body = std::fs::read(&path)
                .unwrap_or_else(|err| panic!("the gallery asked for {url}: {err}"));
            page.fetch_response(NAV, fetch, 200, &[], &url);
            page.fetch_chunk(NAV, fetch, &body);
            page.fetch_done(NAV, fetch);
            page.end_task();
            served = true;
        }
        let drew = page.begin_frame(0.0);
        let (adds, _) = page.take_resource_messages();
        for add in adds {
            if let ToBrowser::AddFont { key, source, .. } = add
                && let ipc::FontSource::Bytes(bytes) = &source
                && bytes.len() == icons_len
            {
                icons = Some(key);
            }
        }
        let asked = page.take_frame_request();
        if let Some(list) = drew {
            last = Some(list);
        }
        if !served && !asked && page.take_fetch_messages().is_empty() {
            break;
        }
    }
    Gallery {
        page,
        icons,
        last: last.expect("the gallery drew a frame"),
    }
}

/// A border box in page CSS px: left, top, right, bottom.
type Edges = (f32, f32, f32, f32);

/// The clipping ancestors around a fragment: each one's node and padding box.
type Clips = Rc<Vec<(NodeId, Edges)>>;

/// One element's box fragment, with the padding boxes of the clipping
/// ancestors around it.
struct Placed {
    node: NodeId,
    border: Edges,
    clips: Clips,
    /// The fragment was laid out hidden, waiting for a font.
    hidden_text: bool,
}

/// Every box fragment of the main frame at its page position.
fn placed(page: &Page) -> Vec<Placed> {
    let tree = page.main_frame().fragments.as_ref().unwrap();
    let mut found = Vec::new();
    let mut stack: Vec<(Point, Rc<Fragment>, Clips)> = tree
        .root
        .iter()
        .map(|(at, f)| (*at, Rc::clone(f), Rc::new(Vec::new())))
        .collect();
    let px = Au::to_f32_px;
    while let Some((at, fragment, clips)) = stack.pop() {
        let mut inner = Rc::clone(&clips);
        match &*fragment {
            Fragment::Box(b) => {
                let border = (
                    px(at.x),
                    px(at.y),
                    px(at.x + b.size.width),
                    px(at.y + b.size.height),
                );
                if let Some(node) = b.node {
                    found.push(Placed {
                        node,
                        border,
                        clips: Rc::clone(&clips),
                        hidden_text: false,
                    });
                    if b.is_scroll_container {
                        let style = page.main_frame().boxes.get(b.box_id).map(|l| &l.style);
                        let border_widths =
                            style.map(|s| layout::border_widths(s)).unwrap_or_default();
                        let mut list = (*clips).clone();
                        list.push((
                            node,
                            (
                                border.0 + px(border_widths.left),
                                border.1 + px(border_widths.top),
                                border.2 - px(border_widths.right),
                                border.3 - px(border_widths.bottom),
                            ),
                        ));
                        inner = Rc::new(list);
                    }
                }
            }
            Fragment::Text(text) if text.hidden_font => found.push(Placed {
                node: text.node,
                border: (0.0, 0.0, 0.0, 0.0),
                clips: Rc::clone(&clips),
                hidden_text: true,
            }),
            _ => {}
        }
        for (offset, child) in fragment.children().iter().rev() {
            stack.push((at.offset_by(*offset), Rc::clone(child), Rc::clone(&inner)));
        }
    }
    found
}

/// The control roles the gallery shows. Their containers (`tablist`,
/// `listbox`, `menu`, `radiogroup`, `dialog`) hold them and so overlap them.
const CONTROL_ROLES: &[&str] = &[
    "tab",
    "option",
    "menuitem",
    "menuitemradio",
    "radio",
    "switch",
    "checkbox",
    "progressbar",
];

fn is_control(page: &Page, node: NodeId) -> bool {
    let Some(element) = page
        .main_frame()
        .document
        .node(node)
        .and_then(|n| n.as_element())
    else {
        return false;
    };
    let role = element.attribute(&QualName::attribute(LocalName::from("role")));
    if role.is_some_and(|role| CONTROL_ROLES.contains(&role)) {
        return true;
    }
    matches!(&*element.name().local, "button" | "input")
}

/// Whether `a` is `b` or one of its flat tree ancestors.
fn contains(page: &Page, a: NodeId, b: NodeId) -> bool {
    let doc = &page.main_frame().document;
    let mut current = Some(b);
    while let Some(node) = current {
        if node == a {
            return true;
        }
        current = doc.flat_parent(node);
    }
    false
}

fn describe(page: &Page, node: NodeId) -> String {
    let doc = &page.main_frame().document;
    let Some(element) = doc.node(node).and_then(|n| n.as_element()) else {
        return format!("{node:?}");
    };
    let attr = |name: &str| element.attribute(&QualName::attribute(LocalName::from(name)));
    format!(
        "<{} role={:?} class={:?} label={:?}>",
        &*element.name().local,
        attr("role"),
        attr("class"),
        attr("aria-label"),
    )
}

/// Every overlap between two controls neither of which holds the other,
/// and every control cut by a clipping ancestor or by the page's right edge.
fn problems(gallery: &Gallery, width: f32) -> Vec<String> {
    let page = &gallery.page;
    let all = placed(page);
    let mut found = Vec::new();
    for hidden in all.iter().filter(|p| p.hidden_text) {
        found.push(format!(
            "text still hidden: {}",
            describe(page, hidden.node)
        ));
    }
    let controls: Vec<&Placed> = all
        .iter()
        .filter(|p| !p.hidden_text && is_control(page, p.node))
        .collect();
    // Half a pixel of slack for rounding to layout units.
    const SLACK: f32 = 0.5;
    for control in &controls {
        let (l, t, r, b) = control.border;
        for (clip_node, (cl, ct, cr, cb)) in control.clips.iter() {
            if l < cl - SLACK || t < ct - SLACK || r > cr + SLACK || b > cb + SLACK {
                found.push(format!(
                    "{} {:?} is clipped by {} {:?}",
                    describe(page, control.node),
                    control.border,
                    describe(page, *clip_node),
                    (cl, ct, cr, cb)
                ));
            }
        }
        if r > width + SLACK {
            found.push(format!(
                "{} {:?} runs past the page's right edge at {width}",
                describe(page, control.node),
                control.border
            ));
        }
    }
    for (i, a) in controls.iter().enumerate() {
        for b in &controls[i + 1..] {
            let overlap_x = a.border.2.min(b.border.2) - a.border.0.max(b.border.0);
            let overlap_y = a.border.3.min(b.border.3) - a.border.1.max(b.border.1);
            if overlap_x > SLACK
                && overlap_y > SLACK
                && !contains(page, a.node, b.node)
                && !contains(page, b.node, a.node)
            {
                found.push(format!(
                    "{} {:?} overlaps {} {:?}",
                    describe(page, a.node),
                    a.border,
                    describe(page, b.node),
                    b.border
                ));
            }
        }
    }
    found
}

fn check(width: f32) {
    let gallery = open(width, 800.0);
    let controls = placed(&gallery.page)
        .iter()
        .filter(|p| is_control(&gallery.page, p.node))
        .count();
    assert!(controls > 200, "only {controls} controls laid out");
    let problems = problems(&gallery, width);
    assert!(
        problems.is_empty(),
        "{} problems at {width} px:\n{}",
        problems.len(),
        problems.join("\n")
    );

    // The icons draw from icons.ttf, through `@font-face` (AC-10).
    let icons = gallery.icons.expect("icons.ttf was never registered");
    let icon_runs = gallery
        .last
        .items
        .iter()
        .filter(|item| matches!(item.kind, ItemKind::Text { font, .. } if font == icons))
        .count();
    assert!(icon_runs > 50, "only {icon_runs} runs drew in icons.ttf");
}

#[test]
fn the_gallery_lays_out_without_overlaps_or_wrong_clipping_at_1280_px() {
    // AC-20
    check(1280.0);
}

#[test]
fn the_gallery_lays_out_without_overlaps_or_wrong_clipping_at_640_px() {
    // AC-20, the 200% case.
    check(640.0);
}

/// The hover budget (AC-19): run with `cargo test --release -- --ignored
/// budgets`. It hovers every gallery button and times the update (style,
/// layout, paint), logging a `warn` above 2 ms; it never fails on time.
#[test]
#[ignore = "timed; run in release with --ignored budgets"]
fn budgets_a_hover_change_restyles_the_gallery_in_under_2_ms() {
    const BUDGET: Duration = Duration::from_millis(2);
    // Tall enough that every control is on screen without scrolling.
    let mut gallery = open(1280.0, 20_000.0);
    let buttons: Vec<(f32, f32)> = placed(&gallery.page)
        .iter()
        .filter(|p| {
            gallery
                .page
                .main_frame()
                .document
                .node(p.node)
                .and_then(|n| n.as_element())
                .is_some_and(|e| &*e.name().local == "button")
        })
        .map(|p| {
            (
                (p.border.0 + p.border.2) / 2.0,
                (p.border.1 + p.border.3) / 2.0,
            )
        })
        .collect();
    assert!(!buttons.is_empty());
    let page = &mut gallery.page;
    let mut slowest = Duration::ZERO;
    let mut over = 0;
    for &(x, y) in &buttons {
        let started = Instant::now();
        page.input(InputEvent::PointerMove { x, y });
        let _ = page.begin_frame(0.0);
        let took = started.elapsed();
        let _ = page.take_resource_messages();
        let _ = page.take_frame_request();
        slowest = slowest.max(took);
        if took > BUDGET {
            over += 1;
            tracing::warn!(x, y, ?took, "a hover update went over the 2 ms budget");
        }
    }
    eprintln!(
        "hover budget: {} buttons, slowest {slowest:?}, {over} over {BUDGET:?}",
        buttons.len()
    );
}
