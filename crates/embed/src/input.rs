//! winit input as the page's input events, and the page's pointer shapes as
//! OS cursors (spec 0006, Value sourcing: Pointer button, Key, OS cursor).

use ipc::{CursorKind, Key, Modifiers, PointerButton};
use winit::event::MouseButton;
use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey};
use winit::window::CursorIcon;

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
    }
}
