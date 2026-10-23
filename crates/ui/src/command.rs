//! Chrome commands: what `data-command`, native menu items and shortcuts
//! ask for (spec 0003, interface surface). Parsing is pure; `embed` carries
//! out the effect.

use std::str::FromStr;

use crate::ChromeError;
use crate::address::{AddressStyle, EngineKind};
use crate::settings::{AccentSetting, Settings, Theme};

/// Which way to move the interface size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoomStep {
    /// One step bigger.
    In,
    /// One step smaller.
    Out,
    /// Back to 100%.
    Reset,
}

impl ZoomStep {
    const fn as_str(self) -> &'static str {
        match self {
            Self::In => "in",
            Self::Out => "out",
            Self::Reset => "reset",
        }
    }
}

impl FromStr for ZoomStep {
    type Err = ChromeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        [Self::In, Self::Out, Self::Reset]
            .into_iter()
            .find(|z| z.as_str() == s)
            .ok_or_else(|| ChromeError::UnknownValue {
                name: "ui.zoom",
                value: s.to_owned(),
            })
    }
}

/// A window action from a caption button or menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowAction {
    /// Minimize the window.
    Minimize,
    /// Maximize it, or restore it when maximized.
    Maximize,
    /// Close it.
    Close,
}

/// A typed chrome command. The content process never sends one
/// (spec 0003, security model).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromeCommand {
    /// `appearance.theme`: set and save the theme.
    SetTheme(Theme),
    /// `appearance.accent`: set and save the accent.
    SetAccent(AccentSetting),
    /// `ui.zoom`: step the interface size and save it.
    Zoom(ZoomStep),
    /// `settings.open`: open or focus the settings window.
    OpenSettings,
    /// `window.minimize`, `window.maximize`, `window.close`.
    Window(WindowAction),
    /// `menu.app`: open the app menu (Windows, Linux).
    AppMenu,
    /// `dev.open_file`: try the open file dialog (dev builds).
    DevOpenFile,
    /// `dev.save_file`: try the save file dialog (dev builds).
    DevSaveFile,
    /// `nav.back`: go back one entry (spec 0008, AC-1).
    Back,
    /// `nav.forward`: go forward one entry.
    Forward,
    /// `nav.reload`: load the current entry again.
    Reload,
    /// `nav.stop`: stop the load in progress (spec 0008, AC-26). The
    /// reload button carries it while a load runs (AC-25).
    Stop,
    /// `address.open`: focus the address field, or open the palette
    /// (spec 0008, AC-5, AC-6).
    OpenAddress,
    /// `address.go`: load what the address holds, as Enter would (the
    /// intent row, AC-7).
    AddressGo,
    /// `settings.address_style`: the palette or the inline field (spec
    /// 0008, AC-32).
    SetAddressStyle(AddressStyle),
    /// `settings.search_engine`: a preset, or Custom, which only shows
    /// its URL field until a valid template is saved (AC-32).
    SetSearchEngine(EngineKind),
    /// `settings.custom_search`: Enter in the Custom field; the host reads
    /// and checks the field's text (AC-32).
    SaveCustomSearch,
}

impl ChromeCommand {
    /// The `data-command` name and `data-value`, the inverse of
    /// [`parse_command`].
    pub fn parts(self) -> (&'static str, Option<&'static str>) {
        match self {
            Self::SetTheme(theme) => ("appearance.theme", Some(theme.as_str())),
            Self::SetAccent(accent) => ("appearance.accent", Some(accent.as_str())),
            Self::Zoom(step) => ("ui.zoom", Some(step.as_str())),
            Self::OpenSettings => ("settings.open", None),
            Self::Window(WindowAction::Minimize) => ("window.minimize", None),
            Self::Window(WindowAction::Maximize) => ("window.maximize", None),
            Self::Window(WindowAction::Close) => ("window.close", None),
            Self::AppMenu => ("menu.app", None),
            Self::DevOpenFile => ("dev.open_file", None),
            Self::DevSaveFile => ("dev.save_file", None),
            Self::Back => ("nav.back", None),
            Self::Forward => ("nav.forward", None),
            Self::Reload => ("nav.reload", None),
            Self::Stop => ("nav.stop", None),
            Self::OpenAddress => ("address.open", None),
            Self::AddressGo => ("address.go", None),
            Self::SetAddressStyle(style) => ("settings.address_style", Some(style.as_str())),
            Self::SetSearchEngine(kind) => ("settings.search_engine", Some(kind.as_str())),
            Self::SaveCustomSearch => ("settings.custom_search", None),
        }
    }

    /// A stable string id, `name` or `name=value`, for native menu items.
    pub fn id(self) -> String {
        match self.parts() {
            (name, Some(value)) => format!("{name}={value}"),
            (name, None) => name.to_owned(),
        }
    }

    /// Parses an id made by [`ChromeCommand::id`].
    pub fn from_id(id: &str) -> Result<Self, ChromeError> {
        match id.split_once('=') {
            Some((name, value)) => parse_command(name, Some(value)),
            None => parse_command(id, None),
        }
    }

    /// The settings after this command, or `None` when it does not change
    /// settings (or changes nothing, like zooming in at 200%).
    pub fn apply(self, settings: &Settings) -> Option<Settings> {
        let mut next = settings.clone();
        match self {
            Self::SetTheme(theme) => next.theme = theme,
            Self::SetAccent(accent) => next.accent = accent,
            Self::Zoom(step) => next.ui_zoom = settings.ui_zoom.step(step),
            Self::SetAddressStyle(style) => next.address_style = style,
            // Custom changes nothing until its template is saved.
            Self::SetSearchEngine(kind) => next.search = kind.preset()?,
            _ => return None,
        }
        (next != *settings).then_some(next)
    }
}

/// Parses a `data-command` name and its optional `data-value`.
pub fn parse_command(name: &str, value: Option<&str>) -> Result<ChromeCommand, ChromeError> {
    let command = match (name, value) {
        ("appearance.theme", Some(v)) => ChromeCommand::SetTheme(v.parse()?),
        ("appearance.accent", Some(v)) => ChromeCommand::SetAccent(v.parse()?),
        ("ui.zoom", Some(v)) => ChromeCommand::Zoom(v.parse()?),
        ("settings.open", None) => ChromeCommand::OpenSettings,
        ("window.minimize", None) => ChromeCommand::Window(WindowAction::Minimize),
        ("window.maximize", None) => ChromeCommand::Window(WindowAction::Maximize),
        ("window.close", None) => ChromeCommand::Window(WindowAction::Close),
        ("menu.app", None) => ChromeCommand::AppMenu,
        ("dev.open_file", None) => ChromeCommand::DevOpenFile,
        ("dev.save_file", None) => ChromeCommand::DevSaveFile,
        ("nav.back", None) => ChromeCommand::Back,
        ("nav.forward", None) => ChromeCommand::Forward,
        ("nav.reload", None) => ChromeCommand::Reload,
        ("nav.stop", None) => ChromeCommand::Stop,
        ("address.open", None) => ChromeCommand::OpenAddress,
        ("address.go", None) => ChromeCommand::AddressGo,
        ("settings.address_style", Some(v)) => ChromeCommand::SetAddressStyle(v.parse()?),
        ("settings.search_engine", Some(v)) => ChromeCommand::SetSearchEngine(v.parse()?),
        ("settings.custom_search", None) => ChromeCommand::SaveCustomSearch,
        _ => return Err(ChromeError::UnknownCommand(name.to_owned())),
    };
    Ok(command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::UiZoom;
    use crate::theme::Accent;

    #[test]
    fn parses_known_commands() {
        assert_eq!(
            parse_command("appearance.theme", Some("dark")).unwrap(),
            ChromeCommand::SetTheme(Theme::Dark)
        );
        assert_eq!(
            parse_command("appearance.accent", Some("green")).unwrap(),
            ChromeCommand::SetAccent(AccentSetting::Preset(Accent::Green))
        );
        assert_eq!(
            parse_command("ui.zoom", Some("in")).unwrap(),
            ChromeCommand::Zoom(ZoomStep::In)
        );
        assert_eq!(
            parse_command("settings.open", None).unwrap(),
            ChromeCommand::OpenSettings
        );
    }

    #[test]
    fn unknown_command_is_an_error() {
        let err = parse_command("appearance.font", Some("serif")).unwrap_err();
        assert!(matches!(err, ChromeError::UnknownCommand(name) if name == "appearance.font"));
    }

    #[test]
    fn unknown_value_is_an_error() {
        let err = parse_command("appearance.theme", Some("purple")).unwrap_err();
        assert!(matches!(
            err,
            ChromeError::UnknownValue {
                name: "appearance.theme",
                ..
            }
        ));
    }

    #[test]
    fn a_missing_or_extra_value_is_unknown() {
        assert!(parse_command("appearance.theme", None).is_err());
        assert!(parse_command("settings.open", Some("x")).is_err());
    }

    #[test]
    fn ids_round_trip() {
        let all = Theme::ALL
            .into_iter()
            .map(ChromeCommand::SetTheme)
            .chain(AccentSetting::all().map(ChromeCommand::SetAccent))
            .chain([ZoomStep::In, ZoomStep::Out, ZoomStep::Reset].map(ChromeCommand::Zoom))
            .chain([
                ChromeCommand::OpenSettings,
                ChromeCommand::Window(WindowAction::Minimize),
                ChromeCommand::Window(WindowAction::Maximize),
                ChromeCommand::Window(WindowAction::Close),
                ChromeCommand::AppMenu,
                ChromeCommand::DevOpenFile,
                ChromeCommand::DevSaveFile,
                ChromeCommand::Back,
                ChromeCommand::Forward,
                ChromeCommand::Reload,
                ChromeCommand::Stop,
                ChromeCommand::OpenAddress,
                ChromeCommand::AddressGo,
                ChromeCommand::SetAddressStyle(AddressStyle::Inline),
                ChromeCommand::SaveCustomSearch,
            ])
            .chain(EngineKind::ALL.map(ChromeCommand::SetSearchEngine));
        for command in all {
            assert_eq!(ChromeCommand::from_id(&command.id()).unwrap(), command);
        }
    }

    #[test]
    fn apply_changes_settings_only_when_something_changes() {
        let settings = Settings::default();
        let dark = ChromeCommand::SetTheme(Theme::Dark)
            .apply(&settings)
            .unwrap();
        assert_eq!(dark.theme, Theme::Dark);
        assert_eq!(
            ChromeCommand::SetTheme(Theme::System).apply(&settings),
            None
        );
        let max = Settings {
            ui_zoom: UiZoom::nearest(200),
            ..settings.clone()
        };
        assert_eq!(ChromeCommand::Zoom(ZoomStep::In).apply(&max), None);
        assert_eq!(ChromeCommand::OpenSettings.apply(&settings), None);
    }
}
