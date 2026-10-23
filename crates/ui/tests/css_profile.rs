//! Every chrome stylesheet stays inside the chrome CSS profile, and chrome
//! HTML carries no script, event handler or inline style (spec 0003, AC-3).
//!
//! The walk is token level (cssparser's tokenizer, not a full CSS parser):
//! enough to name the property, value, selector or at rule that breaks the
//! profile, with its file and line.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

use std::fs;
use std::path::{Path, PathBuf};

use cssparser::{ParseError, Parser, Token};

/// Properties chrome CSS may set.
const PROPERTIES: &[&str] = &[
    "display",
    "flex-direction",
    "flex-wrap",
    "flex",
    "align-items",
    "align-self",
    "justify-content",
    "gap",
    "order",
    "position",
    "top",
    "right",
    "bottom",
    "left",
    "z-index",
    "box-sizing",
    "width",
    "height",
    "min-width",
    "min-height",
    "max-width",
    "max-height",
    "margin",
    "margin-top",
    "margin-right",
    "margin-bottom",
    "margin-left",
    "padding",
    "padding-top",
    "padding-right",
    "padding-bottom",
    "padding-left",
    "overflow",
    "color",
    "background-color",
    "border",
    "border-width",
    "border-style",
    "border-color",
    "border-top",
    "border-right",
    "border-bottom",
    "border-left",
    "border-top-color",
    "border-right-color",
    "border-bottom-color",
    "border-left-color",
    "border-top-width",
    "border-right-width",
    "border-bottom-width",
    "border-left-width",
    "border-radius",
    "outline",
    "outline-offset",
    "opacity",
    "visibility",
    "cursor",
    "font-family",
    "font-size",
    "font-weight",
    "line-height",
    "text-align",
    "white-space",
    "text-overflow",
    "user-select",
];

/// Keywords allowed in any value the profile does not narrow further.
const KEYWORDS: &[&str] = &[
    "auto",
    "none",
    "normal",
    "row",
    "column",
    "wrap",
    "nowrap",
    "center",
    "flex-start",
    "flex-end",
    "space-between",
    "stretch",
    "baseline",
    "border-box",
    "content-box",
    "solid",
    "transparent",
    "currentColor",
    "visible",
    "hidden",
    "left",
    "right",
    "default",
    "pointer",
    "text",
    "system-ui",
    "ChromeIcons",
];

/// Properties whose keywords the profile narrows (spec 0003, chrome CSS profile).
const NARROWED: &[(&str, &[&str])] = &[
    (
        "display",
        &["block", "inline", "inline-block", "flex", "none"],
    ),
    ("position", &["static", "relative", "absolute"]),
    ("overflow", &["visible", "hidden"]),
    ("border-style", &["none", "solid"]),
    ("white-space", &["normal", "nowrap"]),
    ("text-overflow", &["ellipsis"]),
    ("user-select", &["none"]),
];

const PSEUDO_CLASSES: &[&str] = &[
    "hover",
    "active",
    "focus-visible",
    "disabled",
    "first-child",
    "last-child",
    "root",
];

/// The file being checked and the misses found so far.
struct Check<'a> {
    file: &'a str,
    /// `tokens.css` alone may hold custom properties and literals.
    tokens: bool,
    misses: Vec<String>,
}

type Result = std::result::Result<(), ParseError<()>>;

impl Check<'_> {
    fn miss(&mut self, p: &Parser<'_>, what: String) {
        let line = p.current_source_location().line + 1;
        self.misses.push(format!("{}:{line}: {what}", self.file));
    }

    fn stylesheet(&mut self, p: &mut Parser<'_>) {
        while !p.is_exhausted() {
            let token = match p.next_including_whitespace() {
                Ok(token) => token.clone(),
                Err(_) => break,
            };
            match token {
                Token::WhiteSpace(_) | Token::CDO | Token::CDC => {}
                Token::AtKeyword(name) if name.as_ref() == "font-face" => {
                    self.skip_to_block(p);
                    let _ = p.parse_nested_block(|p| self.declarations(p, true));
                }
                Token::AtKeyword(name) => {
                    self.miss(p, format!("at rule `@{name}` is not in the profile"));
                    self.skip_to_block(p);
                }
                first => self.rule(p, first),
            }
        }
    }

    fn skip_to_block(&mut self, p: &mut Parser<'_>) {
        while let Ok(token) = p.next() {
            if matches!(token, Token::CurlyBracketBlock) {
                return;
            }
        }
    }

    /// A style rule: selector tokens up to `{`, then declarations.
    fn rule(&mut self, p: &mut Parser<'_>, first: Token<'_>) {
        let mut token = Some(first);
        let mut previous_colon = false;
        while let Some(current) = token {
            match &current {
                Token::CurlyBracketBlock => {
                    let _ = p.parse_nested_block(|p| self.declarations(p, false));
                    return;
                }
                Token::Ident(_) | Token::IDHash(_) | Token::WhiteSpace(_) | Token::Comma => {}
                Token::Delim('.' | '>') => {}
                Token::Colon if previous_colon => {
                    // `::placeholder` is the one pseudo element.
                    match p.next_including_whitespace() {
                        Ok(Token::Ident(name)) if name.as_ref() == "placeholder" => {}
                        Ok(other) => {
                            let other = other.clone();
                            self.miss(
                                p,
                                format!(
                                    "pseudo element `::{}` is not in the profile",
                                    show(&other)
                                ),
                            );
                        }
                        Err(_) => {}
                    }
                    previous_colon = false;
                    token = p.next_including_whitespace().ok().cloned();
                    continue;
                }
                Token::Colon => {
                    if let Ok(next) = p.next_including_whitespace() {
                        let next = next.clone();
                        match &next {
                            Token::Ident(name) if PSEUDO_CLASSES.contains(&name.as_ref()) => {}
                            Token::Colon => {
                                previous_colon = true;
                                token = Some(next);
                                continue;
                            }
                            other => self.miss(
                                p,
                                format!("pseudo class `:{}` is not in the profile", show(other)),
                            ),
                        }
                    }
                }
                Token::SquareBracketBlock => {
                    let _ = p.parse_nested_block(|p| self.attribute(p));
                }
                other => self.miss(
                    p,
                    format!("selector part `{}` is not in the profile", show(other)),
                ),
            }
            previous_colon = false;
            token = p.next_including_whitespace().ok().cloned();
        }
    }

    /// `[a]` or `[a="v"]`.
    fn attribute<'i>(&mut self, p: &mut Parser<'i>) -> Result {
        let ok = matches!(p.next(), Ok(Token::Ident(_)))
            && (p.is_exhausted()
                || (matches!(p.next(), Ok(Token::Delim('=')))
                    && matches!(p.next(), Ok(Token::QuotedString(_) | Token::Ident(_)))
                    && p.is_exhausted()));
        if !ok {
            self.miss(p, "attribute selector beyond `[a]` and `[a=\"v\"]`".into());
        }
        while p.next().is_ok() {}
        Ok(())
    }

    fn declarations<'i>(&mut self, p: &mut Parser<'i>, font_face: bool) -> Result {
        while !p.is_exhausted() {
            let name = match p.next() {
                Ok(Token::Ident(name)) => name.to_string(),
                Ok(Token::Semicolon) => continue,
                Ok(other) => {
                    let other = other.clone();
                    self.miss(p, format!("expected a property, found `{}`", show(&other)));
                    continue;
                }
                Err(_) => break,
            };
            let _ = p.expect_colon();
            let allowed = if font_face {
                matches!(name.as_str(), "font-family" | "src")
            } else if name.starts_with("--") {
                self.tokens
            } else {
                PROPERTIES.contains(&name.as_str()) && !self.tokens
            };
            if !allowed {
                let place = if self.tokens {
                    "tokens.css, which holds only custom properties"
                } else {
                    "the profile"
                };
                self.miss(p, format!("property `{name}` is not allowed in {place}"));
            }
            let _ = p.parse_until_after(cssparser::Delimiter::Semicolon, |p| {
                self.value(p, &name, font_face);
                Ok::<(), ParseError<()>>(())
            });
        }
        Ok(())
    }

    fn value(&mut self, p: &mut Parser<'_>, property: &str, font_face: bool) {
        let narrowed = NARROWED
            .iter()
            .find(|(name, _)| *name == property)
            .map(|(_, words)| *words);
        while let Ok(token) = p.next() {
            let token = token.clone();
            match &token {
                Token::Ident(word) => {
                    let ok = match narrowed {
                        Some(words) => words.contains(&word.as_ref()),
                        None => KEYWORDS.contains(&word.as_ref()) || self.tokens,
                    };
                    if !ok {
                        self.miss(p, format!("`{property}: {word}` is not in the profile"));
                    }
                }
                Token::Number { int_value, .. } if property == "font-weight" => {
                    if !matches!(int_value, Some(400 | 600)) {
                        self.miss(p, "font-weight other than 400 or 600".into());
                    }
                }
                Token::Number { .. } | Token::Percentage { .. } | Token::Comma => {}
                Token::Dimension { unit, .. } => {
                    let unit = unit.as_ref();
                    let ok = self.tokens && matches!(unit, "px" | "rem" | "ms");
                    if !ok {
                        let reason = if matches!(unit, "px" | "rem") {
                            "only tokens.css holds lengths"
                        } else {
                            "unit not in the profile"
                        };
                        self.miss(p, format!("`{property}` uses a `{unit}` literal: {reason}"));
                    }
                }
                Token::Hash(_) | Token::IDHash(_) if !self.tokens => {
                    self.miss(
                        p,
                        format!("`{property}` uses a color literal: only tokens.css holds colors"),
                    );
                }
                Token::Hash(_) | Token::IDHash(_) => {}
                Token::UnquotedUrl(_) if font_face => {}
                Token::Function(name) => {
                    let name = name.to_string();
                    let ok = match name.as_str() {
                        "var" => true,
                        "rgb" | "rgba" | "cubic-bezier" => self.tokens,
                        "url" => font_face,
                        _ => false,
                    };
                    if !ok {
                        self.miss(p, format!("`{property}` uses `{name}()`, not allowed here"));
                    }
                    if name == "var" {
                        let _ = p.parse_nested_block(|p| {
                            if !matches!(p.next(), Ok(Token::Ident(n)) if n.starts_with("--")) {
                                self.miss(p, "var() needs a custom property name".into());
                            }
                            if p.next().is_ok() {
                                self.value(p, property, font_face);
                            }
                            Ok::<(), ParseError<()>>(())
                        });
                    } else {
                        let _ = p.parse_nested_block(|p| {
                            while p.next().is_ok() {}
                            Ok::<(), ParseError<()>>(())
                        });
                    }
                }
                Token::Delim('!') => self.miss(p, format!("`{property}` uses `!important`")),
                other => self.miss(
                    p,
                    format!("`{property}` value `{}` is not in the profile", show(other)),
                ),
            }
        }
    }
}

fn show(token: &Token<'_>) -> String {
    match token {
        Token::Ident(s) | Token::Function(s) | Token::AtKeyword(s) => s.to_string(),
        other => format!("{other:?}"),
    }
}

fn check_css(file: &str, css: &str) -> Vec<String> {
    let mut check = Check {
        file,
        tokens: file.ends_with("tokens.css"),
        misses: Vec::new(),
    };
    check.stylesheet(&mut Parser::new(css));
    check.misses
}

fn check_html(file: &str, html: &str) -> Vec<String> {
    let mut misses = Vec::new();
    for (index, line) in html.lines().enumerate() {
        let lower = line.to_ascii_lowercase();
        let mut problem = |what: &str| misses.push(format!("{file}:{}: {what}", index + 1));
        if lower.contains("<script") {
            problem("chrome HTML runs no script");
        }
        if lower.contains(" style=") {
            problem("inline style attribute (only Rust sets the root font size)");
        }
        let handler = lower.split_whitespace().any(|word| {
            word.strip_prefix("on")
                .and_then(|rest| rest.split_once('='))
                .is_some_and(|(name, _)| {
                    !name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase())
                })
        });
        if handler {
            problem("`on…` event handler attribute");
        }
    }
    misses
}

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files(&path, out);
        } else {
            out.push(path);
        }
    }
}

#[test]
fn every_chrome_file_stays_in_the_profile() {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let mut paths = Vec::new();
    files(&assets, &mut paths);
    let mut misses = Vec::new();
    let mut checked = 0;
    for path in paths {
        let name = path.strip_prefix(&assets).unwrap().display().to_string();
        match path.extension().and_then(|e| e.to_str()) {
            Some("css") => misses.extend(check_css(&name, &fs::read_to_string(&path).unwrap())),
            Some("html") => misses.extend(check_html(&name, &fs::read_to_string(&path).unwrap())),
            _ => continue,
        }
        checked += 1;
    }
    assert!(
        checked >= 2,
        "expected tokens.css and controls.css at least"
    );
    assert!(
        misses.is_empty(),
        "chrome CSS profile misses:\n{}",
        misses.join("\n")
    );
}

#[test]
fn rejects_what_the_profile_leaves_out() {
    let cases = [
        ".a { box-shadow: var(--x); }",
        ".a { display: grid; }",
        ".a { position: fixed; }",
        ".a { width: 12px; }",
        ".a { color: #ff0000; }",
        ".a { color: rgb(1, 2, 3); }",
        ".a { width: calc(var(--x)); }",
        ".a { color: var(--x) !important; }",
        ".a::before { color: var(--x); }",
        ".a:not(.b) { color: var(--x); }",
        ".a + .b { color: var(--x); }",
        ".a[href^=\"x\"] { color: var(--x); }",
        "@media (prefers-color-scheme: dark) { .a { color: var(--x); } }",
        ".a { --local: 1px; }",
        ".a { font-weight: 700; }",
    ];
    for css in cases {
        assert!(
            !check_css("controls.css", css).is_empty(),
            "should fail: {css}"
        );
    }
}

#[test]
fn misses_name_the_file_and_line() {
    let misses = check_css(
        "x.css",
        ".a { color: var(--x); }\n.b {\n  box-shadow: var(--y);\n}\n",
    );
    assert_eq!(misses.len(), 1, "{misses:?}");
    assert!(misses[0].starts_with("x.css:3:"), "{}", misses[0]);
}

#[test]
fn accepts_the_canonical_pattern() {
    let css = r#"
.segmented > [role="radio"] { min-height: var(--control-height); padding: 0 var(--space-3); }
.segmented > [role="radio"]:hover { background-color: var(--fill-hover); }
.segmented > [role="radio"][aria-checked="true"] { background-color: var(--surface-raised); }
.segmented > [role="radio"]:focus-visible { outline: var(--focus-width) solid var(--accent); }
.text-field::placeholder { color: var(--text-secondary); }
"#;
    assert_eq!(check_css("controls.css", css), Vec::<String>::new());
}

#[test]
fn html_rules() {
    assert!(!check_html("a.html", "<script>alert(1)</script>").is_empty());
    assert!(!check_html("a.html", "<button onclick=\"x()\">").is_empty());
    assert!(!check_html("a.html", "<div style=\"color: red\">").is_empty());
    assert!(check_html("a.html", "<button data-command=\"settings.open\">").is_empty());
}
