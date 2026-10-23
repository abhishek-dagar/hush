//! The slice 1 property table: longhands, shorthands and declarations
//! (spec 0004, child 0003, Property table, AC-20).
//!
//! Every longhand has a typed parser returning a [`Value`]; a shorthand
//! parses into the values of its longhands. A declaration whose value holds
//! `var()` keeps its text and is parsed after substitution, at computed
//! value time.

use std::cell::RefCell;
use std::rc::Rc;

use base::atoms::Atom;
use cssparser::{Delimiter, ParseError, Parser, Token, parse_important};

use crate::values::{
    LengthRules, ParseResult, SpecifiedColor, SpecifiedLength, invalid, parse_color,
    parse_font_family, parse_keyword, parse_length, parse_url,
};
use crate::{
    AlignItems, BorderStyle, BoxSizing, Cursor, Display, FlexDirection, FlexWrap, FontStyle,
    JustifyContent, ListStylePosition, ListStyleType, OutlineStyle, Overflow, Position, TextAlign,
    TextDecorationLine, TextOverflow, UserSelect, Visibility, WhiteSpace,
};

macro_rules! longhands {
    ($($variant:ident = $name:literal, $inherited:literal;)*) => {
        /// A longhand property of the slice 1 table.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub(crate) enum Longhand {
            $($variant),*
        }

        impl Longhand {
            /// Every longhand, in table order.
            pub(crate) const ALL: &'static [Self] = &[$(Self::$variant),*];

            /// The property name.
            pub(crate) fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name),*
                }
            }

            /// Whether the property inherits by default.
            pub(crate) fn is_inherited(self) -> bool {
                match self {
                    $(Self::$variant => $inherited),*
                }
            }
        }
    };
}

longhands! {
    Display = "display", false;
    Width = "width", false;
    Height = "height", false;
    MinWidth = "min-width", false;
    MinHeight = "min-height", false;
    MaxWidth = "max-width", false;
    MaxHeight = "max-height", false;
    BoxSizing = "box-sizing", false;
    MarginTop = "margin-top", false;
    MarginRight = "margin-right", false;
    MarginBottom = "margin-bottom", false;
    MarginLeft = "margin-left", false;
    PaddingTop = "padding-top", false;
    PaddingRight = "padding-right", false;
    PaddingBottom = "padding-bottom", false;
    PaddingLeft = "padding-left", false;
    BorderTopWidth = "border-top-width", false;
    BorderRightWidth = "border-right-width", false;
    BorderBottomWidth = "border-bottom-width", false;
    BorderLeftWidth = "border-left-width", false;
    BorderTopStyle = "border-top-style", false;
    BorderRightStyle = "border-right-style", false;
    BorderBottomStyle = "border-bottom-style", false;
    BorderLeftStyle = "border-left-style", false;
    BorderTopColor = "border-top-color", false;
    BorderRightColor = "border-right-color", false;
    BorderBottomColor = "border-bottom-color", false;
    BorderLeftColor = "border-left-color", false;
    BorderTopLeftRadius = "border-top-left-radius", false;
    BorderTopRightRadius = "border-top-right-radius", false;
    BorderBottomRightRadius = "border-bottom-right-radius", false;
    BorderBottomLeftRadius = "border-bottom-left-radius", false;
    BackgroundColor = "background-color", false;
    BackgroundImage = "background-image", false;
    Opacity = "opacity", false;
    FontFamily = "font-family", true;
    FontSize = "font-size", true;
    FontWeight = "font-weight", true;
    FontStyle = "font-style", true;
    Color = "color", true;
    LineHeight = "line-height", true;
    TextAlign = "text-align", true;
    WhiteSpace = "white-space", true;
    TextDecorationLine = "text-decoration-line", true;
    Visibility = "visibility", true;
    ListStyleType = "list-style-type", true;
    ListStylePosition = "list-style-position", true;
    FlexDirection = "flex-direction", false;
    RowGap = "row-gap", false;
    ColumnGap = "column-gap", false;
    FlexWrap = "flex-wrap", false;
    FlexGrow = "flex-grow", false;
    FlexShrink = "flex-shrink", false;
    FlexBasis = "flex-basis", false;
    JustifyContent = "justify-content", false;
    AlignItems = "align-items", false;
    AlignSelf = "align-self", false;
    Order = "order", false;
    OverflowX = "overflow-x", false;
    OverflowY = "overflow-y", false;
    Position = "position", false;
    Top = "top", false;
    Right = "right", false;
    Bottom = "bottom", false;
    Left = "left", false;
    ZIndex = "z-index", false;
    OutlineWidth = "outline-width", false;
    OutlineStyle = "outline-style", false;
    OutlineColor = "outline-color", false;
    OutlineOffset = "outline-offset", false;
    TextOverflow = "text-overflow", false;
    Cursor = "cursor", true;
    UserSelect = "user-select", true;
}

impl Longhand {
    /// How many longhands there are.
    pub(crate) const COUNT: usize = Self::ALL.len();

    /// The longhand's position in [`Longhand::ALL`].
    pub(crate) fn index(self) -> usize {
        self as usize
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|longhand| longhand.name().eq_ignore_ascii_case(name))
    }
}

/// A shorthand of the slice 1 table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shorthand {
    Margin,
    Padding,
    Border,
    BorderTop,
    BorderRight,
    BorderBottom,
    BorderLeft,
    BorderWidth,
    BorderStyle,
    BorderColor,
    BorderRadius,
    Background,
    Font,
    TextDecoration,
    ListStyle,
    Gap,
    Flex,
    Overflow,
    Outline,
}

impl Shorthand {
    fn from_name(name: &str) -> Option<Self> {
        let name = name.to_ascii_lowercase();
        Some(match name.as_str() {
            "margin" => Self::Margin,
            "padding" => Self::Padding,
            "border" => Self::Border,
            "border-top" => Self::BorderTop,
            "border-right" => Self::BorderRight,
            "border-bottom" => Self::BorderBottom,
            "border-left" => Self::BorderLeft,
            "border-width" => Self::BorderWidth,
            "border-style" => Self::BorderStyle,
            "border-color" => Self::BorderColor,
            "border-radius" => Self::BorderRadius,
            "background" => Self::Background,
            "font" => Self::Font,
            "text-decoration" => Self::TextDecoration,
            "list-style" => Self::ListStyle,
            "gap" => Self::Gap,
            "flex" => Self::Flex,
            "overflow" => Self::Overflow,
            "outline" => Self::Outline,
            _ => return None,
        })
    }

    /// The longhands it sets, every one of them, even those it resets to
    /// their initial value.
    pub(crate) fn longhands(self) -> &'static [Longhand] {
        use Longhand as L;
        match self {
            Self::Margin => &[L::MarginTop, L::MarginRight, L::MarginBottom, L::MarginLeft],
            Self::Padding => &[
                L::PaddingTop,
                L::PaddingRight,
                L::PaddingBottom,
                L::PaddingLeft,
            ],
            Self::Border => &[
                L::BorderTopWidth,
                L::BorderRightWidth,
                L::BorderBottomWidth,
                L::BorderLeftWidth,
                L::BorderTopStyle,
                L::BorderRightStyle,
                L::BorderBottomStyle,
                L::BorderLeftStyle,
                L::BorderTopColor,
                L::BorderRightColor,
                L::BorderBottomColor,
                L::BorderLeftColor,
            ],
            Self::BorderTop => &[L::BorderTopWidth, L::BorderTopStyle, L::BorderTopColor],
            Self::BorderRight => &[
                L::BorderRightWidth,
                L::BorderRightStyle,
                L::BorderRightColor,
            ],
            Self::BorderBottom => &[
                L::BorderBottomWidth,
                L::BorderBottomStyle,
                L::BorderBottomColor,
            ],
            Self::BorderLeft => &[L::BorderLeftWidth, L::BorderLeftStyle, L::BorderLeftColor],
            Self::BorderWidth => &[
                L::BorderTopWidth,
                L::BorderRightWidth,
                L::BorderBottomWidth,
                L::BorderLeftWidth,
            ],
            Self::BorderStyle => &[
                L::BorderTopStyle,
                L::BorderRightStyle,
                L::BorderBottomStyle,
                L::BorderLeftStyle,
            ],
            Self::BorderColor => &[
                L::BorderTopColor,
                L::BorderRightColor,
                L::BorderBottomColor,
                L::BorderLeftColor,
            ],
            Self::BorderRadius => &[
                L::BorderTopLeftRadius,
                L::BorderTopRightRadius,
                L::BorderBottomRightRadius,
                L::BorderBottomLeftRadius,
            ],
            Self::Background => &[L::BackgroundColor, L::BackgroundImage],
            Self::Font => &[
                L::FontStyle,
                L::FontWeight,
                L::FontSize,
                L::LineHeight,
                L::FontFamily,
            ],
            Self::TextDecoration => &[L::TextDecorationLine],
            Self::ListStyle => &[L::ListStyleType, L::ListStylePosition],
            Self::Gap => &[L::RowGap, L::ColumnGap],
            Self::Flex => &[L::FlexGrow, L::FlexShrink, L::FlexBasis],
            Self::Overflow => &[L::OverflowX, L::OverflowY],
            Self::Outline => &[L::OutlineWidth, L::OutlineStyle, L::OutlineColor],
        }
    }
}

/// `font-size` as written.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum FontSize {
    /// A keyword, as its factor of `medium`.
    Keyword(f32),
    /// A length or a percentage of the parent's size.
    Length(SpecifiedLength),
    /// `smaller`.
    Smaller,
    /// `larger`.
    Larger,
}

/// `font-weight` as written.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum FontWeight {
    /// A number from 1 to 1000 (`normal` is 400, `bold` 700).
    Absolute(u16),
    /// `bolder`.
    Bolder,
    /// `lighter`.
    Lighter,
}

/// `line-height` as written.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum LineHeight {
    /// `normal`.
    Normal,
    /// A multiple of the font size, inherited as the number.
    Number(f32),
    /// A length, or a percentage of the font size.
    Length(SpecifiedLength),
}

/// `flex-basis` as written.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum FlexBasis {
    /// `auto`.
    Auto,
    /// `content`.
    Content,
    /// A length or percentage.
    Length(SpecifiedLength),
}

/// A longhand's value as written.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value {
    Display(Display),
    /// A length, or `auto` (sizes, minimums, margins) or `none` (maximums)
    /// as `None`.
    Size(Option<SpecifiedLength>),
    /// A length: padding, border widths, radii.
    Length(SpecifiedLength),
    BorderStyle(BorderStyle),
    Color(SpecifiedColor),
    /// An absolute image URL, or `none`.
    Image(Option<Rc<str>>),
    /// `opacity`, already clamped to 0..1; `flex-grow` and `flex-shrink`.
    Number(f32),
    /// `order`.
    Integer(i32),
    FontFamily(Rc<[Atom]>),
    FontSize(FontSize),
    FontWeight(FontWeight),
    FontStyle(FontStyle),
    LineHeight(LineHeight),
    TextAlign(TextAlign),
    WhiteSpace(WhiteSpace),
    TextDecorationLine(TextDecorationLine),
    Visibility(Visibility),
    ListStyleType(ListStyleType),
    ListStylePosition(ListStylePosition),
    BoxSizing(BoxSizing),
    FlexDirection(FlexDirection),
    FlexWrap(FlexWrap),
    FlexBasis(FlexBasis),
    JustifyContent(JustifyContent),
    AlignItems(AlignItems),
    /// `align-self`; `None` is `auto`.
    AlignSelf(Option<AlignItems>),
    Overflow(Overflow),
    Position(Position),
    /// `z-index`; `None` is `auto`.
    ZIndex(Option<i32>),
    OutlineStyle(OutlineStyle),
    TextOverflow(TextOverflow),
    Cursor(Cursor),
    UserSelect(UserSelect),
}

impl Value {
    fn uses_viewport(&self) -> bool {
        match *self {
            Self::Size(Some(length))
            | Self::Length(length)
            | Self::FlexBasis(FlexBasis::Length(length))
            | Self::FontSize(FontSize::Length(length))
            | Self::LineHeight(LineHeight::Length(length)) => length.uses_viewport(),
            _ => false,
        }
    }
}

/// `inherit`, `initial` and `unset` (`revert` counts as `unset` until
/// cascade layers and origins are reverted properly).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CssWide {
    Inherit,
    Initial,
    Unset,
}

/// A value that held `var()`, kept as text until computed value time.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Unparsed {
    /// The value's text.
    pub(crate) css: Rc<str>,
    /// The shorthand it was written for, if it was one.
    pub(crate) shorthand: Option<Shorthand>,
    /// The URL `url()` resolves against.
    pub(crate) base: Option<Rc<url::Url>>,
}

/// What a declaration holds.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DeclaredValue {
    Value(Value),
    Wide(CssWide),
    /// It held `var()`.
    Unparsed(Rc<Unparsed>),
    /// A custom property's text, which may hold `var()` itself.
    Custom(Rc<str>),
}

/// Which property a declaration sets.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PropertyKey {
    Longhand(Longhand),
    Custom(Atom),
}

/// One declaration, with shorthands already split into longhands.
#[derive(Debug, Clone, PartialEq)]
pub struct Declaration {
    pub(crate) property: PropertyKey,
    pub(crate) value: DeclaredValue,
    pub(crate) important: bool,
}

impl Declaration {
    /// Whether the declaration is `!important`.
    pub fn important(&self) -> bool {
        self.important
    }

    /// The property name, a shorthand already split into its longhands.
    pub fn name(&self) -> String {
        match &self.property {
            PropertyKey::Longhand(longhand) => longhand.name().to_owned(),
            PropertyKey::Custom(name) => format!("--{name}"),
        }
    }

    /// Whether resolving it may read the viewport size (`vw`, `vh`, or a
    /// `var()` whose text could hold them).
    pub(crate) fn uses_viewport(&self) -> bool {
        let text_mentions = |text: &str| {
            let lower = text.to_ascii_lowercase();
            ["vw", "vh", "vmin", "vmax"]
                .iter()
                .any(|unit| lower.contains(unit))
        };
        match &self.value {
            DeclaredValue::Value(value) => value.uses_viewport(),
            DeclaredValue::Wide(_) => false,
            DeclaredValue::Unparsed(unparsed) => text_mentions(&unparsed.css),
            DeclaredValue::Custom(text) => text_mentions(text),
        }
    }
}

/// What property parsers need beyond the tokens.
#[derive(Debug, Clone, Default)]
pub(crate) struct ParseContext {
    /// The URL `url()` resolves against: the sheet's, or the document's for
    /// `style` attributes.
    pub(crate) base: Option<Rc<url::Url>>,
    /// The unsupported keywords already mapped to a supported value in
    /// this sheet, each logged once (spec 0006, Unsupported keywords).
    pub(crate) mapped: RefCell<Vec<&'static str>>,
}

impl ParseContext {
    /// Notes that `property: keyword` computed to a nearby supported value,
    /// with a `debug` log the first time in this sheet.
    fn note_mapped(&self, property: &'static str, keyword: &'static str) {
        let mut mapped = self.mapped.borrow_mut();
        if mapped.contains(&keyword) {
            return;
        }
        mapped.push(keyword);
        tracing::debug!(property, keyword, "css keyword mapped to a supported value");
    }
}

/// Parses the declaration `name: <value>` into longhand declarations. An
/// unknown property or an invalid value fails, and the caller drops it.
pub(crate) fn parse_declaration(
    name: &str,
    input: &mut Parser<'_>,
    context: &ParseContext,
) -> ParseResult<Vec<Declaration>> {
    let start = input.position();
    let has_var = input.parse_until_before(Delimiter::Bang, |input| {
        Ok::<_, ParseError<()>>(contains_var(input))
    })?;
    let text = input.slice_from(start).trim();
    let important = input.try_parse(parse_important).is_ok();
    input.expect_exhausted()?;

    let wide = css_wide(text);
    if let Some(custom) = name.strip_prefix("--") {
        let value = match wide {
            Some(wide) => DeclaredValue::Wide(wide),
            None => DeclaredValue::Custom(Rc::from(text)),
        };
        return Ok(vec![Declaration {
            property: PropertyKey::Custom(Atom::from(custom)),
            value,
            important,
        }]);
    }

    let (longhands, shorthand) = match Longhand::from_name(name) {
        Some(longhand) => (vec![longhand], None),
        None => match Shorthand::from_name(name) {
            Some(shorthand) => (shorthand.longhands().to_vec(), Some(shorthand)),
            None => return invalid(),
        },
    };
    let declare = |longhand, value| Declaration {
        property: PropertyKey::Longhand(longhand),
        value,
        important,
    };
    if let Some(wide) = wide {
        return Ok(longhands
            .into_iter()
            .map(|longhand| declare(longhand, DeclaredValue::Wide(wide)))
            .collect());
    }
    if has_var {
        let unparsed = Rc::new(Unparsed {
            css: Rc::from(text),
            shorthand,
            base: context.base.clone(),
        });
        return Ok(longhands
            .into_iter()
            .map(|longhand| declare(longhand, DeclaredValue::Unparsed(Rc::clone(&unparsed))))
            .collect());
    }
    let values = parse_values(text, longhands.first().copied(), shorthand, context)
        .ok_or(ParseError::unexpected_token())?;
    Ok(values
        .into_iter()
        .map(|(longhand, value)| declare(longhand, DeclaredValue::Value(value)))
        .collect())
}

/// Parses `text` as the value of `shorthand`, else of `longhand`: the
/// longhand values it sets, or `None` when it is invalid. Also used after
/// `var()` substitution.
pub(crate) fn parse_values(
    text: &str,
    longhand: Option<Longhand>,
    shorthand: Option<Shorthand>,
    context: &ParseContext,
) -> Option<Vec<(Longhand, Value)>> {
    let mut parser = Parser::new(text);
    parser
        .parse_entirely(|input| match (shorthand, longhand) {
            (Some(shorthand), _) => parse_shorthand(shorthand, input, context),
            (None, Some(longhand)) => {
                Ok(vec![(longhand, parse_longhand(longhand, input, context)?)])
            }
            (None, None) => invalid(),
        })
        .ok()
}

fn css_wide(text: &str) -> Option<CssWide> {
    let lower = text.to_ascii_lowercase();
    Some(match lower.as_str() {
        "inherit" => CssWide::Inherit,
        "initial" => CssWide::Initial,
        "unset" | "revert" | "revert-layer" => CssWide::Unset,
        _ => return None,
    })
}

/// Consumes the rest of `input` and says whether it holds a `var()`.
fn contains_var(input: &mut Parser<'_>) -> bool {
    let mut found = false;
    loop {
        let nested = match input.next() {
            Err(_) => return found,
            Ok(Token::Function(name)) => {
                found |= name.eq_ignore_ascii_case("var");
                true
            }
            Ok(Token::ParenthesisBlock | Token::SquareBracketBlock | Token::CurlyBracketBlock) => {
                true
            }
            Ok(_) => false,
        };
        if nested {
            found |= input
                .parse_nested_block(|input| Ok::<_, ParseError<()>>(contains_var(input)))
                .unwrap_or(false);
        }
    }
}

fn parse_longhand(
    longhand: Longhand,
    input: &mut Parser<'_>,
    context: &ParseContext,
) -> ParseResult<Value> {
    use Longhand as L;
    Ok(match longhand {
        L::Display => Value::Display(parse_display(input)?),
        L::Width | L::Height | L::MinWidth | L::MinHeight => {
            Value::Size(parse_or_keyword(input, "auto", LengthRules::NON_NEGATIVE)?)
        }
        L::MaxWidth | L::MaxHeight => {
            Value::Size(parse_or_keyword(input, "none", LengthRules::NON_NEGATIVE)?)
        }
        L::MarginTop | L::MarginRight | L::MarginBottom | L::MarginLeft => {
            Value::Size(parse_or_keyword(input, "auto", LengthRules::ANY)?)
        }
        L::BoxSizing => Value::BoxSizing(parse_keyword(
            input,
            &[
                ("content-box", BoxSizing::ContentBox),
                ("border-box", BoxSizing::BorderBox),
            ],
        )?),
        L::PaddingTop | L::PaddingRight | L::PaddingBottom | L::PaddingLeft => {
            Value::Length(parse_length(input, LengthRules::NON_NEGATIVE)?)
        }
        L::BorderTopWidth | L::BorderRightWidth | L::BorderBottomWidth | L::BorderLeftWidth => {
            Value::Length(parse_border_width(input)?)
        }
        L::BorderTopStyle | L::BorderRightStyle | L::BorderBottomStyle | L::BorderLeftStyle => {
            Value::BorderStyle(parse_border_style(input)?)
        }
        L::BorderTopColor
        | L::BorderRightColor
        | L::BorderBottomColor
        | L::BorderLeftColor
        | L::BackgroundColor
        | L::Color => Value::Color(parse_color(input)?),
        L::BorderTopLeftRadius
        | L::BorderTopRightRadius
        | L::BorderBottomRightRadius
        | L::BorderBottomLeftRadius => {
            let radius = parse_length(input, LengthRules::NON_NEGATIVE)?;
            // A second, vertical radius is accepted and ignored.
            let _ = input.try_parse(|i| parse_length(i, LengthRules::NON_NEGATIVE));
            Value::Length(radius)
        }
        L::BackgroundImage => {
            let image = parse_image(input, context)?;
            // Only the first layer is drawn.
            while input.try_parse(Parser::expect_comma).is_ok() {
                parse_image(input, context)?;
            }
            Value::Image(image)
        }
        L::Opacity => Value::Number(parse_opacity(input)?),
        L::FontFamily => Value::FontFamily(Rc::from(parse_font_family(input)?)),
        L::FontSize => Value::FontSize(parse_font_size(input)?),
        L::FontWeight => Value::FontWeight(parse_font_weight(input)?),
        L::FontStyle => Value::FontStyle(parse_font_style(input)?),
        L::LineHeight => Value::LineHeight(parse_line_height(input)?),
        L::TextAlign => Value::TextAlign(parse_keyword(
            input,
            &[
                ("left", TextAlign::Left),
                ("right", TextAlign::Right),
                ("center", TextAlign::Center),
                ("justify", TextAlign::Justify),
                ("start", TextAlign::Start),
                ("end", TextAlign::End),
                ("-webkit-center", TextAlign::Center),
                ("-moz-center", TextAlign::Center),
                ("match-parent", TextAlign::Start),
            ],
        )?),
        L::WhiteSpace => Value::WhiteSpace(parse_keyword(
            input,
            &[
                ("normal", WhiteSpace::Normal),
                ("pre", WhiteSpace::Pre),
                ("nowrap", WhiteSpace::Nowrap),
                ("pre-wrap", WhiteSpace::PreWrap),
                ("pre-line", WhiteSpace::PreLine),
                ("break-spaces", WhiteSpace::PreWrap),
            ],
        )?),
        L::TextDecorationLine => Value::TextDecorationLine(parse_decoration_line(input)?),
        L::Visibility => Value::Visibility(parse_keyword(
            input,
            &[
                ("visible", Visibility::Visible),
                ("hidden", Visibility::Hidden),
                ("collapse", Visibility::Collapse),
            ],
        )?),
        L::ListStyleType => Value::ListStyleType(parse_list_style_type(input)?),
        L::ListStylePosition => Value::ListStylePosition(parse_list_style_position(input)?),
        L::FlexDirection => Value::FlexDirection(parse_mapped_keyword(
            input,
            context,
            longhand,
            &[
                ("row", FlexDirection::Row),
                ("column", FlexDirection::Column),
            ],
            &[
                ("row-reverse", FlexDirection::Row),
                ("column-reverse", FlexDirection::Column),
            ],
        )?),
        L::RowGap | L::ColumnGap => Value::Length(parse_gap(input)?),
        L::FlexWrap => Value::FlexWrap(parse_mapped_keyword(
            input,
            context,
            longhand,
            &[("nowrap", FlexWrap::Nowrap), ("wrap", FlexWrap::Wrap)],
            &[("wrap-reverse", FlexWrap::Wrap)],
        )?),
        L::FlexGrow | L::FlexShrink => Value::Number(parse_non_negative_number(input)?),
        L::FlexBasis => Value::FlexBasis(parse_flex_basis(input)?),
        L::JustifyContent => Value::JustifyContent(parse_justify_content(input, context)?),
        L::AlignItems => Value::AlignItems(parse_align(input, context, longhand)?),
        L::AlignSelf => {
            if input.try_parse(|i| i.expect_ident_matching("auto")).is_ok() {
                Value::AlignSelf(None)
            } else {
                Value::AlignSelf(Some(parse_align(input, context, longhand)?))
            }
        }
        L::Order => Value::Integer(input.expect_integer()?),
        L::OverflowX | L::OverflowY => Value::Overflow(parse_overflow(input, context, longhand)?),
        L::Position => Value::Position(parse_mapped_keyword(
            input,
            context,
            longhand,
            &[
                ("static", Position::Static),
                ("relative", Position::Relative),
                ("absolute", Position::Absolute),
            ],
            &[
                ("fixed", Position::Absolute),
                ("sticky", Position::Relative),
            ],
        )?),
        L::Top | L::Right | L::Bottom | L::Left => {
            Value::Size(parse_or_keyword(input, "auto", LengthRules::ANY)?)
        }
        L::ZIndex => {
            if input.try_parse(|i| i.expect_ident_matching("auto")).is_ok() {
                Value::ZIndex(None)
            } else {
                Value::ZIndex(Some(input.expect_integer()?))
            }
        }
        L::OutlineWidth => Value::Length(parse_border_width(input)?),
        L::OutlineStyle => Value::OutlineStyle(parse_outline_style(input, context)?),
        L::OutlineColor => Value::Color(parse_color(input)?),
        L::OutlineOffset => Value::Length(parse_length(input, LengthRules::SIGNED_ABSOLUTE)?),
        L::TextOverflow => Value::TextOverflow(parse_keyword(
            input,
            &[
                ("clip", TextOverflow::Clip),
                ("ellipsis", TextOverflow::Ellipsis),
            ],
        )?),
        L::Cursor => Value::Cursor(parse_cursor(input, context)?),
        L::UserSelect => Value::UserSelect(parse_mapped_keyword(
            input,
            context,
            Longhand::UserSelect,
            &[("auto", UserSelect::Auto), ("none", UserSelect::None)],
            &[
                ("text", UserSelect::Auto),
                ("all", UserSelect::Auto),
                ("contain", UserSelect::Auto),
            ],
        )?),
    })
}

/// The `cursor` keywords outside the profile, which compute to `auto`
/// (spec 0006, Unsupported keywords).
const OTHER_CURSORS: &[&str] = &[
    "none",
    "context-menu",
    "help",
    "progress",
    "wait",
    "cell",
    "crosshair",
    "vertical-text",
    "alias",
    "copy",
    "move",
    "no-drop",
    "not-allowed",
    "grab",
    "grabbing",
    "all-scroll",
    "col-resize",
    "row-resize",
    "n-resize",
    "e-resize",
    "s-resize",
    "w-resize",
    "ne-resize",
    "nw-resize",
    "se-resize",
    "sw-resize",
    "ew-resize",
    "ns-resize",
    "nesw-resize",
    "nwse-resize",
    "zoom-in",
    "zoom-out",
];

/// `cursor`: `[<url> [<x> <y>]?,]* <keyword>`. Images are skipped, so the
/// keyword after them is the value; keywords outside the profile compute
/// to `auto`.
fn parse_cursor(input: &mut Parser<'_>, context: &ParseContext) -> ParseResult<Cursor> {
    while input
        .try_parse(|input| {
            input.expect_url_or_string()?;
            let _ = input.try_parse(|input| {
                input.expect_number()?;
                input.expect_number()
            });
            input.expect_comma()
        })
        .is_ok()
    {}
    let mapped: Vec<(&'static str, Cursor)> = OTHER_CURSORS
        .iter()
        .map(|&name| (name, Cursor::Auto))
        .collect();
    parse_mapped_keyword(
        input,
        context,
        Longhand::Cursor,
        &[
            ("auto", Cursor::Auto),
            ("default", Cursor::Default),
            ("pointer", Cursor::Pointer),
            ("text", Cursor::Text),
        ],
        &mapped,
    )
}

/// `outline-style`: `none`, `solid`, or any other style, which draws as
/// `solid` (spec 0006, Unsupported keywords).
fn parse_outline_style(
    input: &mut Parser<'_>,
    context: &ParseContext,
) -> ParseResult<OutlineStyle> {
    parse_mapped_keyword(
        input,
        context,
        Longhand::OutlineStyle,
        &[("none", OutlineStyle::None), ("solid", OutlineStyle::Solid)],
        &[
            ("auto", OutlineStyle::Solid),
            ("dotted", OutlineStyle::Solid),
            ("dashed", OutlineStyle::Solid),
            ("double", OutlineStyle::Solid),
            ("groove", OutlineStyle::Solid),
            ("ridge", OutlineStyle::Solid),
            ("inset", OutlineStyle::Solid),
            ("outset", OutlineStyle::Solid),
        ],
    )
}

/// `outline`: `<width> || <style> || <color>`, each optional but at least
/// one; the color defaults to `currentcolor`.
fn parse_outline(
    input: &mut Parser<'_>,
    context: &ParseContext,
) -> ParseResult<Vec<(Longhand, Value)>> {
    let (mut width, mut style, mut color) = (None, None, None);
    loop {
        if width.is_none()
            && let Ok(w) = input.try_parse(parse_border_width)
        {
            width = Some(w);
        } else if style.is_none()
            && let Ok(s) = input.try_parse(|i| parse_outline_style(i, context))
        {
            style = Some(s);
        } else if color.is_none()
            && let Ok(c) = input.try_parse(parse_color)
        {
            color = Some(c);
        } else {
            break;
        }
    }
    if width.is_none() && style.is_none() && color.is_none() {
        return invalid();
    }
    Ok(vec![
        (
            Longhand::OutlineWidth,
            Value::Length(width.unwrap_or(MEDIUM_BORDER)),
        ),
        (
            Longhand::OutlineStyle,
            Value::OutlineStyle(style.unwrap_or_default()),
        ),
        (
            Longhand::OutlineColor,
            Value::Color(color.unwrap_or(SpecifiedColor::CurrentColor)),
        ),
    ])
}

/// One `overflow` axis. `scroll`, `auto` and `clip` compute to `hidden`
/// (spec 0006, Unsupported keywords).
fn parse_overflow(
    input: &mut Parser<'_>,
    context: &ParseContext,
    longhand: Longhand,
) -> ParseResult<Overflow> {
    parse_mapped_keyword(
        input,
        context,
        longhand,
        &[("visible", Overflow::Visible), ("hidden", Overflow::Hidden)],
        &[
            ("scroll", Overflow::Hidden),
            ("auto", Overflow::Hidden),
            ("clip", Overflow::Hidden),
        ],
    )
}

/// One of `supported`, or one of `mapped`: a keyword the property defines
/// beyond the profile, which computes to its nearest supported value
/// (spec 0006, Unsupported keywords).
fn parse_mapped_keyword<T: Copy>(
    input: &mut Parser<'_>,
    context: &ParseContext,
    longhand: Longhand,
    supported: &[(&str, T)],
    mapped: &[(&'static str, T)],
) -> ParseResult<T> {
    let ident = input.expect_ident()?;
    if let Some(&(_, value)) = supported
        .iter()
        .find(|(name, _)| ident.eq_ignore_ascii_case(name))
    {
        return Ok(value);
    }
    let &(keyword, value) = mapped
        .iter()
        .find(|(name, _)| ident.eq_ignore_ascii_case(name))
        .ok_or(ParseError::unexpected_token())?;
    context.note_mapped(longhand.name(), keyword);
    Ok(value)
}

fn parse_non_negative_number(input: &mut Parser<'_>) -> ParseResult<f32> {
    let number = input.expect_number()?;
    if number < 0.0 {
        return invalid();
    }
    Ok(number)
}

fn parse_flex_basis(input: &mut Parser<'_>) -> ParseResult<FlexBasis> {
    if let Ok(keyword) = input.try_parse(|input| {
        parse_keyword(
            input,
            &[("auto", FlexBasis::Auto), ("content", FlexBasis::Content)],
        )
    }) {
        return Ok(keyword);
    }
    parse_length(input, LengthRules::NON_NEGATIVE).map(FlexBasis::Length)
}

/// `justify-content` on a flex container. `normal` and `stretch` behave as
/// `flex-start` there; `start`, `end`, `left` and `right` map by meaning.
fn parse_justify_content(
    input: &mut Parser<'_>,
    context: &ParseContext,
) -> ParseResult<JustifyContent> {
    skip_overflow_position(input);
    parse_mapped_keyword(
        input,
        context,
        Longhand::JustifyContent,
        &[
            ("normal", JustifyContent::FlexStart),
            ("flex-start", JustifyContent::FlexStart),
            ("flex-end", JustifyContent::FlexEnd),
            ("center", JustifyContent::Center),
            ("space-between", JustifyContent::SpaceBetween),
            ("space-around", JustifyContent::SpaceAround),
            ("space-evenly", JustifyContent::SpaceEvenly),
        ],
        &[
            ("stretch", JustifyContent::FlexStart),
            ("start", JustifyContent::FlexStart),
            ("left", JustifyContent::FlexStart),
            ("end", JustifyContent::FlexEnd),
            ("right", JustifyContent::FlexEnd),
        ],
    )
}

/// `align-items`, or `align-self` other than `auto`. `normal` behaves as
/// `stretch` on flex items; the baseline and logical keywords map to the
/// nearest flex keyword.
fn parse_align(
    input: &mut Parser<'_>,
    context: &ParseContext,
    longhand: Longhand,
) -> ParseResult<AlignItems> {
    // `first baseline` and `last baseline` are two words.
    if input
        .try_parse(|input| {
            parse_keyword(input, &[("first", ()), ("last", ())])?;
            input.expect_ident_matching("baseline")?;
            Ok::<_, ParseError<()>>(())
        })
        .is_ok()
    {
        context.note_mapped(longhand.name(), "baseline");
        return Ok(AlignItems::FlexStart);
    }
    skip_overflow_position(input);
    parse_mapped_keyword(
        input,
        context,
        longhand,
        &[
            ("normal", AlignItems::Stretch),
            ("stretch", AlignItems::Stretch),
            ("flex-start", AlignItems::FlexStart),
            ("flex-end", AlignItems::FlexEnd),
            ("center", AlignItems::Center),
        ],
        &[
            ("baseline", AlignItems::FlexStart),
            ("start", AlignItems::FlexStart),
            ("self-start", AlignItems::FlexStart),
            ("end", AlignItems::FlexEnd),
            ("self-end", AlignItems::FlexEnd),
        ],
    )
}

/// Skips a leading `safe` or `unsafe`, which only matter when the content
/// overflows its container.
fn skip_overflow_position(input: &mut Parser<'_>) {
    let _ = input.try_parse(|input| parse_keyword(input, &[("safe", ()), ("unsafe", ())]));
}

fn parse_display(input: &mut Parser<'_>) -> ParseResult<Display> {
    let ident = input.expect_ident()?.to_ascii_lowercase();
    Ok(match ident.as_str() {
        "block" => Display::Block,
        "inline" => Display::Inline,
        "inline-block" => Display::InlineBlock,
        "list-item" => Display::ListItem,
        "none" => Display::None,
        "flex" => Display::Flex,
        // Spec 0006, AC-1: an inline flex container lays out as an inline
        // block until inline flex layout exists.
        "inline-flex" => Display::InlineBlock,
        // Inline level keywords whose layout arrives later.
        "inline-grid" | "inline-table" | "contents" | "ruby" | "ruby-text" | "ruby-base" => {
            Display::Inline
        }
        // Block level ones, table parts included (feature 11).
        "grid" | "table" | "flow-root" | "run-in" | "table-row-group" | "table-header-group"
        | "table-footer-group" | "table-row" | "table-cell" | "table-column-group"
        | "table-column" | "table-caption" => Display::Block,
        _ => return invalid(),
    })
}

/// A length, or `keyword` as `None`.
fn parse_or_keyword(
    input: &mut Parser<'_>,
    keyword: &str,
    rules: LengthRules,
) -> ParseResult<Option<SpecifiedLength>> {
    if input
        .try_parse(|i| i.expect_ident_matching(keyword))
        .is_ok()
    {
        return Ok(None);
    }
    parse_length(input, rules).map(Some)
}

fn parse_border_width(input: &mut Parser<'_>) -> ParseResult<SpecifiedLength> {
    if let Ok(width) = input
        .try_parse(|input| parse_keyword(input, &[("thin", 1.0), ("medium", 3.0), ("thick", 5.0)]))
    {
        return Ok(SpecifiedLength::Px(width));
    }
    parse_length(input, LengthRules::ABSOLUTE)
}

/// `row-gap` or `column-gap`: `normal` (zero for flex containers) or a
/// length or percentage that is not negative.
fn parse_gap(input: &mut Parser<'_>) -> ParseResult<SpecifiedLength> {
    if input
        .try_parse(|i| i.expect_ident_matching("normal"))
        .is_ok()
    {
        return Ok(SpecifiedLength::Px(0.0));
    }
    parse_length(input, LengthRules::NON_NEGATIVE)
}

/// The initial border width, `medium`.
const MEDIUM_BORDER: SpecifiedLength = SpecifiedLength::Px(3.0);

fn parse_border_style(input: &mut Parser<'_>) -> ParseResult<BorderStyle> {
    parse_keyword(
        input,
        &[
            ("none", BorderStyle::None),
            ("hidden", BorderStyle::Hidden),
            ("solid", BorderStyle::Solid),
            ("dashed", BorderStyle::Dashed),
            ("dotted", BorderStyle::Dotted),
            // Slice 1 draws the rest as solid.
            ("double", BorderStyle::Solid),
            ("groove", BorderStyle::Solid),
            ("ridge", BorderStyle::Solid),
            ("inset", BorderStyle::Solid),
            ("outset", BorderStyle::Solid),
        ],
    )
}

/// One `background-image` layer: `none`, a `url()`, or a gradient, which
/// slice 1 does not draw and so computes to `none`.
fn parse_image(input: &mut Parser<'_>, context: &ParseContext) -> ParseResult<Option<Rc<str>>> {
    if input.try_parse(|i| i.expect_ident_matching("none")).is_ok() {
        return Ok(None);
    }
    if let Ok(url) = input.try_parse(|i| parse_url(i, context.base.as_deref())) {
        return Ok(Some(url));
    }
    let name = input.expect_function()?.to_ascii_lowercase();
    if !name.ends_with("gradient") {
        return invalid();
    }
    input.parse_nested_block(|input| {
        while input.next().is_ok() {}
        Ok(None)
    })
}

fn parse_opacity(input: &mut Parser<'_>) -> ParseResult<f32> {
    let value = match *input.next()? {
        Token::Number { value, .. } => value,
        Token::Percentage { unit_value, .. } => unit_value,
        _ => return invalid(),
    };
    Ok(value.clamp(0.0, 1.0))
}

fn parse_font_size(input: &mut Parser<'_>) -> ParseResult<FontSize> {
    // CSS Fonts 4's keyword table, as factors of `medium`.
    const KEYWORDS: &[(&str, f32)] = &[
        ("xx-small", 3.0 / 5.0),
        ("x-small", 3.0 / 4.0),
        ("small", 8.0 / 9.0),
        ("medium", 1.0),
        ("large", 6.0 / 5.0),
        ("x-large", 3.0 / 2.0),
        ("xx-large", 2.0),
        ("xxx-large", 3.0),
    ];
    if let Ok(factor) = input.try_parse(|i| parse_keyword(i, KEYWORDS)) {
        return Ok(FontSize::Keyword(factor));
    }
    if let Ok(relative) = input.try_parse(|input| {
        parse_keyword(
            input,
            &[("smaller", FontSize::Smaller), ("larger", FontSize::Larger)],
        )
    }) {
        return Ok(relative);
    }
    parse_length(input, LengthRules::NON_NEGATIVE).map(FontSize::Length)
}

fn parse_font_weight(input: &mut Parser<'_>) -> ParseResult<FontWeight> {
    if let Ok(weight) = input.try_parse(|input| {
        parse_keyword(
            input,
            &[
                ("normal", FontWeight::Absolute(400)),
                ("bold", FontWeight::Absolute(700)),
                ("bolder", FontWeight::Bolder),
                ("lighter", FontWeight::Lighter),
            ],
        )
    }) {
        return Ok(weight);
    }
    let weight = input.expect_number()?;
    if !(1.0..=1000.0).contains(&weight) {
        return invalid();
    }
    Ok(FontWeight::Absolute(weight.round() as u16))
}

fn parse_font_style(input: &mut Parser<'_>) -> ParseResult<FontStyle> {
    let style = parse_keyword(
        input,
        &[
            ("normal", FontStyle::Normal),
            ("italic", FontStyle::Italic),
            ("oblique", FontStyle::Oblique),
        ],
    )?;
    if style == FontStyle::Oblique {
        // An oblique angle is accepted and ignored.
        let _ = input.try_parse(|i| match *i.next()? {
            Token::Dimension { .. } => Ok(()),
            _ => invalid(),
        });
    }
    Ok(style)
}

fn parse_line_height(input: &mut Parser<'_>) -> ParseResult<LineHeight> {
    if input
        .try_parse(|i| i.expect_ident_matching("normal"))
        .is_ok()
    {
        return Ok(LineHeight::Normal);
    }
    if let Ok(number) = input.try_parse(Parser::expect_number) {
        if number < 0.0 {
            return invalid();
        }
        return Ok(LineHeight::Number(number));
    }
    parse_length(input, LengthRules::NON_NEGATIVE).map(LineHeight::Length)
}

/// `text-decoration-line`: `none`, or any of `underline`, `overline`,
/// `line-through`, `blink`. Only an underline is kept.
fn parse_decoration_line(input: &mut Parser<'_>) -> ParseResult<TextDecorationLine> {
    if input.try_parse(|i| i.expect_ident_matching("none")).is_ok() {
        return Ok(TextDecorationLine::None);
    }
    let mut underline = false;
    let mut any = false;
    while let Ok(line) = input.try_parse(|input| {
        parse_keyword(
            input,
            &[
                ("underline", true),
                ("overline", false),
                ("line-through", false),
                ("blink", false),
            ],
        )
    }) {
        underline |= line;
        any = true;
    }
    if !any {
        return invalid();
    }
    Ok(if underline {
        TextDecorationLine::Underline
    } else {
        TextDecorationLine::None
    })
}

fn parse_list_style_type(input: &mut Parser<'_>) -> ParseResult<ListStyleType> {
    let ident = input.expect_ident()?.to_ascii_lowercase();
    Ok(match ident.as_str() {
        "disc" => ListStyleType::Disc,
        "circle" => ListStyleType::Circle,
        "square" => ListStyleType::Square,
        "none" => ListStyleType::None,
        // Every other counter style falls back to `decimal`, as an unknown
        // counter style name does in CSS Counter Styles.
        _ => ListStyleType::Decimal,
    })
}

fn parse_list_style_position(input: &mut Parser<'_>) -> ParseResult<ListStylePosition> {
    parse_keyword(
        input,
        &[
            ("outside", ListStylePosition::Outside),
            ("inside", ListStylePosition::Inside),
        ],
    )
}

/// One to four values for the top, right, bottom and left sides, as CSS
/// box shorthands repeat them.
fn parse_four<T: Clone>(
    input: &mut Parser<'_>,
    mut parse: impl FnMut(&mut Parser<'_>) -> ParseResult<T>,
) -> ParseResult<[T; 4]> {
    let top = parse(input)?;
    let Ok(right) = input.try_parse(&mut parse) else {
        return Ok([top.clone(), top.clone(), top.clone(), top]);
    };
    let Ok(bottom) = input.try_parse(&mut parse) else {
        return Ok([top.clone(), right.clone(), top, right]);
    };
    let Ok(left) = input.try_parse(&mut parse) else {
        return Ok([top, right.clone(), bottom, right]);
    };
    Ok([top, right, bottom, left])
}

fn parse_shorthand(
    shorthand: Shorthand,
    input: &mut Parser<'_>,
    context: &ParseContext,
) -> ParseResult<Vec<(Longhand, Value)>> {
    let longhands = shorthand.longhands();
    let sides = |values: [Value; 4]| -> Vec<(Longhand, Value)> {
        longhands.iter().copied().zip(values).collect()
    };
    Ok(match shorthand {
        Shorthand::Margin => sides(
            parse_four(input, |i| parse_or_keyword(i, "auto", LengthRules::ANY))?.map(Value::Size),
        ),
        Shorthand::Padding => sides(
            parse_four(input, |i| parse_length(i, LengthRules::NON_NEGATIVE))?.map(Value::Length),
        ),
        Shorthand::BorderWidth => sides(parse_four(input, parse_border_width)?.map(Value::Length)),
        Shorthand::BorderStyle => {
            sides(parse_four(input, parse_border_style)?.map(Value::BorderStyle))
        }
        Shorthand::BorderColor => sides(parse_four(input, parse_color)?.map(Value::Color)),
        Shorthand::BorderRadius => {
            let radii = parse_four(input, |i| parse_length(i, LengthRules::NON_NEGATIVE))?;
            // Vertical radii after a `/` are accepted and ignored.
            if input.try_parse(|i| i.expect_delim('/')).is_ok() {
                parse_four(input, |i| parse_length(i, LengthRules::NON_NEGATIVE))?;
            }
            sides(radii.map(Value::Length))
        }
        Shorthand::Border
        | Shorthand::BorderTop
        | Shorthand::BorderRight
        | Shorthand::BorderBottom
        | Shorthand::BorderLeft => {
            let (width, style, color) = parse_border_side(input)?;
            // `border` sets every side; the others one each. Longhands are
            // listed widths, then styles, then colors.
            let per_side = longhands.len() / 3;
            longhands
                .iter()
                .enumerate()
                .map(|(i, &longhand)| {
                    let value = match i / per_side {
                        0 => Value::Length(width),
                        1 => Value::BorderStyle(style),
                        _ => Value::Color(color),
                    };
                    (longhand, value)
                })
                .collect()
        }
        Shorthand::Background => parse_background(input, context)?,
        Shorthand::Font => parse_font(input)?,
        Shorthand::TextDecoration => parse_text_decoration(input)?,
        Shorthand::ListStyle => parse_list_style(input, context)?,
        Shorthand::Gap => {
            // `gap: <row> <column>?`, the column one defaulting to the row one.
            let row = parse_gap(input)?;
            let column = input.try_parse(parse_gap).unwrap_or(row);
            vec![
                (Longhand::RowGap, Value::Length(row)),
                (Longhand::ColumnGap, Value::Length(column)),
            ]
        }
        Shorthand::Flex => parse_flex(input)?,
        Shorthand::Overflow => {
            // `overflow: <x> <y>?`, the y one defaulting to the x one.
            let x = parse_overflow(input, context, Longhand::OverflowX)?;
            let y = input
                .try_parse(|i| parse_overflow(i, context, Longhand::OverflowY))
                .unwrap_or(x);
            vec![
                (Longhand::OverflowX, Value::Overflow(x)),
                (Longhand::OverflowY, Value::Overflow(y)),
            ]
        }
        Shorthand::Outline => parse_outline(input, context)?,
    })
}

/// `flex`: `none`, or `<grow> <shrink>? || <basis>`. A missing shrink is 1
/// and a missing basis is `0%`, as browsers compute it.
fn parse_flex(input: &mut Parser<'_>) -> ParseResult<Vec<(Longhand, Value)>> {
    let flex = |grow: f32, shrink: f32, basis: FlexBasis| {
        vec![
            (Longhand::FlexGrow, Value::Number(grow)),
            (Longhand::FlexShrink, Value::Number(shrink)),
            (Longhand::FlexBasis, Value::FlexBasis(basis)),
        ]
    };
    if input.try_parse(|i| i.expect_ident_matching("none")).is_ok() {
        return Ok(flex(0.0, 0.0, FlexBasis::Auto));
    }
    let (mut grow, mut shrink, mut basis) = (None, None, None);
    while !input.is_exhausted() {
        if grow.is_none()
            && let Ok(g) = input.try_parse(parse_non_negative_number)
        {
            grow = Some(g);
            shrink = input.try_parse(parse_non_negative_number).ok();
        } else if basis.is_none() {
            basis = Some(parse_flex_basis(input)?);
        } else {
            return invalid();
        }
    }
    if grow.is_none() && basis.is_none() {
        return invalid();
    }
    Ok(flex(
        grow.unwrap_or(1.0),
        shrink.unwrap_or(1.0),
        basis.unwrap_or(FlexBasis::Length(SpecifiedLength::Percent(0.0))),
    ))
}

/// `<width> || <style> || <color>`, each optional but at least one.
fn parse_border_side(
    input: &mut Parser<'_>,
) -> ParseResult<(SpecifiedLength, BorderStyle, SpecifiedColor)> {
    let (mut width, mut style, mut color) = (None, None, None);
    loop {
        if width.is_none()
            && let Ok(w) = input.try_parse(parse_border_width)
        {
            width = Some(w);
        } else if style.is_none()
            && let Ok(s) = input.try_parse(parse_border_style)
        {
            style = Some(s);
        } else if color.is_none()
            && let Ok(c) = input.try_parse(parse_color)
        {
            color = Some(c);
        } else {
            break;
        }
    }
    if width.is_none() && style.is_none() && color.is_none() {
        return invalid();
    }
    Ok((
        width.unwrap_or(MEDIUM_BORDER),
        style.unwrap_or_default(),
        color.unwrap_or(SpecifiedColor::CurrentColor),
    ))
}

/// `background`: only the color and the first image are kept. Every other
/// part (position, size, repeat, attachment, boxes) is skipped, so a common
/// `background: #fff url(x) no-repeat center` still sets its color.
fn parse_background(
    input: &mut Parser<'_>,
    context: &ParseContext,
) -> ParseResult<Vec<(Longhand, Value)>> {
    let mut color = None;
    let mut image = None;
    let mut first_layer = true;
    let mut any = false;
    while !input.is_exhausted() {
        if input.try_parse(Parser::expect_comma).is_ok() {
            // Only the final layer may carry a color.
            color = None;
            first_layer = false;
            continue;
        }
        if let Ok(c) = input.try_parse(parse_color) {
            color = Some(c);
        } else if let Ok(i) = input.try_parse(|i| parse_image(i, context)) {
            if first_layer && image.is_none() {
                image = Some(i);
            }
        } else {
            let skippable = matches!(
                *input.next()?,
                Token::Ident(_)
                    | Token::Dimension { .. }
                    | Token::Percentage { .. }
                    | Token::Number { .. }
                    | Token::Delim('/')
            );
            if !skippable {
                return invalid();
            }
        }
        any = true;
    }
    if !any {
        return invalid();
    }
    Ok(vec![
        (
            Longhand::BackgroundColor,
            Value::Color(color.unwrap_or(SpecifiedColor::Rgba(crate::Color::TRANSPARENT))),
        ),
        (Longhand::BackgroundImage, Value::Image(image.flatten())),
    ])
}

/// `font`: `[style || weight || variant || stretch]? size [/ line-height]?
/// family`. System font keywords are not supported.
fn parse_font(input: &mut Parser<'_>) -> ParseResult<Vec<(Longhand, Value)>> {
    let (mut style, mut weight) = (None, None);
    for _ in 0..4 {
        if input
            .try_parse(|i| i.expect_ident_matching("normal"))
            .is_ok()
        {
            continue;
        }
        if style.is_none()
            && let Ok(s) = input.try_parse(parse_font_style)
        {
            style = Some(s);
            continue;
        }
        if weight.is_none()
            && let Ok(w) = input.try_parse(parse_font_weight)
        {
            weight = Some(w);
            continue;
        }
        // `font-variant: small-caps` and the `font-stretch` keywords are
        // accepted and ignored.
        let ignored = input.try_parse(|input| {
            parse_keyword(
                input,
                &[
                    ("small-caps", ()),
                    ("ultra-condensed", ()),
                    ("extra-condensed", ()),
                    ("condensed", ()),
                    ("semi-condensed", ()),
                    ("semi-expanded", ()),
                    ("expanded", ()),
                    ("extra-expanded", ()),
                    ("ultra-expanded", ()),
                ],
            )
        });
        if ignored.is_err() {
            break;
        }
    }
    let size = parse_font_size(input)?;
    let line_height = if input.try_parse(|i| i.expect_delim('/')).is_ok() {
        parse_line_height(input)?
    } else {
        LineHeight::Normal
    };
    let family = parse_font_family(input)?;
    Ok(vec![
        (
            Longhand::FontStyle,
            Value::FontStyle(style.unwrap_or_default()),
        ),
        (
            Longhand::FontWeight,
            Value::FontWeight(weight.unwrap_or(FontWeight::Absolute(400))),
        ),
        (Longhand::FontSize, Value::FontSize(size)),
        (Longhand::LineHeight, Value::LineHeight(line_height)),
        (Longhand::FontFamily, Value::FontFamily(Rc::from(family))),
    ])
}

/// `text-decoration`: the line, with the style, color and thickness parts
/// accepted and ignored.
fn parse_text_decoration(input: &mut Parser<'_>) -> ParseResult<Vec<(Longhand, Value)>> {
    let mut line = None;
    let mut any = false;
    while !input.is_exhausted() {
        if line.is_none()
            && let Ok(l) = input.try_parse(parse_decoration_line)
        {
            line = Some(l);
        } else if input.try_parse(parse_color).is_err()
            && input
                .try_parse(|input| {
                    parse_keyword(
                        input,
                        &[
                            ("solid", ()),
                            ("double", ()),
                            ("dotted", ()),
                            ("dashed", ()),
                            ("wavy", ()),
                            ("auto", ()),
                            ("from-font", ()),
                        ],
                    )
                })
                .is_err()
        {
            parse_length(input, LengthRules::NON_NEGATIVE)?;
        }
        any = true;
    }
    if !any {
        return invalid();
    }
    Ok(vec![(
        Longhand::TextDecorationLine,
        Value::TextDecorationLine(line.unwrap_or_default()),
    )])
}

/// `list-style`: `<type> || <position> || <image>`, where `none` sets
/// whichever of type and image is not otherwise given.
fn parse_list_style(
    input: &mut Parser<'_>,
    context: &ParseContext,
) -> ParseResult<Vec<(Longhand, Value)>> {
    let (mut kind, mut position, mut nones, mut any) = (None, None, 0, false);
    while !input.is_exhausted() {
        if input.try_parse(|i| i.expect_ident_matching("none")).is_ok() {
            nones += 1;
        } else if position.is_none()
            && let Ok(p) = input.try_parse(parse_list_style_position)
        {
            position = Some(p);
        } else if input
            .try_parse(|i| parse_url(i, context.base.as_deref()))
            .is_ok()
        {
            // List marker images arrive with a later slice.
        } else if kind.is_none() {
            kind = Some(parse_list_style_type(input)?);
        } else {
            return invalid();
        }
        any = true;
    }
    if !any || nones > 2 {
        return invalid();
    }
    let kind = match kind {
        Some(kind) => kind,
        None if nones > 0 => ListStyleType::None,
        None => ListStyleType::Disc,
    };
    Ok(vec![
        (Longhand::ListStyleType, Value::ListStyleType(kind)),
        (
            Longhand::ListStylePosition,
            Value::ListStylePosition(position.unwrap_or_default()),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Vec<(String, DeclaredValue, bool)> {
        let (name, value) = text.split_once(':').unwrap();
        let mut parser = Parser::new(value);
        parse_declaration(name.trim(), &mut parser, &ParseContext::default())
            .map(|declarations| {
                declarations
                    .into_iter()
                    .map(|d| (d.name(), d.value, d.important))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn value(text: &str) -> Value {
        match parse(text).as_slice() {
            [(_, DeclaredValue::Value(value), _)] => value.clone(),
            other => panic!("{text} gave {other:?}"),
        }
    }

    #[test]
    fn shorthands_expand_to_every_longhand() {
        let margin = parse("margin: 1px 2px 3px");
        let names: Vec<_> = margin.iter().map(|(n, _, _)| n.as_str()).collect();
        assert_eq!(
            names,
            ["margin-top", "margin-right", "margin-bottom", "margin-left"]
        );
        assert_eq!(
            margin[3].1,
            DeclaredValue::Value(Value::Size(Some(SpecifiedLength::Px(2.0))))
        );

        let border = parse("border: 2px dashed red");
        assert_eq!(border.len(), 12);
        assert_eq!(
            border[0].1,
            DeclaredValue::Value(Value::Length(SpecifiedLength::Px(2.0)))
        );
        assert_eq!(
            border[5].1,
            DeclaredValue::Value(Value::BorderStyle(BorderStyle::Dashed))
        );

        let font = parse("font: italic bold 12px/1.5 Georgia, serif");
        assert_eq!(font.len(), 5);
        assert_eq!(
            font[3].1,
            DeclaredValue::Value(Value::LineHeight(LineHeight::Number(1.5)))
        );

        let background = parse("background: #fff url(/a.png) no-repeat center / cover");
        assert_eq!(background.len(), 0, "url() needs a base to resolve");
        let background = parse("background: #00f no-repeat left top");
        assert_eq!(
            background[1].1,
            DeclaredValue::Value(Value::Image(None)),
            "the color is kept and the other parts skipped"
        );
        assert_eq!(
            value("list-style-type: lower-roman"),
            Value::ListStyleType(ListStyleType::Decimal)
        );
        let list = parse("list-style: none inside");
        assert_eq!(
            list[0].1,
            DeclaredValue::Value(Value::ListStyleType(ListStyleType::None))
        );
    }

    #[test]
    fn flex_direction_and_gap_parse_with_their_keyword_mappings() {
        // Spec 0006, AC-1
        assert_eq!(
            value("flex-direction: column-reverse"),
            Value::FlexDirection(FlexDirection::Column)
        );
        let gap = parse("gap: 4px 10%");
        assert_eq!(
            gap.iter().map(|(n, _, _)| n.as_str()).collect::<Vec<_>>(),
            ["row-gap", "column-gap"]
        );
        assert_eq!(
            gap[1].1,
            DeclaredValue::Value(Value::Length(SpecifiedLength::Percent(10.0)))
        );
        assert_eq!(parse("gap: 6px")[1].1, parse("gap: 6px")[0].1);
        assert_eq!(
            value("column-gap: normal"),
            Value::Length(SpecifiedLength::Px(0.0))
        );
        assert!(parse("gap: -1px").is_empty());
    }

    /// The grow, shrink and basis values `flex: <text>` expands to.
    fn flex(text: &str) -> Option<(f32, f32, FlexBasis)> {
        let declarations = parse(&format!("flex: {text}"));
        let values: Vec<&Value> = declarations
            .iter()
            .filter_map(|(_, value, _)| match value {
                DeclaredValue::Value(value) => Some(value),
                _ => None,
            })
            .collect();
        match values.as_slice() {
            [
                Value::Number(grow),
                Value::Number(shrink),
                Value::FlexBasis(basis),
            ] => Some((*grow, *shrink, *basis)),
            _ => None,
        }
    }

    #[test]
    fn the_flex_shorthand_fills_grow_shrink_and_basis() {
        // Spec 0006, AC-1
        let zero = FlexBasis::Length(SpecifiedLength::Percent(0.0));
        assert_eq!(flex("none"), Some((0.0, 0.0, FlexBasis::Auto)));
        assert_eq!(flex("auto"), Some((1.0, 1.0, FlexBasis::Auto)));
        assert_eq!(flex("2"), Some((2.0, 1.0, zero)));
        assert_eq!(flex("1 0"), Some((1.0, 0.0, zero)));
        assert_eq!(
            flex("1 1 120px"),
            Some((1.0, 1.0, FlexBasis::Length(SpecifiedLength::Px(120.0))))
        );
        assert_eq!(
            flex("50% 2"),
            Some((2.0, 1.0, FlexBasis::Length(SpecifiedLength::Percent(50.0))))
        );
        assert_eq!(flex("content"), Some((1.0, 1.0, FlexBasis::Content)));
        for bad in ["-1", "1 2 3", "auto auto", "1px 2px"] {
            assert_eq!(flex(bad), None, "flex: {bad} should be dropped");
        }
    }

    #[test]
    fn flex_longhands_parse_and_map_unsupported_keywords() {
        // Spec 0006, AC-1 and Unsupported keywords
        assert_eq!(
            value("flex-wrap: wrap-reverse"),
            Value::FlexWrap(FlexWrap::Wrap)
        );
        assert_eq!(value("flex-grow: 2.5"), Value::Number(2.5));
        assert!(parse("flex-shrink: -1").is_empty());
        assert_eq!(
            value("flex-basis: content"),
            Value::FlexBasis(FlexBasis::Content)
        );
        assert_eq!(value("order: -2"), Value::Integer(-2));
        assert!(parse("order: 1.5").is_empty());
        for (text, expected) in [
            ("normal", JustifyContent::FlexStart),
            ("space-evenly", JustifyContent::SpaceEvenly),
            ("end", JustifyContent::FlexEnd),
            ("safe center", JustifyContent::Center),
            ("stretch", JustifyContent::FlexStart),
        ] {
            assert_eq!(
                value(&format!("justify-content: {text}")),
                Value::JustifyContent(expected),
                "justify-content: {text}"
            );
        }
        for (text, expected) in [
            ("normal", AlignItems::Stretch),
            ("center", AlignItems::Center),
            ("baseline", AlignItems::FlexStart),
            ("last baseline", AlignItems::FlexStart),
            ("self-end", AlignItems::FlexEnd),
        ] {
            assert_eq!(
                value(&format!("align-items: {text}")),
                Value::AlignItems(expected),
                "align-items: {text}"
            );
        }
        assert_eq!(value("align-self: auto"), Value::AlignSelf(None));
        assert_eq!(
            value("align-self: flex-end"),
            Value::AlignSelf(Some(AlignItems::FlexEnd))
        );
        assert!(parse("align-items: auto").is_empty());
        let overflow = parse("overflow: hidden auto");
        assert_eq!(
            overflow
                .iter()
                .map(|(n, _, _)| n.as_str())
                .collect::<Vec<_>>(),
            ["overflow-x", "overflow-y"]
        );
        assert_eq!(
            overflow[1].1,
            DeclaredValue::Value(Value::Overflow(Overflow::Hidden)),
            "auto computes to hidden"
        );
        assert_eq!(
            parse("overflow: visible")[1].1,
            parse("overflow: visible")[0].1
        );
        assert!(parse("justify-content: space").is_empty());
    }

    #[test]
    fn positioning_longhands_parse_and_map_fixed_and_sticky() {
        // Spec 0006, AC-1 and Unsupported keywords
        assert_eq!(
            value("position: absolute"),
            Value::Position(Position::Absolute)
        );
        assert_eq!(
            value("position: fixed"),
            Value::Position(Position::Absolute)
        );
        assert_eq!(
            value("position: sticky"),
            Value::Position(Position::Relative)
        );
        assert!(parse("position: float").is_empty());
        assert_eq!(value("top: auto"), Value::Size(None));
        assert_eq!(
            value("left: -10%"),
            Value::Size(Some(SpecifiedLength::Percent(-10.0)))
        );
        assert_eq!(value("z-index: -3"), Value::ZIndex(Some(-3)));
        assert_eq!(value("z-index: auto"), Value::ZIndex(None));
        assert!(parse("z-index: 1.5").is_empty());
        assert_eq!(
            value("text-overflow: ellipsis"),
            Value::TextOverflow(TextOverflow::Ellipsis)
        );
        assert!(parse("text-overflow: \"…\"").is_empty());
    }

    #[test]
    fn cursor_and_user_select_keep_the_profile_and_map_the_rest() {
        // Spec 0006, AC-1 and Unsupported keywords
        assert_eq!(value("cursor: pointer"), Value::Cursor(Cursor::Pointer));
        assert_eq!(value("cursor: TEXT"), Value::Cursor(Cursor::Text));
        assert_eq!(value("cursor: grab"), Value::Cursor(Cursor::Auto));
        assert_eq!(
            value("cursor: url(hand.png) 4 4, url(b.cur), pointer"),
            Value::Cursor(Cursor::Pointer),
            "images are skipped for the keyword after them"
        );
        assert!(parse("cursor: url(hand.png)").is_empty());
        assert!(parse("cursor: banana").is_empty());
        assert_eq!(
            value("user-select: none"),
            Value::UserSelect(UserSelect::None)
        );
        assert_eq!(
            value("user-select: all"),
            Value::UserSelect(UserSelect::Auto)
        );
        assert!(Longhand::Cursor.is_inherited() && Longhand::UserSelect.is_inherited());
    }

    #[test]
    fn outline_parses_its_longhands_and_maps_styles_to_solid() {
        // Spec 0006, AC-1 and Unsupported keywords
        let outline = parse("outline: dashed 2px");
        assert_eq!(
            outline
                .iter()
                .map(|(n, _, _)| n.as_str())
                .collect::<Vec<_>>(),
            ["outline-width", "outline-style", "outline-color"]
        );
        assert_eq!(
            outline[1].1,
            DeclaredValue::Value(Value::OutlineStyle(OutlineStyle::Solid))
        );
        assert_eq!(
            outline[2].1,
            DeclaredValue::Value(Value::Color(SpecifiedColor::CurrentColor))
        );
        assert_eq!(
            value("outline-style: auto"),
            Value::OutlineStyle(OutlineStyle::Solid)
        );
        assert_eq!(
            value("outline-offset: -2px"),
            Value::Length(SpecifiedLength::Px(-2.0))
        );
        assert!(parse("outline-offset: 10%").is_empty());
        assert!(parse("outline: 2px 3px").is_empty());
        assert!(parse("outline-style: hidden").is_empty());
    }

    #[test]
    fn each_mapped_flex_keyword_is_noted_once_per_sheet() {
        let context = ParseContext::default();
        for text in ["row-reverse", "row-reverse", "column-reverse"] {
            let mut parser = Parser::new(text);
            parse_declaration("flex-direction", &mut parser, &context).unwrap();
        }
        assert_eq!(*context.mapped.borrow(), ["row-reverse", "column-reverse"]);
    }

    #[test]
    fn invalid_values_and_unknown_properties_drop_alone() {
        // AC-20
        for bad in [
            "colour: red",
            "color: 12px",
            "width: -3px",
            "margin: 1px 2px 3px 4px 5px",
            "display: banana",
            "opacity: red",
            "color: red !ie",
            "font: bold",
            "background-image: element(#a)",
        ] {
            assert!(parse(bad).is_empty(), "{bad} should be dropped");
        }
    }

    #[test]
    fn var_values_and_wide_keywords_wait_for_the_cascade() {
        let var = parse("margin: var(--gap) 0 !important");
        assert_eq!(var.len(), 4);
        assert!(var.iter().all(|(_, _, important)| *important));
        assert!(matches!(&var[0].1, DeclaredValue::Unparsed(u) if &*u.css == "var(--gap) 0"));

        let custom = parse("--Brand-Color:  #c00 ");
        assert_eq!(custom[0].0, "--Brand-Color");
        assert_eq!(custom[0].1, DeclaredValue::Custom(Rc::from("#c00")));

        assert!(matches!(
            parse("border-color: inherit").as_slice(),
            [(_, DeclaredValue::Wide(CssWide::Inherit), _), ..]
        ));
        assert_eq!(value("display: flex"), Value::Display(Display::Flex));
        assert_eq!(
            value("display: inline-flex"),
            Value::Display(Display::InlineBlock),
            "inline flex lays out as an inline block (spec 0006, AC-1)"
        );
    }
}
