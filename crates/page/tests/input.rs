//! Spec 0006, milestones 1 and 4: pointer moves, presses, keys and window
//! focus restyle a real committed page on the next frame, through parse,
//! style, layout and hit testing, move focus through the Tab order and
//! report activations and cursors, with no script engine involved (AC-21).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

use base::atoms::Atom;
use display_list::{PageId, PointF, SizeF};
use dom::ElementState;
use ipc::{
    CursorKind, DocumentKind, FetchId, FocusDirection, InputEvent, Key, Modifiers, NavigationId,
    PointerButton,
};
use page::{InputOutcome, Page};

const NAV: NavigationId = NavigationId(1);

/// A committed, parsed page that drew its first frame.
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
    page.end_task();
    page.fetch_chunk(NAV, FetchId::DOCUMENT, html.as_bytes());
    page.end_task();
    page.fetch_done(NAV, FetchId::DOCUMENT);
    page.end_task();
    assert!(page.begin_frame(0.0).is_some(), "no first frame");
    // Let the load settle, so later frames show only what input changed.
    while page.take_frame_request() && page.begin_frame(0.0).is_some() {}
    page
}

fn element(page: &Page, id: &str) -> dom::NodeId {
    page.main_frame()
        .document
        .element_by_id(&Atom::from(id))
        .unwrap_or_else(|| panic!("no #{id}"))
}

fn background(page: &Page, id: &str) -> String {
    let doc = &page.main_frame().document;
    let dump = doc
        .node(element(page, id))
        .and_then(|n| n.as_element())
        .and_then(|e| e.style())
        .map(|s| s.dump())
        .unwrap_or_default();
    dump.split("; ")
        .find_map(|pair| pair.strip_prefix("background-color: "))
        .unwrap_or("transparent")
        .to_owned()
}

/// Which element is under `x`, `y`.
fn hit(page: &Page, x: f32, y: f32) -> Option<dom::NodeId> {
    page.hit_test(PointF { x, y }).map(|(hit, _)| hit.node)
}

/// Moves the pointer and runs the frame that applies it.
fn move_to(page: &mut Page, x: f32, y: f32) {
    page.input(InputEvent::PointerMove { x, y });
    assert!(page.take_frame_request(), "a move asks for a frame");
    page.begin_frame(0.0);
}

const ROW: &str = "<!doctype html><style>
    body { margin: 0 }
    .row { display: flex; gap: 10px }
    .row > div { width: 100px; height: 20px }
    .item:hover { background-color: #ff0000 }
    </style>
    <div class=row id=row><div class=item id=a></div><div class=item id=b></div></div>";

#[test]
fn a_flex_row_places_items_side_by_side_with_the_gap_between() {
    // AC-3, AC-20 (first part)
    let page = load(ROW);
    assert_eq!(hit(&page, 50.0, 5.0), Some(element(&page, "a")));
    assert_eq!(
        hit(&page, 105.0, 5.0),
        Some(element(&page, "row")),
        "the gap belongs to the container"
    );
    assert_eq!(hit(&page, 150.0, 5.0), Some(element(&page, "b")));
    assert_eq!(hit(&page, 215.0, 5.0), Some(element(&page, "row")));
}

#[test]
fn hover_follows_the_pointer_on_the_next_frame() {
    // AC-12, AC-19, AC-21
    let mut page = load(ROW);
    assert_eq!(background(&page, "a"), "transparent");

    move_to(&mut page, 150.0, 5.0);
    assert_eq!(background(&page, "b"), "#ff0000ff");
    assert_eq!(background(&page, "a"), "transparent");

    move_to(&mut page, 50.0, 5.0);
    assert_eq!(background(&page, "a"), "#ff0000ff");
    assert_eq!(background(&page, "b"), "transparent");

    page.input(InputEvent::PointerLeave);
    page.begin_frame(0.0);
    assert_eq!(background(&page, "a"), "transparent");
}

#[test]
fn moves_between_frames_hit_test_once_at_the_latest_point() {
    let mut page = load(ROW);
    page.input(InputEvent::PointerMove { x: 50.0, y: 5.0 });
    page.input(InputEvent::PointerMove { x: 150.0, y: 5.0 });
    page.begin_frame(0.0);
    assert_eq!(background(&page, "a"), "transparent");
    assert_eq!(background(&page, "b"), "#ff0000ff");
}

#[test]
fn hover_restyles_nothing_when_no_rule_uses_it() {
    // AC-19
    let mut page = load(
        "<!doctype html><style>.row { display: flex } .row > div { width: 50px; height: 9px }
        </style><div class=row><div></div></div>",
    );
    page.input(InputEvent::PointerMove { x: 10.0, y: 12.0 });
    assert!(page.take_frame_request());
    assert!(
        page.begin_frame(0.0).is_none(),
        "a hover nobody styles restyles nothing and draws nothing new"
    );
    assert!(!page.take_frame_request());
}

fn state(page: &Page, id: &str) -> ElementState {
    page.main_frame()
        .document
        .node(element(page, id))
        .and_then(|n| n.as_element())
        .map(|e| e.state())
        .unwrap_or(ElementState::empty())
}

fn has(page: &Page, id: &str, bit: ElementState) -> bool {
    state(page, id).contains(bit)
}

fn press(page: &mut Page, x: f32, y: f32) -> InputOutcome {
    page.input(InputEvent::PointerDown {
        x,
        y,
        button: PointerButton::Primary,
    })
}

fn release(page: &mut Page, x: f32, y: f32) -> InputOutcome {
    page.input(InputEvent::PointerUp {
        x,
        y,
        button: PointerButton::Primary,
    })
}

fn key_with(page: &mut Page, key: Key, shift: bool, pressed: bool) -> InputOutcome {
    page.input(InputEvent::Key {
        key,
        modifiers: Modifiers {
            shift,
            ..Modifiers::default()
        },
        pressed,
        repeat: false,
    })
}

/// A key down then up; the outcome of the down.
fn tap(page: &mut Page, key: Key) -> InputOutcome {
    let down = key_with(page, key.clone(), false, true);
    key_with(page, key, false, false);
    down
}

fn focused(page: &Page) -> Option<dom::NodeId> {
    page.focused().map(|focus| focus.node)
}

fn activated(outcome: &InputOutcome) -> Option<dom::NodeId> {
    outcome.activated.map(|node| node.node)
}

/// Lets a frame run, so styles and layout follow the last input.
fn frame(page: &mut Page) {
    page.begin_frame(0.0);
}

/// Stacked 100 × 20 blocks; `#one` holds two 40 px wide spans.
const CONTROLS: &str = "<!doctype html><style>
    body { margin: 0 }
    button, div { display: block; width: 100px; height: 20px; margin: 0; padding: 0; border: 0 }
    span { display: inline-block; width: 40px; height: 20px }
    </style>
    <button id=one><span id=icon></span><span id=label></span></button>
    <button id=off disabled></button>
    <div id=plain></div>";

#[test]
fn a_press_holds_active_until_release_and_activates_the_common_ancestor() {
    // AC-12, AC-15, Critical test scenarios: Activation
    let mut page = load(CONTROLS);
    press(&mut page, 10.0, 10.0);
    assert!(has(&page, "icon", ElementState::ACTIVE));
    assert!(has(&page, "one", ElementState::ACTIVE));
    assert_eq!(focused(&page), Some(element(&page, "one")));

    // Dragging off keeps it active; the release lands on the label.
    page.input(InputEvent::PointerMove { x: 300.0, y: 300.0 });
    frame(&mut page);
    assert!(has(&page, "one", ElementState::ACTIVE));
    let up = release(&mut page, 60.0, 10.0);
    assert_eq!(activated(&up), Some(element(&page, "one")));
    assert!(!has(&page, "one", ElementState::ACTIVE));
    assert!(!has(&page, "icon", ElementState::ACTIVE));

    // Press on the button, release on the plain block: their common
    // ancestor activates, not the button.
    press(&mut page, 10.0, 10.0);
    let up = release(&mut page, 10.0, 50.0);
    assert_eq!(
        activated(&up),
        page.main_frame()
            .document
            .flat_parent(element(&page, "one"))
    );
}

#[test]
fn a_disabled_button_takes_no_active_no_focus_and_no_activation() {
    // AC-15, AC-16
    let mut page = load(CONTROLS);
    press(&mut page, 10.0, 10.0);
    release(&mut page, 10.0, 10.0);
    assert_eq!(focused(&page), Some(element(&page, "one")));

    press(&mut page, 10.0, 30.0);
    assert!(has(&page, "off", ElementState::DISABLED));
    assert!(!has(&page, "off", ElementState::ACTIVE));
    assert!(
        has(&page, "off", ElementState::HOVER),
        "hover still matches"
    );
    assert_eq!(
        focused(&page),
        None,
        "a press on nothing focusable clears focus"
    );
    let up = release(&mut page, 10.0, 30.0);
    assert_eq!(activated(&up), None);
}

const TAB_ORDER: &str = "<!doctype html><style>
    body { margin: 0 } div, button { display: block; height: 20px; margin: 0; padding: 0; border: 0 }
    </style>
    <div id=b tabindex=2></div><div id=a tabindex=1></div><div id=neg tabindex=-1></div>
    <button id=btn></button><div id=gone tabindex=0 style='display: none'></div>";

#[test]
fn tab_walks_positive_indexes_then_tree_order_and_leaves_at_the_end() {
    // AC-13, AC-14, Critical test scenarios: Focus
    let mut page = load(TAB_ORDER);
    tap(&mut page, Key::Tab);
    assert_eq!(focused(&page), Some(element(&page, "a")));
    assert!(has(
        &page,
        "a",
        ElementState::FOCUS | ElementState::FOCUS_VISIBLE
    ));
    tap(&mut page, Key::Tab);
    assert_eq!(focused(&page), Some(element(&page, "b")));
    assert!(!has(&page, "a", ElementState::FOCUS));
    tap(&mut page, Key::Tab);
    assert_eq!(focused(&page), Some(element(&page, "btn")));
    let out = tap(&mut page, Key::Tab);
    assert_eq!(out.focus_left, Some(FocusDirection::Forward));
    assert_eq!(focused(&page), None);

    // The browser hands focus back from the other end.
    page.input(InputEvent::FocusEnter {
        direction: FocusDirection::Backward,
    });
    assert_eq!(focused(&page), Some(element(&page, "btn")));
    key_with(&mut page, Key::Tab, true, true);
    assert_eq!(focused(&page), Some(element(&page, "b")));

    // A click focuses the `-1` element without a ring; a key adds it.
    press(&mut page, 5.0, 45.0);
    release(&mut page, 5.0, 45.0);
    assert_eq!(focused(&page), Some(element(&page, "neg")));
    assert!(has(&page, "neg", ElementState::FOCUS));
    assert!(!has(&page, "neg", ElementState::FOCUS_VISIBLE));
    let out = tap(&mut page, Key::Character("x".into()));
    assert!(has(&page, "neg", ElementState::FOCUS_VISIBLE));
    assert_eq!(out.unused_key, Some(Key::Character("x".into())));
    let out = tap(&mut page, Key::Tab);
    assert_eq!(out.focus_left, None);
    assert_eq!(
        focused(&page),
        Some(element(&page, "btn")),
        "from the `-1` element Tab goes on from its place in tree order"
    );
}

#[test]
fn enter_and_space_activate_a_focused_button() {
    // AC-15
    let mut page = load(TAB_ORDER);
    page.input(InputEvent::FocusEnter {
        direction: FocusDirection::Backward,
    });
    let out = tap(&mut page, Key::Enter);
    assert_eq!(activated(&out), Some(element(&page, "btn")));

    let down = key_with(&mut page, Key::Space, false, true);
    assert_eq!(activated(&down), None);
    assert!(has(&page, "btn", ElementState::ACTIVE));
    let up = key_with(&mut page, Key::Space, false, false);
    assert_eq!(activated(&up), Some(element(&page, "btn")));
    assert!(!has(&page, "btn", ElementState::ACTIVE));

    assert_eq!(tap(&mut page, Key::Escape).unused_key, Some(Key::Escape));
    let ctrl_tab = page.input(InputEvent::Key {
        key: Key::Tab,
        modifiers: Modifiers {
            ctrl: true,
            ..Modifiers::default()
        },
        pressed: true,
        repeat: false,
    });
    assert_eq!(
        ctrl_tab.unused_key,
        Some(Key::Tab),
        "chords are not focus moves"
    );
}

#[test]
fn window_blur_clears_hover_and_active_and_hides_focus_until_it_returns() {
    // AC-12, AC-13, Critical test scenarios: Window blur
    let mut page = load(TAB_ORDER);
    page.input(InputEvent::FocusEnter {
        direction: FocusDirection::Backward,
    });
    key_with(&mut page, Key::Space, false, true);
    assert!(has(&page, "btn", ElementState::ACTIVE));

    page.input(InputEvent::WindowFocus { focused: false });
    assert!(!has(&page, "btn", ElementState::ACTIVE));
    assert!(!has(&page, "btn", ElementState::FOCUS));
    assert_eq!(
        focused(&page),
        Some(element(&page, "btn")),
        "the node is kept"
    );
    let up = key_with(&mut page, Key::Space, false, false);
    assert_eq!(activated(&up), None, "the blur ended the press");

    page.input(InputEvent::WindowFocus { focused: true });
    assert!(has(
        &page,
        "btn",
        ElementState::FOCUS | ElementState::FOCUS_VISIBLE
    ));
}

#[test]
fn a_focused_element_that_loses_its_box_loses_focus() {
    // AC-13
    let mut page = load(TAB_ORDER);
    page.input(InputEvent::FocusEnter {
        direction: FocusDirection::Forward,
    });
    let a = element(&page, "a");
    assert_eq!(focused(&page), Some(a));
    page.main_frame_mut()
        .document
        .set_attribute(
            a,
            base::atoms::QualName::attribute(base::atoms::LocalName::from("style")),
            "display: none",
        )
        .unwrap();
    frame(&mut page);
    assert_eq!(focused(&page), None);
}

#[test]
fn hover_follows_a_layout_change_under_a_still_pointer() {
    // AC-12, Decisions: Hover after layout
    let mut page = load(ROW);
    move_to(&mut page, 50.0, 5.0);
    assert_eq!(background(&page, "a"), "#ff0000ff");
    let a = element(&page, "a");
    page.main_frame_mut()
        .document
        .set_attribute(
            a,
            base::atoms::QualName::attribute(base::atoms::LocalName::from("style")),
            "display: none",
        )
        .unwrap();
    frame(&mut page);
    assert!(
        page.take_frame_request(),
        "a new layout asks to hit test again"
    );
    frame(&mut page);
    assert_eq!(
        background(&page, "b"),
        "#ff0000ff",
        "#b slid under the pointer"
    );
}

#[test]
fn the_cursor_follows_links_text_and_user_select() {
    // AC-18
    let mut page = load(
        "<!doctype html><style>
        body { margin: 0; font-size: 16px; line-height: 20px }
        a, p, div { display: block; height: 20px; margin: 0 }
        .flat { user-select: none } .hand { cursor: pointer }
        </style>
        <a href=next id=link></a><p id=text>words</p><p class=flat>words</p>
        <div class=hand><span>x</span></div>",
    );
    // The page reports a shape only when it changes, as the browser keeps it.
    let mut shown = CursorKind::Default;
    let mut cursor_at = |page: &mut Page, x: f32, y: f32| {
        move_to(page, x, y);
        if let Some(cursor) = page.take_cursor_change() {
            shown = cursor;
        }
        shown
    };
    assert_eq!(
        cursor_at(&mut page, 5.0, 5.0),
        CursorKind::Pointer,
        "a[href]"
    );
    assert_eq!(
        cursor_at(&mut page, 700.0, 25.0),
        CursorKind::Default,
        "beside the text"
    );
    assert_eq!(
        cursor_at(&mut page, 700.0, 65.0),
        CursorKind::Pointer,
        "inherited"
    );
    if has_text(&page) {
        assert_eq!(cursor_at(&mut page, 5.0, 25.0), CursorKind::Text, "text");
        assert_eq!(
            cursor_at(&mut page, 5.0, 45.0),
            CursorKind::Default,
            "user-select: none text"
        );
    }
}

/// Whether the machine has a font, so text has fragments to hit.
fn has_text(page: &Page) -> bool {
    page.main_frame()
        .fragments
        .as_ref()
        .is_some_and(|tree| format!("{tree:?}").contains("Text(TextFragment"))
}
