//! The user's chrome overrides and their `settings.toml` form
//! (spec 0003, data model). Parsing and writing are pure; `embed` does the
//! file IO.

use std::fmt;
use std::str::FromStr;

use toml::{Table, Value};

use crate::ChromeError;
use crate::address::{AddressStyle, EngineKind, SearchEngine};
use crate::command::ZoomStep;
use crate::theme::Accent;

/// The largest `settings.toml` the browser reads. A bigger file counts as
/// unparsable (spec 0003, AC-11).
pub const MAX_SETTINGS_BYTES: usize = 64 * 1024;

/// The settings file format version this build writes.
const VERSION: i64 = 1;

/// The color mode the user asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    /// Follow the OS.
    #[default]
    System,
    /// Always light.
    Light,
    /// Always dark.
    Dark,
}

impl Theme {
    /// Every theme, in menu order.
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    /// The settings and command value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    /// The label shown in menus.
    pub const fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }
}

impl FromStr for Theme {
    type Err = ChromeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|t| t.as_str() == s)
            .ok_or_else(|| ChromeError::UnknownValue {
                name: "appearance.theme",
                value: s.to_owned(),
            })
    }
}

/// The accent the user asked for: the OS one, or a preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AccentSetting {
    /// The OS accent, snapped to the nearest preset.
    #[default]
    System,
    /// One preset, whatever the OS says.
    Preset(Accent),
}

impl AccentSetting {
    /// Every choice, in menu order: system, then the presets.
    pub fn all() -> impl Iterator<Item = Self> {
        std::iter::once(Self::System).chain(Accent::ALL.into_iter().map(Self::Preset))
    }

    /// The settings and command value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Preset(accent) => accent.as_str(),
        }
    }

    /// The label in menus and the settings window. `system` is the preset
    /// the OS accent snaps to, shown as "System (Blue)".
    pub fn label(self, system: Accent) -> String {
        match self {
            Self::System => format!("System ({})", system.label()),
            Self::Preset(accent) => accent.label().to_owned(),
        }
    }
}

impl FromStr for AccentSetting {
    type Err = ChromeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "system" {
            return Ok(Self::System);
        }
        Accent::ALL
            .into_iter()
            .find(|a| a.as_str() == s)
            .map(Self::Preset)
            .ok_or_else(|| ChromeError::UnknownValue {
                name: "appearance.accent",
                value: s.to_owned(),
            })
    }
}

/// The chrome interface size, always one of [`UiZoom::STEPS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiZoom(u16);

impl UiZoom {
    /// The allowed sizes in percent (spec 0003, AC-6).
    pub const STEPS: [u16; 8] = [80, 90, 100, 110, 125, 150, 175, 200];
    /// 100%.
    pub const DEFAULT: Self = Self(100);

    /// The step nearest to `percent`; the smaller one on a tie.
    pub fn nearest(percent: i64) -> Self {
        let mut best = Self::STEPS[0];
        for step in Self::STEPS {
            if (i64::from(step) - percent).abs() < (i64::from(best) - percent).abs() {
                best = step;
            }
        }
        Self(best)
    }

    /// The size in percent.
    pub const fn percent(self) -> u16 {
        self.0
    }

    /// The layout zoom factor, e.g. 1.25 for 125%.
    pub fn factor(self) -> f32 {
        f32::from(self.0) / 100.0
    }

    /// The next size in `step`'s direction. Stays put at either end.
    pub fn step(self, step: ZoomStep) -> Self {
        let index = Self::STEPS.iter().position(|&s| s == self.0);
        let Some(index) = index else {
            return Self::nearest(i64::from(self.0));
        };
        match step {
            ZoomStep::In => Self(Self::STEPS[(index + 1).min(Self::STEPS.len() - 1)]),
            ZoomStep::Out => Self(Self::STEPS[index.saturating_sub(1)]),
            ZoomStep::Reset => Self::DEFAULT,
        }
    }
}

impl Default for UiZoom {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The user's chrome overrides (spec 0003, `settings.toml`; spec 0008,
/// AC-33).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Settings {
    /// `appearance.theme`.
    pub theme: Theme,
    /// `appearance.accent`.
    pub accent: AccentSetting,
    /// `appearance.ui_zoom`.
    pub ui_zoom: UiZoom,
    /// `navigation.address_style`.
    pub address_style: AddressStyle,
    /// `search.engine`, with `search.custom_url` for Custom.
    pub search: SearchEngine,
}

/// A setting that was missing a valid value and fell back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingWarning {
    /// The dotted key, e.g. `appearance.theme`.
    pub key: &'static str,
    /// What was wrong and what was used instead.
    pub message: String,
}

impl fmt::Display for SettingWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.key, self.message)
    }
}

/// `settings.toml` as a TOML table. Keys this build does not know are kept,
/// so a rewrite never drops another version's settings (spec 0003, AC-11).
/// Comments are not kept.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SettingsDocument {
    table: Table,
}

impl SettingsDocument {
    /// Parses the file's text. Over [`MAX_SETTINGS_BYTES`] or not TOML is an
    /// error; the caller backs the file up and starts on defaults.
    pub fn parse(text: &str) -> Result<Self, ChromeError> {
        if text.len() > MAX_SETTINGS_BYTES {
            return Err(ChromeError::SettingsTooLarge);
        }
        Ok(Self {
            table: text.parse::<Table>()?,
        })
    }

    /// The settings the file holds. Each invalid or wrongly typed value
    /// falls back on its own, with a warning; the rest still apply.
    pub fn settings(&self) -> (Settings, Vec<SettingWarning>) {
        let mut warnings = Vec::new();
        let mut settings = Settings::default();

        match self.table.get("version") {
            None | Some(Value::Integer(VERSION)) => {}
            Some(other) => warnings.push(SettingWarning {
                key: "version",
                message: format!("`{other}` is not a known version, read as version {VERSION}"),
            }),
        }

        self.read_navigation(&mut settings, &mut warnings);
        let appearance = match self.table.get("appearance") {
            None => return (settings, warnings),
            Some(Value::Table(table)) => table,
            Some(other) => {
                warnings.push(SettingWarning {
                    key: "appearance",
                    message: format!("`{other}` is not a table, using defaults"),
                });
                return (settings, warnings);
            }
        };

        if let Some(value) = appearance.get("theme") {
            match value.as_str().map(str::parse::<Theme>) {
                Some(Ok(theme)) => settings.theme = theme,
                _ => warnings.push(reset("appearance.theme", value, Theme::default().as_str())),
            }
        }
        if let Some(value) = appearance.get("accent") {
            match value.as_str().map(str::parse::<AccentSetting>) {
                Some(Ok(accent)) => settings.accent = accent,
                _ => warnings.push(reset(
                    "appearance.accent",
                    value,
                    AccentSetting::default().as_str(),
                )),
            }
        }
        if let Some(value) = appearance.get("ui_zoom") {
            match value.as_integer() {
                Some(percent) => {
                    let zoom = UiZoom::nearest(percent);
                    if i64::from(zoom.percent()) != percent {
                        warnings.push(SettingWarning {
                            key: "appearance.ui_zoom",
                            message: format!("{percent} is not a step, using {}", zoom.percent()),
                        });
                    }
                    settings.ui_zoom = zoom;
                }
                None => warnings.push(reset(
                    "appearance.ui_zoom",
                    value,
                    &UiZoom::DEFAULT.percent().to_string(),
                )),
            }
        }
        (settings, warnings)
    }

    /// The address style and the search engine (spec 0008, AC-33). An
    /// invalid value is reset to its default with a warning; Custom with a
    /// missing or invalid `search.custom_url` falls back to DuckDuckGo.
    fn read_navigation(&self, settings: &mut Settings, warnings: &mut Vec<SettingWarning>) {
        let table =
            |name: &'static str, warnings: &mut Vec<SettingWarning>| match self.table.get(name) {
                None => None,
                Some(Value::Table(table)) => Some(table.clone()),
                Some(other) => {
                    warnings.push(SettingWarning {
                        key: name,
                        message: format!("`{other}` is not a table, using defaults"),
                    });
                    None
                }
            };
        if let Some(navigation) = table("navigation", warnings)
            && let Some(value) = navigation.get("address_style")
        {
            match value.as_str().map(str::parse::<AddressStyle>) {
                Some(Ok(style)) => settings.address_style = style,
                _ => warnings.push(reset(
                    "navigation.address_style",
                    value,
                    AddressStyle::default().as_str(),
                )),
            }
        }
        let Some(search) = table("search", warnings) else {
            return;
        };
        let Some(value) = search.get("engine") else {
            return;
        };
        let default = SearchEngine::default();
        match value.as_str().map(str::parse::<EngineKind>) {
            Some(Ok(EngineKind::Custom)) => {
                let custom = search
                    .get("custom_url")
                    .and_then(Value::as_str)
                    .map(SearchEngine::custom);
                match custom {
                    Some(Ok(engine)) => settings.search = engine,
                    _ => warnings.push(SettingWarning {
                        key: "search.custom_url",
                        message: format!(
                            "missing or not an http or https address with %s, using `{}`",
                            default.as_str()
                        ),
                    }),
                }
            }
            Some(Ok(kind)) => settings.search = kind.preset().unwrap_or_default(),
            _ => warnings.push(reset("search.engine", value, default.as_str())),
        }
    }

    /// Writes `settings` into the table, keeping every other key. A non
    /// table `appearance` value is replaced.
    pub fn store(&mut self, settings: &Settings) {
        self.table.insert("version".into(), Value::Integer(VERSION));
        let appearance = self
            .table
            .entry("appearance")
            .or_insert_with(|| Value::Table(Table::new()));
        if !appearance.is_table() {
            *appearance = Value::Table(Table::new());
        }
        if let Value::Table(appearance) = appearance {
            appearance.insert("theme".into(), settings.theme.as_str().into());
            appearance.insert("accent".into(), settings.accent.as_str().into());
            appearance.insert(
                "ui_zoom".into(),
                Value::Integer(i64::from(settings.ui_zoom.percent())),
            );
        }
        if let Value::Table(navigation) = section(&mut self.table, "navigation") {
            navigation.insert(
                "address_style".into(),
                settings.address_style.as_str().into(),
            );
        }
        if let Value::Table(search) = section(&mut self.table, "search") {
            search.insert("engine".into(), settings.search.as_str().into());
            // A preset keeps the last custom template, so Custom can come
            // back to it.
            if let SearchEngine::Custom(template) = &settings.search {
                search.insert("custom_url".into(), template.as_str().into());
            }
        }
    }

    /// The file text to write.
    pub fn to_toml(&self) -> String {
        self.table.to_string()
    }
}

/// The `name` table of `table`, made one if it is missing or not a table.
fn section<'t>(table: &'t mut Table, name: &str) -> &'t mut Value {
    let value = table
        .entry(name)
        .or_insert_with(|| Value::Table(Table::new()));
    if !value.is_table() {
        *value = Value::Table(Table::new());
    }
    value
}

fn reset(key: &'static str, value: &Value, default: &str) -> SettingWarning {
    SettingWarning {
        key,
        message: format!("`{value}` is not allowed, using `{default}`"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &str) -> (Settings, Vec<SettingWarning>) {
        SettingsDocument::parse(text).unwrap().settings()
    }

    #[test]
    fn empty_file_gives_defaults() {
        let (settings, warnings) = read("");
        assert_eq!(settings, Settings::default());
        assert!(warnings.is_empty());
    }

    #[test]
    fn reads_every_key() {
        let (settings, warnings) = read(
            "version = 1\n[appearance]\ntheme = \"dark\"\naccent = \"green\"\nui_zoom = 150\n",
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(settings.theme, Theme::Dark);
        assert_eq!(settings.accent, AccentSetting::Preset(Accent::Green));
        assert_eq!(settings.ui_zoom.percent(), 150);
    }

    #[test]
    fn one_bad_value_resets_only_that_key() {
        let (settings, warnings) =
            read("[appearance]\ntheme = \"purple\"\naccent = \"pink\"\nui_zoom = 125\n");
        assert_eq!(settings.theme, Theme::System);
        assert_eq!(settings.accent, AccentSetting::Preset(Accent::Pink));
        assert_eq!(settings.ui_zoom.percent(), 125);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].key, "appearance.theme");
    }

    #[test]
    fn wrong_types_reset_with_a_warning() {
        let (settings, warnings) =
            read("[appearance]\ntheme = 3\naccent = true\nui_zoom = \"big\"\n");
        assert_eq!(settings, Settings::default());
        assert_eq!(warnings.len(), 3);
    }

    #[test]
    fn off_step_zoom_snaps_to_the_nearest_step() {
        let (settings, warnings) = read("[appearance]\nui_zoom = 130\n");
        assert_eq!(settings.ui_zoom.percent(), 125);
        assert_eq!(warnings.len(), 1);
        assert_eq!(
            read("[appearance]\nui_zoom = 5000\n").0.ui_zoom.percent(),
            200
        );
        assert_eq!(read("[appearance]\nui_zoom = -3\n").0.ui_zoom.percent(), 80);
    }

    #[test]
    fn unknown_version_is_read_as_version_one() {
        let (settings, warnings) = read("version = 7\n[appearance]\ntheme = \"light\"\n");
        assert_eq!(settings.theme, Theme::Light);
        assert_eq!(warnings[0].key, "version");
    }

    #[test]
    fn broken_toml_is_an_error() {
        assert!(matches!(
            SettingsDocument::parse("appearance = ["),
            Err(ChromeError::SettingsParse(_))
        ));
    }

    #[test]
    fn a_file_over_64_kb_is_an_error() {
        let big = format!("# {}\n", "x".repeat(MAX_SETTINGS_BYTES));
        assert!(matches!(
            SettingsDocument::parse(&big),
            Err(ChromeError::SettingsTooLarge)
        ));
    }

    #[test]
    fn rewrite_keeps_unknown_keys() {
        let mut doc = SettingsDocument::parse(
            "[appearance]\ntheme = \"light\"\nfuture_key = 1\n[tabs]\nlayout = \"vertical\"\n",
        )
        .unwrap();
        let settings = Settings {
            theme: Theme::Dark,
            ..Settings::default()
        };
        doc.store(&settings);

        let again = SettingsDocument::parse(&doc.to_toml()).unwrap();
        assert_eq!(again.settings().0, settings);
        let text = again.to_toml();
        assert!(text.contains("future_key = 1"), "{text}");
        assert!(text.contains("layout = \"vertical\""), "{text}");
        assert!(text.contains("version = 1"), "{text}");
    }

    #[test]
    fn store_replaces_a_non_table_appearance() {
        let mut doc = SettingsDocument::parse("appearance = 3\n").unwrap();
        doc.store(&Settings::default());
        let (settings, warnings) = SettingsDocument::parse(&doc.to_toml()).unwrap().settings();
        assert_eq!(settings, Settings::default());
        assert!(warnings.is_empty());
    }

    #[test]
    fn zoom_steps_stop_at_the_ends() {
        let min = UiZoom::nearest(80);
        assert_eq!(min.step(ZoomStep::Out), min);
        let max = UiZoom::nearest(200);
        assert_eq!(max.step(ZoomStep::In), max);
        assert_eq!(UiZoom::DEFAULT.step(ZoomStep::In).percent(), 110);
        assert_eq!(max.step(ZoomStep::Reset), UiZoom::DEFAULT);
    }

    #[test]
    fn the_navigation_keys_read_and_fall_back() {
        // Spec 0008, AC-33
        let (settings, warnings) =
            read("[navigation]\naddress_style = \"inline\"\n[search]\nengine = \"brave\"\n");
        assert_eq!(settings.address_style, AddressStyle::Inline);
        assert_eq!(settings.search, SearchEngine::Brave);
        assert!(warnings.is_empty());

        let (settings, warnings) =
            read("[navigation]\naddress_style = \"floating\"\n[search]\nengine = \"yahoo\"\n");
        assert_eq!(settings.address_style, AddressStyle::Palette);
        assert_eq!(settings.search, SearchEngine::DuckDuckGo);
        let keys: Vec<_> = warnings.iter().map(|w| w.key).collect();
        assert_eq!(keys, ["navigation.address_style", "search.engine"]);

        let (settings, warnings) =
            read("[search]\nengine = \"custom\"\ncustom_url = \"https://kagi.com/search?q=%s\"\n");
        assert_eq!(
            settings.search,
            SearchEngine::custom("https://kagi.com/search?q=%s").unwrap()
        );
        assert!(warnings.is_empty());
        for bad in ["", "custom_url = \"ftp://x/%s\"\n"] {
            let (settings, warnings) = read(&format!("[search]\nengine = \"custom\"\n{bad}"));
            assert_eq!(settings.search, SearchEngine::DuckDuckGo, "{bad}");
            assert_eq!(warnings[0].key, "search.custom_url");
        }
    }

    #[test]
    fn the_navigation_keys_round_trip_and_a_preset_keeps_the_template() {
        // Spec 0008, AC-33
        let mut document = SettingsDocument::parse("").unwrap();
        let custom = Settings {
            address_style: AddressStyle::Inline,
            search: SearchEngine::custom("https://kagi.com/search?q=%s").unwrap(),
            ..Settings::default()
        };
        document.store(&custom);
        let reread = SettingsDocument::parse(&document.to_toml()).unwrap();
        assert_eq!(reread.settings().0, custom);
        document.store(&Settings {
            search: SearchEngine::Google,
            ..custom.clone()
        });
        let text = document.to_toml();
        assert!(text.contains("engine = \"google\""), "{text}");
        assert!(text.contains("kagi.com"), "the template stays: {text}");
    }
}
