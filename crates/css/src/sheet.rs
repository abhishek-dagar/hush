//! Style sheets: rules from `cssparser`'s rule and declaration parsers,
//! with CSS Syntax error recovery (spec 0004, child 0003, Parsing).

use std::rc::Rc;

use base::atoms::Atom;
use cssparser::{
    AtRuleParser, CowRcStr, DeclarationParser, ParseError, Parser, ParserState,
    QualifiedRuleParser, RuleBodyItemParser, RuleBodyParser, StyleSheetParser, Token,
};
use encoding_rs::Encoding;
use tracing::debug;

use crate::font_face::{FontFaceRule, parse_font_face};
use crate::media::MediaList;
use crate::properties::{
    Declaration, ParseContext, PropertySyntax, parse_declaration, parse_property_syntax,
};
use crate::selector::{Selector, parse_selector_list};

/// Where a sheet comes from, which decides its place in the cascade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The browser's own sheet.
    Ua,
    /// The page's sheets and `style` attributes.
    Author,
}

/// One style rule.
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub(crate) selectors: Rc<[Selector]>,
    pub(crate) declarations: Rc<[Declaration]>,
    /// The `@layer` name it belongs to, `None` for unlayered. Resolved to a
    /// `LayerId` by the stylist in first-appearance order (spec 0010, AC-9).
    pub(crate) layer: Option<Atom>,
    /// The `@media` lists around it, outermost first; all must match.
    pub(crate) media: Rc<[Rc<MediaList>]>,
}

/// A registered custom property from `@property` (spec 0010, AC-10).
#[derive(Debug, Clone, PartialEq)]
pub struct PropertyRule {
    /// The custom property name, without the leading `--`.
    pub name: Atom,
    /// The accepted syntax, `*` for anything.
    pub syntax: PropertySyntax,
    /// Whether the property inherits from its parent. Custom properties
    /// inherit by default.
    pub inherits: bool,
    /// The initial value, as text. Used when a declared value does not match
    /// the syntax and the property does not inherit.
    pub initial: Rc<str>,
}

/// A parsed style sheet.
#[derive(Debug, Clone, PartialEq)]
pub struct Stylesheet {
    /// The sheet's origin.
    pub origin: Origin,
    /// The style rules, in source order.
    pub(crate) rules: Vec<Rule>,
    /// The `@font-face` rules, in source order (spec 0006, AC-10).
    pub(crate) font_faces: Vec<FontFaceRule>,
    /// The registered custom properties from `@property`, in source order
    /// (spec 0010, AC-10). Later rules for the same name win.
    pub(crate) properties: Vec<PropertyRule>,
    /// The `media` attribute of the `<link>` or `<style>` it came from.
    pub media: Option<Rc<MediaList>>,
    /// Whether any value may read the viewport size.
    pub(crate) uses_viewport_units: bool,
}

impl Stylesheet {
    /// A sheet with no rules.
    pub fn empty(origin: Origin) -> Self {
        Self {
            origin,
            rules: Vec::new(),
            font_faces: Vec::new(),
            properties: Vec::new(),
            media: None,
            uses_viewport_units: false,
        }
    }

    /// How many style rules it holds.
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Whether it holds no style rules.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// The sheet's `@font-face` rules in source order, top level and inside
    /// `@media` alike (spec 0006, AC-10).
    pub fn font_faces(&self) -> &[FontFaceRule] {
        &self.font_faces
    }
}

/// Parses `text` as a sheet of `origin`, resolving `url()` against
/// `base_url`. Never fails: invalid parts are dropped (CSS Syntax).
pub fn parse_stylesheet(text: &str, origin: Origin, base_url: Option<&str>) -> Stylesheet {
    let context = ParseContext {
        base: base_url
            .and_then(|base| url::Url::parse(base).ok())
            .map(Rc::new),
        ..ParseContext::default()
    };
    let mut parser = TopLevel {
        context: &context,
        media: Vec::new(),
        layer: Vec::new(),
        rules: Vec::new(),
        font_faces: Vec::new(),
        properties: Vec::new(),
    };
    let mut input = Parser::new(text);
    for result in StyleSheetParser::new(&mut input, &mut parser) {
        if let Err((_, source, _)) = result {
            debug!(rule = source, "css rule dropped");
        }
    }
    let TopLevel {
        rules,
        font_faces,
        properties,
        ..
    } = parser;
    let uses_viewport_units = rules
        .iter()
        .flat_map(|rule| rule.declarations.iter())
        .any(Declaration::uses_viewport);
    Stylesheet {
        origin,
        rules,
        font_faces,
        properties,
        media: None,
        uses_viewport_units,
    }
}

/// Parses a `style` attribute's declarations, resolving `url()` against the
/// document URL `base_url`.
pub fn parse_declarations(text: &str, base_url: Option<&str>) -> Vec<Declaration> {
    let context = ParseContext {
        base: base_url
            .and_then(|base| url::Url::parse(base).ok())
            .map(Rc::new),
        ..ParseContext::default()
    };
    parse_declaration_list_text(text, &context)
}

fn parse_declaration_list_text(text: &str, context: &ParseContext) -> Vec<Declaration> {
    let mut input = Parser::new(text);
    parse_declaration_list(&mut input, context)
}

fn parse_declaration_list(input: &mut Parser<'_>, context: &ParseContext) -> Vec<Declaration> {
    let mut parser = Declarations {
        context,
        out: Vec::new(),
    };
    for result in RuleBodyParser::new(input, &mut parser) {
        if let Err((_, source, _)) = result {
            debug!(declaration = source, "css declaration dropped");
        }
    }
    parser.out
}

/// The top level and `@media` bodies: rule lists.
struct TopLevel<'a> {
    context: &'a ParseContext,
    /// The `@media` lists the parser is inside.
    media: Vec<Rc<MediaList>>,
    /// The `@layer` names the parser is inside, outermost first. The last
    /// entry is the layer rules are filed into.
    layer: Vec<Option<Atom>>,
    rules: Vec<Rule>,
    font_faces: Vec<FontFaceRule>,
    /// The registered custom properties from `@property`.
    properties: Vec<PropertyRule>,
}

/// What an at rule the top level reads turned out to be.
enum AtPrelude {
    Media(Rc<MediaList>),
    FontFace,
    /// The `@layer` name, `None` for an anonymous layer.
    Layer(Option<Atom>),
    /// The `@property` name, without the leading `--`.
    Property(Atom),
}

impl<'i> QualifiedRuleParser<'i> for TopLevel<'_> {
    type Prelude = Vec<Selector>;
    type QualifiedRule = ();
    type Error = ();

    fn parse_prelude(&mut self, input: &mut Parser<'i>) -> Result<Vec<Selector>, ParseError<()>> {
        input.skip_whitespace();
        parse_selector_list(input)
    }

    fn parse_block(
        &mut self,
        selectors: Vec<Selector>,
        _: &ParserState,
        input: &mut Parser<'i>,
    ) -> Result<(), ParseError<()>> {
        let declarations = parse_declaration_list(input, self.context);
        if declarations.is_empty() {
            return Ok(());
        }
        self.rules.push(Rule {
            selectors: Rc::from(selectors),
            declarations: Rc::from(declarations),
            layer: self.layer.last().cloned().flatten(),
            media: Rc::from(self.media.clone()),
        });
        Ok(())
    }
}

impl<'i> AtRuleParser<'i> for TopLevel<'_> {
    type Prelude = AtPrelude;
    type AtRule = ();
    type Error = ();

    fn parse_prelude(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i>,
    ) -> Result<AtPrelude, ParseError<()>> {
        if name.eq_ignore_ascii_case("media") {
            return Ok(AtPrelude::Media(Rc::new(MediaList::parse_from(input))));
        }
        if name.eq_ignore_ascii_case("font-face") {
            input.expect_exhausted()?;
            return Ok(AtPrelude::FontFace);
        }
        if name.eq_ignore_ascii_case("layer") {
            // `@layer name { }` or an anonymous `@layer { }`. Only the first
            // name matters for the block form in this feature.
            let names: Vec<Atom> = input
                .parse_comma_separated(|input| -> Result<Atom, ParseError<()>> {
                    let ident = input.expect_ident()?;
                    Ok(Atom::from(ident.as_ref()))
                })
                .unwrap_or_default();
            return Ok(AtPrelude::Layer(names.into_iter().next()));
        }
        if name.eq_ignore_ascii_case("property") {
            // `@property --foo { ... }`. The name must start with `--`.
            let ident = input.expect_ident()?;
            let Some(name) = ident.as_ref().strip_prefix("--") else {
                debug!(at_rule = &*name, "@property name missing --");
                return Err(ParseError::unexpected_token());
            };
            return Ok(AtPrelude::Property(Atom::from(name)));
        }
        // `@import`, `@supports`, `@keyframes`, `@layer` and the rest
        // arrive with later features (spec 0004, Follow-up).
        debug!(at_rule = &*name, "css at rule skipped");
        Err(ParseError::unexpected_token())
    }

    fn parse_block(
        &mut self,
        prelude: AtPrelude,
        _: &ParserState,
        input: &mut Parser<'i>,
    ) -> Result<(), ParseError<()>> {
        let media = match prelude {
            AtPrelude::Media(media) => media,
            AtPrelude::FontFace => {
                let media = Rc::from(self.media.clone());
                self.font_faces
                    .extend(parse_font_face(input, self.context, media));
                return Ok(());
            }
            AtPrelude::Layer(name) => {
                self.layer.push(name);
                for result in StyleSheetParser::new(input, self) {
                    if let Err((_, source, _)) = result {
                        debug!(rule = source, "css rule dropped");
                    }
                }
                self.layer.pop();
                return Ok(());
            }
            AtPrelude::Property(name) => {
                let rule = match parse_property_block(input, name.clone()) {
                    Some(rule) => rule,
                    None => {
                        debug!(property = &*name, "@property rule ignored");
                        return Ok(());
                    }
                };
                self.properties.push(rule);
                return Ok(());
            }
        };
        self.media.push(media);
        for result in StyleSheetParser::new(input, self) {
            if let Err((_, source, _)) = result {
                debug!(rule = source, "css rule dropped");
            }
        }
        self.media.pop();
        Ok(())
    }
}

/// Parses the block of an `@property --name { }` rule: `syntax`, `inherits`
/// and `initial-value` declarations (spec 0010, AC-10).
fn parse_property_block(input: &mut Parser<'_>, name: Atom) -> Option<PropertyRule> {
    let mut syntax = None;
    let mut inherits = true;
    let mut initial = None;
    loop {
        input.skip_whitespace();
        if input.is_exhausted() {
            break;
        }
        let decl_name = input.expect_ident_cloned().ok()?;
        input.expect_colon().ok()?;
        let value = read_property_value(input)?;
        eprintln!("DEBUG property value {value:?}");
        match decl_name.as_ref().to_ascii_lowercase().as_str() {
            "syntax" => syntax = parse_property_syntax(&value),
            "inherits" => inherits = value.trim().eq_ignore_ascii_case("true"),
            "initial-value" => {
                initial = Some(
                    value
                        .trim()
                        .trim_matches(|c| c == '"' || c == '\'')
                        .to_string(),
                )
            }
            _ => {}
        }
    }
    let syntax = syntax?;
    let initial = initial?;
    Some(PropertyRule {
        name,
        syntax,
        inherits,
        initial: Rc::from(initial),
    })
}

/// Reads one declaration's value as raw text, stopping at `;` or at the end
/// of the enclosing block.
fn read_property_value(input: &mut Parser<'_>) -> Option<String> {
    let start = input.position();
    loop {
        match input.next() {
            Ok(Token::Semicolon) => break,
            Err(_) => break,
            Ok(_) => {}
        }
    }
    let mut text = input.slice_from(start).trim().to_string();
    if text.ends_with(';') {
        text.pop();
    }
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// A declaration list: a rule's body or a `style` attribute.
struct Declarations<'a> {
    context: &'a ParseContext,
    out: Vec<Declaration>,
}

impl<'i> DeclarationParser<'i> for Declarations<'_> {
    type Declaration = ();
    type Error = ();

    fn parse_value(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i>,
        _: &ParserState,
    ) -> Result<(), ParseError<()>> {
        let declarations = parse_declaration(&name, input, self.context)?;
        self.out.extend(declarations);
        Ok(())
    }
}

impl<'i> AtRuleParser<'i> for Declarations<'_> {
    type Prelude = ();
    type AtRule = ();
    type Error = ();
}

impl<'i> QualifiedRuleParser<'i> for Declarations<'_> {
    type Prelude = ();
    type QualifiedRule = ();
    type Error = ();
}

impl<'i> RuleBodyItemParser<'i, (), ()> for Declarations<'_> {
    fn parse_declarations(&self) -> bool {
        true
    }

    fn parse_qualified(&self) -> bool {
        // Nested style rules are not supported yet.
        false
    }
}

/// Decodes a style sheet's bytes: a BOM wins, then the HTTP charset, then
/// `@charset`, then the document's encoding (spec 0004, Value sourcing).
/// `encoding_rs` replaces malformed bytes, so this never fails.
pub fn decode_stylesheet(
    bytes: &[u8],
    http_charset: Option<&str>,
    document_encoding: &str,
) -> String {
    let environment = Encoding::for_label(document_encoding.as_bytes());
    let encoding = cssparser::stylesheet_encoding::<EncodingRs>(
        bytes,
        http_charset.map(str::as_bytes),
        environment,
    );
    // `decode` sniffs the BOM first, which overrides everything else.
    let (text, _, _) = encoding.decode(bytes);
    text.into_owned()
}

/// `cssparser`'s encoding hooks, answered by `encoding_rs`.
struct EncodingRs;

impl cssparser::EncodingSupport for EncodingRs {
    type Encoding = &'static Encoding;

    fn utf8() -> Self::Encoding {
        encoding_rs::UTF_8
    }

    fn is_utf16_be_or_le(encoding: &Self::Encoding) -> bool {
        *encoding == encoding_rs::UTF_16LE || *encoding == encoding_rs::UTF_16BE
    }

    fn from_label(ascii_label: &[u8]) -> Option<Self::Encoding> {
        Encoding::for_label(ascii_label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_media_and_recovery() {
        let sheet = parse_stylesheet(
            "p { color: red; bogus: 1; margin: 0 }
             @import url(x.css);
             a:nth-child(2) { color: blue }
             @media screen and (min-width: 600px) {
               h1, h2 { font-size: 2em }
               @media print { pre { color: red } }
             }
             @font-face { font-family: X }
             div { color: blue",
            Origin::Author,
            Some("https://example.com/"),
        );
        assert_eq!(sheet.len(), 4, "{sheet:#?}");
        let names: Vec<_> = sheet.rules[0]
            .declarations
            .iter()
            .map(Declaration::name)
            .collect();
        assert_eq!(
            names,
            [
                "color",
                "margin-top",
                "margin-right",
                "margin-bottom",
                "margin-left"
            ]
        );
        assert_eq!(sheet.rules[1].selectors.len(), 2);
        assert_eq!(sheet.rules[1].media.len(), 1);
        assert_eq!(sheet.rules[2].media.len(), 2, "nested @media");
        assert_eq!(
            sheet.rules[3].declarations.len(),
            1,
            "an unclosed block still counts"
        );
    }

    #[test]
    fn style_attributes_parse_as_declaration_lists() {
        let declarations = parse_declarations("color: red; ; width: 10px !important; x", None);
        let names: Vec<_> = declarations.iter().map(Declaration::name).collect();
        assert_eq!(names, ["color", "width"]);
        assert!(declarations[1].important());
    }

    #[test]
    fn at_property_registers_a_custom_property() {
        let sheet = parse_stylesheet(
            "@property --gap { syntax: \"<length>\"; inherits: false; initial-value: 8px; }
             @property --color { syntax: \"<color>\"; initial-value: red; }
             @property --bogus { syntax: \"<nope>\"; }
             p { --gap: 16px; color: red; }",
            Origin::Author,
            None,
        );
        // The invalid syntax makes the third rule ignored as a whole.
        let names: Vec<_> = sheet.properties.iter().map(|r| r.name.clone()).collect();
        assert_eq!(names, vec![Atom::from("gap"), Atom::from("color")]);
        let gap = sheet.properties.iter().find(|r| r.name == *"gap").unwrap();
        assert!(!gap.inherits);
        assert_eq!(gap.initial.as_ref(), "8px");
        let color = sheet
            .properties
            .iter()
            .find(|r| r.name == *"color")
            .unwrap();
        assert!(color.inherits);
        assert_eq!(color.initial.as_ref(), "red");
        assert_eq!(sheet.len(), 1);
    }

    #[test]
    fn decoding_follows_bom_http_charset_rule_then_document() {
        // AC-6's order, for style sheets.
        let latin1 = b"p::after { content: \"\xe9\" }";
        assert!(decode_stylesheet(latin1, None, "windows-1252").contains('é'));
        assert!(decode_stylesheet(latin1, Some("iso-8859-1"), "utf-8").contains('é'));
        assert!(!decode_stylesheet(latin1, None, "utf-8").contains('é'));
        let charset = b"@charset \"windows-1252\"; p { content: \"\xe9\" }";
        assert!(decode_stylesheet(charset, None, "utf-8").contains('é'));
        let bom = b"\xef\xbb\xbfp { content: \"\xc3\xa9\" }";
        assert!(decode_stylesheet(bom, Some("windows-1252"), "windows-1252").contains('é'));
    }
}
