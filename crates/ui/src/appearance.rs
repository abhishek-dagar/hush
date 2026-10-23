//! What the OS says about appearance, read live and never saved
//! (spec 0003, `OsAppearance`). `embed` fills it in per OS.

use crate::color::Rgba;

/// The desktop OS the chrome adapts to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    /// macOS.
    Mac,
    /// Windows.
    Windows,
    /// Linux and the other free desktops.
    Linux,
}

impl Os {
    /// The OS this binary was compiled for.
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Mac
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Linux
        }
    }

    /// The `data-os` attribute value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mac => "macos",
            Self::Windows => "windows",
            Self::Linux => "linux",
        }
    }
}

/// A window button the chrome draws itself (Windows, Linux).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptionButton {
    /// Minimize.
    Minimize,
    /// Maximize or restore.
    Maximize,
    /// Close.
    Close,
}

/// Which side of the caption row the window buttons sit on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The left edge.
    Left,
    /// The right edge.
    Right,
}

/// Where the window buttons go and in what order, left to right.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ButtonLayout {
    /// The side of the caption row.
    pub side: Side,
    /// The buttons, left to right. May be empty (a desktop can hide them).
    pub order: Vec<CaptionButton>,
}

impl Default for ButtonLayout {
    /// Right, minimize, maximize, close: Windows, and Linux when the desktop says nothing.
    fn default() -> Self {
        Self {
            side: Side::Right,
            order: vec![
                CaptionButton::Minimize,
                CaptionButton::Maximize,
                CaptionButton::Close,
            ],
        }
    }
}

impl ButtonLayout {
    /// Parses GNOME's `button-layout` string, e.g. `appmenu:minimize,close`
    /// or `close,minimize:`. Buttons before the colon go left, after it
    /// right. The chrome keeps one side: the left one if it has any window
    /// button, else the right. Names it does not draw (`appmenu`, `icon`,
    /// `spacer`) are skipped, as are repeats.
    pub fn parse_gnome(layout: &str) -> Self {
        let (left, right) = layout.split_once(':').unwrap_or(("", layout));
        let buttons = |part: &str| {
            let mut out = Vec::new();
            for name in part.split(',').map(str::trim) {
                let button = match name {
                    "minimize" => CaptionButton::Minimize,
                    "maximize" => CaptionButton::Maximize,
                    "close" => CaptionButton::Close,
                    _ => continue,
                };
                if !out.contains(&button) {
                    out.push(button);
                }
            }
            out
        };
        let left = buttons(left);
        if left.is_empty() {
            Self {
                side: Side::Right,
                order: buttons(right),
            }
        } else {
            Self {
                side: Side::Left,
                order: left,
            }
        }
    }
}

/// The OS appearance settings the chrome follows (spec 0003, data model).
#[derive(Debug, Clone, PartialEq)]
pub struct OsAppearance {
    /// The compile target.
    pub os: Os,
    /// The OS is in dark mode.
    pub dark: bool,
    /// The OS accent color, if it has one.
    pub accent: Option<Rgba>,
    /// Increase contrast (macOS) or high contrast (Windows, Linux) is on.
    pub high_contrast: bool,
    /// The OS asks apps to reduce motion.
    pub reduce_motion: bool,
    /// The OS text size factor, from 1.0 to 2.25.
    text_scale: f32,
    /// Window button side and order (Linux only).
    pub button_layout: ButtonLayout,
}

impl OsAppearance {
    /// The smallest text scale the chrome takes from the OS.
    pub const MIN_TEXT_SCALE: f32 = 1.0;
    /// The largest text scale the chrome takes from the OS.
    pub const MAX_TEXT_SCALE: f32 = 2.25;

    /// The values used when the OS gives none: light, no accent, normal
    /// contrast, full motion, unscaled text, default buttons.
    pub fn fallback(os: Os) -> Self {
        Self {
            os,
            dark: false,
            accent: None,
            high_contrast: false,
            reduce_motion: false,
            text_scale: Self::MIN_TEXT_SCALE,
            button_layout: ButtonLayout::default(),
        }
    }

    /// The OS text size factor.
    pub fn text_scale(&self) -> f32 {
        self.text_scale
    }

    /// Sets the text size factor, clamped to 1.0 to 2.25. A value that is
    /// not a number counts as 1.0.
    pub fn set_text_scale(&mut self, scale: f32) {
        self.text_scale = if scale.is_nan() {
            Self::MIN_TEXT_SCALE
        } else {
            scale.clamp(Self::MIN_TEXT_SCALE, Self::MAX_TEXT_SCALE)
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use CaptionButton::{Close, Maximize, Minimize};

    #[test]
    fn gnome_default_layout_is_close_on_the_right() {
        let layout = ButtonLayout::parse_gnome("appmenu:close");
        assert_eq!(layout.side, Side::Right);
        assert_eq!(layout.order, [Close]);
    }

    #[test]
    fn buttons_before_the_colon_go_left_in_that_order() {
        let layout = ButtonLayout::parse_gnome("close,minimize:");
        assert_eq!(layout.side, Side::Left);
        assert_eq!(layout.order, [Close, Minimize]);
    }

    #[test]
    fn unknown_names_and_repeats_are_skipped() {
        let layout = ButtonLayout::parse_gnome("icon:spacer,minimize,minimize,maximize,close");
        assert_eq!(layout.side, Side::Right);
        assert_eq!(layout.order, [Minimize, Maximize, Close]);
    }

    #[test]
    fn no_colon_means_right() {
        let layout = ButtonLayout::parse_gnome("minimize,close");
        assert_eq!(layout.side, Side::Right);
        assert_eq!(layout.order, [Minimize, Close]);
    }

    #[test]
    fn text_scale_is_clamped() {
        let mut os = OsAppearance::fallback(Os::Linux);
        os.set_text_scale(3.0);
        assert_eq!(os.text_scale(), 2.25);
        os.set_text_scale(0.5);
        assert_eq!(os.text_scale(), 1.0);
        os.set_text_scale(f32::NAN);
        assert_eq!(os.text_scale(), 1.0);
        os.set_text_scale(1.5);
        assert_eq!(os.text_scale(), 1.5);
    }
}
