//! Media queries: `all`, `screen`, `print` and widths and heights against
//! the viewport, plus OS preference features (spec 0004, child 0003,
//! Media queries, AC-19; spec 0010, AC-12, AC-13).

use cssparser::Parser;
use serde::{Deserialize, Serialize};

use crate::values::{LengthRules, ParseResult, SpecifiedLength, invalid, parse_length};

/// What media queries are evaluated against.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Device {
    /// The viewport width in CSS px.
    pub width: f32,
    /// The viewport height in CSS px.
    pub height: f32,
    /// The OS color scheme the page sees (spec 0010, AC-13).
    pub color_scheme: ColorScheme,
    /// Whether the user asked the OS to reduce motion (spec 0010, AC-13).
    pub reduced_motion: bool,
}

/// The OS color scheme (spec 0010, AC-13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ColorScheme {
    /// The light color scheme.
    #[default]
    Light,
    /// The dark color scheme.
    Dark,
}

/// The font size `em` and `rem` mean inside a media query: the initial
/// `medium`, never the page's own.
const MEDIA_EM_PX: f32 = 16.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MediaType {
    All,
    Screen,
    /// `print`, or any type this browser is not.
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    Width,
    Height,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Range {
    Min,
    Max,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Feature {
    Width {
        axis: Axis,
        min: Option<f32>,
        max: Option<f32>,
    },
    Orientation(Orientation),
    AspectRatio(f32),
    PrefersColorScheme(ColorScheme),
    PrefersReducedMotion(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Orientation {
    Portrait,
    Landscape,
}

#[derive(Debug, Clone, PartialEq)]
struct MediaQuery {
    negated: bool,
    media_type: MediaType,
    features: Vec<Feature>,
}

/// A comma list of media queries. Empty matches everything; a query that
/// fails to parse becomes `not all`, so it never matches.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MediaList {
    /// `None` is a query that failed to parse.
    queries: Vec<Option<MediaQuery>>,
}

impl MediaList {
    /// Parses a `media` attribute value.
    pub fn parse(text: &str) -> Self {
        let mut parser = Parser::new(text);
        Self::parse_from(&mut parser)
    }

    /// Parses the prelude of `@media`.
    pub(crate) fn parse_from(input: &mut Parser<'_>) -> Self {
        if input.is_exhausted() {
            return Self::default();
        }
        let queries = input.parse_comma_separated_ignoring_errors(|input| {
            let query = input.try_parse(|i| i.parse_entirely(parse_query)).ok();
            if query.is_none() {
                // Consume the rest, so the bad query stays in the list as
                // `not all` instead of being dropped (an empty list matches).
                while input.next().is_ok() {}
            }
            Ok::<_, cssparser::ParseError<()>>(query)
        });
        Self { queries }
    }

    /// Whether the list matches `device`.
    pub fn evaluate(&self, device: &Device) -> bool {
        self.queries.is_empty()
            || self
                .queries
                .iter()
                .any(|query| query.as_ref().is_some_and(|q| q.evaluate(device)))
    }
}

fn parse_query(input: &mut Parser<'_>) -> ParseResult<MediaQuery> {
    let mut negated = false;
    let mut media_type = None;
    if let Ok(ident) = input.try_parse(|i| i.expect_ident_cloned()) {
        let lower = ident.to_ascii_lowercase();
        let named = match lower.as_str() {
            "not" | "only" => {
                negated = lower == "not";
                input.expect_ident_cloned()?.to_ascii_lowercase()
            }
            _ => lower,
        };
        media_type = Some(match named.as_str() {
            "all" => MediaType::All,
            "screen" => MediaType::Screen,
            // The reserved words cannot name a type.
            "and" | "or" | "not" | "only" | "layer" => return invalid(),
            _ => MediaType::Other,
        });
    }
    let mut features = Vec::new();
    loop {
        if media_type.is_some() || !features.is_empty() {
            if input.is_exhausted() {
                break;
            }
            let and = input.expect_ident()?;
            if !and.eq_ignore_ascii_case("and") {
                return invalid();
            }
        }
        input.expect_parenthesis_block()?;
        features.push(input.parse_nested_block(parse_feature)?);
        if input.is_exhausted() {
            break;
        }
    }
    if media_type.is_none() && features.is_empty() {
        return invalid();
    }
    Ok(MediaQuery {
        negated,
        media_type: media_type.unwrap_or(MediaType::All),
        features,
    })
}

/// Parses a range operator: `>=`, `<=`, `>` or `<`.
fn parse_range_op(input: &mut Parser<'_>) -> ParseResult<Range> {
    if input.try_parse(|i| i.expect_delim('>')).is_ok() {
        if input.try_parse(|i| i.expect_delim('=')).is_ok() {
            return Ok(Range::Min);
        }
        return Ok(Range::Min); // `>` treated as `>=` for media queries
    }
    if input.try_parse(|i| i.expect_delim('<')).is_ok() {
        if input.try_parse(|i| i.expect_delim('=')).is_ok() {
            return Ok(Range::Max);
        }
        return Ok(Range::Max); // `<` treated as `<=` for media queries
    }
    invalid()
}

/// Parses one media feature, in either the `name: value` form or the Level 4
/// range form (`width >= 600px`, `400px <= width < 900px`). Unknown features
/// fail, which makes only their own query false (spec 0010, AC-12).
fn parse_feature(input: &mut Parser<'_>) -> ParseResult<Feature> {
    let state = input.state();
    // Try the two-sided Level 4 range form: `value operator feature operator value`
    // (e.g. `400px <= width < 900px`).
    if let Ok(first_px) = parse_length_px(input)
        && let Ok(range1) = parse_range_op(input)
        && let Ok(name) = input.try_parse(|i| i.expect_ident_cloned())
    {
        let lower = name.to_ascii_lowercase();
        if (lower == "width" || lower == "height")
            && let Ok(range2) = parse_range_op(input)
            && let Ok(second_px) = parse_length_px(input)
        {
            input.expect_exhausted()?;
            let axis = if lower == "width" {
                Axis::Width
            } else {
                Axis::Height
            };
            let (min, max) = match (range1, range2) {
                (Range::Max, Range::Max) => {
                    // `400px <= width <= 900px` (or `<` treated as `<=`)
                    (Some(first_px), Some(second_px))
                }
                (Range::Min, Range::Min) => {
                    // `400px >= width >= 900px` is meaningless
                    return invalid();
                }
                _ => return invalid(),
            };
            return Ok(Feature::Width { axis, min, max });
        }
    }
    input.reset(&state);
    // Try the single-sided range form: `feature operator value`
    // (e.g. `width >= 600px`).
    if let Ok(name) = input.try_parse(|i| i.expect_ident_cloned()) {
        let lower = name.to_ascii_lowercase();
        if let Ok(range) = parse_range_op(input)
            && let Ok(px) = parse_length_px(input)
        {
            input.expect_exhausted()?;
            let axis = match lower.as_str() {
                "width" => Axis::Width,
                "height" => Axis::Height,
                _ => return invalid(),
            };
            let (min, max) = match range {
                Range::Min => (Some(px), None),
                Range::Max => (None, Some(px)),
            };
            return Ok(Feature::Width { axis, min, max });
        }
    }
    input.reset(&state);
    // Fall back to the `name: value` form.
    let name = input.expect_ident()?.to_ascii_lowercase();
    match name.as_str() {
        "width" => {
            input.expect_colon()?;
            let px = parse_length_px(input)?;
            input.expect_exhausted()?;
            Ok(Feature::Width {
                axis: Axis::Width,
                min: Some(px),
                max: Some(px),
            })
        }
        "min-width" => {
            input.expect_colon()?;
            let px = parse_length_px(input)?;
            input.expect_exhausted()?;
            Ok(Feature::Width {
                axis: Axis::Width,
                min: Some(px),
                max: None,
            })
        }
        "max-width" => {
            input.expect_colon()?;
            let px = parse_length_px(input)?;
            input.expect_exhausted()?;
            Ok(Feature::Width {
                axis: Axis::Width,
                min: None,
                max: Some(px),
            })
        }
        "height" => {
            input.expect_colon()?;
            let px = parse_length_px(input)?;
            input.expect_exhausted()?;
            Ok(Feature::Width {
                axis: Axis::Height,
                min: Some(px),
                max: Some(px),
            })
        }
        "min-height" => {
            input.expect_colon()?;
            let px = parse_length_px(input)?;
            input.expect_exhausted()?;
            Ok(Feature::Width {
                axis: Axis::Height,
                min: Some(px),
                max: None,
            })
        }
        "max-height" => {
            input.expect_colon()?;
            let px = parse_length_px(input)?;
            input.expect_exhausted()?;
            Ok(Feature::Width {
                axis: Axis::Height,
                min: None,
                max: Some(px),
            })
        }
        "orientation" => {
            input.expect_colon()?;
            let ident = input.expect_ident()?.to_ascii_lowercase();
            let orientation = match ident.as_str() {
                "portrait" => Orientation::Portrait,
                "landscape" => Orientation::Landscape,
                _ => return invalid(),
            };
            input.expect_exhausted()?;
            Ok(Feature::Orientation(orientation))
        }
        "aspect-ratio" => {
            input.expect_colon()?;
            let ratio = parse_aspect_ratio(input)?;
            input.expect_exhausted()?;
            Ok(Feature::AspectRatio(ratio))
        }
        "prefers-color-scheme" => {
            input.expect_colon()?;
            let ident = input.expect_ident()?.to_ascii_lowercase();
            let scheme = match ident.as_str() {
                "light" => ColorScheme::Light,
                "dark" => ColorScheme::Dark,
                _ => return invalid(),
            };
            input.expect_exhausted()?;
            Ok(Feature::PrefersColorScheme(scheme))
        }
        "prefers-reduced-motion" => {
            input.expect_colon()?;
            let ident = input.expect_ident()?.to_ascii_lowercase();
            let reduce = match ident.as_str() {
                "reduce" => true,
                "no-preference" => false,
                _ => return invalid(),
            };
            input.expect_exhausted()?;
            Ok(Feature::PrefersReducedMotion(reduce))
        }
        _ => invalid(),
    }
}

/// Parses a media-query length: px only (em/rem resolve against the initial
/// 16 px font size, so they are already px here).
fn parse_length_px(input: &mut Parser<'_>) -> ParseResult<f32> {
    let length = parse_length(input, LengthRules::ABSOLUTE)?;
    match length {
        SpecifiedLength::Px(px) => Ok(px),
        SpecifiedLength::Em(em) | SpecifiedLength::Rem(em) => Ok(em * MEDIA_EM_PX),
        _ => invalid(),
    }
}

/// Parses an aspect ratio: `width/height` as two positive integers.
fn parse_aspect_ratio(input: &mut Parser<'_>) -> ParseResult<f32> {
    let first = input.expect_integer()?;
    input.expect_delim('/')?;
    let second = input.expect_integer()?;
    if second == 0 {
        return invalid();
    }
    input.expect_exhausted()?;
    Ok(first as f32 / second as f32)
}

impl MediaQuery {
    fn evaluate(&self, device: &Device) -> bool {
        let type_matches = match self.media_type {
            MediaType::All | MediaType::Screen => true,
            MediaType::Other => false,
        };
        let matches = type_matches && self.features.iter().all(|f| f.evaluate(device));
        matches != self.negated
    }
}

impl Feature {
    fn evaluate(&self, device: &Device) -> bool {
        match self {
            Feature::Width { axis, min, max } => {
                let actual = match axis {
                    Axis::Width => device.width,
                    Axis::Height => device.height,
                };
                min.is_none_or(|m| actual >= m) && max.is_none_or(|m| actual <= m)
            }
            Feature::Orientation(orientation) => {
                let actual = if device.width >= device.height {
                    Orientation::Landscape
                } else {
                    Orientation::Portrait
                };
                actual == *orientation
            }
            Feature::AspectRatio(ratio) => {
                let actual = device.width / device.height;
                (actual - ratio).abs() < f32::EPSILON
            }
            Feature::PrefersColorScheme(scheme) => device.color_scheme == *scheme,
            Feature::PrefersReducedMotion(reduce) => device.reduced_motion == *reduce,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDE: Device = Device {
        width: 1280.0,
        height: 800.0,
        color_scheme: ColorScheme::Light,
        reduced_motion: false,
    };
    const NARROW: Device = Device {
        width: 400.0,
        height: 800.0,
        color_scheme: ColorScheme::Light,
        reduced_motion: false,
    };

    fn on(text: &str, device: Device) -> bool {
        MediaList::parse(text).evaluate(&device)
    }

    #[test]
    fn types_features_and_logic_evaluate() {
        // AC-19
        assert!(on("", WIDE));
        assert!(on("all", WIDE));
        assert!(on("screen", WIDE));
        assert!(!on("print", WIDE));
        assert!(!on("tv", WIDE));
        assert!(on("not print", WIDE));
        assert!(!on("not screen", WIDE));
        assert!(on("only screen", WIDE));
        assert!(on("(min-width: 600px)", WIDE));
        assert!(!on("(min-width: 600px)", NARROW));
        assert!(on("screen and (max-width: 40em)", NARROW));
        assert!(!on("screen and (max-width: 40em)", WIDE));
        assert!(on("(min-width: 300px) and (max-width: 500px)", NARROW));
        assert!(on("print, (max-width: 500px)", NARROW));
        assert!(!on("print, (max-width: 500px)", WIDE));
        assert!(on("not screen and (max-width: 500px)", WIDE));
    }

    #[test]
    fn unsupported_queries_never_match_but_keep_their_neighbors() {
        assert!(!on("screen and", WIDE));
        assert!(on("(hover: hover), screen", WIDE));
    }

    #[test]
    fn new_features_evaluate() {
        // AC-12: orientation, aspect-ratio, prefers-color-scheme,
        // prefers-reduced-motion.
        let light = WIDE;
        let dark = Device {
            color_scheme: ColorScheme::Dark,
            ..light
        };
        let reduce = Device {
            reduced_motion: true,
            ..light
        };

        assert!(on("(orientation: landscape)", light));
        assert!(!on("(orientation: portrait)", light));
        assert!(on("(aspect-ratio: 8/5)", light));
        assert!(!on("(aspect-ratio: 16/9)", light));
        assert!(on("(prefers-color-scheme: light)", light));
        assert!(!on("(prefers-color-scheme: dark)", light));
        assert!(on("(prefers-color-scheme: dark)", dark));
        assert!(!on("(prefers-reduced-motion: reduce)", light));
        assert!(on("(prefers-reduced-motion: reduce)", reduce));
        assert!(on("(prefers-reduced-motion: no-preference)", light));
        // Unknown feature makes only its own query false.
        assert!(on("(unknown-feature: x), screen", light));
        assert!(!on("(unknown-feature: x)", light));
        // Range syntax from spec 0004 still works.
        assert!(on("(width >= 600px)", light));
        assert!(!on("(width >= 600px)", NARROW));
        assert!(on("(400px <= width < 900px)", NARROW));
    }
}
