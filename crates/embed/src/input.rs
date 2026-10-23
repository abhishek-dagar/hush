//! winit input as the page's input events, and the page's pointer shapes as
//! OS cursors (spec 0006, Value sourcing: Pointer button, Key, OS cursor).

use ipc::{CursorKind, Key, Modifiers, PointerButton};
use winit::event::{ElementState, KeyEvent, MouseButton};
use winit::keyboard::{Key as WinitKey, KeyCode, ModifiersState, NamedKey, PhysicalKey};
use winit::window::CursorIcon;

/// A platform edit key (spec 0008, AC-15): Cmd on macOS, Ctrl elsewhere,
/// with A, C, X or V.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    not(feature = "chrome"),
    allow(dead_code, reason = "only the browser app has a clipboard")
)]
pub(crate) enum EditKey {
    /// Select the whole value.
    SelectAll,
    /// Copy the selection.
    Copy,
    /// Cut the selection.
    Cut,
    /// Paste the clipboard.
    Paste,
}

/// The edit key `event` is, if any. Keys go by their physical position, as
/// the browser's other shortcuts do; repeats count, so a held paste
/// pastes again.
#[cfg_attr(
    not(feature = "chrome"),
    allow(dead_code, reason = "only the browser app has a clipboard")
)]
pub(crate) fn edit_key(event: &KeyEvent, modifiers: ModifiersState) -> Option<EditKey> {
    if event.state != ElementState::Pressed {
        return None;
    }
    let primary = if cfg!(target_os = "macos") {
        modifiers.super_key() && !modifiers.control_key()
    } else {
        modifiers.control_key() && !modifiers.super_key()
    };
    if !primary || modifiers.alt_key() || modifiers.shift_key() {
        return None;
    }
    let PhysicalKey::Code(code) = event.physical_key else {
        return None;
    };
    Some(match code {
        KeyCode::KeyA => EditKey::SelectAll,
        KeyCode::KeyC => EditKey::Copy,
        KeyCode::KeyX => EditKey::Cut,
        KeyCode::KeyV => EditKey::Paste,
        _ => return None,
    })
}

/// The text a key press types (spec 0008, AC-13): winit's `KeyEvent.text`,
/// with dead keys and the layout applied, unless Cmd or Ctrl is held.
/// Control characters (Enter, Tab, Backspace, Escape) type nothing; their
/// keys do the work.
pub(crate) fn typed_text(event: &KeyEvent, modifiers: ModifiersState) -> Option<String> {
    if event.state != ElementState::Pressed || modifiers.control_key() || modifiers.super_key() {
        return None;
    }
    typed(event.text.as_deref()?)
}

fn typed(text: &str) -> Option<String> {
    let text: String = text.chars().filter(|c| !c.is_control()).collect();
    (!text.is_empty()).then_some(text)
}

/// The page's button for a winit one; other buttons are dropped.
pub(crate) fn pointer_button(button: MouseButton) -> Option<PointerButton> {
    match button {
        MouseButton::Left => Some(PointerButton::Primary),
        MouseButton::Right => Some(PointerButton::Secondary),
        MouseButton::Middle => Some(PointerButton::Middle),
        _ => None,
    }
}

/// The page's key for a winit key event's logical key. A modifier alone is
/// `None`: the browser keeps it.
pub(crate) fn key(logical: &WinitKey) -> Option<Key> {
    Some(match logical {
        WinitKey::Named(named) => match named {
            NamedKey::Tab => Key::Tab,
            NamedKey::Enter => Key::Enter,
            NamedKey::Space => Key::Space,
            NamedKey::Escape => Key::Escape,
            NamedKey::ArrowUp => Key::ArrowUp,
            NamedKey::ArrowDown => Key::ArrowDown,
            NamedKey::ArrowLeft => Key::ArrowLeft,
            NamedKey::ArrowRight => Key::ArrowRight,
            NamedKey::Home => Key::Home,
            NamedKey::End => Key::End,
            NamedKey::PageUp => Key::PageUp,
            NamedKey::PageDown => Key::PageDown,
            NamedKey::Backspace => Key::Backspace,
            NamedKey::Delete => Key::Delete,
            NamedKey::Shift
            | NamedKey::Control
            | NamedKey::Alt
            | NamedKey::AltGraph
            | NamedKey::Super
            | NamedKey::Meta
            | NamedKey::Hyper
            | NamedKey::Fn
            | NamedKey::CapsLock => return None,
            _ => Key::Other,
        },
        WinitKey::Character(text) if text.as_str() == " " => Key::Space,
        WinitKey::Character(text) => Key::Character(text.to_string()),
        _ => Key::Other,
    })
}

/// The page's modifiers for winit's.
pub(crate) fn modifiers(state: ModifiersState) -> Modifiers {
    Modifiers {
        shift: state.shift_key(),
        ctrl: state.control_key(),
        alt: state.alt_key(),
        meta: state.super_key(),
    }
}

/// The OS cursor for a page's pointer shape.
pub(crate) fn cursor_icon(cursor: CursorKind) -> CursorIcon {
    match cursor {
        CursorKind::Default => CursorIcon::Default,
        CursorKind::Pointer => CursorIcon::Pointer,
        CursorKind::Text => CursorIcon::Text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_and_modifiers_map_one_to_one() {
        // Spec 0006, Value sourcing
        assert_eq!(
            pointer_button(MouseButton::Left),
            Some(PointerButton::Primary)
        );
        assert_eq!(
            pointer_button(MouseButton::Right),
            Some(PointerButton::Secondary)
        );
        assert_eq!(pointer_button(MouseButton::Back), None);
        let held = modifiers(ModifiersState::SHIFT | ModifiersState::SUPER);
        assert!(held.shift && held.meta && !held.ctrl && !held.alt);
        assert_eq!(cursor_icon(CursorKind::Pointer), CursorIcon::Pointer);
        assert_eq!(cursor_icon(CursorKind::Text), CursorIcon::Text);
    }

    #[test]
    fn keys_map_by_meaning_and_modifiers_alone_stay_behind() {
        assert_eq!(key(&WinitKey::Named(NamedKey::Tab)), Some(Key::Tab));
        assert_eq!(key(&WinitKey::Named(NamedKey::Space)), Some(Key::Space));
        assert_eq!(key(&WinitKey::Character(" ".into())), Some(Key::Space));
        assert_eq!(
            key(&WinitKey::Character("a".into())),
            Some(Key::Character("a".into()))
        );
        assert_eq!(key(&WinitKey::Named(NamedKey::Shift)), None);
        assert_eq!(key(&WinitKey::Named(NamedKey::F5)), Some(Key::Other));
        assert_eq!(
            key(&WinitKey::Named(NamedKey::Backspace)),
            Some(Key::Backspace),
            "spec 0008, AC-13"
        );
        assert_eq!(key(&WinitKey::Named(NamedKey::Delete)), Some(Key::Delete));
    }

    #[test]
    fn control_characters_type_nothing() {
        // Spec 0008, AC-13
        assert_eq!(typed("é").as_deref(), Some("é"));
        assert_eq!(typed("a\r").as_deref(), Some("a"));
        assert_eq!(typed("\u{8}"), None, "Backspace's text");
        assert_eq!(typed("\t"), None);
    }
}
