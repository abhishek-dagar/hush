//! Media queries: `all`, `screen`, `print` and widths and heights against
//! the viewport, plus OS preference features (spec 0004, child 0003,
//! Media queries, AC-19; spec 0010, AC-12, AC-13).

use std::rc::Rc;

use cssparser::Parser;
use serde::{Deserialize, Serialize};

use crate::values::{LengthRules, ParseResult, SpecifiedLength, invalid, parse_length};
use base::atoms::Atom;

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
pub enum Axis {
    Width,
    Height,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    /// `>=`: inclusive lower bound.
    Min,
    /// `<=`: inclusive upper bound.
    Max,
    /// `>`: strict lower bound.
    StrictMin,
    /// `<`: strict upper bound.
    StrictMax,
}

impl Range {
    /// Whether this is an upper bound (`<=` or `<`).
    fn is_max(self) -> bool {
        matches!(self, Range::Max | Range::StrictMax)
    }
    /// Whether the comparison is strict (`>` or `<`).
    fn is_strict(self) -> bool {
        matches!(self, Range::StrictMin | Range::StrictMax)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Feature {
    Width {
        axis: Axis,
        /// Inclusive lower bound unless `strict`. `None` means unbounded.
        min: Option<(f32, bool)>,
        /// Inclusive upper bound unless `strict`. `None` means unbounded.
        max: Option<(f32, bool)>,
    },
    Orientation(Orientation),
    AspectRatio {
        /// `min-aspect-ratio` lower bound, `None` when not set.
        min: Option<f32>,
        /// `max-aspect-ratio` upper bound, `None` when not set.
        max: Option<f32>,
    },
    PrefersColorScheme(ColorScheme),
    PrefersReducedMotion(bool),
}

/// A `@container` size condition: `all`, `any`, `not`, or a single feature.
#[derive(Debug, Clone, PartialEq)]
pub enum SizeCondition {
    /// All of the conditions must hold (`and`).
    All(Vec<SizeCondition>),
    /// Any of the conditions must hold (`or`).
    Any(Vec<SizeCondition>),
    /// The condition must not hold (`not`).
    Not(Box<SizeCondition>),
    /// A single feature test.
    Feature(ContainerFeature),
}

/// A single `@container` size feature.
#[derive(Debug, Clone, PartialEq)]
pub enum ContainerFeature {
    Width {
        axis: Axis,
        /// Inclusive lower bound unless `strict`. `None` means unbounded.
        min: Option<(f32, bool)>,
        /// Inclusive upper bound unless `strict`. `None` means unbounded.
        max: Option<(f32, bool)>,
    },
    AspectRatio {
        /// Lower bound, `None` when not set.
        min: Option<f32>,
        /// Upper bound, `None` when not set.
        max: Option<f32>,
    },
    Orientation(Orientation),
}

impl ContainerFeature {
    /// Whether the container's size satisfies this feature.
    pub(crate) fn evaluate(&self, width: f32, height: f32) -> bool {
        match self {
            ContainerFeature::Width { axis, min, max } => {
                let actual = match axis {
                    Axis::Width => width,
                    Axis::Height => height,
                };
                let ok_min =
                    min.is_none_or(|(m, strict)| if strict { actual > m } else { actual >= m });
                let ok_max =
                    max.is_none_or(|(m, strict)| if strict { actual < m } else { actual <= m });
                ok_min && ok_max
            }
            ContainerFeature::AspectRatio { min, max } => {
                let actual = width / height;
                min.is_none_or(|m| actual >= m) && max.is_none_or(|m| actual <= m)
            }
            ContainerFeature::Orientation(orientation) => {
                let actual = if width >= height {
                    Orientation::Landscape
                } else {
                    Orientation::Portrait
                };
                actual == *orientation
            }
        }
    }
}

impl SizeCondition {
    /// Whether the container of `width` by `height` satisfies this condition.
    pub(crate) fn evaluate(&self, width: f32, height: f32) -> bool {
        match self {
            SizeCondition::All(conditions) => conditions.iter().all(|c| c.evaluate(width, height)),
            SizeCondition::Any(conditions) => conditions.iter().any(|c| c.evaluate(width, height)),
            SizeCondition::Not(condition) => !condition.evaluate(width, height),
            SizeCondition::Feature(feature) => feature.evaluate(width, height),
        }
    }
}

/// A `@container` condition: the container names to match (from
/// `@container name`) and an optional size condition (from `@container
/// ( ... )`). A rule inside `@container` applies when the element sits in an
/// ancestor container carrying one of `names` (empty = any container) whose
/// size satisfies `condition` (when present).
#[derive(Debug, Clone, PartialEq)]
pub struct ContainerCondition {
    /// The container names the element's nearest container must carry.
    pub names: Rc<[Atom]>,
    /// The size condition, `None` for a name-only `@container name`.
    pub condition: Option<SizeCondition>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
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
        match lower.as_str() {
            "not" | "only" => {
                negated = lower == "not";
                // The media type is optional: `not (width < 100px)` has none.
                if let Ok(mt) = input.try_parse(|i| i.expect_ident_cloned()) {
                    media_type = Some(match mt.to_ascii_lowercase().as_str() {
                        "all" => MediaType::All,
                        "screen" => MediaType::Screen,
                        // The reserved words cannot name a type.
                        "and" | "or" | "not" | "only" | "layer" => return invalid(),
                        _ => MediaType::Other,
                    });
                }
            }
            _ => {
                media_type = Some(match lower.as_str() {
                    "all" => MediaType::All,
                    "screen" => MediaType::Screen,
                    // The reserved words cannot name a type.
                    "and" | "or" | "not" | "only" | "layer" => return invalid(),
                    _ => MediaType::Other,
                });
            }
        }
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
        return Ok(Range::StrictMin);
    }
    if input.try_parse(|i| i.expect_delim('<')).is_ok() {
        if input.try_parse(|i| i.expect_delim('=')).is_ok() {
            return Ok(Range::Max);
        }
        return Ok(Range::StrictMax);
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
                // `400px <= width <= 900px`
                (Range::Max, Range::Max) => (Some((first_px, false)), Some((second_px, false))),
                // `400px <= width < 900px` or `400px > width > 900px` etc.
                (range1, range2) if range1.is_max() && range2.is_max() => (
                    Some((first_px, range1.is_strict())),
                    Some((second_px, range2.is_strict())),
                ),
                // `400px >= width >= 900px` is meaningless
                (Range::Min, Range::Min) | (Range::StrictMin, Range::StrictMin) => {
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
                Range::Min | Range::StrictMin => (Some((px, range.is_strict())), None),
                Range::Max | Range::StrictMax => (None, Some((px, range.is_strict()))),
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
                min: Some((px, false)),
                max: Some((px, false)),
            })
        }
        "min-width" => {
            input.expect_colon()?;
            let px = parse_length_px(input)?;
            input.expect_exhausted()?;
            Ok(Feature::Width {
                axis: Axis::Width,
                min: Some((px, false)),
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
                max: Some((px, false)),
            })
        }
        "height" => {
            input.expect_colon()?;
            let px = parse_length_px(input)?;
            input.expect_exhausted()?;
            Ok(Feature::Width {
                axis: Axis::Height,
                min: Some((px, false)),
                max: Some((px, false)),
            })
        }
        "min-height" => {
            input.expect_colon()?;
            let px = parse_length_px(input)?;
            input.expect_exhausted()?;
            Ok(Feature::Width {
                axis: Axis::Height,
                min: Some((px, false)),
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
                max: Some((px, false)),
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
            Ok(Feature::AspectRatio {
                min: Some(ratio),
                max: Some(ratio),
            })
        }
        "min-aspect-ratio" => {
            input.expect_colon()?;
            let ratio = parse_aspect_ratio(input)?;
            input.expect_exhausted()?;
            Ok(Feature::AspectRatio {
                min: Some(ratio),
                max: None,
            })
        }
        "max-aspect-ratio" => {
            input.expect_colon()?;
            let ratio = parse_aspect_ratio(input)?;
            input.expect_exhausted()?;
            Ok(Feature::AspectRatio {
                min: None,
                max: Some(ratio),
            })
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

/// Parses a `@container` size condition: `not X`, `X and Y`, `X or Y`,
/// `(X)`, or a single feature test such as `width >= 400px` or
/// `min-width: 400px` (spec 0010, AC-17).
pub(crate) fn parse_container_condition(input: &mut Parser<'_>) -> ParseResult<SizeCondition> {
    parse_container_condition_or(input)
}

fn parse_container_condition_or(input: &mut Parser<'_>) -> ParseResult<SizeCondition> {
    let mut result = parse_container_condition_and(input)?;
    loop {
        input.skip_whitespace();
        if input.try_parse(|i| i.expect_ident_matching("or")).is_ok() {
            let rhs = parse_container_condition_and(input)?;
            result = SizeCondition::Any(vec![result, rhs]);
        } else {
            break;
        }
    }
    Ok(result)
}

fn parse_container_condition_and(input: &mut Parser<'_>) -> ParseResult<SizeCondition> {
    let mut result = parse_container_condition_not(input)?;
    loop {
        input.skip_whitespace();
        if input.try_parse(|i| i.expect_ident_matching("and")).is_ok() {
            let rhs = parse_container_condition_not(input)?;
            result = SizeCondition::All(vec![result, rhs]);
        } else {
            break;
        }
    }
    Ok(result)
}

fn parse_container_condition_not(input: &mut Parser<'_>) -> ParseResult<SizeCondition> {
    input.skip_whitespace();
    if input.try_parse(|i| i.expect_ident_matching("not")).is_ok() {
        let result = parse_container_condition_not(input)?;
        return Ok(SizeCondition::Not(Box::new(result)));
    }
    if input.try_parse(|i| i.expect_parenthesis_block()).is_ok() {
        let result = input.parse_nested_block(|input| parse_container_condition(input))?;
        return Ok(result);
    }
    Ok(SizeCondition::Feature(parse_container_feature(input)?))
}

fn parse_container_feature(input: &mut Parser<'_>) -> ParseResult<ContainerFeature> {
    // Try the two-sided Level 4 range form: `400px <= width < 900px`.
    let state = input.state();
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
                (Range::Max, Range::Max) => (Some((first_px, false)), Some((second_px, false))),
                (range1, range2) if range1.is_max() && range2.is_max() => (
                    Some((first_px, range1.is_strict())),
                    Some((second_px, range2.is_strict())),
                ),
                _ => return invalid(),
            };
            return Ok(ContainerFeature::Width { axis, min, max });
        }
    }
    input.reset(&state);
    // Try the single-sided range form: `width >= 400px`.
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
                Range::Min | Range::StrictMin => (Some((px, range.is_strict())), None),
                Range::Max | Range::StrictMax => (None, Some((px, range.is_strict()))),
            };
            return Ok(ContainerFeature::Width { axis, min, max });
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
            Ok(ContainerFeature::Width {
                axis: Axis::Width,
                min: Some((px, false)),
                max: Some((px, false)),
            })
        }
        "min-width" => {
            input.expect_colon()?;
            let px = parse_length_px(input)?;
            input.expect_exhausted()?;
            Ok(ContainerFeature::Width {
                axis: Axis::Width,
                min: Some((px, false)),
                max: None,
            })
        }
        "max-width" => {
            input.expect_colon()?;
            let px = parse_length_px(input)?;
            input.expect_exhausted()?;
            Ok(ContainerFeature::Width {
                axis: Axis::Width,
                min: None,
                max: Some((px, false)),
            })
        }
        "height" => {
            input.expect_colon()?;
            let px = parse_length_px(input)?;
            input.expect_exhausted()?;
            Ok(ContainerFeature::Width {
                axis: Axis::Height,
                min: Some((px, false)),
                max: Some((px, false)),
            })
        }
        "min-height" => {
            input.expect_colon()?;
            let px = parse_length_px(input)?;
            input.expect_exhausted()?;
            Ok(ContainerFeature::Width {
                axis: Axis::Height,
                min: Some((px, false)),
                max: None,
            })
        }
        "max-height" => {
            input.expect_colon()?;
            let px = parse_length_px(input)?;
            input.expect_exhausted()?;
            Ok(ContainerFeature::Width {
                axis: Axis::Height,
                min: None,
                max: Some((px, false)),
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
            Ok(ContainerFeature::Orientation(orientation))
        }
        "aspect-ratio" => {
            input.expect_colon()?;
            let ratio = parse_aspect_ratio(input)?;
            input.expect_exhausted()?;
            Ok(ContainerFeature::AspectRatio {
                min: Some(ratio),
                max: Some(ratio),
            })
        }
        "min-aspect-ratio" => {
            input.expect_colon()?;
            let ratio = parse_aspect_ratio(input)?;
            input.expect_exhausted()?;
            Ok(ContainerFeature::AspectRatio {
                min: Some(ratio),
                max: None,
            })
        }
        "max-aspect-ratio" => {
            input.expect_colon()?;
            let ratio = parse_aspect_ratio(input)?;
            input.expect_exhausted()?;
            Ok(ContainerFeature::AspectRatio {
                min: None,
                max: Some(ratio),
            })
        }
        _ => invalid(),
    }
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
                let ok_min =
                    min.is_none_or(|(m, strict)| if strict { actual > m } else { actual >= m });
                let ok_max =
                    max.is_none_or(|(m, strict)| if strict { actual < m } else { actual <= m });
                ok_min && ok_max
            }
            Feature::Orientation(orientation) => {
                let actual = if device.width >= device.height {
                    Orientation::Landscape
                } else {
                    Orientation::Portrait
                };
                actual == *orientation
            }
            Feature::AspectRatio { min, max } => {
                let actual = device.width / device.height;
                min.is_none_or(|m| actual >= m) && max.is_none_or(|m| actual <= m)
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

    #[test]
    fn strict_range_and_not_without_type() {
        // AC-12: `>` and `<` are strict, not inclusive.
        let at800 = Device {
            width: 800.0,
            height: 600.0,
            ..WIDE
        };
        assert!(!on("(width > 800px)", at800));
        assert!(!on("(width < 800px)", at800));
        assert!(on("(width > 799px)", at800));
        assert!(on("(width < 801px)", at800));
        // Inclusive forms still work.
        assert!(on("(width >= 800px)", at800));
        assert!(on("(width <= 800px)", at800));
        // `not` without a media type negates the feature alone.
        assert!(on("not (width < 100px)", WIDE));
        let tiny = Device {
            width: 50.0,
            height: 50.0,
            ..WIDE
        };
        assert!(on("(width < 100px)", tiny));
        assert!(!on("not (width < 100px)", tiny));
    }

    #[test]
    fn aspect_ratio_range() {
        // AC-12: min-aspect-ratio / max-aspect-ratio.
        let d800x600 = Device {
            width: 800.0,
            height: 600.0,
            ..WIDE
        };
        assert!(on("(min-aspect-ratio: 4/3)", d800x600));
        assert!(on("(max-aspect-ratio: 4/3)", d800x600));
        assert!(!on("(min-aspect-ratio: 16/9)", d800x600));
        assert!(!on("(max-aspect-ratio: 1/1)", d800x600));
        // Plain aspect-ratio is still exact.
        assert!(on("(aspect-ratio: 4/3)", d800x600));
        assert!(!on("(aspect-ratio: 16/9)", d800x600));
    }
}
