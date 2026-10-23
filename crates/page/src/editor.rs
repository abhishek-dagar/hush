//! The single line text editor every text input uses, in pages and in the
//! browser's own chrome (spec 0008, AC-13 to AC-16).
//!
//! The editing rules are pure: a value, a caret and an anchor go in with
//! one operation, and the new value, caret, anchor and any text for the
//! clipboard come out. The value itself lives in the DOM
//! (`Document::input_value`), so layout shows what was typed; the caret
//! and anchor live here. Offsets are bytes, always on grapheme boundaries.

use std::collections::HashMap;

use base::atoms::{LocalName, QualName};
use display_list::ColorF;
use dom::{Document, NodeId};

/// The most text one paste inserts, in bytes (AC-15).
pub const MAX_PASTE_BYTES: usize = 32 * 1024;

/// What selected text sits on in pages: the UA highlight (AC-14).
pub const PAGE_SELECTION: ColorF = ColorF {
    r: 0xac as f32 / 255.0,
    g: 0xce as f32 / 255.0,
    b: 0xf7 as f32 / 255.0,
    a: 1.0,
};

/// One text input's caret and selection (spec 0008, Data model,
/// `EditState`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EditState {
    /// Where the caret sits.
    pub caret: usize,
    /// The other end of the selection, if there is one.
    pub anchor: Option<usize>,
}

impl EditState {
    /// The caret at the end of `value`, nothing selected.
    pub fn at_end(value: &str) -> Self {
        Self {
            caret: value.len(),
            anchor: None,
        }
    }

    /// The selected bytes, `start..end`; empty when nothing is selected.
    pub fn selection(&self) -> (usize, usize) {
        let anchor = self.anchor.unwrap_or(self.caret);
        (self.caret.min(anchor), self.caret.max(anchor))
    }
}

/// Where a caret move goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    /// One grapheme left.
    Left,
    /// One grapheme right.
    Right,
    /// The start.
    Home,
    /// The end.
    End,
}

/// One editing operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditOp<'a> {
    /// Typed text replaces the selection or goes in at the caret.
    Insert(&'a str),
    /// Removes the selection, or the grapheme before the caret.
    Backspace,
    /// Removes the selection, or the grapheme after the caret.
    Delete,
    /// Moves the caret; with `extend` the selection grows instead of
    /// collapsing.
    Move {
        /// Where to.
        to: Motion,
        /// Keep the anchor (Shift held).
        extend: bool,
    },
    /// Selects the whole value.
    SelectAll,
    /// The selection, for the clipboard.
    Copy,
    /// The selection, for the clipboard, removed.
    Cut,
    /// Clipboard text, cleaned (AC-15), then inserted.
    Paste(&'a str),
}

/// What limits an input's edits (AC-10, AC-13).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Limits {
    /// `maxlength`, in UTF 16 code units as HTML counts it.
    pub max_units: Option<usize>,
    /// A cap in bytes, for the address (AC-10).
    pub max_bytes: Option<usize>,
    /// `readonly`: caret, selection and copy only.
    pub readonly: bool,
    /// `type=password`: never copies or cuts.
    pub password: bool,
}

/// The result of one operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    /// The new value, when it changed.
    pub value: Option<String>,
    /// The new caret and selection.
    pub state: EditState,
    /// Text for the clipboard, from a copy or cut of a selection.
    pub clipboard: Option<String>,
}

/// Applies `op` to `value` with the caret and selection in `state`.
pub fn apply(value: &str, state: EditState, op: EditOp<'_>, limits: Limits) -> Applied {
    let boundaries = text::grapheme_boundaries(value);
    let state = snap(state, &boundaries);
    let (start, end) = state.selection();
    let unchanged = |state| Applied {
        value: None,
        state,
        clipboard: None,
    };
    match op {
        EditOp::Move { to, extend } => {
            let target = match to {
                // Without Shift, a selection collapses to its near end.
                Motion::Left if !extend && start != end => start,
                Motion::Right if !extend && start != end => end,
                Motion::Left => previous(&boundaries, state.caret),
                Motion::Right => next(&boundaries, state.caret),
                Motion::Home => 0,
                Motion::End => value.len(),
            };
            let anchor = extend.then(|| state.anchor.unwrap_or(state.caret));
            unchanged(EditState {
                caret: target,
                anchor: anchor.filter(|&a| a != target),
            })
        }
        EditOp::SelectAll => unchanged(EditState {
            caret: value.len(),
            anchor: (!value.is_empty()).then_some(0),
        }),
        EditOp::Copy => Applied {
            clipboard: (start != end && !limits.password).then(|| value[start..end].to_owned()),
            ..unchanged(state)
        },
        EditOp::Cut if limits.password || limits.readonly || start == end => {
            // Read only text still copies (AC-13).
            let copied = (start != end && !limits.password).then(|| value[start..end].to_owned());
            Applied {
                clipboard: copied,
                ..unchanged(state)
            }
        }
        EditOp::Cut => Applied {
            clipboard: Some(value[start..end].to_owned()),
            ..replace(value, start, end, "")
        },
        _ if limits.readonly => unchanged(state),
        EditOp::Backspace if start != end => replace(value, start, end, ""),
        EditOp::Backspace if state.caret == 0 => unchanged(state),
        EditOp::Backspace => replace(value, previous(&boundaries, state.caret), state.caret, ""),
        EditOp::Delete if start != end => replace(value, start, end, ""),
        EditOp::Delete if state.caret == value.len() => unchanged(state),
        EditOp::Delete => replace(value, state.caret, next(&boundaries, state.caret), ""),
        EditOp::Insert(text) => insert(value, (start, end), text, limits),
        EditOp::Paste(text) => insert(value, (start, end), &clean_paste(text), limits),
    }
}

/// Puts `text`, cut to what the limits leave room for, in place of
/// `start..end`. Typing past a limit inserts nothing more (AC-10).
fn insert(value: &str, (start, end): (usize, usize), text: &str, limits: Limits) -> Applied {
    let text: String = text.chars().filter(|&c| c != '\n' && c != '\r').collect();
    let kept = value.len() - (end - start);
    let kept_units = utf16_len(value) - utf16_len(&value[start..end]);
    let room_bytes = limits
        .max_bytes
        .map_or(usize::MAX, |max| max.saturating_sub(kept));
    let room_units = limits
        .max_units
        .map_or(usize::MAX, |max| max.saturating_sub(kept_units));
    // The longest run of whole graphemes that fits.
    let fits = text::grapheme_boundaries(&text)
        .into_iter()
        .rev()
        .find(|&b| b <= room_bytes && utf16_len(&text[..b]) <= room_units)
        .unwrap_or(0);
    if fits == 0 && start == end {
        return Applied {
            value: None,
            state: EditState {
                caret: start,
                anchor: None,
            },
            clipboard: None,
        };
    }
    replace(value, start, end, &text[..fits])
}

/// `value` with `start..end` replaced by `text`, the caret after it.
fn replace(value: &str, start: usize, end: usize, text: &str) -> Applied {
    let mut next = String::with_capacity(value.len() - (end - start) + text.len());
    next.push_str(&value[..start]);
    next.push_str(text);
    next.push_str(&value[end..]);
    Applied {
        value: Some(next),
        state: EditState {
            caret: start + text.len(),
            anchor: None,
        },
        clipboard: None,
    }
}

/// Clipboard text as a paste inserts it (AC-15): line breaks dropped, the
/// ends trimmed, at most [`MAX_PASTE_BYTES`].
pub fn clean_paste(text: &str) -> String {
    let joined: String = text.chars().filter(|&c| c != '\n' && c != '\r').collect();
    let trimmed = joined.trim();
    let mut cut = trimmed.len().min(MAX_PASTE_BYTES);
    while !trimmed.is_char_boundary(cut) {
        cut -= 1;
    }
    trimmed[..cut].to_owned()
}

/// `state` on grapheme boundaries inside `value`, for painting a value
/// that may have changed under it.
pub fn snapped(value: &str, state: EditState) -> EditState {
    snap(state, &text::grapheme_boundaries(value))
}

/// `state` moved onto the nearest boundaries at or before it, so a value
/// that changed under it (a script set it) never splits a grapheme.
fn snap(state: EditState, boundaries: &[usize]) -> EditState {
    let floor = |at: usize| {
        boundaries
            .iter()
            .rev()
            .find(|&&b| b <= at)
            .copied()
            .unwrap_or(0)
    };
    let caret = floor(state.caret);
    EditState {
        caret,
        anchor: state.anchor.map(floor).filter(|&a| a != caret),
    }
}

fn previous(boundaries: &[usize], at: usize) -> usize {
    boundaries
        .iter()
        .rev()
        .find(|&&b| b < at)
        .copied()
        .unwrap_or(0)
}

fn next(boundaries: &[usize], at: usize) -> usize {
    boundaries.iter().find(|&&b| b > at).copied().unwrap_or(at)
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Whether `id` is an `<input>` the editor works on: type absent, `text`,
/// `search`, `url`, `email`, `tel` or `password`, and not disabled
/// (AC-13). `disabled` is checked by the caller, which knows the
/// fieldset rules.
pub(crate) fn editable_type(doc: &Document, id: NodeId) -> bool {
    if !doc.is_text_field(id) {
        return false;
    }
    let kind = attribute(doc, id, "type")
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(
        kind.as_str(),
        "" | "text" | "search" | "url" | "email" | "tel" | "password"
    )
}

/// The limits of the input `id`; `max_bytes` comes from the page.
pub(crate) fn limits(doc: &Document, id: NodeId, max_bytes: Option<usize>) -> Limits {
    let max_units = attribute(doc, id, "maxlength").and_then(|v| v.trim().parse::<usize>().ok());
    Limits {
        max_units,
        max_bytes,
        readonly: attribute(doc, id, "readonly").is_some(),
        password: attribute(doc, id, "type").is_some_and(|t| t.eq_ignore_ascii_case("password")),
    }
}

fn attribute<'a>(doc: &'a Document, id: NodeId, name: &str) -> Option<&'a str> {
    doc.node(id)?
        .as_element()?
        .attribute(&QualName::attribute(LocalName::from(name)))
}

/// Every text input's caret and selection in one document, by element.
#[derive(Debug, Default)]
pub(crate) struct Edits {
    states: HashMap<NodeId, EditState>,
}

impl Edits {
    /// The state of `id`, the caret at the end of `value` the first time.
    pub(crate) fn get(&self, id: NodeId, value: &str) -> EditState {
        self.states
            .get(&id)
            .copied()
            .unwrap_or_else(|| EditState::at_end(value))
    }

    pub(crate) fn set(&mut self, id: NodeId, state: EditState) {
        self.states.insert(id, state);
    }

    /// The document went away with its inputs.
    pub(crate) fn clear(&mut self) {
        self.states.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(value: &str, caret: usize, anchor: Option<usize>, op: EditOp<'_>) -> Applied {
        apply(value, EditState { caret, anchor }, op, Limits::default())
    }

    fn typed(value: &str, caret: usize, text: &str, limits: Limits) -> Option<String> {
        apply(
            value,
            EditState {
                caret,
                anchor: None,
            },
            EditOp::Insert(text),
            limits,
        )
        .value
    }

    #[test]
    fn typing_inserts_at_the_caret_or_replaces_the_selection() {
        // AC-13
        let applied = run("helo", 3, None, EditOp::Insert("l"));
        assert_eq!(applied.value.as_deref(), Some("hello"));
        assert_eq!(applied.state.caret, 4);
        let applied = run("hello world", 6, Some(11), EditOp::Insert("there"));
        assert_eq!(applied.value.as_deref(), Some("hello there"));
        assert_eq!(
            applied.state,
            EditState {
                caret: 11,
                anchor: None
            }
        );
    }

    #[test]
    fn backspace_and_delete_take_one_grapheme_or_the_selection() {
        // AC-13: a thumbs up with a skin tone is one grapheme, 8 bytes.
        let value = "a\u{1F44D}\u{1F3FD}b";
        let applied = run(value, 9, None, EditOp::Backspace);
        assert_eq!(applied.value.as_deref(), Some("ab"));
        assert_eq!(applied.state.caret, 1);
        let applied = run(value, 1, None, EditOp::Delete);
        assert_eq!(applied.value.as_deref(), Some("ab"));
        assert_eq!(run(value, 0, None, EditOp::Backspace).value, None);
        assert_eq!(run(value, 10, None, EditOp::Delete).value, None);
        let applied = run("abcd", 1, Some(3), EditOp::Delete);
        assert_eq!(applied.value.as_deref(), Some("ad"));
    }

    #[test]
    fn arrows_move_by_grapheme_and_collapse_a_selection() {
        // AC-13
        let value = "e\u{301}x";
        let right = EditOp::Move {
            to: Motion::Right,
            extend: false,
        };
        let left = EditOp::Move {
            to: Motion::Left,
            extend: false,
        };
        assert_eq!(run(value, 0, None, right).state.caret, 3);
        assert_eq!(run(value, 4, None, left).state.caret, 3);
        assert_eq!(run(value, 0, None, left).state.caret, 0);
        assert_eq!(
            run("abcd", 1, Some(3), left).state,
            EditState {
                caret: 1,
                anchor: None
            }
        );
        assert_eq!(
            run("abcd", 3, Some(1), right).state,
            EditState {
                caret: 3,
                anchor: None
            }
        );
        let home = EditOp::Move {
            to: Motion::Home,
            extend: false,
        };
        let end = EditOp::Move {
            to: Motion::End,
            extend: true,
        };
        assert_eq!(run("abcd", 2, None, home).state.caret, 0);
        assert_eq!(
            run("abcd", 2, None, end).state,
            EditState {
                caret: 4,
                anchor: Some(2)
            }
        );
    }

    #[test]
    fn maxlength_and_the_byte_cap_stop_typing_and_pasting() {
        // AC-10, AC-13
        let units = Limits {
            max_units: Some(5),
            ..Limits::default()
        };
        assert_eq!(typed("abcd", 4, "xyz", units).as_deref(), Some("abcdx"));
        assert_eq!(typed("abcde", 5, "x", units), None);
        let bytes = Limits {
            max_bytes: Some(8192),
            ..Limits::default()
        };
        let full = "a".repeat(8190);
        let after = typed(&full, 8190, "bcd", bytes).unwrap();
        assert_eq!(after.len(), 8192);
        // A grapheme that does not fit whole is left out.
        assert_eq!(typed(&full, 8190, "\u{1F44D}", bytes), None);
    }

    #[test]
    fn readonly_keeps_the_value_but_moves_selects_and_copies() {
        // AC-13
        let limits = Limits {
            readonly: true,
            ..Limits::default()
        };
        let state = EditState {
            caret: 0,
            anchor: Some(3),
        };
        assert_eq!(apply("abc", state, EditOp::Insert("x"), limits).value, None);
        assert_eq!(apply("abc", state, EditOp::Backspace, limits).value, None);
        let cut = apply("abc", state, EditOp::Cut, limits);
        assert_eq!((cut.value, cut.clipboard.as_deref()), (None, Some("abc")));
        let all = apply("abc", EditState::default(), EditOp::SelectAll, limits);
        assert_eq!(all.state.selection(), (0, 3));
    }

    #[test]
    fn copy_and_cut_need_a_selection_and_never_come_from_a_password() {
        // AC-15
        assert_eq!(run("abc", 1, None, EditOp::Copy).clipboard, None);
        let cut = run("abcd", 1, Some(3), EditOp::Cut);
        assert_eq!(cut.clipboard.as_deref(), Some("bc"));
        assert_eq!(cut.value.as_deref(), Some("ad"));
        let password = Limits {
            password: true,
            ..Limits::default()
        };
        let state = EditState {
            caret: 0,
            anchor: Some(3),
        };
        assert_eq!(apply("abc", state, EditOp::Copy, password).clipboard, None);
        let cut = apply("abc", state, EditOp::Cut, password);
        assert_eq!((cut.value, cut.clipboard), (None, None));
    }

    #[test]
    fn a_paste_drops_line_breaks_trims_and_caps_at_32_kb() {
        // AC-15
        assert_eq!(
            clean_paste("  https://exa\r\nmple.com/a\n "),
            "https://example.com/a"
        );
        let huge = "é".repeat(MAX_PASTE_BYTES);
        let cleaned = clean_paste(&huge);
        assert!(cleaned.len() <= MAX_PASTE_BYTES && cleaned.len() > MAX_PASTE_BYTES - 2);
        let applied = run("ab", 1, None, EditOp::Paste(" x\ny "));
        assert_eq!(applied.value.as_deref(), Some("axyb"));
    }

    #[test]
    fn a_stale_caret_snaps_back_onto_a_boundary() {
        // Spec 0008, Key invariants
        let applied = run("a\u{1F44D}", 3, None, EditOp::Insert("b"));
        assert_eq!(applied.value.as_deref(), Some("ab\u{1F44D}"));
        let applied = run("ab", 9, Some(9), EditOp::Insert("c"));
        assert_eq!(applied.value.as_deref(), Some("abc"));
    }
}
