//! Reads `tokens.css` and resolves its custom properties for one set of
//! root attributes, the way the cascade would on `<html>`.
//!
//! This is not a CSS parser. It accepts only the shape `tokens.css` is
//! written in (spec 0003, Tokens): rules whose selectors are `:root` or
//! compounds of `[data-x="v"]`, holding `--name: value;` declarations. The
//! chrome document's frame metrics and the contrast test read tokens through it, so
//! `tokens.css` stays the one place colors are written.

use std::collections::HashMap;

use crate::ChromeError;
use crate::color::Rgba;

/// How deep `var()` references may nest before resolution gives up.
const MAX_VAR_DEPTH: usize = 8;

/// One `[name="value"]` test.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AttrTest {
    name: String,
    value: String,
}

#[derive(Debug, Clone)]
struct Rule {
    /// Each selector of the list: the attribute tests it needs (none for `:root`).
    selectors: Vec<Vec<AttrTest>>,
    /// The custom properties it sets, in order.
    declarations: Vec<(String, String)>,
}

/// The parsed token rules.
#[derive(Debug, Clone)]
pub struct Tokens {
    rules: Vec<Rule>,
}

/// Token values for one set of root attributes, `var()` still unresolved
/// until asked for.
#[derive(Debug, Clone)]
pub struct ResolvedTokens {
    values: HashMap<String, String>,
}

impl Tokens {
    /// Parses `css`, failing with the line of the first thing it does not accept.
    pub fn parse(css: &str) -> Result<Self, ChromeError> {
        let css = strip_comments(css);
        let mut rules = Vec::new();
        let mut rest = css.as_str();
        let line_of = |at: &str| css[..css.len() - at.len()].matches('\n').count() + 1;

        loop {
            let trimmed = rest.trim_start();
            if trimmed.is_empty() {
                break;
            }
            let open = trimmed
                .find('{')
                .ok_or_else(|| syntax(line_of(trimmed), "expected `{`"))?;
            let close = trimmed[open..]
                .find('}')
                .map(|i| open + i)
                .ok_or_else(|| syntax(line_of(trimmed), "missing `}`"))?;
            let selectors = trimmed[..open]
                .split(',')
                .map(|s| {
                    parse_selector(s.trim())
                        .ok_or_else(|| syntax(line_of(trimmed), "unsupported selector"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let mut declarations = Vec::new();
            for declaration in trimmed[open + 1..close].split(';') {
                let declaration = declaration.trim();
                if declaration.is_empty() {
                    continue;
                }
                let (name, value) = declaration
                    .split_once(':')
                    .filter(|(name, _)| name.trim().starts_with("--"))
                    .ok_or_else(|| {
                        syntax(
                            line_of(trimmed),
                            "tokens.css only holds `--name: value` declarations",
                        )
                    })?;
                declarations.push((name.trim().to_owned(), value.trim().to_owned()));
            }
            rules.push(Rule {
                selectors,
                declarations,
            });
            rest = &trimmed[close + 1..];
        }
        Ok(Self { rules })
    }

    /// The token values on an element carrying `attributes`: matching rules
    /// apply by specificity (attribute count), then source order.
    pub fn resolve(&self, attributes: &[(&str, &str)]) -> ResolvedTokens {
        let matches = |tests: &[AttrTest]| {
            tests.iter().all(|t| {
                attributes
                    .iter()
                    .any(|(name, value)| *name == t.name && *value == t.value)
            })
        };
        let mut applied: Vec<(usize, usize, &Rule)> = self
            .rules
            .iter()
            .enumerate()
            .filter_map(|(order, rule)| {
                rule.selectors
                    .iter()
                    .filter(|tests| matches(tests))
                    .map(Vec::len)
                    .max()
                    .map(|specificity| (specificity, order, rule))
            })
            .collect();
        applied.sort_by_key(|&(specificity, order, _)| (specificity, order));

        let mut values = HashMap::new();
        for (_, _, rule) in applied {
            for (name, value) in &rule.declarations {
                values.insert(name.clone(), value.clone());
            }
        }
        ResolvedTokens { values }
    }
}

impl ResolvedTokens {
    /// The value of `name` with every `var()` replaced.
    pub fn get(&self, name: &str) -> Result<String, ChromeError> {
        self.expand(name, 0)
    }

    /// `name` as a color.
    pub fn color(&self, name: &str) -> Result<Rgba, ChromeError> {
        let value = self.get(name)?;
        Rgba::parse(&value).ok_or_else(|| ChromeError::BadTokenValue {
            name: name.to_owned(),
            value,
            expected: "color",
        })
    }

    /// `name` as a length in px (`8px`, or `0`).
    pub fn px(&self, name: &str) -> Result<f32, ChromeError> {
        let value = self.get(name)?;
        let number = if value == "0" {
            Some(0.0)
        } else {
            value.strip_suffix("px").and_then(|n| n.parse::<f32>().ok())
        };
        number.ok_or_else(|| ChromeError::BadTokenValue {
            name: name.to_owned(),
            value,
            expected: "px length",
        })
    }

    fn expand(&self, name: &str, depth: usize) -> Result<String, ChromeError> {
        let raw = self
            .values
            .get(name)
            .ok_or_else(|| ChromeError::MissingToken(name.to_owned()))?;
        if depth >= MAX_VAR_DEPTH {
            return Err(ChromeError::BadTokenValue {
                name: name.to_owned(),
                value: raw.clone(),
                expected: "value without a var() cycle",
            });
        }
        let mut out = String::new();
        let mut rest = raw.as_str();
        while let Some(start) = rest.find("var(") {
            out.push_str(&rest[..start]);
            let inner_start = start + "var(".len();
            let end = matching_paren(&rest[inner_start..])
                .map(|i| inner_start + i)
                .ok_or_else(|| ChromeError::BadTokenValue {
                    name: name.to_owned(),
                    value: raw.clone(),
                    expected: "closed var()",
                })?;
            let inner = &rest[inner_start..end];
            let (reference, fallback) = match inner.split_once(',') {
                Some((reference, fallback)) => (reference.trim(), Some(fallback.trim())),
                None => (inner.trim(), None),
            };
            match (self.expand(reference, depth + 1), fallback) {
                (Ok(value), _) => out.push_str(&value),
                (Err(ChromeError::MissingToken(_)), Some(fallback)) => out.push_str(fallback),
                (Err(err), _) => return Err(err),
            }
            rest = &rest[end + 1..];
        }
        out.push_str(rest);
        Ok(out)
    }
}

/// `:root`, or one or more `[name="value"]` with nothing between them.
fn parse_selector(selector: &str) -> Option<Vec<AttrTest>> {
    if selector == ":root" {
        return Some(Vec::new());
    }
    let mut tests = Vec::new();
    let mut rest = selector;
    while !rest.is_empty() {
        let inner = rest.strip_prefix('[')?;
        let end = inner.find(']')?;
        let (name, value) = inner[..end].split_once('=')?;
        let value = value.strip_prefix('"')?.strip_suffix('"')?;
        tests.push(AttrTest {
            name: name.to_owned(),
            value: value.to_owned(),
        });
        rest = &inner[end + 1..];
    }
    (!tests.is_empty()).then_some(tests)
}

/// The index of the `)` that closes an already opened paren.
fn matching_paren(text: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in text.char_indices() {
        match c {
            '(' => depth += 1,
            ')' if depth == 0 => return Some(i),
            ')' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// Replaces each comment with spaces, keeping newlines so line numbers hold.
fn strip_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        let end = rest[start..]
            .find("*/")
            .map_or(rest.len(), |i| start + i + 2);
        out.extend(
            rest[start..end]
                .chars()
                .map(|c| if c == '\n' { '\n' } else { ' ' }),
        );
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

fn syntax(line: usize, message: &str) -> ChromeError {
    ChromeError::TokensSyntax {
        line,
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CSS: &str = r#"
/* defaults */
:root, [data-theme="light"] { --surface: #ffffff; --text: #000; --ring: var(--accent); }
[data-theme="dark"] { --surface: #121212; --text: #fff; }
[data-accent="blue"] { --accent: #0A64D6; }
[data-theme="dark"][data-accent="blue"] { --accent: #3D8BFF; }
[data-theme="dark"][data-contrast="more"] { --surface: #000000; }
:root { --gap: 8px; --none: 0; --missing: var(--nope, 4px); }
"#;

    #[test]
    fn root_values_apply_without_attributes() {
        let tokens = Tokens::parse(CSS).unwrap().resolve(&[]);
        assert_eq!(tokens.get("--surface").unwrap(), "#ffffff");
        assert_eq!(tokens.px("--gap").unwrap(), 8.0);
        assert_eq!(tokens.px("--none").unwrap(), 0.0);
    }

    #[test]
    fn more_specific_rules_win() {
        let tokens = Tokens::parse(CSS).unwrap();
        let dark = tokens.resolve(&[("data-theme", "dark"), ("data-accent", "blue")]);
        assert_eq!(dark.get("--surface").unwrap(), "#121212");
        assert_eq!(dark.get("--ring").unwrap(), "#3D8BFF");
        let dark_more = tokens.resolve(&[
            ("data-theme", "dark"),
            ("data-contrast", "more"),
            ("data-accent", "blue"),
        ]);
        assert_eq!(dark_more.get("--surface").unwrap(), "#000000");
        assert_eq!(dark_more.get("--text").unwrap(), "#fff");
    }

    #[test]
    fn var_fallback_and_missing_tokens() {
        let tokens = Tokens::parse(CSS).unwrap().resolve(&[]);
        assert_eq!(tokens.px("--missing").unwrap(), 4.0);
        // `--ring` points at `--accent`, which needs `data-accent`.
        assert!(matches!(
            tokens.get("--ring"),
            Err(ChromeError::MissingToken(_))
        ));
        assert!(matches!(
            tokens.color("--gap"),
            Err(ChromeError::BadTokenValue { .. })
        ));
    }

    #[test]
    fn var_cycles_are_an_error() {
        let tokens = Tokens::parse(":root { --a: var(--b); --b: var(--a); }")
            .unwrap()
            .resolve(&[]);
        assert!(tokens.get("--a").is_err());
    }

    #[test]
    fn rejects_what_tokens_css_never_holds() {
        for bad in [
            ".button { --a: 1px; }",
            ":root { color: red; }",
            "[data-theme=dark] { --a: 1px; }",
            ":root { --a: 1px;",
        ] {
            assert!(Tokens::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn syntax_errors_name_the_line() {
        let err = Tokens::parse(":root { --a: 1px; }\n\n.x { --b: 2px; }").unwrap_err();
        assert!(
            matches!(err, ChromeError::TokensSyntax { line: 3, .. }),
            "{err}"
        );
    }
}
