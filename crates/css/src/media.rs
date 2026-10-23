//! Media queries: `all`, `screen`, `print` and widths and heights against
//! the viewport (spec 0004, child 0003, Media queries, AC-19).

use cssparser::Parser;

use crate::values::{LengthRules, ParseResult, SpecifiedLength, invalid, parse_length};

/// What media queries are evaluated against.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Device {
    /// The viewport width in CSS px.
    pub width: f32,
    /// The viewport height in CSS px.
    pub height: f32,
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
    Exact,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Feature {
    axis: Axis,
    range: Range,
    px: f32,
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

/// The inside of `(min-width: 600px)`. The range syntax (`width >= 600px`)
/// and every other feature fail, which makes the query `not all`.
fn parse_feature(input: &mut Parser<'_>) -> ParseResult<Feature> {
    let name = input.expect_ident()?.to_ascii_lowercase();
    let (range, axis) = match name.as_str() {
        "width" => (Range::Exact, Axis::Width),
        "min-width" => (Range::Min, Axis::Width),
        "max-width" => (Range::Max, Axis::Width),
        "height" => (Range::Exact, Axis::Height),
        "min-height" => (Range::Min, Axis::Height),
        "max-height" => (Range::Max, Axis::Height),
        _ => return invalid(),
    };
    input.expect_colon()?;
    let px = match parse_length(input, LengthRules::ABSOLUTE)? {
        SpecifiedLength::Px(px) => px,
        SpecifiedLength::Em(em) | SpecifiedLength::Rem(em) => em * MEDIA_EM_PX,
        _ => return invalid(),
    };
    input.expect_exhausted()?;
    Ok(Feature { axis, range, px })
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
        let actual = match self.axis {
            Axis::Width => device.width,
            Axis::Height => device.height,
        };
        match self.range {
            Range::Min => actual >= self.px,
            Range::Max => actual <= self.px,
            Range::Exact => actual == self.px,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDE: Device = Device {
        width: 1280.0,
        height: 800.0,
    };
    const NARROW: Device = Device {
        width: 400.0,
        height: 800.0,
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
        assert!(!on("(width >= 600px)", WIDE));
        assert!(!on("(orientation: landscape)", WIDE));
        assert!(!on("screen and", WIDE));
        assert!(on("(hover: hover), screen", WIDE));
    }
}
