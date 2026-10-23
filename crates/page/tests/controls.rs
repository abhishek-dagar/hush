//! Spec 0006, milestone 4: form controls from the UA sheet, `:disabled` and
//! `:enabled`, `::placeholder`, the default focus ring, and how far a
//! state change restyles, on real committed pages (AC-2, AC-16, AC-17,
//! AC-19).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

use std::rc::Rc;

use base::Point;
use base::atoms::{Atom, LocalName, QualName};
use css::{ComputedStyle, OutlineStyle};
use display_list::{PageId, SizeF};
use ipc::{DocumentKind, FetchId, FocusDirection, InputEvent, NavigationId};
use layout::Fragment;
use page::Page;

const NAV: NavigationId = NavigationId(1);

fn load(html: &str) -> Page {
    let viewport = SizeF {
        width: 800.0,
        height: 600.0,
    };
    let mut page = Page::blank(PageId(1), viewport, 1.0, page::ua_stylesheet());
    page.navigate(NAV);
    assert!(page.commit_navigation(
        NAV,
        "http://test/page.html",
        Some("utf-8".into()),
        &DocumentKind::Document
    ));
    page.fetch_chunk(NAV, FetchId::DOCUMENT, html.as_bytes());
    page.fetch_done(NAV, FetchId::DOCUMENT);
    page.end_task();
    assert!(page.begin_frame(0.0).is_some(), "no first frame");
    while page.take_frame_request() && page.begin_frame(0.0).is_some() {}
    page
}

fn element(page: &Page, id: &str) -> dom::NodeId {
    page.main_frame()
        .document
        .element_by_id(&Atom::from(id))
        .unwrap_or_else(|| panic!("no #{id}"))
}

fn style(page: &Page, id: &str) -> Rc<ComputedStyle> {
    let doc = &page.main_frame().document;
    doc.node(element(page, id))
        .and_then(|n| n.as_element())
        .and_then(|e| e.style())
        .cloned()
        .unwrap_or_else(|| panic!("#{id} has no style"))
}

fn set_attribute(page: &mut Page, id: &str, name: &str, value: Option<&str>) {
    let node = element(page, id);
    let name = QualName::attribute(LocalName::from(name));
    let doc = &mut page.main_frame_mut().document;
    match value {
        Some(value) => doc.set_attribute(node, name, value).unwrap(),
        None => doc.remove_attribute(node, &name).unwrap(),
    }
    page.begin_frame(0.0);
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

/// The border box width of `id`'s one box, in CSS px.
fn width(page: &Page, id: &str) -> f32 {
    let target = element(page, id);
    let found: Vec<f32> = walk(page)
        .into_iter()
        .filter_map(|(_, f)| {
            let f = f.as_box()?;
            (f.node == Some(target)).then(|| f.size.width.to_f32_px())
        })
        .collect();
    assert_eq!(found.len(), 1, "#{id} has one box");
    found[0]
}

/// The text drawn inside `id`'s box, with the style it draws with.
fn text_in(page: &Page, id: &str) -> Vec<(String, Rc<ComputedStyle>)> {
    let target = element(page, id);
    walk(page)
        .into_iter()
        .filter_map(|(_, f)| match &*f {
            Fragment::Text(text) if text.node == target => {
                Some((text.text.clone(), Rc::clone(&text.style)))
            }
            _ => None,
        })
        .collect()
}

fn hex(color: css::Color) -> (u8, u8, u8) {
    let byte = |c: f32| (c * 255.0).round() as u8;
    (byte(color.r), byte(color.g), byte(color.b))
}

#[test]
fn unstyled_controls_get_the_flat_ua_look() {
    // AC-17, AC-18
    let page = load("<button id=b>Go</button><input id=i><a id=a href=x>x</a>");
    for id in ["b", "i"] {
        let style = style(&page, id);
        assert_eq!(
            style.display_box.display,
            css::Display::InlineBlock,
            "#{id}"
        );
        assert_eq!(style.border.top.width.to_f32_px(), 1.0, "#{id}");
        assert_eq!(style.border.radius.top_left, css::Length::px(2.0), "#{id}");
    }
    assert_eq!(hex(style(&page, "b").background.color), (0xef, 0xef, 0xef));
    assert_eq!(hex(style(&page, "i").background.color), (0xff, 0xff, 0xff));
    assert_eq!(style(&page, "a").inherited_box.cursor, css::Cursor::Pointer);
    assert_eq!(
        style(&page, "b").inherited_box.user_select,
        css::UserSelect::None
    );
}

#[test]
fn an_input_shows_its_value_or_its_placeholder_on_one_line() {
    // AC-17, Critical test scenarios: Controls
    let mut page = load(
        "<style>input::placeholder { color: #ff0000 }</style>
        <input id=empty placeholder=Search><input id=full value=hello placeholder=Search>
        <input id=secret type=password value=abc><input id=plain>
        <span id=zeros style='display: inline-block; font: 13.333px sans-serif; white-space: pre'>00000000000000000000</span>",
    );
    let empty = text_in(&page, "empty");
    if empty.is_empty() {
        return; // No font on this machine: nothing is shaped.
    }
    assert_eq!(empty[0].0, "Search");
    assert_eq!(
        hex(empty[0].1.inherited_text.color),
        (0xff, 0, 0),
        "::placeholder color"
    );
    let full = text_in(&page, "full");
    assert_eq!(full[0].0, "hello");
    assert_eq!(
        hex(full[0].1.inherited_text.color),
        (0, 0, 0),
        "the value is not a placeholder"
    );
    assert_eq!(text_in(&page, "secret")[0].0, "•••");

    // 20 zeros wide, plus 2 px padding and 1 px border each side.
    let zeros = width(&page, "zeros");
    for id in ["empty", "full", "plain"] {
        let field = width(&page, id);
        assert!(
            (field - (zeros + 6.0)).abs() < 0.5,
            "#{id} is {field} px, 20 zeros are {zeros} px"
        );
    }

    // A value hides the placeholder; clearing it brings it back.
    set_attribute(&mut page, "empty", "value", Some("typed"));
    assert_eq!(text_in(&page, "empty")[0].0, "typed");
    set_attribute(&mut page, "empty", "value", None);
    assert_eq!(text_in(&page, "empty")[0].0, "Search");
}

#[test]
fn disabled_and_enabled_follow_the_attribute_on_the_next_frame() {
    // AC-2, AC-16
    let mut page = load(
        "<style>
        button:disabled { background-color: #ff0000 }
        button:enabled { background-color: #00ff00 }
        fieldset:disabled + p { color: #0000ff }
        </style>
        <button id=b>b</button><fieldset id=f><input id=i></fieldset><p id=after>p</p>",
    );
    assert_eq!(hex(style(&page, "b").background.color), (0, 0xff, 0));
    set_attribute(&mut page, "b", "disabled", Some(""));
    assert_eq!(hex(style(&page, "b").background.color), (0xff, 0, 0));
    set_attribute(&mut page, "b", "disabled", None);
    assert_eq!(hex(style(&page, "b").background.color), (0, 0xff, 0));

    // A fieldset is no control: its `disabled` reaches the input inside
    // it, but `:disabled` never matches the fieldset itself.
    set_attribute(&mut page, "f", "disabled", Some(""));
    let input = page
        .main_frame()
        .document
        .node(element(&page, "i"))
        .unwrap();
    assert!(
        input
            .as_element()
            .unwrap()
            .state()
            .contains(dom::ElementState::DISABLED)
    );
    assert_eq!(hex(style(&page, "after").inherited_text.color), (0, 0, 0));
}

#[test]
fn keyboard_focus_shows_the_ua_ring_and_a_click_does_not() {
    // AC-14, AC-17
    let mut page = load("<button id=b style='display: block; width: 50px'>b</button>");
    assert_eq!(style(&page, "b").outline.style, OutlineStyle::None);
    page.input(InputEvent::FocusEnter {
        direction: FocusDirection::Forward,
    });
    page.begin_frame(0.0);
    let ring = style(&page, "b");
    assert_eq!(ring.outline.style, OutlineStyle::Solid);
    assert_eq!(ring.outline.width.to_f32_px(), 2.0);

    page.input(InputEvent::PointerDown {
        x: 20.0,
        y: 10.0,
        button: ipc::PointerButton::Primary,
    });
    page.begin_frame(0.0);
    assert_eq!(
        style(&page, "b").outline.style,
        OutlineStyle::None,
        "a click on a button shows no ring"
    );
}

#[test]
fn a_state_change_restyles_only_what_its_rules_reach() {
    // AC-19, Critical test scenarios: Invalidation
    let mut page = load(
        "<style>
        body { margin: 0 } #wrap { padding-top: 20px } .row { display: block; height: 20px }
        .row:hover .label { color: #ff0000 }
        </style>
        <div id=wrap><div class=row id=row><span class=label id=label>x</span><span>y</span></div>
        <div id=other><p>a</p><p>b</p><p>c</p></div></div>",
    );
    // Into the page first: `html`, `body` and `#wrap` join the hover chain
    // and stay in it while the pointer moves inside.
    page.input(InputEvent::PointerMove { x: 400.0, y: 5.0 });
    page.begin_frame(0.0);
    page.input(InputEvent::PointerMove { x: 400.0, y: 30.0 });
    page.begin_frame(0.0);
    assert_eq!(
        hex(style(&page, "label").inherited_text.color),
        (0xff, 0, 0)
    );
    let styled = page.last_style_stats().styled;
    assert!(
        (1..=3).contains(&styled),
        "a hover on .row restyled {styled} elements, not just its subtree of 3"
    );
}

/// The space above and below the first text drawn inside `id`'s box, in
/// CSS px.
fn text_gaps(page: &Page, id: &str) -> (f32, f32) {
    let target = element(page, id);
    let (_, fragment) = walk(page)
        .into_iter()
        .find(|(_, f)| f.as_box().is_some_and(|b| b.node == Some(target)))
        .unwrap_or_else(|| panic!("#{id} has no box"));
    let field = fragment.as_box().unwrap();
    let mut stack: Vec<(Point, Rc<Fragment>)> = field.children.clone();
    while let Some((at, fragment)) = stack.pop() {
        if let Fragment::Text(text) = &*fragment {
            let above = at.y.to_f32_px();
            let below = (field.size.height - at.y - text.size.height).to_f32_px();
            return (above, below);
        }
        for (offset, child) in fragment.children() {
            stack.push((at.offset_by(*offset), Rc::clone(child)));
        }
    }
    panic!("#{id} draws no text")
}

#[test]
fn a_tall_input_or_button_centers_its_line() {
    // AC-17: a control taller than its line centers it, as Chrome's
    // inner editor and button content box do.
    let page = load(
        "<style>input, button { box-sizing: border-box; min-height: 40px; padding: 0 8px }</style>
        <input id=field placeholder=Search><input id=filled value=hello>
        <button id=button>Go</button><input id=submit type=submit>",
    );
    if text_in(&page, "field").is_empty() {
        return; // No font on this machine: nothing is shaped.
    }
    for id in ["field", "filled", "button", "submit"] {
        let (above, below) = text_gaps(&page, id);
        assert!(above > 2.0, "#{id} has room above its line: {above} px");
        assert!(
            (above - below).abs() < 1.0,
            "#{id} has {above} px above its line and {below} px below"
        );
    }
}
