//! `@font-face` rules: the descriptors spec 0006 reads (`font-family`,
//! `src`, `font-weight`, `font-style`), parsed into [`FontFaceRule`]
//! (spec 0006, AC-10).
//!
//! `unicode-range`, `font-display` and `font-stretch` parse and are
//! ignored. A rule without a family or a usable `src` is dropped.

use std::rc::Rc;

use base::atoms::Atom;
use cssparser::{
    AtRuleParser, CowRcStr, DeclarationParser, ParseError, Parser, ParserState,
    QualifiedRuleParser, RuleBodyItemParser, RuleBodyParser, Token,
};
use tracing::debug;

use crate::FontStyle;
use crate::media::MediaList;
use crate::properties::ParseContext;
use crate::values::{ParseResult, invalid, parse_keyword, parse_url};

/// One `src` entry that names a file to fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontSource {
    /// The absolute URL, resolved against the sheet's.
    pub url: Rc<str>,
    /// The `format()` hint, lowercased, if any.
    pub format: Option<Atom>,
}

impl FontSource {
    /// Whether the hint names a format the engine cannot load, so the
    /// source is skipped without a fetch (spec 0006, AC-10).
    pub fn is_unsupported(&self) -> bool {
        self.format
            .as_deref()
            .is_some_and(|format| matches!(format, "woff" | "woff2"))
    }
}

/// One `@font-face` rule.
#[derive(Debug, Clone, PartialEq)]
pub struct FontFaceRule {
    /// The family name the rule defines, as written (matching ignores ASCII
    /// case).
    pub family: Atom,
    /// The `url()` sources in order; `local()` ones are left out.
    pub sources: Vec<FontSource>,
    /// The weight range, low to high, each 1 to 1000. One weight is a range
    /// of one.
    pub weight: (u16, u16),
    /// The face's style.
    pub style: FontStyle,
    /// The `@media` lists around it, outermost first; all must match.
    pub(crate) media: Rc<[Rc<MediaList>]>,
}

/// Parses an `@font-face` block. `None` when the rule lacks a family or a
/// `url()` source.
pub(crate) fn parse_font_face(
    input: &mut Parser<'_>,
    context: &ParseContext,
    media: Rc<[Rc<MediaList>]>,
) -> Option<FontFaceRule> {
    let mut parser = Descriptors {
        context,
        family: None,
        sources: None,
        weight: (400, 400),
        style: FontStyle::Normal,
    };
    for result in RuleBodyParser::new(input, &mut parser) {
        if let Err((_, source, _)) = result {
            debug!(descriptor = source, "font face descriptor dropped");
        }
    }
    let (Some(family), Some(sources)) = (parser.family, parser.sources) else {
        debug!("font face without font-family or src dropped");
        return None;
    };
    if sources.is_empty() {
        debug!(%family, "font face without a url() source dropped");
        return None;
    }
    Some(FontFaceRule {
        family,
        sources,
        weight: parser.weight,
        style: parser.style,
        media,
    })
}

struct Descriptors<'a> {
    context: &'a ParseContext,
    family: Option<Atom>,
    sources: Option<Vec<FontSource>>,
    weight: (u16, u16),
    style: FontStyle,
}

impl<'i> DeclarationParser<'i> for Descriptors<'_> {
    type Declaration = ();
    type Error = ();

    fn parse_value(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i>,
        _: &ParserState,
    ) -> Result<(), ParseError<()>> {
        let name = name.to_ascii_lowercase();
        match name.as_str() {
            "font-family" => self.family = Some(parse_family_name(input)?),
            "src" => self.sources = Some(parse_sources(input, self.context)),
            "font-weight" => self.weight = parse_weight_range(input)?,
            "font-style" => self.style = parse_style(input)?,
            // Read and ignored (AC-10).
            "unicode-range" | "font-display" | "font-stretch" => while input.next().is_ok() {},
            _ => return invalid(),
        }
        input.expect_exhausted()?;
        Ok(())
    }
}

impl<'i> AtRuleParser<'i> for Descriptors<'_> {
    type Prelude = ();
    type AtRule = ();
    type Error = ();
}

impl<'i> QualifiedRuleParser<'i> for Descriptors<'_> {
    type Prelude = ();
    type QualifiedRule = ();
    type Error = ();
}

impl<'i> RuleBodyItemParser<'i, (), ()> for Descriptors<'_> {
    fn parse_declarations(&self) -> bool {
        true
    }

    fn parse_qualified(&self) -> bool {
        false
    }
}

/// One family name: a string, or identifiers joined by spaces. A generic
/// family or CSS wide keyword cannot name a face.
fn parse_family_name(input: &mut Parser<'_>) -> ParseResult<Atom> {
    if let Ok(name) = input.try_parse(|i| i.expect_string_cloned()) {
        return Ok(Atom::from(&*name));
    }
    let mut words: Vec<String> = vec![input.expect_ident()?.to_string()];
    while let Ok(word) = input.try_parse(|i| i.expect_ident_cloned()) {
        words.push(word.to_string());
    }
    if let [word] = words.as_slice() {
        let lower = word.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "serif"
                | "sans-serif"
                | "monospace"
                | "cursive"
                | "fantasy"
                | "system-ui"
                | "inherit"
                | "initial"
                | "unset"
                | "default"
        ) {
            return invalid();
        }
    }
    Ok(Atom::from(words.join(" ")))
}

/// The `src` list: each `url()` with its `format()` hint. `local()` and
/// entries that fail to parse are left out.
fn parse_sources(input: &mut Parser<'_>, context: &ParseContext) -> Vec<FontSource> {
    input
        .parse_comma_separated_ignoring_errors(|input| parse_source(input, context))
        .into_iter()
        .flatten()
        .collect()
}

fn parse_source<'i>(
    input: &mut Parser<'i>,
    context: &ParseContext,
) -> ParseResult<Option<FontSource>> {
    if input
        .try_parse(|i| i.expect_function_matching("local"))
        .is_ok()
    {
        input.parse_nested_block(|i| {
            while i.next().is_ok() {}
            Ok::<_, ParseError<()>>(())
        })?;
        return Ok(None);
    }
    let url = parse_url(input, context.base.as_deref())?;
    let mut format = None;
    while let Ok(function) = input.try_parse(|i| match i.next()? {
        Token::Function(name) => Ok(name.to_ascii_lowercase()),
        _ => invalid(),
    }) {
        input.parse_nested_block(|i| {
            if function == "format" {
                let hint = match i.next()? {
                    Token::QuotedString(s) | Token::Ident(s) => s.to_ascii_lowercase(),
                    _ => return invalid(),
                };
                format = Some(Atom::from(hint));
            }
            // `tech()` and the rest are read and ignored.
            while i.next().is_ok() {}
            Ok(())
        })?;
    }
    Ok(Some(FontSource { url, format }))
}

/// `font-weight`: `auto`, one weight, or a range of two, swapped when
/// written high to low.
fn parse_weight_range(input: &mut Parser<'_>) -> ParseResult<(u16, u16)> {
    if input.try_parse(|i| i.expect_ident_matching("auto")).is_ok() {
        return Ok((1, 1000));
    }
    let low = parse_weight(input)?;
    let high = input.try_parse(parse_weight).unwrap_or(low);
    Ok((low.min(high), low.max(high)))
}

fn parse_weight(input: &mut Parser<'_>) -> ParseResult<u16> {
    if let Ok(weight) = input.try_parse(|i| parse_keyword(i, &[("normal", 400), ("bold", 700)])) {
        return Ok(weight);
    }
    let weight = input.expect_number()?;
    if !(1.0..=1000.0).contains(&weight) {
        return invalid();
    }
    Ok(weight.round() as u16)
}

/// `font-style`: `normal`, `italic`, or `oblique` with up to two angles,
/// which are read and ignored.
fn parse_style(input: &mut Parser<'_>) -> ParseResult<FontStyle> {
    let style = parse_keyword(
        input,
        &[
            ("normal", FontStyle::Normal),
            ("italic", FontStyle::Italic),
            ("oblique", FontStyle::Oblique),
        ],
    )?;
    if style == FontStyle::Oblique {
        for _ in 0..2 {
            let _ = input.try_parse(|i| match *i.next()? {
                Token::Dimension { .. } => Ok(()),
                _ => invalid(),
            });
        }
    }
    Ok(style)
}

#[cfg(test)]
mod tests {
    use crate::sheet::{Origin, parse_stylesheet};

    use super::*;

    fn faces(css: &str) -> Vec<FontFaceRule> {
        parse_stylesheet(
            css,
            Origin::Author,
            Some("https://example.com/css/site.css"),
        )
        .font_faces()
        .to_vec()
    }

    #[test]
    fn descriptors_parse_into_a_rule() {
        // Spec 0006, AC-10.
        let faces = faces(
            r#"@font-face {
                font-family: "My Icons";
                src: local(Icons), url(../fonts/icons.woff2) format("woff2"),
                     url(icons.ttf) format(truetype) tech(variations), bogus(1);
                font-weight: 700 300;
                font-style: oblique 10deg 20deg;
                unicode-range: U+0-7F;
                font-display: swap;
                font-stretch: condensed;
            }
            p { color: red }"#,
        );
        assert_eq!(faces.len(), 1);
        let face = &faces[0];
        assert_eq!(&*face.family, "My Icons");
        assert_eq!(face.weight, (300, 700), "a reversed range is swapped");
        assert_eq!(face.style, FontStyle::Oblique);
        let urls: Vec<_> = face.sources.iter().map(|s| &*s.url).collect();
        assert_eq!(
            urls,
            [
                "https://example.com/fonts/icons.woff2",
                "https://example.com/css/icons.ttf"
            ]
        );
        assert!(face.sources[0].is_unsupported());
        assert_eq!(face.sources[1].format.as_deref(), Some("truetype"));
        assert!(!face.sources[1].is_unsupported());
    }

    #[test]
    fn weights_default_and_keywords() {
        let one = |weight: &str| {
            let css = format!("@font-face {{ font-family: A; src: url(a.ttf); {weight} }}");
            faces(&css).first().map(|face| face.weight)
        };
        assert_eq!(one(""), Some((400, 400)));
        assert_eq!(one("font-weight: bold"), Some((700, 700)));
        assert_eq!(one("font-weight: auto"), Some((1, 1000)));
        assert_eq!(one("font-weight: 100 900"), Some((100, 900)));
        // An invalid descriptor is dropped; the rule keeps the default.
        assert_eq!(one("font-weight: bolder"), Some((400, 400)));
        assert_eq!(one("font-weight: 1200"), Some((400, 400)));
    }

    #[test]
    fn rules_without_family_or_url_source_are_dropped() {
        assert!(faces("@font-face { src: url(a.ttf) }").is_empty());
        assert!(faces("@font-face { font-family: A }").is_empty());
        assert!(faces("@font-face { font-family: A; src: local(A) }").is_empty());
        assert!(faces("@font-face { font-family: serif; src: url(a.ttf) }").is_empty());
        assert_eq!(
            faces("@font-face { font-family: Two Words; src: url(a.ttf) }").len(),
            1
        );
    }

    #[test]
    fn faces_inside_media_keep_their_media_and_later_rules_follow() {
        let faces = faces(
            "@font-face { font-family: A; src: url(a.ttf) }
             @media (min-width: 600px) {
               @font-face { font-family: B; src: url(b.ttf) }
             }",
        );
        assert_eq!(faces.len(), 2);
        assert!(faces[0].media.is_empty());
        assert_eq!(faces[1].media.len(), 1);
    }

    #[test]
    fn the_stylist_lists_the_faces_whose_media_match_in_cascade_order() {
        let sheet = Rc::new(parse_stylesheet(
            "@font-face { font-family: A; src: url(a.ttf) }
             @media (min-width: 600px) {
               @font-face { font-family: B; src: url(b.ttf) }
             }",
            Origin::Author,
            Some("https://example.com/"),
        ));
        let names = |width: f32| {
            let device = crate::Device {
                width,
                height: 600.0,
            };
            crate::Stylist::new(&[Rc::clone(&sheet)], device)
                .font_faces()
                .iter()
                .map(|face| face.family.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(800.0), ["A", "B"]);
        assert_eq!(names(400.0), ["A"]);
    }
}
