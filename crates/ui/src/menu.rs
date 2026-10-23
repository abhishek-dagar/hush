//! The one menu definition (spec 0003, menu model). `embed` turns it into
//! muda menus on macOS and Windows; on Linux the chrome document builds
//! its in window app menu from it.

use crate::appearance::Os;
use crate::command::{ChromeCommand, EditAction, WindowAction, ZoomStep};
use crate::settings::{AccentSetting, Settings, Theme};
use crate::theme::Accent;

/// Modifier keys of a shortcut. `primary` is Cmd on macOS and Ctrl on
/// Windows and Linux.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    /// Cmd on macOS, Ctrl elsewhere.
    pub primary: bool,
    /// Option on macOS, Alt elsewhere.
    pub alt: bool,
    /// Shift.
    pub shift: bool,
}

/// The non modifier key of a shortcut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortcutKey {
    /// `,`
    Comma,
    /// `=` (the key that also carries `+`).
    Equal,
    /// `-`
    Minus,
    /// `0` on the main row.
    Digit0,
    /// A letter, lowercase.
    Letter(char),
}

/// A keyboard shortcut, written once for every OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shortcut {
    /// The modifiers held.
    pub modifiers: Modifiers,
    /// The key pressed.
    pub key: ShortcutKey,
}

impl Shortcut {
    /// `primary` + `key`.
    pub const fn primary(key: ShortcutKey) -> Self {
        Self {
            modifiers: Modifiers {
                primary: true,
                alt: false,
                shift: false,
            },
            key,
        }
    }

    /// `primary` + `alt` + `key`.
    pub const fn primary_alt(key: ShortcutKey) -> Self {
        Self {
            modifiers: Modifiers {
                primary: true,
                alt: true,
                shift: false,
            },
            key,
        }
    }

    /// How a menu row shows it on `os`: `⌥⌘=` on macOS, `Ctrl+Alt+=`
    /// elsewhere.
    pub fn label(self, os: Os) -> String {
        let key = match self.key {
            ShortcutKey::Comma => ",".to_owned(),
            ShortcutKey::Equal => "=".to_owned(),
            ShortcutKey::Minus => "-".to_owned(),
            ShortcutKey::Digit0 => "0".to_owned(),
            ShortcutKey::Letter(c) => c.to_uppercase().collect(),
        };
        let m = self.modifiers;
        if os == Os::Mac {
            let mut label = String::new();
            for (held, symbol) in [(m.alt, '⌥'), (m.shift, '⇧'), (m.primary, '⌘')] {
                if held {
                    label.push(symbol);
                }
            }
            return label + &key;
        }
        let names = [(m.primary, "Ctrl"), (m.alt, "Alt"), (m.shift, "Shift")];
        names
            .into_iter()
            .filter(|(held, _)| *held)
            .map(|(_, name)| name)
            .chain([key.as_str()])
            .collect::<Vec<_>>()
            .join("+")
    }
}

/// A standard item the OS implements itself (macOS app and Edit menus).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeItem {
    /// About the app.
    About,
    /// The Services submenu.
    Services,
    /// Hide the app.
    Hide,
    /// Hide the other apps.
    HideOthers,
    /// Show all apps.
    ShowAll,
    /// Quit.
    Quit,
    /// Undo.
    Undo,
    /// Redo.
    Redo,
    /// Cut.
    Cut,
    /// Copy.
    Copy,
    /// Paste.
    Paste,
    /// Select all.
    SelectAll,
}

/// One entry in a menu.
#[derive(Debug, Clone, PartialEq)]
pub enum MenuItem {
    /// An item that runs a chrome command.
    Command {
        /// What the item says.
        label: String,
        /// What it runs.
        command: ChromeCommand,
        /// Its shortcut, if any.
        shortcut: Option<Shortcut>,
        /// `Some` for a checkable item, with its state.
        checked: Option<bool>,
    },
    /// A nested menu.
    Submenu {
        /// What the item says.
        label: String,
        /// The nested items.
        items: Vec<MenuItem>,
    },
    /// A divider line.
    Separator,
    /// An item the OS implements.
    Native(NativeItem),
}

impl MenuItem {
    fn command(label: &str, command: ChromeCommand, shortcut: Option<Shortcut>) -> Self {
        Self::Command {
            label: label.to_owned(),
            command,
            shortcut,
            checked: None,
        }
    }

    fn radio(label: &str, command: ChromeCommand, checked: bool) -> Self {
        Self::Command {
            label: label.to_owned(),
            command,
            shortcut: None,
            checked: Some(checked),
        }
    }

    fn submenu(label: &str, items: Vec<MenuItem>) -> Self {
        Self::Submenu {
            label: label.to_owned(),
            items,
        }
    }
}

/// A menu: on macOS the menu bar (each item a submenu), elsewhere the app
/// menu the `≡` button opens.
#[derive(Debug, Clone, PartialEq)]
pub struct Menu {
    /// The top level items.
    pub items: Vec<MenuItem>,
}

impl Menu {
    /// The command bound to `shortcut` anywhere in this menu.
    pub fn command_for(&self, shortcut: Shortcut) -> Option<ChromeCommand> {
        fn find(items: &[MenuItem], shortcut: Shortcut) -> Option<ChromeCommand> {
            items.iter().find_map(|item| match item {
                MenuItem::Command {
                    command,
                    shortcut: Some(s),
                    ..
                } if *s == shortcut => Some(*command),
                MenuItem::Submenu { items, .. } => find(items, shortcut),
                _ => None,
            })
        }
        find(&self.items, shortcut)
    }

    /// Adds the Developer menu with the file dialog demos (spec 0003,
    /// AC-10). For dev builds only, until the gallery's demo buttons
    /// replace it (milestone 4).
    pub fn with_developer(mut self, os: Os) -> Self {
        let developer = MenuItem::submenu(
            "Developer",
            vec![
                MenuItem::command(
                    "Open File…",
                    ChromeCommand::DevOpenFile,
                    Some(Shortcut::primary_alt(ShortcutKey::Letter('o'))),
                ),
                MenuItem::command(
                    "Save File…",
                    ChromeCommand::DevSaveFile,
                    Some(Shortcut::primary_alt(ShortcutKey::Letter('s'))),
                ),
            ],
        );
        // The macOS menu bar holds only submenus; the app menu elsewhere is
        // one list, so a separator sets the dev items apart.
        if os != Os::Mac {
            self.items.push(MenuItem::Separator);
        }
        self.items.push(developer);
        self
    }
}

/// An Edit menu row that runs `action` on Cmd and `key`.
fn edit_item(label: &str, action: EditAction, key: char) -> MenuItem {
    MenuItem::command(
        label,
        ChromeCommand::Edit(action),
        Some(Shortcut::primary(ShortcutKey::Letter(key))),
    )
}

/// The app's menu for `os`, with checks from `settings`. `system_accent` is
/// the preset the OS accent snaps to, shown as "System (Blue)".
pub fn app_menu(settings: &Settings, system_accent: Accent, os: Os, app_name: &str) -> Menu {
    let settings_item = MenuItem::command(
        "Settings…",
        ChromeCommand::OpenSettings,
        Some(Shortcut::primary(ShortcutKey::Comma)),
    );
    let close_item = MenuItem::command(
        "Close Window",
        ChromeCommand::Window(WindowAction::Close),
        Some(Shortcut::primary(ShortcutKey::Letter('w'))),
    );

    match os {
        Os::Mac => Menu {
            items: vec![
                MenuItem::submenu(
                    app_name,
                    vec![
                        MenuItem::Native(NativeItem::About),
                        MenuItem::Separator,
                        settings_item,
                        MenuItem::Separator,
                        MenuItem::Native(NativeItem::Services),
                        MenuItem::Separator,
                        MenuItem::Native(NativeItem::Hide),
                        MenuItem::Native(NativeItem::HideOthers),
                        MenuItem::Native(NativeItem::ShowAll),
                        MenuItem::Separator,
                        MenuItem::Native(NativeItem::Quit),
                    ],
                ),
                MenuItem::submenu(
                    "Edit",
                    vec![
                        MenuItem::Native(NativeItem::Undo),
                        MenuItem::Native(NativeItem::Redo),
                        MenuItem::Separator,
                        // The browser's own commands, not AppKit's: the
                        // menu takes these keys before the window, and no
                        // AppKit responder edits the engine's fields.
                        edit_item("Cut", EditAction::Cut, 'x'),
                        edit_item("Copy", EditAction::Copy, 'c'),
                        edit_item("Paste", EditAction::Paste, 'v'),
                        edit_item("Select All", EditAction::SelectAll, 'a'),
                    ],
                ),
                MenuItem::submenu("View", view_items(settings, system_accent)),
                MenuItem::submenu(
                    "Window",
                    vec![
                        MenuItem::command(
                            "Minimize",
                            ChromeCommand::Window(WindowAction::Minimize),
                            Some(Shortcut::primary(ShortcutKey::Letter('m'))),
                        ),
                        MenuItem::command(
                            "Zoom",
                            ChromeCommand::Window(WindowAction::Maximize),
                            None,
                        ),
                        MenuItem::Separator,
                        close_item,
                    ],
                ),
            ],
        },
        Os::Windows | Os::Linux => {
            let mut items = view_items(settings, system_accent);
            items.extend([
                MenuItem::Separator,
                settings_item,
                MenuItem::Separator,
                close_item,
            ]);
            Menu { items }
        }
    }
}

/// Appearance, Accent and Interface Size (spec 0003, interface surface).
fn view_items(settings: &Settings, system_accent: Accent) -> Vec<MenuItem> {
    let themes = Theme::ALL
        .into_iter()
        .map(|theme| {
            MenuItem::radio(
                theme.label(),
                ChromeCommand::SetTheme(theme),
                settings.theme == theme,
            )
        })
        .collect();
    let accents = AccentSetting::all()
        .map(|accent| {
            MenuItem::radio(
                &accent.label(system_accent),
                ChromeCommand::SetAccent(accent),
                settings.accent == accent,
            )
        })
        .collect();
    let sizes = vec![
        MenuItem::command(
            "Bigger",
            ChromeCommand::Zoom(ZoomStep::In),
            Some(Shortcut::primary_alt(ShortcutKey::Equal)),
        ),
        MenuItem::command(
            "Smaller",
            ChromeCommand::Zoom(ZoomStep::Out),
            Some(Shortcut::primary_alt(ShortcutKey::Minus)),
        ),
        MenuItem::command(
            "Default Size",
            ChromeCommand::Zoom(ZoomStep::Reset),
            Some(Shortcut::primary_alt(ShortcutKey::Digit0)),
        ),
    ];
    vec![
        MenuItem::submenu("Appearance", themes),
        MenuItem::submenu("Accent", accents),
        MenuItem::submenu("Interface Size", sizes),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_labels_follow_the_os() {
        let zoom_in = Shortcut::primary_alt(ShortcutKey::Equal);
        assert_eq!(zoom_in.label(Os::Mac), "⌥⌘=");
        assert_eq!(zoom_in.label(Os::Linux), "Ctrl+Alt+=");
        let close = Shortcut::primary(ShortcutKey::Letter('w'));
        assert_eq!(close.label(Os::Windows), "Ctrl+W");
    }

    fn checked_labels(items: &[MenuItem]) -> Vec<String> {
        items
            .iter()
            .flat_map(|item| match item {
                MenuItem::Command {
                    label,
                    checked: Some(true),
                    ..
                } => vec![label.clone()],
                MenuItem::Submenu { items, .. } => checked_labels(items),
                _ => vec![],
            })
            .collect()
    }

    #[test]
    fn checks_follow_settings() {
        let settings = Settings {
            theme: Theme::Dark,
            ..Settings::default()
        };
        let menu = app_menu(&settings, Accent::Green, Os::Linux, "Browser");
        assert_eq!(checked_labels(&menu.items), ["Dark", "System (Green)"]);
    }

    #[test]
    fn mac_edit_rows_are_the_browsers_own_commands() {
        // Spec 0008, AC-15: AppKit's own Cut, Copy, Paste and Select All
        // take Cmd+X, C, V and A before the window and edit nothing here.
        let menu = app_menu(&Settings::default(), Accent::Blue, Os::Mac, "Browser");
        let edit = menu
            .items
            .iter()
            .find_map(|item| match item {
                MenuItem::Submenu { label, items } if label == "Edit" => Some(items),
                _ => None,
            })
            .expect("an Edit menu");
        let rows: Vec<(ChromeCommand, Option<Shortcut>)> = edit
            .iter()
            .filter_map(|item| match item {
                MenuItem::Command {
                    command, shortcut, ..
                } => Some((*command, *shortcut)),
                _ => None,
            })
            .collect();
        let expected = [
            (EditAction::Cut, 'x'),
            (EditAction::Copy, 'c'),
            (EditAction::Paste, 'v'),
            (EditAction::SelectAll, 'a'),
        ]
        .map(|(action, key)| {
            (
                ChromeCommand::Edit(action),
                Some(Shortcut::primary(ShortcutKey::Letter(key))),
            )
        });
        assert_eq!(rows, expected);
    }

    #[test]
    fn mac_menu_bar_has_app_edit_view_window() {
        let menu = app_menu(&Settings::default(), Accent::Blue, Os::Mac, "Browser");
        let titles: Vec<&str> = menu
            .items
            .iter()
            .filter_map(|item| match item {
                MenuItem::Submenu { label, .. } => Some(label.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(titles, ["Browser", "Edit", "View", "Window"]);
    }

    #[test]
    fn developer_menu_comes_last_with_its_shortcuts() {
        for os in [Os::Mac, Os::Windows, Os::Linux] {
            let menu =
                app_menu(&Settings::default(), Accent::Blue, os, "Browser").with_developer(os);
            assert!(matches!(
                menu.items.last(),
                Some(MenuItem::Submenu { label, .. }) if label == "Developer"
            ));
            assert_eq!(
                menu.command_for(Shortcut::primary_alt(ShortcutKey::Letter('o'))),
                Some(ChromeCommand::DevOpenFile)
            );
            assert_eq!(
                menu.command_for(Shortcut::primary_alt(ShortcutKey::Letter('s'))),
                Some(ChromeCommand::DevSaveFile)
            );
        }
    }

    #[test]
    fn release_menu_has_no_dev_commands() {
        for os in [Os::Mac, Os::Windows, Os::Linux] {
            let menu = app_menu(&Settings::default(), Accent::Blue, os, "Browser");
            assert_eq!(
                menu.command_for(Shortcut::primary_alt(ShortcutKey::Letter('o'))),
                None
            );
        }
    }

    #[test]
    fn shortcuts_find_their_commands() {
        for os in [Os::Mac, Os::Windows, Os::Linux] {
            let menu = app_menu(&Settings::default(), Accent::Blue, os, "Browser");
            assert_eq!(
                menu.command_for(Shortcut::primary(ShortcutKey::Comma)),
                Some(ChromeCommand::OpenSettings)
            );
            assert_eq!(
                menu.command_for(Shortcut::primary_alt(ShortcutKey::Equal)),
                Some(ChromeCommand::Zoom(ZoomStep::In))
            );
            assert_eq!(
                menu.command_for(Shortcut::primary_alt(ShortcutKey::Digit0)),
                Some(ChromeCommand::Zoom(ZoomStep::Reset))
            );
            assert_eq!(
                menu.command_for(Shortcut::primary(ShortcutKey::Equal)),
                None
            );
        }
    }
}
