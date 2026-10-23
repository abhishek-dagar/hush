//! The resolved chrome theme: settings plus OS appearance in, the values the
//! chrome is drawn with out (spec 0003, `ChromeTheme`).

use crate::appearance::{Os, OsAppearance};
use crate::color::Rgba;
use crate::settings::{AccentSetting, Settings, Theme};

/// An accent preset. Every value the chrome can show is one of these, so
/// each is contrast tested ahead of time (spec 0003, Accent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accent {
    /// Blue, the default.
    Blue,
    /// Purple.
    Purple,
    /// Pink.
    Pink,
    /// Red.
    Red,
    /// Orange.
    Orange,
    /// Yellow.
    Yellow,
    /// Green.
    Green,
    /// Graphite, for low chroma OS accents.
    Graphite,
}

impl Accent {
    /// Every preset, in menu order.
    pub const ALL: [Self; 8] = [
        Self::Blue,
        Self::Purple,
        Self::Pink,
        Self::Red,
        Self::Orange,
        Self::Yellow,
        Self::Green,
        Self::Graphite,
    ];

    /// The settings, command and `data-accent` value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Blue => "blue",
            Self::Purple => "purple",
            Self::Pink => "pink",
            Self::Red => "red",
            Self::Orange => "orange",
            Self::Yellow => "yellow",
            Self::Green => "green",
            Self::Graphite => "graphite",
        }
    }

    /// The label shown in menus and the settings window.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Blue => "Blue",
            Self::Purple => "Purple",
            Self::Pink => "Pink",
            Self::Red => "Red",
            Self::Orange => "Orange",
            Self::Yellow => "Yellow",
            Self::Green => "Green",
            Self::Graphite => "Graphite",
        }
    }

    /// The OKLCH hue angle `snap` compares against: the hue of the preset's
    /// light value in `tokens.css` (a test keeps the two in step).
    pub const fn hue(self) -> f32 {
        match self {
            Self::Blue => 258.3,
            Self::Purple => 298.9,
            Self::Pink => 5.6,
            Self::Red => 26.7,
            Self::Orange => 49.0,
            Self::Yellow => 87.7,
            Self::Green => 147.2,
            Self::Graphite => 286.1,
        }
    }
}

/// Light or dark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    /// Dark text on light surfaces.
    Light,
    /// Light text on dark surfaces.
    Dark,
}

impl Scheme {
    /// The `data-theme` value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

/// Normal or high contrast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Contrast {
    /// The normal palette.
    Normal,
    /// The high contrast palette.
    More,
}

impl Contrast {
    /// The `data-contrast` value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::More => "more",
        }
    }
}

/// Whether motion tokens keep their durations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    /// Durations as written.
    Full,
    /// Every duration is zero.
    Reduce,
}

impl Motion {
    /// The `data-motion` value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Reduce => "reduce",
        }
    }
}

/// What the chrome is drawn with, derived and never saved.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChromeTheme {
    /// The OS, for `data-os`.
    pub os: Os,
    /// Light or dark.
    pub scheme: Scheme,
    /// Normal or high contrast.
    pub contrast: Contrast,
    /// The accent preset in use.
    pub accent: Accent,
    /// Full or reduced motion.
    pub motion: Motion,
    /// The OS's base UI text size in px, before the OS text scale.
    pub base_font_px: f32,
    /// The chrome root font size in px: base times the OS text scale.
    pub root_font_px: f32,
    /// The chrome layout zoom, e.g. 1.25.
    pub zoom: f32,
}

impl ChromeTheme {
    /// The root attributes that select tokens, as (name, value) pairs:
    /// `data-os`, `data-theme`, `data-contrast`, `data-motion`, `data-accent`.
    pub fn attributes(&self) -> [(&'static str, &'static str); 5] {
        [
            ("data-os", self.os.as_str()),
            ("data-theme", self.scheme.as_str()),
            ("data-contrast", self.contrast.as_str()),
            ("data-motion", self.motion.as_str()),
            ("data-accent", self.accent.as_str()),
        ]
    }
}

/// Derives the theme. Pure and total: every input pair gives a valid theme.
pub fn resolve(settings: &Settings, os: &OsAppearance) -> ChromeTheme {
    let scheme = match settings.theme {
        Theme::Light => Scheme::Light,
        Theme::Dark => Scheme::Dark,
        Theme::System if os.dark => Scheme::Dark,
        Theme::System => Scheme::Light,
    };
    let accent = match settings.accent {
        AccentSetting::Preset(accent) => accent,
        AccentSetting::System => os.accent.map_or(Accent::Blue, snap),
    };
    let base_font_px = match os.os {
        Os::Mac => 13.0,
        Os::Windows | Os::Linux => 14.0,
    };
    ChromeTheme {
        os: os.os,
        scheme,
        contrast: if os.high_contrast {
            Contrast::More
        } else {
            Contrast::Normal
        },
        accent,
        motion: if os.reduce_motion {
            Motion::Reduce
        } else {
            Motion::Full
        },
        base_font_px,
        root_font_px: base_font_px * os.text_scale(),
        zoom: settings.ui_zoom.factor(),
    }
}

/// Snaps an OS accent to a preset: chroma under 0.04 is graphite, otherwise
/// the preset with the nearest OKLCH hue.
pub fn snap(color: Rgba) -> Accent {
    const GRAY_CHROMA: f32 = 0.04;
    let (_, chroma, hue) = color.oklch();
    if chroma < GRAY_CHROMA {
        return Accent::Graphite;
    }
    let distance = |accent: Accent| {
        let d = (accent.hue() - hue).abs();
        d.min(360.0 - d)
    };
    let mut best = Accent::Blue;
    for accent in Accent::ALL {
        if accent != Accent::Graphite && distance(accent) < distance(best) {
            best = accent;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::UiZoom;

    fn os(dark: bool) -> OsAppearance {
        let mut appearance = OsAppearance::fallback(Os::Linux);
        appearance.dark = dark;
        appearance
    }

    #[test]
    fn system_theme_follows_the_os() {
        let settings = Settings::default();
        assert_eq!(resolve(&settings, &os(true)).scheme, Scheme::Dark);
        assert_eq!(resolve(&settings, &os(false)).scheme, Scheme::Light);
    }

    #[test]
    fn forced_theme_ignores_the_os() {
        let light = Settings {
            theme: Theme::Light,
            ..Settings::default()
        };
        assert_eq!(resolve(&light, &os(true)).scheme, Scheme::Light);
        let dark = Settings {
            theme: Theme::Dark,
            ..Settings::default()
        };
        assert_eq!(resolve(&dark, &os(false)).scheme, Scheme::Dark);
    }

    #[test]
    fn os_high_contrast_applies_whatever_the_theme() {
        let light = Settings {
            theme: Theme::Light,
            ..Settings::default()
        };
        let mut appearance = os(true);
        appearance.high_contrast = true;
        let theme = resolve(&light, &appearance);
        assert_eq!(theme.scheme, Scheme::Light);
        assert_eq!(theme.contrast, Contrast::More);
    }

    #[test]
    fn accent_snaps_from_the_os_or_falls_back_to_blue() {
        let settings = Settings::default();
        let mut appearance = os(false);
        assert_eq!(resolve(&settings, &appearance).accent, Accent::Blue);
        // GNOME's yellow.
        appearance.accent = Some(Rgba::rgb8(0xE5, 0xA5, 0x0A));
        assert_eq!(resolve(&settings, &appearance).accent, Accent::Yellow);
        let chosen = Settings {
            accent: AccentSetting::Preset(Accent::Green),
            ..Settings::default()
        };
        assert_eq!(resolve(&chosen, &appearance).accent, Accent::Green);
    }

    #[test]
    fn snap_maps_grays_to_graphite_and_hues_to_presets() {
        assert_eq!(snap(Rgba::rgb8(0x8C, 0x8C, 0x8C)), Accent::Graphite);
        // The accent colors macOS reports for each of its accent choices.
        let mac = [
            ((0x00, 0x7A, 0xFF), Accent::Blue),
            ((0xA5, 0x50, 0xA7), Accent::Purple),
            ((0xF7, 0x4F, 0x9E), Accent::Pink),
            ((0xFF, 0x52, 0x57), Accent::Red),
            ((0xF7, 0x82, 0x1B), Accent::Orange),
            ((0xFF, 0xC6, 0x00), Accent::Yellow),
            ((0x62, 0xBA, 0x46), Accent::Green),
            ((0x8C, 0x8C, 0x8C), Accent::Graphite),
        ];
        for ((r, g, b), accent) in mac {
            assert_eq!(snap(Rgba::rgb8(r, g, b)), accent, "#{r:02X}{g:02X}{b:02X}");
        }
    }

    #[test]
    fn font_and_zoom() {
        let settings = Settings {
            ui_zoom: UiZoom::nearest(150),
            ..Settings::default()
        };
        let mut appearance = OsAppearance::fallback(Os::Mac);
        appearance.set_text_scale(1.5);
        let theme = resolve(&settings, &appearance);
        assert_eq!(theme.base_font_px, 13.0);
        assert_eq!(theme.root_font_px, 19.5);
        assert_eq!(theme.zoom, 1.5);
        let windows = resolve(&settings, &OsAppearance::fallback(Os::Windows));
        assert_eq!(windows.root_font_px, 14.0);
    }
}
