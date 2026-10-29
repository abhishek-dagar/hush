//! Specified values: what a declaration says before the cascade computes
//! it, and the parsers for them (spec 0004, child 0003, Parsing).

use std::f32::consts::PI;
use std::rc::Rc;

use base::atoms::Atom;
use cssparser::{ParseError, Parser, Token, match_ignore_ascii_case};

use crate::Color;

/// What every value parser returns. Errors carry no detail: an invalid
/// declaration is simply dropped.
pub(crate) type ParseResult<T> = Result<T, ParseError<()>>;

/// A parse failure at the current position.
pub(crate) fn invalid<T>() -> ParseResult<T> {
    Err(ParseError::unexpected_token())
}

/// A length or percentage as written, before the cascade resolves it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SpecifiedLength {
    /// CSS px (every absolute unit is converted to px when parsed).
    Px(f32),
    /// Multiples of the font size (`ex` and `ch` count as half an `em`).
    Em(f32),
    /// Multiples of the root element's font size.
    Rem(f32),
    /// Percent of the viewport width.
    Vw(f32),
    /// Percent of the viewport height.
    Vh(f32),
    /// Percent of the smaller viewport side.
    Vmin(f32),
    /// Percent of the larger viewport side.
    Vmax(f32),
    /// Percent of the nearest ancestor container's inline size (spec 0010,
    /// AC-17).
    Cqw(f32),
    /// Percent of the nearest ancestor container's block size.
    Cqh(f32),
    /// Percent of the nearest ancestor container's inline size.
    Cqi(f32),
    /// Percent of the nearest ancestor container's block size.
    Cqb(f32),
    /// Percent of the smaller of the container's inline and block size.
    Cqmin(f32),
    /// Percent of the larger of the container's inline and block size.
    Cqmax(f32),
    /// A percentage, 0 to 100, resolved by whoever owns its basis.
    Percent(f32),
    /// A `calc()`, `min()`, `max()` or `clamp()` expression: its leaves stay
    /// as `SpecifiedLength` so `em`, `rem` and viewport units resolve at
    /// computed value time while percentages wait for layout (spec 0010,
    /// AC-8).
    Calc(Rc<SpecifiedCalcExpr>),
}

impl SpecifiedLength {
    /// Zero px.
    pub(crate) const ZERO: Self = Self::Px(0.0);

    fn is_negative(&self) -> bool {
        match self {
            Self::Px(v)
            | Self::Em(v)
            | Self::Rem(v)
            | Self::Vw(v)
            | Self::Vh(v)
            | Self::Vmin(v)
            | Self::Vmax(v)
            | Self::Cqw(v)
            | Self::Cqh(v)
            | Self::Cqi(v)
            | Self::Cqb(v)
            | Self::Cqmin(v)
            | Self::Cqmax(v)
            | Self::Percent(v) => *v < 0.0,
            Self::Calc(_) => false,
        }
    }

    /// Whether resolving it reads the viewport size.
    pub(crate) fn uses_viewport(&self) -> bool {
        match self {
            Self::Vw(_) | Self::Vh(_) | Self::Vmin(_) | Self::Vmax(_) => true,
            Self::Calc(expr) => calc_expr_uses_viewport(expr),
            _ => false,
        }
    }
}

/// A `calc()` family expression as written: its leaves stay as
/// `SpecifiedLength` so `em`, `rem` and viewport units resolve at computed
/// value time while percentages wait for layout (spec 0010, AC-8).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SpecifiedCalcExpr {
    /// One leaf length, as written.
    Leaf(SpecifiedLength),
    /// A sum of terms (`a + b`, `a - b`).
    Sum(Vec<SpecifiedCalcExpr>),
    /// A term scaled by a factor; division is a fractional factor.
    Product(Box<SpecifiedCalcExpr>, f32),
    /// The smallest of several terms (`min()`).
    Min(Vec<SpecifiedCalcExpr>),
    /// The largest of several terms (`max()`).
    Max(Vec<SpecifiedCalcExpr>),
    /// `clamp(min, value, max)`.
    Clamp(
        Box<SpecifiedCalcExpr>,
        Box<SpecifiedCalcExpr>,
        Box<SpecifiedCalcExpr>,
    ),
}

/// Whether any leaf of `expr` resolves against the viewport size.
fn calc_expr_uses_viewport(expr: &SpecifiedCalcExpr) -> bool {
    match expr {
        SpecifiedCalcExpr::Leaf(length) => length.uses_viewport(),
        SpecifiedCalcExpr::Sum(terms)
        | SpecifiedCalcExpr::Min(terms)
        | SpecifiedCalcExpr::Max(terms) => terms.iter().any(calc_expr_uses_viewport),
        SpecifiedCalcExpr::Product(term, _) => calc_expr_uses_viewport(term),
        SpecifiedCalcExpr::Clamp(a, b, c) => {
            calc_expr_uses_viewport(a) || calc_expr_uses_viewport(b) || calc_expr_uses_viewport(c)
        }
    }
}

/// Which lengths a property accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LengthRules {
    /// Percentages are allowed.
    pub(crate) percent: bool,
    /// Values below zero are allowed (margins only, in slice 1).
    pub(crate) negative: bool,
}

impl LengthRules {
    /// Lengths and percentages from zero up: sizes, padding, radii.
    pub(crate) const NON_NEGATIVE: Self = Self {
        percent: true,
        negative: false,
    };
    /// Any length or percentage: margins.
    pub(crate) const ANY: Self = Self {
        percent: true,
        negative: true,
    };
    /// Lengths from zero up, no percentages: border widths.
    pub(crate) const ABSOLUTE: Self = Self {
        percent: false,
        negative: false,
    };
    /// Any length, no percentages: `outline-offset`.
    pub(crate) const SIGNED_ABSOLUTE: Self = Self {
        percent: false,
        negative: true,
    };
}

/// Parses a `<length>` or `<length-percentage>`, including the `calc()`
/// family (`calc`, `min`, `max`, `clamp`). A bare `0` is a length; an
/// expression whose unit cannot be resolved here (`em`, `rem`, viewport
/// Parses a `<length>` or `<length-percentage>`. A bare `0` is a length;
/// `calc()` is not supported yet, so it fails and the declaration drops.
pub(crate) fn parse_length(
    input: &mut Parser<'_>,
    rules: LengthRules,
) -> ParseResult<SpecifiedLength> {
    let length = match *input.next()? {
        Token::Dimension {
            value, ref unit, ..
        } => length_from_unit(value, unit).ok_or(ParseError::unexpected_token())?,
        Token::Percentage { unit_value, .. } if rules.percent => {
            SpecifiedLength::Percent(unit_value * 100.0)
        }
        Token::Number { value: 0.0, .. } => SpecifiedLength::ZERO,
        Token::Function(ref name) => {
            let name = name.to_ascii_lowercase();
            if matches!(name.as_str(), "calc" | "min" | "max" | "clamp") {
                return parse_calc(input, &name, 1)
                    .map(|expr| SpecifiedLength::Calc(Rc::new(expr)));
            }
            return invalid();
        }
        _ => return invalid(),
    };
    if length.is_negative() && !rules.negative {
        return invalid();
    }
    Ok(length)
}

/// Parses `calc()`, `min()`, `max()` or `clamp()` after the function name
/// and opening paren have been consumed. `depth` counts nested functions;
/// deeper than 32 is invalid at parse time (spec 0010, AC-8).
fn parse_calc(input: &mut Parser<'_>, name: &str, depth: u32) -> ParseResult<SpecifiedCalcExpr> {
    if depth > 32 {
        return invalid();
    }
    input.parse_nested_block(|input| match name {
        // Depth counts calc-nesting levels, incremented once per level in
        // `parse_calc_value` when it recurses into a nested function. The
        // sum/product/value parsing inside one function does not add a level.
        "calc" => parse_calc_sum(input, depth),
        "min" => parse_comma(input, depth).and_then(|terms| {
            if terms.is_empty() {
                return invalid();
            }
            Ok(SpecifiedCalcExpr::Min(terms))
        }),
        "max" => parse_comma(input, depth).and_then(|terms| {
            if terms.is_empty() {
                return invalid();
            }
            Ok(SpecifiedCalcExpr::Max(terms))
        }),
        "clamp" => {
            let mut terms = parse_comma(input, depth)?;
            if terms.len() != 3 {
                return invalid();
            }
            Ok(SpecifiedCalcExpr::Clamp(
                Box::new(terms.remove(0)),
                Box::new(terms.remove(0)),
                Box::new(terms.remove(0)),
            ))
        }
        _ => invalid(),
    })
}

/// A `calc()` sum: terms added or subtracted.
fn parse_calc_sum(input: &mut Parser<'_>, depth: u32) -> ParseResult<SpecifiedCalcExpr> {
    let mut terms = Vec::new();
    terms.push(parse_calc_product(input, depth)?);
    loop {
        if input.try_parse(|i| i.expect_delim('+')).is_ok() {
            terms.push(parse_calc_product(input, depth)?);
        } else if input.try_parse(|i| i.expect_delim('-')).is_ok() {
            terms.push(SpecifiedCalcExpr::Product(
                Box::new(parse_calc_product(input, depth)?),
                -1.0,
            ));
        } else {
            break;
        }
    }
    Ok(if terms.len() == 1 {
        terms.remove(0)
    } else {
        SpecifiedCalcExpr::Sum(terms)
    })
}

/// A product: a value optionally multiplied or divided by a number.
/// Handles both `value * number` (e.g. `10px * 2`) and `number * value`
/// (e.g. `2 * 10px`). A leading bare number is a scalar multiplier.
fn parse_calc_product(input: &mut Parser<'_>, depth: u32) -> ParseResult<SpecifiedCalcExpr> {
    // A leading number is a scalar: `2 * 10px`. Try `number * value` first,
    // restoring the position when it is not followed by `*`.
    let saved = input.state();
    let scalar = input
        .next_including_whitespace()
        .ok()
        .and_then(|token| match token {
            Token::Number { value, .. } => Some(*value),
            _ => None,
        });
    if let Some(value) = scalar
        && input.try_parse(|i| i.expect_delim('*')).is_ok()
    {
        let rhs = parse_calc_value(input, depth)?;
        return Ok(SpecifiedCalcExpr::Product(Box::new(rhs), value));
    }
    input.reset(&saved);
    let base = parse_calc_value(input, depth)?;
    if input.try_parse(|i| i.expect_delim('*')).is_ok() {
        let factor: f32 = input.expect_number()?;
        return Ok(SpecifiedCalcExpr::Product(Box::new(base), factor));
    }
    if input.try_parse(|i| i.expect_delim('/')).is_ok() {
        let divisor: f32 = input.expect_number()?;
        return Ok(SpecifiedCalcExpr::Product(Box::new(base), 1.0 / divisor));
    }
    Ok(base)
}

/// One value: a dimension, a percentage, a parenthesised sum, or a nested
/// `calc()`/`min()`/`max()`/`clamp()`.
fn parse_calc_value(input: &mut Parser<'_>, depth: u32) -> ParseResult<SpecifiedCalcExpr> {
    if input.try_parse(|i| i.expect_delim('(')).is_ok() {
        let inner = parse_calc_sum(input, depth)?;
        input.expect_delim(')')?;
        return Ok(inner);
    }
    match *input.next()? {
        Token::ParenthesisBlock => {
            // A parenthesised sum: `(50% - 10px)`. cssparser delivers this as a
            // ParenthesisBlock; parse_nested_block consumes it and parses the
            // inner as a calc sum without adding a calc-nesting level.
            input.parse_nested_block(|input| parse_calc_sum(input, depth))
        }
        Token::Dimension {
            value, ref unit, ..
        } => length_from_unit(value, unit)
            .map(SpecifiedCalcExpr::Leaf)
            .ok_or(ParseError::unexpected_token()),
        Token::Percentage { unit_value, .. } => Ok(SpecifiedCalcExpr::Leaf(
            SpecifiedLength::Percent(unit_value * 100.0),
        )),
        Token::Function(ref name) => {
            let name = name.to_ascii_lowercase();
            if matches!(name.as_str(), "calc" | "min" | "max" | "clamp") {
                parse_calc(input, &name, depth + 1)
            } else {
                invalid()
            }
        }
        _ => invalid(),
    }
}

/// Comma separated calc sums, as for `min()`, `max()` and `clamp()`.
fn parse_comma(input: &mut Parser<'_>, depth: u32) -> ParseResult<Vec<SpecifiedCalcExpr>> {
    input.parse_comma_separated(|input| parse_calc_sum(input, depth))
}

fn length_from_unit(value: f32, unit: &str) -> Option<SpecifiedLength> {
    use SpecifiedLength as L;
    Some(match_ignore_ascii_case! { unit,
        "px" => L::Px(value),
        "em" => L::Em(value),
        "rem" => L::Rem(value),
        // Without font metrics here, both are half an em, the common
        // fallback browsers use too.
        "ex" | "ch" => L::Em(value * 0.5),
        "vw" => L::Vw(value),
        "vh" => L::Vh(value),
        "vmin" => L::Vmin(value),
        "vmax" => L::Vmax(value),
        "cqw" => L::Cqw(value),
        "cqh" => L::Cqh(value),
        "cqi" => L::Cqi(value),
        "cqb" => L::Cqb(value),
        "cqmin" => L::Cqmin(value),
        "cqmax" => L::Cqmax(value),
        "in" => L::Px(value * 96.0),
        "cm" => L::Px(value * 96.0 / 2.54),
        "mm" => L::Px(value * 96.0 / 25.4),
        "q" => L::Px(value * 96.0 / 101.6),
        "pt" => L::Px(value * 96.0 / 72.0),
        "pc" => L::Px(value * 16.0),
        _ => return None,
    })
}

/// A color as written: `currentcolor` waits for the element's `color`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum SpecifiedColor {
    /// A resolved color.
    Rgba(Color),
    /// `currentcolor`.
    CurrentColor,
}

impl SpecifiedColor {
    /// Resolves `currentcolor` to `current`.
    pub(crate) fn resolve(self, current: Color) -> Color {
        match self {
            Self::Rgba(color) => color,
            Self::CurrentColor => current,
        }
    }
}

/// Parses a `<color>`: the named colors (`cssparser`'s table of the 148
/// CSS keywords), `transparent`, `currentcolor`, hex in 3, 4, 6 or 8
/// digits, `rgb()`/`rgba()` and `hsl()`/`hsla()` in both the comma and the
/// space syntax.
pub(crate) fn parse_color(input: &mut Parser<'_>) -> ParseResult<SpecifiedColor> {
    let token = input.next()?.clone();
    let rgba = |(r, g, b): (u8, u8, u8), a: f32| {
        let mut color = Color::rgb8(r, g, b);
        color.a = a;
        SpecifiedColor::Rgba(color)
    };
    match token {
        Token::Ident(ref name) => {
            if name.eq_ignore_ascii_case("transparent") {
                return Ok(SpecifiedColor::Rgba(Color::TRANSPARENT));
            }
            if name.eq_ignore_ascii_case("currentcolor") {
                return Ok(SpecifiedColor::CurrentColor);
            }
            cssparser::color::parse_named_color(&name.to_ascii_lowercase())
                .map(|rgb| rgba(rgb, 1.0))
                .map_err(|()| ParseError::unexpected_token())
        }
        Token::Hash(ref value) | Token::IDHash(ref value) => {
            cssparser::color::parse_hash_color(value.as_bytes())
                .map(|(r, g, b, a)| rgba((r, g, b), a))
                .map_err(|()| ParseError::unexpected_token())
        }
        Token::Function(ref name) => {
            let name = name.to_ascii_lowercase();
            input.parse_nested_block(|input| match name.as_str() {
                "rgb" | "rgba" => parse_rgb(input),
                "hsl" | "hsla" => parse_hsl(input),
                _ => invalid(),
            })
        }
        _ => invalid(),
    }
}

/// Parses an HTML legacy color attribute value (`bgcolor`, etc.).
/// Follows the HTML "rules for parsing a legacy colour value": named
/// colors, `#rgb` and `#rrggbb` hex, odd legacy forms, and recursive
/// stripping of non-alphanumeric characters. Returns `None` for the empty
/// string and for `transparent`, which produce no color hint.
pub(crate) fn parse_legacy_color(value: &str) -> Option<SpecifiedColor> {
    let mut value = value.trim();
    if value.is_empty() {
        return None;
    }
    if value.eq_ignore_ascii_case("transparent") {
        return None;
    }
    // Hex: #rgb or #rrggbb
    if let Some(stripped) = value.strip_prefix('#') {
        if let Some(color) = parse_legacy_hex(stripped) {
            return Some(color);
        }
        // Invalid hex: drop the `#` and retry as a name.
        value = stripped;
    }
    // Named color
    if let Some(color) = parse_named_legacy(value) {
        return Some(color);
    }
    // Strip non-alphanumeric characters and retry.
    let cleaned: String = value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    if cleaned.is_empty() || cleaned == value {
        return None;
    }
    parse_legacy_color(&cleaned)
}

/// Parses `#rgb` or `#rrggbb` hex digits into a color.
fn parse_legacy_hex(value: &str) -> Option<SpecifiedColor> {
    let digits = value.as_bytes();
    let (r, g, b) = if digits.len() == 3 {
        let r = hex_digit(digits[0])?;
        let g = hex_digit(digits[1])?;
        let b = hex_digit(digits[2])?;
        ((r << 4) | r, (g << 4) | g, (b << 4) | b)
    } else if digits.len() == 6 {
        (
            (hex_digit(digits[0])? << 4) | hex_digit(digits[1])?,
            (hex_digit(digits[2])? << 4) | hex_digit(digits[3])?,
            (hex_digit(digits[4])? << 4) | hex_digit(digits[5])?,
        )
    } else {
        return None;
    };
    Some(SpecifiedColor::Rgba(Color::rgb8(r, g, b)))
}

/// Converts one hex digit to its numeric value.
fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Parses a named color from the CSS named color table.
fn parse_named_legacy(value: &str) -> Option<SpecifiedColor> {
    cssparser::color::parse_named_color(&value.to_ascii_lowercase())
        .ok()
        .map(|(r, g, b)| SpecifiedColor::Rgba(Color::rgb8(r, g, b)))
}

/// The inside of `rgb()`: three numbers or three percentages, then an
/// optional alpha, separated by commas or by spaces with a `/` before alpha.
fn parse_rgb(input: &mut Parser<'_>) -> ParseResult<SpecifiedColor> {
    let channel = |input: &mut Parser<'_>| -> ParseResult<f32> {
        match *input.next()? {
            Token::Number { value, .. } => Ok(value / 255.0),
            Token::Percentage { unit_value, .. } => Ok(unit_value),
            _ => invalid(),
        }
    };
    let r = channel(input)?;
    let commas = input.try_parse(Parser::expect_comma).is_ok();
    let g = channel(input)?;
    if commas {
        input.expect_comma()?;
    }
    let b = channel(input)?;
    let a = parse_alpha(input, commas)?;
    input.expect_exhausted()?;
    Ok(SpecifiedColor::Rgba(Color::rgba(
        r.clamp(0.0, 1.0),
        g.clamp(0.0, 1.0),
        b.clamp(0.0, 1.0),
        a,
    )))
}

/// The inside of `hsl()`: a hue (a number of degrees or an angle), then
/// saturation and lightness, then an optional alpha.
fn parse_hsl(input: &mut Parser<'_>) -> ParseResult<SpecifiedColor> {
    let hue = match *input.next()? {
        Token::Number { value, .. } => value,
        Token::Dimension {
            value, ref unit, ..
        } => match_ignore_ascii_case! { unit,
            "deg" => value,
            "rad" => value * 180.0 / PI,
            "grad" => value * 0.9,
            "turn" => value * 360.0,
            _ => return invalid(),
        },
        _ => return invalid(),
    };
    let commas = input.try_parse(Parser::expect_comma).is_ok();
    let percent = |input: &mut Parser<'_>, commas: bool| -> ParseResult<f32> {
        match *input.next()? {
            Token::Percentage { unit_value, .. } => Ok(unit_value),
            // The space syntax also takes plain numbers, out of 100.
            Token::Number { value, .. } if !commas => Ok(value / 100.0),
            _ => invalid(),
        }
    };
    let saturation = percent(input, commas)?.clamp(0.0, 1.0);
    if commas {
        input.expect_comma()?;
    }
    let lightness = percent(input, commas)?.clamp(0.0, 1.0);
    let a = parse_alpha(input, commas)?;
    input.expect_exhausted()?;
    let (r, g, b) = hsl_to_rgb(hue, saturation, lightness);
    Ok(SpecifiedColor::Rgba(Color::rgba(r, g, b, a)))
}

/// An optional alpha after the channels: `, a` in the comma syntax, `/ a`
/// in the space syntax. A number or a percentage, clamped to 0..1.
fn parse_alpha(input: &mut Parser<'_>, commas: bool) -> ParseResult<f32> {
    if input.is_exhausted() {
        return Ok(1.0);
    }
    if commas {
        input.expect_comma()?;
    } else {
        input.expect_delim('/')?;
    }
    let alpha = match *input.next()? {
        Token::Number { value, .. } => value,
        Token::Percentage { unit_value, .. } => unit_value,
        _ => return invalid(),
    };
    Ok(alpha.clamp(0.0, 1.0))
}

/// The CSS Color 4 conversion: `hue` in degrees, the rest 0..1.
fn hsl_to_rgb(hue: f32, saturation: f32, lightness: f32) -> (f32, f32, f32) {
    let hue = hue.rem_euclid(360.0);
    let channel = |n: f32| {
        let k = (n + hue / 30.0) % 12.0;
        let a = saturation * lightness.min(1.0 - lightness);
        lightness - a * (k - 3.0).min(9.0 - k).clamp(-1.0, 1.0)
    };
    (channel(0.0), channel(8.0), channel(4.0))
}

/// Parses `font-family`: a comma list of quoted names or runs of idents.
/// Generic families are kept lowercased so `text` can recognise them.
pub(crate) fn parse_font_family(input: &mut Parser<'_>) -> ParseResult<Vec<Atom>> {
    input.parse_comma_separated(|input| {
        if let Ok(name) = input.try_parse(|i| i.expect_string_cloned()) {
            return Ok(Atom::from(&*name));
        }
        let mut words: Vec<String> = vec![input.expect_ident()?.to_string()];
        while let Ok(word) = input.try_parse(|i| i.expect_ident_cloned()) {
            words.push(word.to_string());
        }
        if let [word] = words.as_slice() {
            let lower = word.to_ascii_lowercase();
            if GENERIC_FAMILIES.contains(&lower.as_str()) {
                return Ok(Atom::from(lower));
            }
            // A CSS wide keyword cannot name a family.
            if matches!(lower.as_str(), "inherit" | "initial" | "unset" | "default") {
                return invalid();
            }
        }
        Ok(Atom::from(words.join(" ")))
    })
}

/// The generic family keywords.
const GENERIC_FAMILIES: &[&str] = &[
    "serif",
    "sans-serif",
    "monospace",
    "cursive",
    "fantasy",
    "system-ui",
];

/// Parses `url(...)` or `url("...")` and resolves it against `base`. A URL
/// that cannot be resolved fails the parse.
pub(crate) fn parse_url(
    input: &mut Parser<'_>,
    base: Option<&url::Url>,
) -> ParseResult<std::rc::Rc<str>> {
    let text = input.expect_url()?;
    let resolved = match base {
        Some(base) => base.join(&text),
        None => url::Url::parse(&text),
    };
    match resolved {
        Ok(url) => Ok(std::rc::Rc::from(url.as_str())),
        Err(_) => invalid(),
    }
}

/// Parses one of `keywords`, ASCII case insensitively, giving its value.
pub(crate) fn parse_keyword<T: Clone>(
    input: &mut Parser<'_>,
    keywords: &[(&str, T)],
) -> ParseResult<T> {
    let ident = input.expect_ident()?;
    keywords
        .iter()
        .find(|(name, _)| ident.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
        .ok_or(ParseError::unexpected_token())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn color(text: &str) -> Option<SpecifiedColor> {
        let mut parser = Parser::new(text);
        parser.parse_entirely(|p| parse_color(p)).ok()
    }

    fn rgba(text: &str) -> [u8; 4] {
        match color(text) {
            Some(SpecifiedColor::Rgba(c)) => {
                [c.r, c.g, c.b, c.a].map(|v| (v * 255.0).round() as u8)
            }
            other => panic!("{text} parsed as {other:?}"),
        }
    }

    #[test]
    fn every_color_syntax_resolves() {
        // AC-20
        assert_eq!(rgba("rebeccapurple"), [102, 51, 153, 255]);
        assert_eq!(rgba("RED"), [255, 0, 0, 255]);
        assert_eq!(rgba("transparent"), [0, 0, 0, 0]);
        assert_eq!(rgba("#0f0"), [0, 255, 0, 255]);
        assert_eq!(rgba("#0f08"), [0, 255, 0, 136]);
        assert_eq!(rgba("#336699"), [51, 102, 153, 255]);
        assert_eq!(rgba("#33669980"), [51, 102, 153, 128]);
        assert_eq!(rgba("rgb(10, 20, 30)"), [10, 20, 30, 255]);
        assert_eq!(rgba("rgba(10, 20, 30, 0.5)"), [10, 20, 30, 128]);
        assert_eq!(rgba("rgb(10 20 30 / 50%)"), [10, 20, 30, 128]);
        assert_eq!(rgba("rgb(100%, 0%, 0%)"), [255, 0, 0, 255]);
        assert_eq!(rgba("hsl(120, 100%, 50%)"), [0, 255, 0, 255]);
        assert_eq!(rgba("hsla(240deg 100% 50% / 0.25)"), [0, 0, 255, 64]);
        assert_eq!(rgba("hsl(0.5turn, 100%, 25%)"), [0, 128, 128, 255]);
        assert_eq!(color("currentColor"), Some(SpecifiedColor::CurrentColor));
        for bad in [
            "#12",
            "notacolor",
            "rgb(1, 2)",
            "rgb(1, 2 3)",
            "hsl(1, 2, 3)",
        ] {
            assert_eq!(color(bad), None, "{bad}");
        }
    }

    fn length(text: &str, rules: LengthRules) -> Option<SpecifiedLength> {
        let mut parser = Parser::new(text);
        parser.parse_entirely(|p| parse_length(p, rules)).ok()
    }

    #[test]
    fn lengths_keep_their_unit_until_the_cascade() {
        use SpecifiedLength as L;
        let any = LengthRules::ANY;
        assert_eq!(length("12px", any), Some(L::Px(12.0)));
        assert_eq!(length("1.5em", any), Some(L::Em(1.5)));
        assert_eq!(length("2rem", any), Some(L::Rem(2.0)));
        assert_eq!(length("50%", any), Some(L::Percent(50.0)));
        assert_eq!(length("10vw", any), Some(L::Vw(10.0)));
        assert_eq!(length("10cqw", any), Some(L::Cqw(10.0)));
        assert_eq!(length("10cqh", any), Some(L::Cqh(10.0)));
        assert_eq!(length("10cqi", any), Some(L::Cqi(10.0)));
        assert_eq!(length("10cqb", any), Some(L::Cqb(10.0)));
        assert_eq!(length("10cqmin", any), Some(L::Cqmin(10.0)));
        assert_eq!(length("10cqmax", any), Some(L::Cqmax(10.0)));
        assert_eq!(length("1in", any), Some(L::Px(96.0)));
        assert_eq!(length("12pt", any), Some(L::Px(16.0)));
        assert_eq!(length("0", any), Some(L::ZERO));
        assert_eq!(length("-4px", any), Some(L::Px(-4.0)));
        assert_eq!(length("-4px", LengthRules::NON_NEGATIVE), None);
        assert_eq!(length("10%", LengthRules::ABSOLUTE), None);
        assert_eq!(length("12", any), None, "only zero may drop its unit");
        assert!(matches!(
            length("calc(1px + 2px)", any),
            Some(SpecifiedLength::Calc(_))
        ));
        assert!(matches!(
            length("min(10px, 20%)", any),
            Some(SpecifiedLength::Calc(_))
        ));
        assert!(matches!(
            length("max(1em, 2rem)", any),
            Some(SpecifiedLength::Calc(_))
        ));
        assert!(matches!(
            length("clamp(100px, 50vw, 200px)", any),
            Some(SpecifiedLength::Calc(_))
        ));
        // Deeper than 32 nested functions is invalid at parse time.
        let deep = "calc(".repeat(33) + "0px" + &")".repeat(33);
        assert_eq!(length(&deep, any), None);
    }

    #[test]
    fn calc_family_parses_minus_and_nested() {
        // Subtraction becomes a product by -1; `*` and `/` scale terms.
        let any = LengthRules::ANY;
        assert!(matches!(
            length("calc(10px - 3px)", any),
            Some(SpecifiedLength::Calc(_))
        ));
        assert!(matches!(
            length("calc(2px * 3)", any),
            Some(SpecifiedLength::Calc(_))
        ));
        assert!(matches!(
            length("calc(10px / 2)", any),
            Some(SpecifiedLength::Calc(_))
        ));
        assert!(matches!(
            length("min(calc(1px + 2px), 50%)", any),
            Some(SpecifiedLength::Calc(_))
        ));
        // Non-math functions still fail.
        assert_eq!(length("foo(1px)", any), None);
    }

    #[test]
    fn calc_parens_and_depth_limit() {
        // AC-8: parentheses parse, and depth counts once per nesting level.
        let any = LengthRules::ANY;
        assert!(matches!(
            length("calc((50% - 10px) / 2)", any),
            Some(SpecifiedLength::Calc(_))
        ));
        assert!(matches!(
            length("calc((calc(10px + 2px) * 3))", any),
            Some(SpecifiedLength::Calc(_))
        ));
        // `value * number` and `number * value` both work; `/` scales by the
        // reciprocal.
        assert!(matches!(
            length("calc(2 * 10px)", any),
            Some(SpecifiedLength::Calc(_))
        ));
        assert!(matches!(
            length("calc(100px / 2)", any),
            Some(SpecifiedLength::Calc(_))
        ));
        // 17 to 32 nested calc() levels are valid; 33 is not.
        for n in 17..=32 {
            let deep = "calc(".repeat(n) + "0px" + &")".repeat(n);
            assert!(length(&deep, any).is_some(), "depth {n} should be valid");
        }
        let too_deep = "calc(".repeat(33) + "0px" + &")".repeat(33);
        assert_eq!(length(&too_deep, any), None);
    }

    #[test]
    fn font_families_keep_names_and_lowercase_generics() {
        let mut parser = Parser::new("\"Iowan Old Style\", Palatino Linotype, SERIF");
        let families = parse_font_family(&mut parser).unwrap();
        assert_eq!(
            families,
            ["Iowan Old Style", "Palatino Linotype", "serif"].map(Atom::from)
        );
    }

    #[test]
    fn legacy_color_parses_named_and_hex_and_refuses_invalid() {
        // Named colors, case insensitive.
        assert_eq!(
            parse_legacy_color("red"),
            Some(SpecifiedColor::Rgba(Color::rgb8(255, 0, 0)))
        );
        assert_eq!(
            parse_legacy_color("RED"),
            Some(SpecifiedColor::Rgba(Color::rgb8(255, 0, 0)))
        );
        // #rgb and #rrggbb.
        assert_eq!(
            parse_legacy_color("#0f0"),
            Some(SpecifiedColor::Rgba(Color::rgb8(0, 255, 0)))
        );
        assert_eq!(
            parse_legacy_color("#336699"),
            Some(SpecifiedColor::Rgba(Color::rgb8(51, 102, 153)))
        );
        // Invalid hex falls back to the name after the #.
        assert_eq!(parse_legacy_color("#zzz"), None);
        // Empty and transparent give no hint.
        assert_eq!(parse_legacy_color(""), None);
        assert_eq!(parse_legacy_color("transparent"), None);
        // Legacy odd forms: strip non alphanumeric and retry.
        assert_eq!(
            parse_legacy_color("  red!  "),
            Some(SpecifiedColor::Rgba(Color::rgb8(255, 0, 0)))
        );
    }
}
