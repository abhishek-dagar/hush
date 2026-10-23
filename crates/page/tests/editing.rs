//! Spec 0008, milestone 2: the single line editor real pages use, through
//! `Text`, `SelectAll`, `Copy`, `Cut` and `Paste` (AC-10, AC-13, AC-15,
//! AC-16). The rules are pure (`page::editor`); these tests prove the page
//! wires them to the focused input, the DOM value and the clipboard answer.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

use base::atoms::Atom;
use display_list::{PageId, SizeF};
use dom::{FrameId, NodeId};
use ipc::{DocumentKind, FetchId, InputEvent, Key, Modifiers, NavigationId, PointerButton};
use page::{EditState, MAX_PASTE_BYTES, Page};

const NAV: NavigationId = NavigationId(1);

/// A committed, parsed, laid out page.
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

/// Focuses `id` as Tab would, so the editor works on it.
fn focus(page: &mut Page, id: &str) {
    let node = element(page, id);
    assert!(page.move_focus(Some(node)), "#{id} takes focus");
}

/// The focused input's value and caret.
fn state(page: &Page) -> (String, EditState) {
    page.edit_state().expect("an editable input has focus")
}

fn type_text(page: &mut Page, text: &str) {
    page.input(InputEvent::Text { text: text.into() });
}

fn key(page: &mut Page, key: Key) {
    key_with(page, key, Modifiers::default());
}

fn key_with(page: &mut Page, key: Key, modifiers: Modifiers) {
    for pressed in [true, false] {
        page.input(InputEvent::Key {
            key: key.clone(),
            modifiers,
            pressed,
            repeat: false,
        });
    }
}

/// Runs the update and any frames it asks for.
fn settle(page: &mut Page) {
    page.end_task();
    while page.take_frame_request() {
        assert!(page.begin_frame(0.0).is_some(), "a frame");
    }
}

/// The horizontal scroll of a text input's text.
fn scroll_x(page: &Page, node: NodeId) -> f32 {
    page.scroll_offset(paint::scroll_id(FrameId::MAIN.0, node))
        .x
}

/// A primary press and release at `x`, `y` in page CSS px.
fn click(page: &mut Page, x: f32, y: f32) {
    page.input(InputEvent::PointerDown {
        x,
        y,
        button: PointerButton::Primary,
    });
    page.input(InputEvent::PointerUp {
        x,
        y,
        button: PointerButton::Primary,
    });
}

#[test]
fn a_click_puts_the_caret_between_the_text() {
    // AC-13: a press in a text input moves the caret where it landed.
    let mut page =
        load("<input id=f value='abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz'>");
    let node = element(&page, "f");
    let rect = page
        .element_boxes()
        .into_iter()
        .find(|(id, _)| *id == node)
        .expect("the input has a box")
        .1;
    let (x, y) = (
        rect.origin.x + rect.size.width / 2.0,
        rect.origin.y + rect.size.height / 2.0,
    );
    click(&mut page, x, y);
    let (value, state) = state(&page);
    assert!(
        state.caret > 0 && state.caret < value.len(),
        "the caret lands inside, not at an end: {}",
        state.caret
    );
}

#[test]
fn typing_inserts_at_the_caret_and_backspace_removes_one_grapheme() {
    // AC-13: an emoji with a skin tone modifier is one grapheme.
    let mut page = load("<input id=f value='a&#x1F44D;&#x1F3FD;b'>");
    focus(&mut page, "f");
    type_text(&mut page, "c");
    assert_eq!(state(&page).0, "a\u{1F44D}\u{1F3FD}bc");
    key(&mut page, Key::Backspace);
    assert_eq!(state(&page).0, "a\u{1F44D}\u{1F3FD}b", "c goes");
    key(&mut page, Key::Backspace);
    assert_eq!(state(&page).0, "a\u{1F44D}\u{1F3FD}", "b goes");
    key(&mut page, Key::Backspace);
    assert_eq!(state(&page).0, "a", "the whole emoji goes");
    key(&mut page, Key::Delete);
    assert_eq!(state(&page).0, "a", "nothing after the caret");
    key(&mut page, Key::Home);
    key(&mut page, Key::Delete);
    assert_eq!(state(&page).0, "", "Delete removes the grapheme after");
}

#[test]
fn maxlength_counts_utf_16_units_and_stops_typing_past_it() {
    // AC-10
    let mut page = load("<input id=f maxlength=3>");
    focus(&mut page, "f");
    type_text(&mut page, "abcde");
    assert_eq!(state(&page).0, "abc");
    type_text(&mut page, "d");
    assert_eq!(state(&page).0, "abc", "past the limit nothing more goes in");

    // A surrogate pair is two units, as HTML counts it.
    let mut emoji = load("<input id=f maxlength=1>");
    focus(&mut emoji, "f");
    type_text(&mut emoji, "\u{1F44D}");
    assert_eq!(state(&emoji).0, "", "one emoji needs two units");
}

#[test]
fn the_chrome_cap_stops_at_8192_bytes() {
    // AC-10: the address caps the whole value in bytes.
    let mut page = load("<input id=f>");
    page.set_text_cap(Some(8192));
    focus(&mut page, "f");
    type_text(&mut page, &"a".repeat(9000));
    assert_eq!(state(&page).0.len(), 8192);
    type_text(&mut page, "b");
    assert_eq!(state(&page).0.len(), 8192);
}

#[test]
fn select_all_copy_and_cut_answer_with_the_selection() {
    // AC-15
    let mut page = load("<input id=f value=hello>");
    focus(&mut page, "f");
    page.input(InputEvent::SelectAll);
    assert_eq!(state(&page).1.selection(), (0, 5));

    let copied = page.input(InputEvent::Copy { request: 7 });
    assert_eq!(copied.copied, Some((7, "hello".into())));
    assert_eq!(state(&page).0, "hello", "copy leaves the value");

    let cut = page.input(InputEvent::Cut { request: 8 });
    assert_eq!(cut.copied, Some((8, "hello".into())));
    assert_eq!(state(&page).0, "", "cut removes the selection");
}

#[test]
fn a_password_never_copies_or_cuts() {
    // AC-15
    let mut page = load("<input id=p type=password value=secret>");
    focus(&mut page, "p");
    page.input(InputEvent::SelectAll);
    assert_eq!(page.input(InputEvent::Copy { request: 1 }).copied, None);
    assert_eq!(page.input(InputEvent::Cut { request: 2 }).copied, None);
    assert_eq!(state(&page).0, "secret", "the value stays");
}

#[test]
fn readonly_takes_no_edit_but_still_copies() {
    // AC-13: read only text still copies.
    let mut page = load("<input id=r value=readonly readonly>");
    focus(&mut page, "r");
    type_text(&mut page, "x");
    key(&mut page, Key::Backspace);
    assert_eq!(state(&page).0, "readonly");
    page.input(InputEvent::SelectAll);
    assert_eq!(
        page.input(InputEvent::Copy { request: 3 }).copied,
        Some((3, "readonly".into()))
    );
    let cut = page.input(InputEvent::Cut { request: 4 });
    assert_eq!(cut.copied, Some((4, "readonly".into())));
    assert_eq!(state(&page).0, "readonly", "cut changes nothing");
}

#[test]
fn paste_drops_line_breaks_trims_and_caps_at_32_kb() {
    // AC-15
    let mut page = load("<input id=f>");
    focus(&mut page, "f");
    page.input(InputEvent::Paste {
        text: "  https://a.test/\r\nb  ".into(),
    });
    assert_eq!(state(&page).0, "https://a.test/b");

    let mut long = load("<input id=f>");
    focus(&mut long, "f");
    let huge = "é".repeat(MAX_PASTE_BYTES);
    long.input(InputEvent::Paste { text: huge });
    let value = state(&long).0;
    assert!(value.len() <= MAX_PASTE_BYTES, "{} bytes", value.len());
    assert!(value.len() > MAX_PASTE_BYTES - 4, "{} bytes", value.len());
}

#[test]
fn cmd_left_and_right_keep_their_text_meaning() {
    // AC-29: in a field they are start and end, not back and forward.
    if !cfg!(target_os = "macos") {
        return;
    }
    let mut page = load("<input id=f value=hello>");
    focus(&mut page, "f");
    let cmd = Modifiers {
        meta: true,
        ..Modifiers::default()
    };
    key_with(&mut page, Key::ArrowLeft, cmd);
    assert_eq!(state(&page).1.caret, 0);
    type_text(&mut page, "X");
    assert_eq!(state(&page).0, "Xhello");
    key_with(&mut page, Key::ArrowRight, cmd);
    assert_eq!(state(&page).1.caret, 6);
}

#[test]
fn setting_the_value_from_outside_puts_the_caret_at_its_end() {
    // AC-12: the chrome mirrors the address into its input.
    let mut page = load("<input id=f value=old>");
    focus(&mut page, "f");
    key(&mut page, Key::Home);
    let node = element(&page, "f");
    page.set_input_text(node, "fresh");
    assert_eq!(state(&page), ("fresh".into(), EditState::at_end("fresh")));
    type_text(&mut page, "!");
    assert_eq!(state(&page).0, "fresh!");
}

#[test]
fn a_mirrored_value_shows_from_its_start() {
    // AC-3, AC-12: a long URL shows its host, not its tail.
    let long = "https://example.com/".to_owned() + &"path/".repeat(50);
    let mut page = load("<input id=f value=old>");
    focus(&mut page, "f");
    let node = element(&page, "f");
    page.set_input_text(node, &long);
    settle(&mut page);
    assert_eq!(scroll_x(&page, node), 0.0, "the start stays in view");
}

#[test]
fn selecting_the_whole_value_shows_its_start() {
    // AC-12: Cmd+A and Cmd+L select all and show the host first.
    let long = "abcdefghijklmnopqrstuvwxyz".repeat(10);
    let mut page = load(&format!("<input id=f value='{long}'>"));
    focus(&mut page, "f");
    let node = element(&page, "f");
    page.input(InputEvent::SelectAll);
    settle(&mut page);
    assert_eq!(scroll_x(&page, node), 0.0, "the start stays in view");
}
