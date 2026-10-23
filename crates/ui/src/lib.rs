//! The browser chrome's pure core (spec 0003): settings, the OS appearance
//! model, theme resolution, chrome commands, the menu model, the design
//! tokens, the keyboard model, and the chrome document the engine renders
//! from the embedded assets.
//!
//! Nothing here touches the OS, a file or a window. `embed` reads the OS and
//! `settings.toml`, hands the values in, and carries out the effects
//! (spec 0003, functional core). Only the browser app links this crate.

#![forbid(unsafe_code)]

mod appearance;
mod assets;
mod color;
mod command;
mod document;
mod frame;
mod keyboard;
mod menu;
mod settings;
mod theme;
mod tokens;

pub use appearance::{ButtonLayout, CaptionButton, Os, OsAppearance, Side};
pub use assets::{Asset, ORIGIN, asset};
pub use color::{Rgba, contrast_ratio};
pub use command::{ChromeCommand, WindowAction, ZoomStep, parse_command};
pub use document::{
    CHROME_PAGE, ChromeDocument, ChromeFrame, ChromeOutcome, ChromeSurface, NavView,
};
pub use frame::{FrameLayout, FrameMetrics, Hit, PointF, RectF, ResizeEdge, SizeF};
pub use keyboard::{Composite, Item, Move, NavKey, composite_key};
pub use menu::{Menu, MenuItem, Modifiers, NativeItem, Shortcut, ShortcutKey, app_menu};
pub use settings::{
    AccentSetting, MAX_SETTINGS_BYTES, SettingWarning, Settings, SettingsDocument, Theme, UiZoom,
};
pub use theme::{Accent, ChromeTheme, Contrast, Motion, Scheme, resolve, snap};
pub use tokens::{ResolvedTokens, Tokens};

/// The chrome's design tokens, embedded in the binary: the chrome loads no
/// file or network asset (spec 0003, key invariants).
pub const TOKENS_CSS: &str = include_str!("../assets/tokens.css");

/// Errors from the chrome core.
#[derive(Debug, thiserror::Error)]
pub enum ChromeError {
    /// A `data-command` or menu id names no known command.
    #[error("unknown chrome command `{0}`")]
    UnknownCommand(String),
    /// A known command or setting got a value outside its allowed set.
    #[error("`{value}` is not a valid value for `{name}`")]
    UnknownValue {
        /// The command or setting name.
        name: &'static str,
        /// The value that was given.
        value: String,
    },
    /// `settings.toml` is over the size cap and is treated as unparsable.
    #[error("the settings file is over {MAX_SETTINGS_BYTES} bytes")]
    SettingsTooLarge,
    /// `settings.toml` is not valid TOML.
    #[error("the settings file is not valid TOML")]
    SettingsParse(#[from] toml::de::Error),
    /// `tokens.css` is not in the shape the token reader accepts.
    #[error("tokens.css, line {line}: {message}")]
    TokensSyntax {
        /// The 1 based line of the problem.
        line: usize,
        /// What is wrong.
        message: String,
    },
    /// A token the caller needs is not defined for the resolved theme.
    #[error("token `{0}` is not defined")]
    MissingToken(String),
    /// A chrome document names a file the binary does not embed (the
    /// gallery, in a release build).
    #[error("`{0}` is not an embedded chrome asset")]
    MissingAsset(String),
    /// A token's value is not of the kind the caller asked for.
    #[error("token `{name}` is `{value}`, not a {expected}")]
    BadTokenValue {
        /// The token name.
        name: String,
        /// Its resolved value.
        value: String,
        /// What was expected, e.g. `color` or `px length`.
        expected: &'static str,
    },
}
