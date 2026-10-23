//! Every color pair the chrome can show meets WCAG AA, for every theme,
//! contrast and accent preset (spec 0003, AC-2). `tokens.css` is the input;
//! `docs/design.md` lists the same pairs.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

use ui::{Accent, ResolvedTokens, Rgba, TOKENS_CSS, Tokens, contrast_ratio};

/// Text and icons.
const TEXT: f32 = 4.5;
/// Focus rings, field borders, accent fills (WCAG 1.4.11).
const NON_TEXT: f32 = 3.0;

const SURFACES: [&str; 3] = ["--surface-chrome", "--surface-page", "--surface-raised"];
const TEXTS: [&str; 2] = ["--text-primary", "--text-secondary"];

struct Mode {
    theme: &'static str,
    contrast: &'static str,
}

const MODES: [Mode; 4] = [
    Mode {
        theme: "light",
        contrast: "normal",
    },
    Mode {
        theme: "dark",
        contrast: "normal",
    },
    Mode {
        theme: "light",
        contrast: "more",
    },
    Mode {
        theme: "dark",
        contrast: "more",
    },
];

fn resolve(mode: &Mode, accent: Accent) -> ResolvedTokens {
    Tokens::parse(TOKENS_CSS).unwrap().resolve(&[
        ("data-theme", mode.theme),
        ("data-contrast", mode.contrast),
        ("data-accent", accent.as_str()),
    ])
}

fn color(tokens: &ResolvedTokens, name: &str) -> Rgba {
    tokens.color(name).unwrap_or_else(|err| panic!("{err}"))
}

/// Each surface on its own, and with the hover and pressed fills over it.
fn backgrounds(tokens: &ResolvedTokens) -> Vec<(String, Rgba)> {
    let mut out = Vec::new();
    for surface in SURFACES {
        let base = color(tokens, surface);
        assert_eq!(base.a, 1.0, "{surface} must be opaque");
        out.push((surface.to_owned(), base));
        for fill in ["--fill-hover", "--fill-pressed"] {
            out.push((
                format!("{fill} over {surface}"),
                color(tokens, fill).over(base),
            ));
        }
    }
    out
}

#[test]
fn every_documented_pair_meets_aa() {
    let mut misses = Vec::new();
    let mut check = |label: String, fg: Rgba, bg: Rgba, min: f32| {
        let ratio = contrast_ratio(fg, bg);
        if ratio < min {
            misses.push(format!("{label}: {ratio:.2}:1, needs {min}:1"));
        }
    };

    for mode in &MODES {
        for accent in Accent::ALL {
            let tokens = resolve(mode, accent);
            let at = |pair: &str| {
                format!(
                    "{}/{}/{} {pair}",
                    mode.theme,
                    mode.contrast,
                    accent.as_str()
                )
            };
            // High contrast holds the accent to the text ratio (spec 0003, Tokens).
            let accent_min = if mode.contrast == "more" {
                TEXT
            } else {
                NON_TEXT
            };

            for (background, bg) in backgrounds(&tokens) {
                for text in TEXTS {
                    check(
                        at(&format!("{text} on {background}")),
                        color(&tokens, text),
                        bg,
                        TEXT,
                    );
                }
            }
            for surface in SURFACES {
                let bg = color(&tokens, surface);
                for (token, min) in [
                    ("--accent", accent_min),
                    ("--accent-strong", accent_min),
                    ("--focus-ring", accent_min),
                    ("--border-strong", NON_TEXT),
                ] {
                    check(
                        at(&format!("{token} on {surface}")),
                        color(&tokens, token),
                        bg,
                        min,
                    );
                }
            }
            // Selected text in a field keeps its color (spec 0008, AC-14).
            check(
                at("--text-primary on --selection"),
                color(&tokens, "--text-primary"),
                color(&tokens, "--selection"),
                TEXT,
            );
            // The palette's glass over the darkest and lightest page.
            for page in ["#000000", "#FFFFFF"] {
                let under = ui::Rgba::parse(page).unwrap();
                let glass = color(&tokens, "--glass-surface").over(under);
                for text in TEXTS {
                    check(
                        at(&format!("{text} on --glass-surface over {page}")),
                        color(&tokens, text),
                        glass,
                        TEXT,
                    );
                }
            }
            for fill in ["--accent", "--accent-strong"] {
                check(
                    at(&format!("--on-accent on {fill}")),
                    color(&tokens, "--on-accent"),
                    color(&tokens, fill),
                    TEXT,
                );
            }
        }
    }
    assert!(misses.is_empty(), "contrast misses:\n{}", misses.join("\n"));
}

#[test]
fn every_mode_defines_every_color_token() {
    for mode in &MODES {
        for accent in Accent::ALL {
            let tokens = resolve(mode, accent);
            for name in [
                "--fill-hover",
                "--fill-pressed",
                "--text-disabled",
                "--border-subtle",
                "--accent",
                "--accent-strong",
                "--on-accent",
                "--focus-ring",
                "--selection",
            ]
            .into_iter()
            .chain(SURFACES)
            .chain(TEXTS)
            {
                color(&tokens, name);
            }
        }
    }
}

/// `snap` compares against `Accent::hue`; it must stay the hue of the
/// preset's light value here.
#[test]
fn snap_hues_match_the_light_presets() {
    let light = &MODES[0];
    for accent in Accent::ALL {
        let (_, _, hue) = color(&resolve(light, accent), "--accent").oklch();
        let diff = (hue - accent.hue()).abs();
        assert!(
            diff.min(360.0 - diff) < 1.0,
            "{accent:?}: tokens.css hue {hue:.1}, Accent::hue {}",
            accent.hue()
        );
    }
}
