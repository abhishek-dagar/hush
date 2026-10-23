//! The chrome's keyboard model for composites (spec 0003, keyboard model):
//! a `radiogroup`, `tablist`, `listbox` or `menu` is one `Tab` stop with
//! roving focus inside, written once per role here instead of per control.
//!
//! The engine already moves focus with `Tab`, and activates buttons with
//! `Enter` and `Space`. The keys it does not use come here, with the
//! composite around the focused item described as plain data.

/// A composite role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Composite {
    /// `role="radiogroup"` of `role="radio"`: arrows move and select.
    RadioGroup,
    /// `role="tablist"` of `role="tab"`: arrows along the orientation move,
    /// `Enter` selects.
    TabList {
        /// `aria-orientation="vertical"`.
        vertical: bool,
    },
    /// `role="listbox"` of `role="option"`: `Up` and `Down` move, `Enter`
    /// selects.
    ListBox,
    /// `role="menu"` of `menuitem` and `menuitemradio`: arrows move, the
    /// first letter jumps, `Enter` and `Space` activate.
    Menu,
}

impl Composite {
    /// The composite a container `role` attribute names.
    pub fn from_role(role: &str, vertical: bool) -> Option<Self> {
        Some(match role {
            "radiogroup" => Self::RadioGroup,
            "tablist" => Self::TabList { vertical },
            "listbox" => Self::ListBox,
            "menu" => Self::Menu,
            _ => return None,
        })
    }

    /// Whether an item `role` belongs in this composite.
    pub fn holds(self, role: &str) -> bool {
        match self {
            Self::RadioGroup => role == "radio",
            Self::TabList { .. } => role == "tab",
            Self::ListBox => role == "option",
            Self::Menu => matches!(role, "menuitem" | "menuitemradio" | "menuitemcheckbox"),
        }
    }
}

/// A key the composite model reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavKey {
    /// Arrow up.
    Up,
    /// Arrow down.
    Down,
    /// Arrow left.
    Left,
    /// Arrow right.
    Right,
    /// Home.
    Home,
    /// End.
    End,
    /// Enter.
    Enter,
    /// Space.
    Space,
    /// A printable character, for type ahead.
    Char(char),
}

/// One item of a composite, in tree order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// `disabled` or `aria-disabled="true"`: skipped by arrows.
    pub disabled: bool,
    /// Its text, for type ahead.
    pub label: String,
}

/// What a key does inside a composite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    /// Focus the item at this index.
    Focus(usize),
    /// Focus the item at this index and activate it (radio groups select
    /// as they move).
    FocusAndActivate(usize),
    /// Activate the current item.
    Activate,
    /// The key means nothing here.
    Nothing,
}

/// What `key` does in `composite`, whose `items` are in tree order with
/// focus on `current`. Arrows wrap and skip disabled items.
pub fn composite_key(composite: Composite, items: &[Item], current: usize, key: &NavKey) -> Move {
    let step = |forward: bool| step(items, current, forward);
    let first = || (0..items.len()).find(|&i| !items[i].disabled);
    let last = || (0..items.len()).rfind(|&i| !items[i].disabled);
    let focus = |index: Option<usize>| index.map_or(Move::Nothing, Move::Focus);
    match composite {
        Composite::RadioGroup => match key {
            NavKey::Up | NavKey::Left => step(false).map_or(Move::Nothing, Move::FocusAndActivate),
            NavKey::Down | NavKey::Right => {
                step(true).map_or(Move::Nothing, Move::FocusAndActivate)
            }
            _ => Move::Nothing,
        },
        Composite::TabList { vertical } => match (key, vertical) {
            (NavKey::Left, false) | (NavKey::Up, true) => focus(step(false)),
            (NavKey::Right, false) | (NavKey::Down, true) => focus(step(true)),
            (NavKey::Home, _) => focus(first()),
            (NavKey::End, _) => focus(last()),
            (NavKey::Enter, _) => Move::Activate,
            _ => Move::Nothing,
        },
        Composite::ListBox => match key {
            NavKey::Up => focus(step(false)),
            NavKey::Down => focus(step(true)),
            NavKey::Home => focus(first()),
            NavKey::End => focus(last()),
            NavKey::Enter => Move::Activate,
            _ => Move::Nothing,
        },
        Composite::Menu => match key {
            NavKey::Up => focus(step(false)),
            NavKey::Down => focus(step(true)),
            NavKey::Home => focus(first()),
            NavKey::End => focus(last()),
            NavKey::Enter | NavKey::Space => Move::Activate,
            NavKey::Char(c) => focus(type_ahead(items, current, *c)),
            NavKey::Left | NavKey::Right => Move::Nothing,
        },
    }
}

/// The next enabled item after (or before) `current`, wrapping. `None`
/// when no other item is enabled.
fn step(items: &[Item], current: usize, forward: bool) -> Option<usize> {
    let len = items.len();
    (1..len)
        .map(|offset| {
            if forward {
                (current + offset) % len
            } else {
                (current + len - offset) % len
            }
        })
        .find(|&i| !items[i].disabled)
}

/// The next enabled item after `current` whose label starts with `c`,
/// ignoring case, wrapping round to `current` itself last.
fn type_ahead(items: &[Item], current: usize, c: char) -> Option<usize> {
    let len = items.len();
    let wanted: Vec<char> = c.to_lowercase().collect();
    (1..=len).map(|offset| (current + offset) % len).find(|&i| {
        let label: Vec<char> = items[i]
            .label
            .trim_start()
            .chars()
            .take(1)
            .flat_map(char::to_lowercase)
            .collect();
        !items[i].disabled && label == wanted
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(labels: &[(&str, bool)]) -> Vec<Item> {
        labels
            .iter()
            .map(|&(label, disabled)| Item {
                disabled,
                label: label.to_owned(),
            })
            .collect()
    }

    #[test]
    fn radio_arrows_move_and_select_wrapping_past_disabled() {
        // AC-7
        let radios = items(&[("System", false), ("Light", true), ("Dark", false)]);
        let key = |current, key| composite_key(Composite::RadioGroup, &radios, current, &key);
        assert_eq!(key(0, NavKey::Right), Move::FocusAndActivate(2));
        assert_eq!(key(2, NavKey::Down), Move::FocusAndActivate(0), "wraps");
        assert_eq!(key(0, NavKey::Left), Move::FocusAndActivate(2));
        assert_eq!(key(0, NavKey::Home), Move::Nothing);
    }

    #[test]
    fn tabs_follow_their_orientation() {
        let tabs = items(&[("A", false), ("B", false), ("C", false)]);
        let horizontal = Composite::TabList { vertical: false };
        let vertical = Composite::TabList { vertical: true };
        assert_eq!(
            composite_key(horizontal, &tabs, 0, &NavKey::Right),
            Move::Focus(1)
        );
        assert_eq!(
            composite_key(horizontal, &tabs, 0, &NavKey::Down),
            Move::Nothing
        );
        assert_eq!(
            composite_key(vertical, &tabs, 0, &NavKey::Down),
            Move::Focus(1)
        );
        assert_eq!(
            composite_key(vertical, &tabs, 1, &NavKey::End),
            Move::Focus(2)
        );
        assert_eq!(
            composite_key(vertical, &tabs, 1, &NavKey::Enter),
            Move::Activate
        );
    }

    #[test]
    fn list_rows_move_up_and_down() {
        let rows = items(&[("A", false), ("B", false)]);
        let key = |current, key| composite_key(Composite::ListBox, &rows, current, &key);
        assert_eq!(key(0, NavKey::Down), Move::Focus(1));
        assert_eq!(key(0, NavKey::Up), Move::Focus(1), "wraps");
        assert_eq!(key(0, NavKey::Right), Move::Nothing);
        assert_eq!(key(1, NavKey::Enter), Move::Activate);
    }

    #[test]
    fn menus_jump_to_the_first_letter() {
        // AC-9
        let menu = items(&[
            ("System", false),
            ("Light", false),
            ("Dark", false),
            ("Settings…", false),
            ("Close Window", true),
        ]);
        let key = |current, key| composite_key(Composite::Menu, &menu, current, &key);
        assert_eq!(key(0, NavKey::Char('s')), Move::Focus(3), "the next S");
        assert_eq!(key(3, NavKey::Char('S')), Move::Focus(0), "wraps, any case");
        assert_eq!(key(0, NavKey::Char('c')), Move::Nothing, "disabled");
        assert_eq!(
            key(3, NavKey::Down),
            Move::Focus(0),
            "skips disabled, wraps"
        );
        assert_eq!(key(0, NavKey::End), Move::Focus(3));
        assert_eq!(key(2, NavKey::Space), Move::Activate);
    }

    #[test]
    fn a_lone_item_has_nowhere_to_go() {
        let one = items(&[("Only", false)]);
        assert_eq!(
            composite_key(Composite::Menu, &one, 0, &NavKey::Down),
            Move::Nothing
        );
        assert_eq!(
            composite_key(Composite::RadioGroup, &[], 0, &NavKey::Down),
            Move::Nothing
        );
    }

    #[test]
    fn roles_map_to_composites_and_their_items() {
        assert_eq!(
            Composite::from_role("tablist", true),
            Some(Composite::TabList { vertical: true })
        );
        assert_eq!(Composite::from_role("group", false), None);
        assert!(Composite::Menu.holds("menuitemradio"));
        assert!(!Composite::RadioGroup.holds("option"));
    }
}
