//! The slice 1 property table: longhands, shorthands and declarations
//! (spec 0004, child 0003, Property table, AC-20).
//!
//! Every longhand has a typed parser returning a [`Value`]; a shorthand
//! parses into the values of its longhands. A declaration whose value holds
//! `var()` keeps its text and is parsed after substitution, at computed
//! value time.

use std::cell::RefCell;
use std::rc::Rc;

use base::Au;
use base::atoms::Atom;
use cssparser::{Delimiter, ParseError, Parser, Token, parse_important};

use crate::style::{ContainerName, ContainerType};
use crate::values::{
    LengthRules, ParseResult, SpecifiedCalcExpr, SpecifiedColor, SpecifiedLength, invalid,
    parse_color, parse_font_family, parse_keyword, parse_length, parse_url,
};
use crate::{
    AlignItems, AspectRatio, BorderCollapse, BorderStyle, BoxSizing, CalcExpr, CaptionSide, Clear,
    ClipRect, Cursor, Display, EmptyCells, FlexDirection, FlexWrap, Float, FontStyle, GridArea,
    GridAutoFlow, GridLine, JustifyContent, Length, ListStylePosition, ListStyleType, OutlineStyle,
    Overflow, OverflowWrap, Position, RepeatCount, TableLayout, TextAlign, TextDecorationLine,
    TextOverflow, Transform, TransformFn, TransformStyle, UserSelect, VerticalAlign, Visibility,
    WhiteSpace,
};

use crate::style::{Angle, AnimationDirection, FillMode, PlayState, TimingFunction};

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
    OverflowWrap = "overflow-wrap", true;
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
    AlignContent = "align-content", false;
    JustifyItems = "justify-items", false;
    AlignItems = "align-items", false;
    JustifySelf = "justify-self", false;
    AlignSelf = "align-self", false;
    Order = "order", false;
    Transform = "transform", false;
    Translate = "translate", false;
    Rotate = "rotate", false;
    Scale = "scale", false;
    TransformOrigin = "transform-origin", false;
    TransformStyle = "transform-style", false;
    Perspective = "perspective", false;
    PerspectiveOrigin = "perspective-origin", false;
    GridTemplateColumns = "grid-template-columns", false;
    GridTemplateRows = "grid-template-rows", false;
    GridAutoColumns = "grid-auto-columns", false;
    GridAutoRows = "grid-auto-rows", false;
    GridAutoFlow = "grid-auto-flow", false;
    GridColumnStart = "grid-column-start", false;
    GridColumnEnd = "grid-column-end", false;
    GridRowStart = "grid-row-start", false;
    GridRowEnd = "grid-row-end", false;
    GridTemplateAreas = "grid-template-areas", false;
    OverflowX = "overflow-x", false;
    OverflowY = "overflow-y", false;
    Float = "float", false;
    Clear = "clear", false;
    Position = "position", false;
    Top = "top", false;
    Right = "right", false;
    Bottom = "bottom", false;
    Left = "left", false;
    Clip = "clip", false;
    ZIndex = "z-index", false;
    OutlineWidth = "outline-width", false;
    OutlineStyle = "outline-style", false;
    OutlineColor = "outline-color", false;
    OutlineOffset = "outline-offset", false;
    TextOverflow = "text-overflow", false;
    Cursor = "cursor", true;
    UserSelect = "user-select", true;
    VerticalAlign = "vertical-align", false;
    TableLayout = "table-layout", false;
    BorderCollapse = "border-collapse", true;
    BorderSpacing = "border-spacing", true;
    CaptionSide = "caption-side", true;
    EmptyCells = "empty-cells", true;
    AspectRatio = "aspect-ratio", false;
    ContainerType = "container-type", false;
    ContainerName = "container-name", false;
    AnimationName = "animation-name", false;
    AnimationDuration = "animation-duration", false;
    AnimationDelay = "animation-delay", false;
    AnimationTimingFunction = "animation-timing-function", false;
    AnimationIterationCount = "animation-iteration-count", false;
    AnimationDirection = "animation-direction", false;
    AnimationFillMode = "animation-fill-mode", false;
    AnimationPlayState = "animation-play-state", false;
    TransitionProperty = "transition-property", false;
    TransitionDuration = "transition-duration", false;
    TransitionDelay = "transition-delay", false;
    TransitionTimingFunction = "transition-timing-function", false;
}

impl Longhand {
    /// How many longhands there are.
    pub(crate) const COUNT: usize = Self::ALL.len();

    /// The longhand's position in [`Longhand::ALL`].
    pub(crate) fn index(self) -> usize {
        self as usize
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
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
    PlaceContent,
    PlaceItems,
    PlaceSelf,
    GridRow,
    GridColumn,
    GridArea,
    GridTemplate,
    Grid,
    Animation,
    Transition,
}

impl Shorthand {
    pub(crate) fn from_name(name: &str) -> Option<Self> {
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
            "place-content" => Self::PlaceContent,
            "place-items" => Self::PlaceItems,
            "place-self" => Self::PlaceSelf,
            "grid-row" => Self::GridRow,
            "grid-column" => Self::GridColumn,
            "grid-area" => Self::GridArea,
            "grid-template" => Self::GridTemplate,
            "grid" => Self::Grid,
            "animation" => Self::Animation,
            "transition" => Self::Transition,
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
            Self::PlaceContent => &[L::AlignContent, L::JustifyContent],
            Self::PlaceItems => &[L::AlignItems, L::JustifyItems],
            Self::PlaceSelf => &[L::AlignSelf, L::JustifySelf],
            Self::GridRow => &[L::GridRowStart, L::GridRowEnd],
            Self::GridColumn => &[L::GridColumnStart, L::GridColumnEnd],
            Self::GridArea => &[
                L::GridRowStart,
                L::GridColumnStart,
                L::GridRowEnd,
                L::GridColumnEnd,
            ],
            Self::GridTemplate => &[
                L::GridTemplateRows,
                L::GridTemplateColumns,
                L::GridTemplateAreas,
            ],
            Self::Grid => &[
                L::GridTemplateRows,
                L::GridTemplateColumns,
                L::GridTemplateAreas,
                L::GridAutoRows,
                L::GridAutoColumns,
                L::GridAutoFlow,
            ],
            Self::Animation => &[
                L::AnimationName,
                L::AnimationDuration,
                L::AnimationTimingFunction,
                L::AnimationDelay,
                L::AnimationIterationCount,
                L::AnimationDirection,
                L::AnimationFillMode,
                L::AnimationPlayState,
            ],
            Self::Transition => &[
                L::TransitionProperty,
                L::TransitionDuration,
                L::TransitionDelay,
                L::TransitionTimingFunction,
            ],
        }
    }
}

/// `font-size` as written.
#[derive(Debug, Clone, PartialEq)]
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
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum LineHeight {
    /// `normal`.
    Normal,
    /// A multiple of the font size, inherited as the number.
    Number(f32),
    /// A length, or a percentage of the font size.
    Length(SpecifiedLength),
}

/// `flex-basis` as written.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum FlexBasis {
    /// `auto`.
    Auto,
    /// `content`.
    Content,
    /// A length or percentage.
    Length(SpecifiedLength),
}

/// A grid template as written: its track entries keep lengths specified so
/// the cascade can resolve `em` and `calc()` against the computed font size
/// (spec 0010, AC-1).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct SpecifiedTrackList {
    /// The entries in source order.
    pub(crate) entries: Vec<SpecifiedTrackEntry>,
}

/// One entry of a [`SpecifiedTrackList`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SpecifiedTrackEntry {
    /// A single track size.
    Size(SpecifiedTrackSize),
    /// Names for the line at this position.
    LineNames(Vec<Atom>),
    /// `repeat(count, tracks)`.
    Repeat {
        /// How many times to repeat.
        count: RepeatCount,
        /// The tracks to repeat.
        tracks: Vec<SpecifiedTrackEntry>,
    },
}

/// One track's size as written.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SpecifiedTrackSize {
    /// A single breadth.
    Breadth(SpecifiedBreadth),
    /// `minmax(min, max)`.
    MinMax(SpecifiedBreadth, SpecifiedBreadth),
    /// `fit-content(<length>)`.
    FitContent(SpecifiedLength),
}

/// A track breadth as written.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SpecifiedBreadth {
    /// A fixed length or percentage.
    Length(SpecifiedLength),
    /// A fraction of the free space.
    Fr(f32),
    /// `auto`.
    Auto,
    /// `min-content`.
    MinContent,
    /// `max-content`.
    MaxContent,
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
    OverflowWrap(OverflowWrap),
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
    /// `align-self` or `justify-self`; `None` is `auto`.
    AlignSelf(Option<AlignItems>),
    Overflow(Overflow),
    Float(Float),
    Clear(Clear),
    Position(Position),
    /// `z-index`; `None` is `auto`.
    ZIndex(Option<i32>),
    /// `clip: rect(...)`, or `None` for `auto`.
    Clip(Option<ClipRect>),
    OutlineStyle(OutlineStyle),
    TextOverflow(TextOverflow),
    Cursor(Cursor),
    UserSelect(UserSelect),
    VerticalAlign(VerticalAlign),
    TableLayout(TableLayout),
    BorderCollapse(BorderCollapse),
    BorderSpacing((Au, Au)),
    CaptionSide(CaptionSide),
    EmptyCells(EmptyCells),
    /// `aspect-ratio`: the computed width-to-height ratio.
    AspectRatio(AspectRatio),
    /// `container-type`: `normal`, `inline-size` or `size` (spec 0010, AC-16).
    ContainerType(ContainerType),
    /// `container-name`: custom-ident names (spec 0010, AC-16).
    ContainerName(ContainerName),
    /// `animation-name`: the keyframes animation to play.
    AnimationName(Atom),
    /// `animation-duration`: seconds.
    AnimationDuration(f64),
    /// `animation-delay`: seconds.
    AnimationDelay(f64),
    /// `animation-timing-function`.
    AnimationTimingFunction(TimingFunction),
    /// `animation-iteration-count`: `None` means `infinite`.
    AnimationIterationCount(Option<f64>),
    /// `animation-direction`.
    AnimationDirection(AnimationDirection),
    /// `animation-fill-mode`.
    AnimationFillMode(FillMode),
    /// `animation-play-state`: running or paused.
    AnimationPlayState(PlayState),
    /// `transition-property`: `None` means `all`.
    TransitionProperty(Option<Atom>),
    /// `transition-duration`: seconds.
    TransitionDuration(f64),
    /// `transition-delay`: seconds.
    TransitionDelay(f64),
    /// `transition-timing-function`.
    TransitionTimingFunction(TimingFunction),
    /// `transform`: the computed transform functions.
    Transform(Rc<Transform>),
    /// `translate`: the computed translate offsets.
    Translate(Length, Option<Length>),
    /// `rotate`: the computed rotation angle.
    Rotate(Angle),
    /// `scale`: the computed scale factors (unitless numbers).
    Scale(f32, Option<f32>),
    /// `transform-origin`: origin in border box coordinates.
    TransformOrigin(Length, Length),
    /// `transform-style`: flat or preserve-3d (flat renders both here).
    TransformStyle(TransformStyle),
    /// `perspective`: distance to the viewer.
    Perspective(Length),
    /// `perspective-origin`: origin of the perspective view.
    PerspectiveOrigin(Length, Length),
    /// `grid-template-columns` / `grid-template-rows`.
    GridTracks(SpecifiedTrackList),
    /// `grid-auto-columns` / `grid-auto-rows`.
    GridAutoTracks(Vec<SpecifiedTrackSize>),
    /// `grid-auto-flow`.
    GridAutoFlow(GridAutoFlow),
    /// `grid-column-start` / `grid-column-end` / `grid-row-start` /
    /// `grid-row-end`.
    GridLine(GridLine),
    /// `grid-template-areas`: named area cell ranges.
    GridAreas(Vec<GridArea>),
}

impl Value {
    fn uses_viewport(&self) -> bool {
        match self {
            Self::Size(Some(length))
            | Self::Length(length)
            | Self::FlexBasis(FlexBasis::Length(length))
            | Self::FontSize(FontSize::Length(length))
            | Self::LineHeight(LineHeight::Length(length)) => length.uses_viewport(),
            Self::GridTracks(_)
            | Self::GridAutoTracks(_)
            | Self::GridAutoFlow(_)
            | Self::GridLine(_)
            | Self::GridAreas(_) => false,
            _ => false,
        }
    }
}

/// The `<syntax>` of a registered custom property, as written in `@property`
/// (spec 0010, AC-10). `*` accepts anything; a named type accepts only that
/// type; `|` lists accept any alternative; `+` and `#` accept one or more
/// comma separated repetitions of the inner type.
#[derive(Debug, Clone, PartialEq)]
pub enum PropertySyntax {
    /// `*`: any value.
    Star,
    /// `<length>`: a length or percentage.
    Length,
    /// `<number>`: a number.
    Number,
    /// `<percentage>`: a percentage.
    Percentage,
    /// `<length-percentage>`: a length or a percentage.
    LengthPercentage,
    /// `<color>`: a color.
    Color,
    /// `<angle>`: an angle.
    Angle,
    /// `<integer>`: an integer.
    Integer,
    /// `<transform-list>`: a list of transform functions.
    TransformList,
    /// `a | b`: any of the alternatives.
    Or(Vec<PropertySyntax>),
    /// `a+` or `a#`: one or more comma separated repetitions of `a`.
    Repeated(Box<PropertySyntax>),
}

/// Whether `text` parses as `syntax`. `text` is the raw declared value of a
/// custom property, after `var()` substitution.
pub(crate) fn syntax_matches(text: &str, syntax: &PropertySyntax) -> bool {
    let text = text.trim();
    if text.is_empty() {
        return false;
    }
    match syntax {
        PropertySyntax::Star => true,
        PropertySyntax::Length => parse_length_only(text).is_some(),
        PropertySyntax::Number => text.parse::<f32>().is_ok(),
        PropertySyntax::Percentage => parse_percentage_only(text).is_some(),
        PropertySyntax::LengthPercentage => {
            parse_length_only(text).is_some() || parse_percentage_only(text).is_some()
        }
        PropertySyntax::Color => parse_color_only(text).is_some(),
        PropertySyntax::Angle => parse_angle_only(text).is_some(),
        PropertySyntax::Integer => text.parse::<i32>().is_ok(),
        PropertySyntax::TransformList => parse_transform_list_only(text).is_some(),
        PropertySyntax::Or(alts) => alts.iter().any(|alt| syntax_matches(text, alt)),
        PropertySyntax::Repeated(inner) => {
            let parts: Vec<&str> = text.split(',').map(|p| p.trim()).collect();
            !parts.is_empty() && parts.iter().all(|p| syntax_matches(p, inner))
        }
    }
}

/// Parses a single `<length>` or `<length-percentage>` from `text`, accepting
/// only when the whole string is consumed.
fn parse_length_only(text: &str) -> Option<SpecifiedLength> {
    let mut parser = Parser::new(text);
    let value = parse_length(&mut parser, LengthRules::ANY).ok()?;
    parser.is_exhausted().then_some(value)
}

/// Parses a single `<percentage>` from `text`, accepting only the whole string.
fn parse_percentage_only(text: &str) -> Option<SpecifiedLength> {
    let mut parser = Parser::new(text);
    match *parser.next().ok()? {
        Token::Percentage { unit_value, .. } => parser
            .is_exhausted()
            .then_some(SpecifiedLength::Percent(unit_value * 100.0)),
        _ => None,
    }
}

/// Parses a single `<color>` from `text`, accepting only the whole string.
fn parse_color_only(text: &str) -> Option<SpecifiedColor> {
    let mut parser = Parser::new(text);
    let value = parse_color(&mut parser).ok()?;
    parser.is_exhausted().then_some(value)
}

/// Parses a single `<angle>` from `text`, accepting only the whole string.
fn parse_angle_only(text: &str) -> Option<()> {
    let mut parser = Parser::new(text);
    let ok = match *parser.next().ok()? {
        Token::Dimension { ref unit, .. } => {
            let unit = unit.to_ascii_lowercase();
            matches!(unit.as_ref(), "deg" | "rad" | "grad" | "turn")
        }
        Token::Number { value: 0.0, .. } => true,
        _ => false,
    };
    parser.is_exhausted().then_some(()).filter(|_| ok)
}

/// Parses a single `<transform-list>` from `text`, accepting only the whole
/// string. One or more transform functions separated by whitespace or commas.
fn parse_transform_list_only(text: &str) -> Option<()> {
    let mut parser = Parser::new(text);
    let mut saw_fn = false;
    loop {
        match parser.next_including_whitespace_and_comments() {
            Ok(Token::Function(name)) => {
                let name = name.as_ref().to_ascii_lowercase();
                if !matches!(
                    name.as_str(),
                    "translate"
                        | "translatex"
                        | "translatey"
                        | "translatez"
                        | "scale"
                        | "scalex"
                        | "scaley"
                        | "scalez"
                        | "scale3d"
                        | "rotate"
                        | "rotatex"
                        | "rotatey"
                        | "rotatez"
                        | "rotate3d"
                        | "skew"
                        | "skewx"
                        | "skewy"
                        | "matrix"
                        | "matrix3d"
                        | "perspective"
                ) {
                    return None;
                }
                saw_fn = true;
                parser
                    .parse_nested_block(|_| -> Result<(), ParseError<()>> { Ok(()) })
                    .ok()?;
            }
            Ok(Token::WhiteSpace(_)) | Ok(Token::Comment(_)) | Ok(Token::Comma) => continue,
            Ok(_) => break,
            Err(_) => break,
        }
    }
    parser.is_exhausted().then_some(()).filter(|_| saw_fn)
}

/// Parses the `syntax` string of an `@property` rule (spec 0010, AC-10).
/// Handles `*` and the simple bracketed types (`<length>`, `<number>`, and so
/// on). `|` / `+` / `#` micro syntax is not parsed here; such a rule is
/// ignored as a whole, which is spec compliant.
pub(crate) fn parse_property_syntax(text: &str) -> Option<PropertySyntax> {
    let text = text.trim().trim_matches(|c| c == '"' || c == '\'');
    if text.eq_ignore_ascii_case("*") {
        return Some(PropertySyntax::Star);
    }
    let inner = text.strip_prefix('<').and_then(|t| t.strip_suffix('>'))?;
    match inner.to_ascii_lowercase().as_str() {
        "length" => Some(PropertySyntax::Length),
        "number" => Some(PropertySyntax::Number),
        "percentage" => Some(PropertySyntax::Percentage),
        "length-percentage" => Some(PropertySyntax::LengthPercentage),
        "color" => Some(PropertySyntax::Color),
        "angle" => Some(PropertySyntax::Angle),
        "integer" => Some(PropertySyntax::Integer),
        "transform-list" => Some(PropertySyntax::TransformList),
        _ => None,
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
        L::OverflowWrap => Value::OverflowWrap(parse_keyword(
            input,
            &[
                ("normal", OverflowWrap::Normal),
                ("break-word", OverflowWrap::BreakWord),
                ("anywhere", OverflowWrap::BreakWord),
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
        L::JustifyContent | L::AlignContent => {
            Value::JustifyContent(parse_content_alignment(input, context, longhand)?)
        }
        L::JustifyItems | L::AlignItems => {
            Value::AlignItems(parse_item_alignment(input, context, longhand)?)
        }
        L::JustifySelf | L::AlignSelf => {
            if input.try_parse(|i| i.expect_ident_matching("auto")).is_ok() {
                Value::AlignSelf(None)
            } else {
                Value::AlignSelf(Some(parse_item_alignment(input, context, longhand)?))
            }
        }
        L::Order => Value::Integer(input.expect_integer()?),
        L::Transform => Value::Transform(Rc::new(parse_transform(input)?)),
        L::Translate => {
            let (x, y) = parse_translate(input)?;
            Value::Translate(x, y)
        }
        L::Rotate => Value::Rotate(parse_rotate(input)?),
        L::Scale => {
            let (x, y) = parse_scale(input)?;
            Value::Scale(x, y)
        }
        L::TransformOrigin => {
            let (x, y) = parse_transform_origin(input)?;
            Value::TransformOrigin(x, y)
        }
        L::TransformStyle => Value::TransformStyle(parse_transform_style(input)?),
        L::Perspective => Value::Perspective(parse_perspective(input)?),
        L::PerspectiveOrigin => {
            let (x, y) = parse_perspective_origin(input)?;
            Value::PerspectiveOrigin(x, y)
        }
        L::GridTemplateColumns | L::GridTemplateRows => Value::GridTracks(parse_track_list(input)?),
        L::GridAutoColumns | L::GridAutoRows => {
            Value::GridAutoTracks(parse_track_size_list(input)?)
        }
        L::GridAutoFlow => Value::GridAutoFlow(parse_auto_flow(input)?),
        L::GridTemplateAreas => Value::GridAreas(parse_grid_template_areas(input)?),
        L::GridColumnStart | L::GridColumnEnd | L::GridRowStart | L::GridRowEnd => {
            Value::GridLine(parse_grid_line(input)?)
        }
        L::OverflowX | L::OverflowY => Value::Overflow(parse_overflow(input, context, longhand)?),
        L::Float => Value::Float(parse_float(input)?),
        L::Clear => Value::Clear(parse_clear(input)?),
        L::Position => Value::Position(parse_keyword(
            input,
            &[
                ("static", Position::Static),
                ("relative", Position::Relative),
                ("absolute", Position::Absolute),
                ("fixed", Position::Fixed),
                ("sticky", Position::Sticky),
            ],
        )?),
        L::Top | L::Right | L::Bottom | L::Left => {
            Value::Size(parse_or_keyword(input, "auto", LengthRules::ANY)?)
        }
        L::Clip => Value::Clip(parse_clip(input)?),
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
        L::VerticalAlign => Value::VerticalAlign(parse_vertical_align(input)?),
        L::TableLayout => Value::TableLayout(parse_mapped_keyword(
            input,
            context,
            longhand,
            &[("auto", TableLayout::Auto)],
            &[("fixed", TableLayout::Fixed)],
        )?),
        L::BorderCollapse => Value::BorderCollapse(parse_mapped_keyword(
            input,
            context,
            longhand,
            &[
                ("separate", BorderCollapse::Separate),
                ("collapse", BorderCollapse::Collapse),
            ],
            &[],
        )?),
        L::BorderSpacing => Value::BorderSpacing(parse_border_spacing(input)?),
        L::CaptionSide => Value::CaptionSide(parse_mapped_keyword(
            input,
            context,
            longhand,
            &[("top", CaptionSide::Top)],
            &[("bottom", CaptionSide::Bottom)],
        )?),
        L::EmptyCells => Value::EmptyCells(parse_mapped_keyword(
            input,
            context,
            longhand,
            &[("show", EmptyCells::Show)],
            &[("hide", EmptyCells::Hide)],
        )?),
        L::AspectRatio => Value::AspectRatio(parse_aspect_ratio(input)?),
        L::ContainerType => Value::ContainerType(parse_container_type(input)?),
        L::ContainerName => Value::ContainerName(parse_container_name(input)?),
        L::AnimationName => Value::AnimationName(Atom::from(input.expect_ident()?.as_ref())),
        L::AnimationDuration => Value::AnimationDuration(parse_css_time(input)?),
        L::AnimationDelay => Value::AnimationDelay(parse_css_time(input)?),
        L::AnimationTimingFunction => {
            Value::AnimationTimingFunction(parse_timing_function(input, context, longhand)?)
        }
        L::AnimationIterationCount => {
            if input
                .try_parse(|i| i.expect_ident_matching("infinite"))
                .is_ok()
            {
                Value::AnimationIterationCount(None)
            } else {
                Value::AnimationIterationCount(Some(input.expect_number()? as f64))
            }
        }
        L::AnimationDirection => {
            Value::AnimationDirection(parse_animation_direction(input, context, longhand)?)
        }
        L::AnimationFillMode => {
            Value::AnimationFillMode(parse_animation_fill_mode(input, context, longhand)?)
        }
        L::AnimationPlayState => {
            Value::AnimationPlayState(parse_animation_play_state(input, context, longhand)?)
        }
        L::TransitionProperty => {
            if input.try_parse(|i| i.expect_ident_matching("all")).is_ok() {
                Value::TransitionProperty(None)
            } else {
                Value::TransitionProperty(Some(Atom::from(input.expect_ident()?.as_ref())))
            }
        }
        L::TransitionDuration => Value::TransitionDuration(parse_css_time(input)?),
        L::TransitionDelay => Value::TransitionDelay(parse_css_time(input)?),
        L::TransitionTimingFunction => {
            Value::TransitionTimingFunction(parse_timing_function(input, context, longhand)?)
        }
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
            ("zoom-in", Cursor::ZoomIn),
            ("zoom-out", Cursor::ZoomOut),
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

/// The initial border width, `medium`.
const MEDIUM_BORDER: SpecifiedLength = SpecifiedLength::Px(3.0);

/// One `overflow` axis (spec 0009, child 0002, AC-6). No keyword maps away
/// anymore: `scroll`, `auto` and `clip` compute to themselves.
fn parse_overflow(
    input: &mut Parser<'_>,
    _context: &ParseContext,
    _longhand: Longhand,
) -> ParseResult<Overflow> {
    parse_keyword(
        input,
        &[
            ("visible", Overflow::Visible),
            ("hidden", Overflow::Hidden),
            ("clip", Overflow::Clip),
            ("scroll", Overflow::Scroll),
            ("auto", Overflow::Auto),
        ],
    )
}

/// `float`: `none`, `left` or `right` (spec 0009, child 0001).
fn parse_float(input: &mut Parser<'_>) -> ParseResult<Float> {
    parse_keyword(
        input,
        &[
            ("none", Float::None),
            ("left", Float::Left),
            ("right", Float::Right),
        ],
    )
}

/// `clear`: `none`, `left`, `right` or `both` (spec 0009, child 0001).
fn parse_clear(input: &mut Parser<'_>) -> ParseResult<Clear> {
    parse_keyword(
        input,
        &[
            ("none", Clear::None),
            ("left", Clear::Left),
            ("right", Clear::Right),
            ("both", Clear::Both),
        ],
    )
}

/// `vertical-align`: `baseline`, `top`, `middle`, `bottom`. Any other
/// keyword acts as `baseline` (spec 0009, child 0004, AC-16).
fn parse_vertical_align(input: &mut Parser<'_>) -> ParseResult<VerticalAlign> {
    let ident = input.expect_ident()?;
    Ok(match ident.to_ascii_lowercase().as_str() {
        "baseline" => VerticalAlign::Baseline,
        "top" => VerticalAlign::Top,
        "middle" => VerticalAlign::Middle,
        "bottom" => VerticalAlign::Bottom,
        _ => VerticalAlign::Baseline,
    })
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

/// `linear`, `ease`, `ease-in`, `ease-out`, `ease-in-out`, `steps(n)`,
/// `step-start`, `step-end` or `cubic-bezier(...)` (spec 0010, AC-23).
/// `cubic-bezier` and `steps` with a timing other than `linear` fall back to
/// `ease` here, which is fine for the values this slice animates.
fn parse_timing_function(
    input: &mut Parser<'_>,
    _context: &ParseContext,
    _longhand: Longhand,
) -> ParseResult<TimingFunction> {
    let ident = input.expect_ident()?;
    // Only the keyword timing functions: any other ident is not a timing
    // function, and in the `animation` shorthand it is the animation name
    // (spec 0010, AC-24).
    Ok(if ident.eq_ignore_ascii_case("linear") {
        TimingFunction::Linear
    } else if ident.eq_ignore_ascii_case("ease") {
        TimingFunction::Ease
    } else if ident.eq_ignore_ascii_case("ease-in") {
        TimingFunction::EaseIn
    } else if ident.eq_ignore_ascii_case("ease-out") {
        TimingFunction::EaseOut
    } else if ident.eq_ignore_ascii_case("ease-in-out") {
        TimingFunction::EaseInOut
    } else if ident.eq_ignore_ascii_case("step-start") || ident.eq_ignore_ascii_case("step-end") {
        TimingFunction::Steps(1)
    } else {
        return invalid();
    })
}

/// `normal`, `reverse`, `alternate`, `alternate-reverse` (spec 0010, AC-24).
fn parse_animation_direction(
    input: &mut Parser<'_>,
    context: &ParseContext,
    longhand: Longhand,
) -> ParseResult<AnimationDirection> {
    parse_mapped_keyword(
        input,
        context,
        longhand,
        &[
            ("normal", AnimationDirection::Normal),
            ("reverse", AnimationDirection::Reverse),
            ("alternate", AnimationDirection::Alternate),
            ("alternate-reverse", AnimationDirection::AlternateReverse),
        ],
        &[],
    )
}

/// `none`, `forwards`, `backwards`, `both` (spec 0010, AC-24).
fn parse_animation_fill_mode(
    input: &mut Parser<'_>,
    context: &ParseContext,
    longhand: Longhand,
) -> ParseResult<FillMode> {
    parse_mapped_keyword(
        input,
        context,
        longhand,
        &[
            ("none", FillMode::None),
            ("forwards", FillMode::Forwards),
            ("backwards", FillMode::Backwards),
            ("both", FillMode::Both),
        ],
        &[],
    )
}

/// `running` or `paused` (spec 0010, AC-24).
fn parse_animation_play_state(
    input: &mut Parser<'_>,
    context: &ParseContext,
    longhand: Longhand,
) -> ParseResult<PlayState> {
    parse_mapped_keyword(
        input,
        context,
        longhand,
        &[
            ("running", PlayState::Running),
            ("paused", PlayState::Paused),
        ],
        &[],
    )
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

/// `justify-content` or `align-content`: the content distribution used by
/// flex and grid (spec 0010, AC-4). `safe` and `unsafe` are accepted and do
/// not change the result.
fn parse_content_alignment(
    input: &mut Parser<'_>,
    context: &ParseContext,
    longhand: Longhand,
) -> ParseResult<JustifyContent> {
    skip_overflow_position(input);
    parse_mapped_keyword(
        input,
        context,
        longhand,
        &[
            ("normal", JustifyContent::Normal),
            ("start", JustifyContent::Start),
            ("end", JustifyContent::End),
            ("flex-start", JustifyContent::FlexStart),
            ("flex-end", JustifyContent::FlexEnd),
            ("center", JustifyContent::Center),
            ("stretch", JustifyContent::Stretch),
            ("space-between", JustifyContent::SpaceBetween),
            ("space-around", JustifyContent::SpaceAround),
            ("space-evenly", JustifyContent::SpaceEvenly),
        ],
        &[
            ("left", JustifyContent::Start),
            ("right", JustifyContent::End),
        ],
    )
}

/// `align-items`, `justify-items`, `align-self` or `justify-self`: the item
/// alignment used by flex and grid (spec 0010, AC-4). `baseline` behaves as
/// `start` in this feature.
fn parse_item_alignment(
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
        return Ok(AlignItems::Baseline);
    }
    skip_overflow_position(input);
    parse_mapped_keyword(
        input,
        context,
        longhand,
        &[
            ("normal", AlignItems::Normal),
            ("stretch", AlignItems::Stretch),
            ("start", AlignItems::Start),
            ("end", AlignItems::End),
            ("self-start", AlignItems::Start),
            ("self-end", AlignItems::End),
            ("flex-start", AlignItems::FlexStart),
            ("flex-end", AlignItems::FlexEnd),
            ("center", AlignItems::Center),
            ("baseline", AlignItems::Baseline),
        ],
        &[],
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
        "inline-grid" => Display::InlineGrid,
        "contents" | "ruby" | "ruby-text" | "ruby-base" => Display::Inline,
        // Block level ones (spec 0006, AC-3).
        "grid" => Display::Grid,
        "run-in" => Display::Block,
        // Table parts (spec 0009, child 0004, AC-14).
        "inline-table" => Display::InlineTable,
        "table" => Display::Table,
        "table-caption" => Display::TableCaption,
        "table-column-group" => Display::TableColumnGroup,
        "table-column" => Display::TableColumn,
        "table-row-group" => Display::TableRowGroup,
        "table-header-group" => Display::TableHeaderGroup,
        "table-footer-group" => Display::TableFooterGroup,
        "table-row" => Display::TableRow,
        "table-cell" => Display::TableCell,
        // `flow-root` is a block that always makes a BFC (spec 0009, child
        // 0001).
        "flow-root" => Display::FlowRoot,
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

/// `transform`: every 2D and 3D transform function (spec 0010, AC-19).
fn parse_transform(input: &mut Parser<'_>) -> ParseResult<Transform> {
    if input.try_parse(|i| i.expect_ident_matching("none")).is_ok() {
        return Ok(Transform::default());
    }
    let name = input.expect_function()?.to_ascii_lowercase();
    let fns = vec![input.parse_nested_block(|input| match name.as_str() {
        "translate" => {
            let x = parse_transform_length(input)?;
            let y = if input.try_parse(Parser::expect_comma).is_ok() {
                Some(parse_transform_length(input)?)
            } else {
                None
            };
            Ok(TransformFn::Translate(x, y))
        }
        "translatex" => Ok(TransformFn::TranslateX(parse_transform_length(input)?)),
        "translatey" => Ok(TransformFn::TranslateY(parse_transform_length(input)?)),
        "scale" => {
            let x = parse_transform_number(input)?;
            let y = if input.try_parse(Parser::expect_comma).is_ok() {
                Some(parse_transform_number(input)?)
            } else {
                None
            };
            Ok(TransformFn::Scale(x, y))
        }
        "scalex" => Ok(TransformFn::ScaleX(parse_transform_number(input)?)),
        "scaley" => Ok(TransformFn::ScaleY(parse_transform_number(input)?)),
        "rotate" => Ok(TransformFn::Rotate(parse_angle(input)?)),
        "skew" => {
            let ax = parse_angle(input)?;
            let ay = if input.try_parse(Parser::expect_comma).is_ok() {
                Some(parse_angle(input)?)
            } else {
                None
            };
            Ok(TransformFn::Skew(ax, ay))
        }
        "skewx" => Ok(TransformFn::SkewX(parse_angle(input)?)),
        "skewy" => Ok(TransformFn::SkewY(parse_angle(input)?)),
        "translatez" => Ok(TransformFn::TranslateZ(parse_transform_length(input)?)),
        "translate3d" => {
            let x = parse_transform_length(input)?;
            let y = if input.try_parse(Parser::expect_comma).is_ok() {
                parse_transform_length(input)?
            } else {
                return invalid();
            };
            let z = if input.try_parse(Parser::expect_comma).is_ok() {
                parse_transform_length(input)?
            } else {
                return invalid();
            };
            Ok(TransformFn::Translate3d(x, y, z))
        }
        "scalez" => Ok(TransformFn::ScaleZ(parse_transform_number(input)?)),
        "scale3d" => {
            let x = parse_transform_number(input)?;
            let y = if input.try_parse(Parser::expect_comma).is_ok() {
                parse_transform_number(input)?
            } else {
                return invalid();
            };
            let z = if input.try_parse(Parser::expect_comma).is_ok() {
                parse_transform_number(input)?
            } else {
                return invalid();
            };
            Ok(TransformFn::Scale3d(x, y, z))
        }
        "rotatex" => Ok(TransformFn::RotateX(parse_angle(input)?)),
        "rotatey" => Ok(TransformFn::RotateY(parse_angle(input)?)),
        "rotatez" => Ok(TransformFn::RotateZ(parse_angle(input)?)),
        "rotate3d" => {
            let x = parse_transform_number(input)?;
            let y = if input.try_parse(Parser::expect_comma).is_ok() {
                parse_transform_number(input)?
            } else {
                return invalid();
            };
            let z = if input.try_parse(Parser::expect_comma).is_ok() {
                parse_transform_number(input)?
            } else {
                return invalid();
            };
            let angle = parse_angle(input)?;
            Ok(TransformFn::Rotate3d(x, y, z, angle))
        }
        "matrix" => {
            let m = parse_matrix6(input)?;
            Ok(TransformFn::Matrix(m[0], m[1], m[2], m[3], m[4], m[5]))
        }
        "matrix3d" => {
            let m = parse_matrix16(input)?;
            Ok(TransformFn::Matrix3d(m))
        }
        "perspective" => Ok(TransformFn::Perspective(parse_transform_length(input)?)),
        _ => invalid(),
    })?];
    Ok(Transform {
        transform: Rc::from(fns),
        translate: None,
        rotate: None,
        scale: None,
        origin: Transform::default().origin,
        style: TransformStyle::Flat,
        perspective: None,
        perspective_origin: None,
        binding_id: None,
    })
}

/// `translate`, `scale`, `perspective`: a `<length>` or `<percentage>`.
fn parse_transform_length(input: &mut Parser<'_>) -> ParseResult<Length> {
    let sl = parse_length(input, LengthRules::ANY)?;
    match sl {
        SpecifiedLength::Px(px) => Ok(Length::px(px)),
        SpecifiedLength::Percent(p) => Ok(Length::Percent(p)),
        SpecifiedLength::Calc(expr) => {
            Ok(Length::Calc(Rc::new(convert_calc_expr((*expr).clone())?)))
        }
        _ => invalid(),
    }
}

/// A unitless transform number, as in `scale(1.5)`.
fn parse_transform_number(input: &mut Parser<'_>) -> ParseResult<f32> {
    match *input.next()? {
        cssparser::Token::Number { value, .. } => Ok(value),
        _ => invalid(),
    }
}

/// An `<angle>`: `deg`, `rad`, `grad` or `turn` (spec 0010, AC-19).
fn parse_angle(input: &mut Parser<'_>) -> ParseResult<Angle> {
    match *input.next()? {
        cssparser::Token::Dimension {
            value, ref unit, ..
        } => match unit.as_ref().to_ascii_lowercase().as_str() {
            "deg" => Ok(Angle::Deg(value)),
            "rad" => Ok(Angle::Rad(value)),
            "grad" => Ok(Angle::Grad(value)),
            "turn" => Ok(Angle::Turn(value)),
            _ => invalid(),
        },
        _ => invalid(),
    }
}

/// `matrix(a, b, c, d, tx, ty)`.
fn parse_matrix6(input: &mut Parser<'_>) -> ParseResult<[f32; 6]> {
    let mut m = [0.0; 6];
    for i in &mut m {
        *i = parse_transform_number(input)?;
    }
    Ok(m)
}

/// `matrix3d(...)`: 16 values.
fn parse_matrix16(input: &mut Parser<'_>) -> ParseResult<[f32; 16]> {
    let mut m = [0.0; 16];
    for i in &mut m {
        *i = parse_transform_number(input)?;
    }
    Ok(m)
}

/// Convert a specified calc expression to a computed-value-time calc
/// expression. Non-px leaves make the declaration drop.
fn convert_calc_expr(expr: SpecifiedCalcExpr) -> ParseResult<CalcExpr> {
    match expr {
        SpecifiedCalcExpr::Leaf(sl) => Ok(CalcExpr::Leaf(specified_length_to_length(sl)?)),
        SpecifiedCalcExpr::Sum(terms) => Ok(CalcExpr::Sum(
            terms
                .into_iter()
                .map(convert_calc_expr)
                .collect::<ParseResult<_>>()?,
        )),
        SpecifiedCalcExpr::Product(term, factor) => Ok(CalcExpr::Product(
            Box::new(convert_calc_expr((*term).clone())?),
            factor,
        )),
        SpecifiedCalcExpr::Min(terms) => Ok(CalcExpr::Min(
            terms
                .into_iter()
                .map(convert_calc_expr)
                .collect::<ParseResult<_>>()?,
        )),
        SpecifiedCalcExpr::Max(terms) => Ok(CalcExpr::Max(
            terms
                .into_iter()
                .map(convert_calc_expr)
                .collect::<ParseResult<_>>()?,
        )),
        SpecifiedCalcExpr::Clamp(a, b, c) => Ok(CalcExpr::Clamp(
            Box::new(convert_calc_expr((*a).clone())?),
            Box::new(convert_calc_expr((*b).clone())?),
            Box::new(convert_calc_expr((*c).clone())?),
        )),
    }
}

/// Convert a specified length to a computed-value-time length. Only px,
/// percent and calc-with-px leaves are supported here; anything else
/// drops the declaration (spec 0010, AC-8, invalid CSS is dropped).
fn specified_length_to_length(sl: SpecifiedLength) -> ParseResult<Length> {
    match sl {
        SpecifiedLength::Px(px) => Ok(Length::px(px)),
        SpecifiedLength::Percent(p) => Ok(Length::Percent(p)),
        SpecifiedLength::Calc(expr) => {
            Ok(Length::Calc(Rc::new(convert_calc_expr((*expr).clone())?)))
        }
        _ => invalid(),
    }
}

/// `translate(tx, ty?)`: parse the longhand `translate` property.
fn parse_translate(input: &mut Parser<'_>) -> ParseResult<(Length, Option<Length>)> {
    let x = parse_transform_length(input)?;
    let y = input.try_parse(parse_transform_length).ok();
    Ok((x, y))
}

/// `rotate(a)`: parse the longhand `rotate` property.
fn parse_rotate(input: &mut Parser<'_>) -> ParseResult<Angle> {
    parse_angle(input)
}

/// `scale(sx, sy?)`: parse the longhand `scale` property.
fn parse_scale(input: &mut Parser<'_>) -> ParseResult<(f32, Option<f32>)> {
    let x = parse_transform_number(input)?;
    let y = input.try_parse(parse_transform_number).ok();
    Ok((x, y))
}

/// `transform-origin`: one or two lengths, percentages or keywords, and
/// an optional z length this slice ignores (spec 0010, AC-19).
fn parse_transform_origin(input: &mut Parser<'_>) -> ParseResult<(Length, Length)> {
    let origin = parse_origin(input)?;
    let _z = input.try_parse(parse_transform_length);
    Ok(origin)
}

/// A `<position>` style origin: one or two of a length, a percentage or
/// `left`, `center`, `right`, `top`, `bottom`. A missing value is
/// `center`; `top` or `bottom` first names the vertical one.
fn parse_origin(input: &mut Parser<'_>) -> ParseResult<(Length, Length)> {
    #[derive(PartialEq)]
    enum Axis {
        X,
        Y,
        Either,
    }
    let one = |input: &mut Parser<'_>| -> ParseResult<(Length, Axis)> {
        if let Ok(length) = input.try_parse(parse_transform_length) {
            return Ok((length, Axis::Either));
        }
        let ident = input.expect_ident()?.to_ascii_lowercase();
        Ok(match ident.as_str() {
            "left" => (Length::Percent(0.0), Axis::X),
            "right" => (Length::Percent(100.0), Axis::X),
            "top" => (Length::Percent(0.0), Axis::Y),
            "bottom" => (Length::Percent(100.0), Axis::Y),
            "center" => (Length::Percent(50.0), Axis::Either),
            _ => return invalid(),
        })
    };
    let center = Length::Percent(50.0);
    let (first, first_axis) = one(input)?;
    let Ok((second, second_axis)) = input.try_parse(one) else {
        return Ok(if first_axis == Axis::Y {
            (center, first)
        } else {
            (first, center)
        });
    };
    if first_axis == Axis::Y || second_axis == Axis::X {
        if first_axis == Axis::X || second_axis == Axis::Y {
            return invalid();
        }
        return Ok((second, first));
    }
    Ok((first, second))
}

/// `transform-style`: `flat` or `preserve-3d` (both accepted, flat
/// renders in this slice, spec 0010, AC-19).
fn parse_transform_style(input: &mut Parser<'_>) -> ParseResult<TransformStyle> {
    match *input.next()? {
        cssparser::Token::Ident(ref value) => {
            let v = value.as_ref().to_ascii_lowercase();
            if v == "preserve-3d" {
                Ok(TransformStyle::Preserve3d)
            } else if v == "flat" {
                Ok(TransformStyle::Flat)
            } else {
                invalid()
            }
        }
        _ => invalid(),
    }
}

/// `perspective(d)`: parse the longhand `perspective` property.
fn parse_perspective(input: &mut Parser<'_>) -> ParseResult<Length> {
    parse_transform_length(input)
}

/// `perspective-origin`: like `transform-origin`, without a z.
fn parse_perspective_origin(input: &mut Parser<'_>) -> ParseResult<(Length, Length)> {
    parse_origin(input)
}

/// `grid-template-columns` / `grid-template-rows`: `none`, or a list of
/// track sizes and named lines (spec 0010, AC-1).
fn parse_track_list(input: &mut Parser<'_>) -> ParseResult<SpecifiedTrackList> {
    if input.try_parse(|i| i.expect_ident_matching("none")).is_ok() {
        return Ok(SpecifiedTrackList::default());
    }
    let entries = parse_track_entries(input, false)?;
    if entries.is_empty() {
        return invalid();
    }
    Ok(SpecifiedTrackList { entries })
}

/// A run of track entries: sizes, named lines and `repeat()`. Inside a
/// `repeat()` the run ends at the block's end.
fn parse_track_entries(
    input: &mut Parser<'_>,
    in_repeat: bool,
) -> ParseResult<Vec<SpecifiedTrackEntry>> {
    let mut entries = Vec::new();
    loop {
        if let Ok(names) = input.try_parse(parse_line_names) {
            entries.push(SpecifiedTrackEntry::LineNames(names));
            continue;
        }
        if let Ok(repeat) = input.try_parse(parse_repeat_entry) {
            entries.push(repeat);
            continue;
        }
        if in_repeat && input.is_exhausted() {
            break;
        }
        match input.try_parse(parse_track_size) {
            Ok(size) => entries.push(SpecifiedTrackEntry::Size(size)),
            Err(_) => break,
        }
    }
    Ok(entries)
}

/// `[name name]`: names for the line at this position.
fn parse_line_names(input: &mut Parser<'_>) -> ParseResult<Vec<Atom>> {
    match input.next()? {
        Token::SquareBracketBlock => input.parse_nested_block(|input| {
            let mut names = Vec::new();
            while let Ok(name) = input.try_parse(|i| i.expect_ident().map(|name| name.to_string()))
            {
                names.push(Atom::from(name.as_str()));
            }
            if names.is_empty() {
                return invalid();
            }
            Ok(names)
        }),
        _ => invalid(),
    }
}

/// `repeat(auto-fill, [col] minmax(100px, 1fr))`.
fn parse_repeat_entry(input: &mut Parser<'_>) -> ParseResult<SpecifiedTrackEntry> {
    if input
        .try_parse(|i| i.expect_function_matching("repeat"))
        .is_err()
    {
        return invalid();
    }
    input.parse_nested_block(|input| {
        let count = if input
            .try_parse(|i| i.expect_ident_matching("auto-fill"))
            .is_ok()
        {
            RepeatCount::AutoFill
        } else if input
            .try_parse(|i| i.expect_ident_matching("auto-fit"))
            .is_ok()
        {
            RepeatCount::AutoFit
        } else {
            let count = input.expect_integer()?;
            if !(1..=1000).contains(&count) {
                return invalid();
            }
            RepeatCount::Int(count as u16)
        };
        input.expect_comma()?;
        let tracks = parse_track_entries(input, true)?;
        if tracks.is_empty() {
            return invalid();
        }
        Ok(SpecifiedTrackEntry::Repeat { count, tracks })
    })
}

/// A single track size: a breadth, `minmax(min, max)` or
/// `fit-content(<length>)`.
fn parse_track_size(input: &mut Parser<'_>) -> ParseResult<SpecifiedTrackSize> {
    if input
        .try_parse(|i| i.expect_function_matching("minmax"))
        .is_ok()
    {
        return input.parse_nested_block(|input| {
            let min = parse_breadth(input)?;
            input.expect_comma()?;
            let max = parse_breadth(input)?;
            Ok(SpecifiedTrackSize::MinMax(min, max))
        });
    }
    if input
        .try_parse(|i| i.expect_function_matching("fit-content"))
        .is_ok()
    {
        return input.parse_nested_block(|input| {
            let length = parse_length(input, LengthRules::NON_NEGATIVE)?;
            Ok(SpecifiedTrackSize::FitContent(length))
        });
    }
    parse_breadth(input).map(SpecifiedTrackSize::Breadth)
}

/// One track breadth: a length or percentage, `<n>fr`, `auto`,
/// `min-content` or `max-content`.
fn parse_breadth(input: &mut Parser<'_>) -> ParseResult<SpecifiedBreadth> {
    if input.try_parse(|i| i.expect_ident_matching("auto")).is_ok() {
        return Ok(SpecifiedBreadth::Auto);
    }
    if input
        .try_parse(|i| i.expect_ident_matching("min-content"))
        .is_ok()
    {
        return Ok(SpecifiedBreadth::MinContent);
    }
    if input
        .try_parse(|i| i.expect_ident_matching("max-content"))
        .is_ok()
    {
        return Ok(SpecifiedBreadth::MaxContent);
    }
    let fr = input.try_parse(|input| match input.next()? {
        Token::Dimension { value, unit, .. }
            if unit.eq_ignore_ascii_case("fr") && *value >= 0.0 && value.is_finite() =>
        {
            Ok(SpecifiedBreadth::Fr(*value))
        }
        _ => invalid(),
    });
    if let Ok(fr) = fr {
        return Ok(fr);
    }
    parse_length(input, LengthRules::NON_NEGATIVE).map(SpecifiedBreadth::Length)
}

/// A list of track sizes for `grid-auto-columns` / `grid-auto-rows`, with
/// names and `repeat()` ignored (those are template only).
fn parse_track_size_list(input: &mut Parser<'_>) -> ParseResult<Vec<SpecifiedTrackSize>> {
    let entries = parse_track_entries(input, false)?;
    let mut sizes = Vec::new();
    fn push(entries: Vec<SpecifiedTrackEntry>, sizes: &mut Vec<SpecifiedTrackSize>) {
        for entry in entries {
            match entry {
                SpecifiedTrackEntry::Size(size) => sizes.push(size),
                SpecifiedTrackEntry::Repeat {
                    count: RepeatCount::Int(count),
                    tracks,
                } => {
                    for _ in 0..count {
                        push(tracks.clone(), sizes);
                    }
                }
                SpecifiedTrackEntry::Repeat { .. } | SpecifiedTrackEntry::LineNames(_) => {}
            }
        }
    }
    push(entries, &mut sizes);
    if sizes.is_empty() {
        return invalid();
    }
    Ok(sizes)
}

/// `grid-auto-flow`: `row` or `column`, with `dense` in either order.
fn parse_auto_flow(input: &mut Parser<'_>) -> ParseResult<GridAutoFlow> {
    let mut flow = GridAutoFlow::default();
    for _ in 0..2 {
        let Ok(ident) = input.try_parse(|i| i.expect_ident().map(|name| name.to_string())) else {
            break;
        };
        if ident.eq_ignore_ascii_case("row") {
            flow.column = false;
        } else if ident.eq_ignore_ascii_case("column") {
            flow.column = true;
        } else if ident.eq_ignore_ascii_case("dense") {
            flow.dense = true;
        } else {
            return invalid();
        }
    }
    Ok(flow)
}

/// `grid-template-areas`: one quoted string per row, each token an area
/// name or a run of dots for an empty cell (spec 0010, AC-2). Every name
/// must form a rectangle.
fn parse_grid_template_areas(input: &mut Parser<'_>) -> ParseResult<Vec<GridArea>> {
    let mut rows: Vec<Vec<Atom>> = Vec::new();
    while !input.is_exhausted() {
        match input.next()? {
            Token::QuotedString(row) => {
                let cells: Vec<Atom> = row.split_whitespace().map(Atom::from).collect();
                if cells.is_empty() {
                    return invalid();
                }
                if let Some(first) = rows.first()
                    && first.len() != cells.len()
                {
                    return invalid();
                }
                rows.push(cells);
            }
            _ => return invalid(),
        }
    }
    if rows.is_empty() {
        return invalid();
    }
    let mut areas: Vec<GridArea> = Vec::new();
    for (row, cells) in rows.iter().enumerate() {
        for (column, cell) in cells.iter().enumerate() {
            if cell.chars().all(|c| c == '.') {
                continue;
            }
            if let Some(area) = areas.iter_mut().find(|area| area.name == *cell) {
                area.row_start = area.row_start.min(row);
                area.row_end = area.row_end.max(row + 1);
                area.col_start = area.col_start.min(column);
                area.col_end = area.col_end.max(column + 1);
            } else {
                areas.push(GridArea {
                    name: cell.clone(),
                    row_start: row,
                    row_end: row + 1,
                    col_start: column,
                    col_end: column + 1,
                });
            }
        }
    }
    // Named areas must be rectangles: every cell inside one carries the
    // same name.
    for area in &areas {
        for row in rows
            .iter()
            .skip(area.row_start)
            .take(area.row_end.saturating_sub(area.row_start))
        {
            for cell in row
                .iter()
                .skip(area.col_start)
                .take(area.col_end.saturating_sub(area.col_start))
            {
                if *cell != area.name {
                    return invalid();
                }
            }
        }
    }
    Ok(areas)
}

/// `grid-column-start` / `grid-column-end` / `grid-row-start` /
/// `grid-row-end`: `auto`, a named line, a line number, or `span`.
fn parse_grid_line(input: &mut Parser<'_>) -> ParseResult<GridLine> {
    if input.try_parse(|i| i.expect_ident_matching("auto")).is_ok() {
        return Ok(GridLine::Auto);
    }
    if input.try_parse(|i| i.expect_ident_matching("span")).is_ok() {
        let count = input
            .try_parse(|i| i.expect_integer())
            .ok()
            .map_or(1, |count| count.clamp(1, 1000) as u16);
        let name = input
            .try_parse(|i| i.expect_ident().map(|name| name.to_string()))
            .ok()
            .map(|name| Atom::from(name.as_str()));
        return Ok(GridLine::Span { count, name });
    }
    if let Ok(ident) = input.try_parse(|i| i.expect_ident().map(|name| name.to_string())) {
        let index = input.try_parse(|i| i.expect_integer()).unwrap_or(1);
        if index == 0 {
            return invalid();
        }
        return Ok(GridLine::Line {
            index,
            name: Some(Atom::from(ident.as_str())),
        });
    }
    let index = input.expect_integer()?;
    if index == 0 {
        return invalid();
    }
    let name = input
        .try_parse(|i| i.expect_ident().map(|name| name.to_string()))
        .ok()
        .map(|name| Atom::from(name.as_str()));
    Ok(GridLine::Line { index, name })
}

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

/// `aspect-ratio: auto [<ratio>]?` or `<ratio>` (spec 0010, AC-7). A
/// `<ratio>` is `width / height`; a single number means width over 1.
fn parse_aspect_ratio(input: &mut Parser<'_>) -> ParseResult<AspectRatio> {
    let auto = input.try_parse(|i| i.expect_ident_matching("auto")).is_ok();
    let ratio = if auto {
        parse_optional_ratio(input)?
    } else {
        Some(parse_ratio(input)?)
    };
    Ok(AspectRatio { auto, ratio })
}

/// `width / height`, or a single number meaning width over 1.
fn parse_ratio(input: &mut Parser<'_>) -> ParseResult<(f32, f32)> {
    let width = parse_non_negative_number(input)?;
    let height = if input.try_parse(|i| i.expect_delim('/')).is_ok() {
        parse_non_negative_number(input)?
    } else {
        1.0
    };
    Ok((width, height))
}

/// An optional `<ratio>`: `None` when the input does not start with one,
/// leaving the parser position where it was.
fn parse_optional_ratio(input: &mut Parser<'_>) -> ParseResult<Option<(f32, f32)>> {
    match input.try_parse(parse_ratio) {
        Ok(ratio) => Ok(Some(ratio)),
        Err(_) => Ok(None),
    }
}

/// `container-type: normal | inline-size | size` (spec 0010, AC-16).
fn parse_container_type(input: &mut Parser<'_>) -> ParseResult<ContainerType> {
    parse_keyword(
        input,
        &[
            ("normal", ContainerType::Normal),
            ("inline-size", ContainerType::InlineSize),
            ("size", ContainerType::Size),
        ],
    )
}

/// `container-name: <custom-ident>+` (spec 0010, AC-16).
fn parse_container_name(input: &mut Parser<'_>) -> ParseResult<ContainerName> {
    let mut names = Vec::new();
    loop {
        input.skip_whitespace();
        match input.next() {
            Ok(cssparser::Token::Ident(name)) => {
                names.push(Atom::from(name.as_ref()));
            }
            _ => break,
        }
    }
    if names.is_empty() {
        return invalid();
    }
    Ok(ContainerName(names))
}

/// `clip: auto` or `clip: rect(top, right, bottom, left)` (CSS 2.2 section
/// 11.1.2; spec 0009, child 0003, AC-12). `auto` and a missing edge are
/// `None` (the border box's edge). Definite lengths resolve here; em, rem
/// and percent edges cannot be resolved without layout and read as `None`.
fn parse_clip(input: &mut Parser<'_>) -> ParseResult<Option<ClipRect>> {
    if input.try_parse(|i| i.expect_ident_matching("auto")).is_ok() {
        return Ok(None);
    }
    let name = input.expect_function()?.to_ascii_lowercase();
    if name != "rect" {
        return invalid();
    }
    input.parse_nested_block(|input| {
        let edge = |input: &mut Parser<'_>| -> ParseResult<Option<Au>> {
            if input.try_parse(|i| i.expect_ident_matching("auto")).is_ok() {
                Ok(None)
            } else {
                Ok(match parse_length(input, LengthRules::ANY)? {
                    SpecifiedLength::Px(px) => Some(Au::from_px(px)),
                    // Non-px edges need the font size or border box to
                    // resolve; treat as `auto` for now.
                    _ => None,
                })
            }
        };
        let top = edge(input)?;
        input.expect_comma()?;
        let right = edge(input)?;
        input.expect_comma()?;
        let bottom = edge(input)?;
        input.expect_comma()?;
        let left = edge(input)?;
        Ok(Some(ClipRect {
            top,
            right,
            bottom,
            left,
        }))
    })
}

/// `border-spacing`: one or two non negative lengths. One length applies to
/// both axes; two set horizontal then vertical (spec 0009, child 0004, AC-17).
fn parse_border_spacing(input: &mut Parser<'_>) -> ParseResult<(Au, Au)> {
    let first = match parse_length(input, LengthRules::ABSOLUTE)? {
        SpecifiedLength::Px(px) => Au::from_px(px),
        // Percentages resolve against the containing block at layout time;
        // treat as zero until then.
        _ => Au::ZERO,
    };
    let second = if input.try_parse(|i| i.expect_comma()).is_ok() {
        match parse_length(input, LengthRules::ABSOLUTE)? {
            SpecifiedLength::Px(px) => Au::from_px(px),
            _ => Au::ZERO,
        }
    } else {
        first
    };
    Ok((first, second))
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
                        0 => Value::Length(width.clone()),
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
            let column = input.try_parse(parse_gap).unwrap_or(row.clone());
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
        Shorthand::PlaceContent => parse_place_content(input, context)?,
        Shorthand::PlaceItems => parse_place_items(input, context)?,
        Shorthand::PlaceSelf => parse_place_self(input, context)?,
        Shorthand::GridRow => parse_grid_axis(input, Longhand::GridRowStart, Longhand::GridRowEnd)?,
        Shorthand::GridColumn => {
            parse_grid_axis(input, Longhand::GridColumnStart, Longhand::GridColumnEnd)?
        }
        Shorthand::GridArea => parse_grid_area(input)?,
        Shorthand::GridTemplate => parse_grid_template(input)?,
        Shorthand::Grid => parse_grid(input)?,
        Shorthand::Transition => parse_transition(input, context)?,
        Shorthand::Animation => parse_animation(input, context)?,
    })
}

/// A `<time>` value: `<number>s` or `<number>ms`, in seconds. CSS tokenizes
/// the number and unit together as one dimension token, so `2s` is a
/// `Dimension`, not a number then an ident.
fn parse_css_time(input: &mut Parser<'_>) -> ParseResult<f64> {
    let number = match input.next()? {
        Token::Dimension { value, unit, .. } => {
            let seconds: f64 = if unit.eq_ignore_ascii_case("s") {
                1.0
            } else if unit.eq_ignore_ascii_case("ms") {
                0.001
            } else {
                return invalid();
            };
            // Through the shortest decimal text, so `0.2s` is 0.2 and not
            // the nearest `f32` widened (`elapsedTime` shows it).
            let value: f64 = value.to_string().parse().unwrap_or(f64::from(*value));
            value * seconds
        }
        _ => return invalid(),
    };
    Ok(number)
}

/// Keywords that are values, not property names, in a `transition` shorthand.
fn is_transition_value_keyword(name: &str) -> bool {
    matches!(
        name,
        "all"
            | "none"
            | "always"
            | "avoid"
            | "auto"
            | "initial"
            | "inherit"
            | "unset"
            | "revert"
            | "linear"
            | "ease"
            | "ease-in"
            | "ease-out"
            | "ease-in-out"
            | "step-start"
            | "step-end"
            | "steps"
            | "cubic-bezier"
            | "running"
            | "paused"
    )
}

/// `transition: <property> <duration> <timing-function> <delay>` (spec 0010,
/// AC-23). One or more comma separated. The first time is the duration and
/// the second is the delay.
fn parse_transition(
    input: &mut Parser<'_>,
    context: &ParseContext,
) -> ParseResult<Vec<(Longhand, Value)>> {
    let mut out = Vec::new();
    loop {
        // `all` (also the default) is `None`; `none` names no property.
        let property = input
            .try_parse(|i| {
                let ident = i.expect_ident()?;
                let name = ident.as_ref();
                if name.eq_ignore_ascii_case("all") {
                    return Ok(None);
                }
                if name.eq_ignore_ascii_case("none") {
                    return Ok(Some(Atom::from("none")));
                }
                if is_transition_value_keyword(name) {
                    return invalid();
                }
                Ok(Some(Atom::from(name.to_ascii_lowercase())))
            })
            .unwrap_or(None);
        let duration = input.try_parse(parse_css_time).ok();
        let timing = input
            .try_parse(|i| parse_timing_function(i, context, Longhand::TransitionTimingFunction))
            .ok();
        let delay = input.try_parse(parse_css_time).ok();
        out.push((
            Longhand::TransitionProperty,
            Value::TransitionProperty(property),
        ));
        out.push((
            Longhand::TransitionDuration,
            Value::TransitionDuration(duration.unwrap_or(0.0)),
        ));
        out.push((
            Longhand::TransitionDelay,
            Value::TransitionDelay(delay.unwrap_or(0.0)),
        ));
        out.push((
            Longhand::TransitionTimingFunction,
            Value::TransitionTimingFunction(timing.unwrap_or(TimingFunction::Ease)),
        ));
        if input.try_parse(|i| i.expect_delim(',')).is_err() {
            break;
        }
    }
    Ok(out)
}

/// One part of an `animation` shorthand value.
/// `animation: <name> <duration> <timing-function> ...` (spec 0010, AC-24).
/// One or more comma separated. Parts may appear in any order; the name is
/// the final non-keyword ident.
fn parse_animation(
    input: &mut Parser<'_>,
    context: &ParseContext,
) -> ParseResult<Vec<(Longhand, Value)>> {
    let mut out = Vec::new();
    loop {
        let mut name: Option<Atom> = None;
        let mut duration: Option<f64> = None;
        let mut delay: Option<f64> = None;
        let mut timing: Option<TimingFunction> = None;
        let mut iteration: Option<f64> = None;
        let mut direction: Option<AnimationDirection> = None;
        let mut fill_mode: Option<FillMode> = None;
        let mut play_state: Option<PlayState> = None;
        loop {
            // A time.
            if let Ok(t) = input.try_parse(parse_css_time) {
                if duration.is_none() {
                    duration = Some(t);
                } else {
                    delay = Some(t);
                }
                continue;
            }
            // A timing function.
            if let Ok(t) = input
                .try_parse(|i| parse_timing_function(i, context, Longhand::AnimationTimingFunction))
            {
                timing = Some(t);
                continue;
            }
            // An iteration count: a number or `infinite`.
            if input
                .try_parse(|i| i.expect_ident_matching("infinite"))
                .is_ok()
            {
                iteration = None;
                continue;
            }
            if let Ok(n) = input.try_parse(|i| i.expect_number()) {
                iteration = Some(n as f64);
                continue;
            }
            // A direction, fill mode or play state keyword.
            if let Ok(d) = input
                .try_parse(|i| parse_animation_direction(i, context, Longhand::AnimationDirection))
            {
                direction = Some(d);
                continue;
            }
            if let Ok(f) = input
                .try_parse(|i| parse_animation_fill_mode(i, context, Longhand::AnimationFillMode))
            {
                fill_mode = Some(f);
                continue;
            }
            if let Ok(p) = input
                .try_parse(|i| parse_animation_play_state(i, context, Longhand::AnimationPlayState))
            {
                play_state = Some(p);
                continue;
            }
            // Anything left is the animation name (an ident). The name may
            // come before or after the other parts, so keep going until the
            // next part fails to parse (spec 0010, AC-24).
            if let Ok(ident) = input.expect_ident() {
                name = Some(Atom::from(ident.as_ref()));
                continue;
            }
            break;
        }
        out.push((
            Longhand::AnimationName,
            Value::AnimationName(name.unwrap_or(Atom::from("none"))),
        ));
        out.push((
            Longhand::AnimationDuration,
            Value::AnimationDuration(duration.unwrap_or(0.0)),
        ));
        out.push((
            Longhand::AnimationDelay,
            Value::AnimationDelay(delay.unwrap_or(0.0)),
        ));
        out.push((
            Longhand::AnimationTimingFunction,
            Value::AnimationTimingFunction(timing.unwrap_or(TimingFunction::Ease)),
        ));
        out.push((
            Longhand::AnimationIterationCount,
            Value::AnimationIterationCount(iteration),
        ));
        out.push((
            Longhand::AnimationDirection,
            Value::AnimationDirection(direction.unwrap_or(AnimationDirection::Normal)),
        ));
        out.push((
            Longhand::AnimationFillMode,
            Value::AnimationFillMode(fill_mode.unwrap_or(FillMode::None)),
        ));
        out.push((
            Longhand::AnimationPlayState,
            Value::AnimationPlayState(play_state.unwrap_or(PlayState::Running)),
        ));
        if input.try_parse(|i| i.expect_delim(',')).is_err() {
            break;
        }
    }
    Ok(out)
}

/// The alignment keywords shared by `place-*`: the second value defaults to
/// the first.
fn parse_place_content(
    input: &mut Parser<'_>,
    context: &ParseContext,
) -> ParseResult<Vec<(Longhand, Value)>> {
    let align = parse_content_alignment(input, context, Longhand::AlignContent)?;
    let justify = input
        .try_parse(|i| parse_content_alignment(i, context, Longhand::JustifyContent))
        .unwrap_or(align);
    Ok(vec![
        (Longhand::AlignContent, Value::JustifyContent(align)),
        (Longhand::JustifyContent, Value::JustifyContent(justify)),
    ])
}

fn parse_place_items(
    input: &mut Parser<'_>,
    context: &ParseContext,
) -> ParseResult<Vec<(Longhand, Value)>> {
    let align = parse_item_alignment(input, context, Longhand::AlignItems)?;
    let justify = input
        .try_parse(|i| parse_item_alignment(i, context, Longhand::JustifyItems))
        .unwrap_or(align);
    Ok(vec![
        (Longhand::AlignItems, Value::AlignItems(align)),
        (Longhand::JustifyItems, Value::AlignItems(justify)),
    ])
}

fn parse_place_self(
    input: &mut Parser<'_>,
    context: &ParseContext,
) -> ParseResult<Vec<(Longhand, Value)>> {
    let align = parse_self_alignment(input, context, Longhand::AlignSelf)?;
    let justify = input
        .try_parse(|i| parse_self_alignment(i, context, Longhand::JustifySelf))
        .unwrap_or(align);
    Ok(vec![
        (Longhand::AlignSelf, Value::AlignSelf(align)),
        (Longhand::JustifySelf, Value::AlignSelf(justify)),
    ])
}

/// One `align-self` or `justify-self` value, `auto` included.
fn parse_self_alignment(
    input: &mut Parser<'_>,
    context: &ParseContext,
    longhand: Longhand,
) -> ParseResult<Option<AlignItems>> {
    if input.try_parse(|i| i.expect_ident_matching("auto")).is_ok() {
        return Ok(None);
    }
    parse_item_alignment(input, context, longhand).map(Some)
}

/// `grid-row` / `grid-column`: `<start> / <end>?`, the end defaulting to
/// `auto`.
fn parse_grid_axis(
    input: &mut Parser<'_>,
    start: Longhand,
    end: Longhand,
) -> ParseResult<Vec<(Longhand, Value)>> {
    let start_value = input.try_parse(parse_grid_line).unwrap_or(GridLine::Auto);
    let end_value = if input.try_parse(|i| i.expect_delim('/')).is_ok() {
        parse_grid_line(input)?
    } else {
        GridLine::Auto
    };
    Ok(vec![
        (start, Value::GridLine(start_value)),
        (end, Value::GridLine(end_value)),
    ])
}

/// `grid-area: <row-start> / <column-start> / <row-end> / <column-end>`,
/// missing parts being `auto`. When an omitted part matches a custom-ident
/// already given, it takes that ident (CSS Grid Level 1 section 8.4).
fn parse_grid_area(input: &mut Parser<'_>) -> ParseResult<Vec<(Longhand, Value)>> {
    let optional =
        |input: &mut Parser<'_>| input.try_parse(parse_grid_line).unwrap_or(GridLine::Auto);
    let row_start = optional(input);
    let mut column_start = GridLine::Auto;
    let mut row_end = GridLine::Auto;
    let mut column_end = GridLine::Auto;
    for slot in [&mut column_start, &mut row_end, &mut column_end] {
        if input.try_parse(|i| i.expect_delim('/')).is_err() {
            break;
        }
        *slot = parse_grid_line(input)?;
    }
    // CSS Grid: an omitted column start copies row start when both would be
    // a custom ident; likewise the ends.
    if let GridLine::Line { name: Some(_), .. } = &row_start
        && column_start == GridLine::Auto
    {
        column_start = row_start.clone();
    }
    if let GridLine::Line { name: Some(_), .. } = &row_start
        && row_end == GridLine::Auto
    {
        row_end = row_start.clone();
    }
    if let GridLine::Line { name: Some(_), .. } = &column_start
        && column_end == GridLine::Auto
    {
        column_end = column_start.clone();
    }
    Ok(vec![
        (Longhand::GridRowStart, Value::GridLine(row_start)),
        (Longhand::GridColumnStart, Value::GridLine(column_start)),
        (Longhand::GridRowEnd, Value::GridLine(row_end)),
        (Longhand::GridColumnEnd, Value::GridLine(column_end)),
    ])
}

/// `grid-template`: `none`, `<track-list> / <track-list>?`, or the
/// named-areas form `[ <string> <track-size>? ]+ [ / <track-list> ]?`
/// (spec 0010, AC-1).
fn parse_grid_template(input: &mut Parser<'_>) -> ParseResult<Vec<(Longhand, Value)>> {
    if input.try_parse(|i| i.expect_ident_matching("none")).is_ok() {
        return Ok(vec![
            (
                Longhand::GridTemplateRows,
                Value::GridTracks(Default::default()),
            ),
            (
                Longhand::GridTemplateColumns,
                Value::GridTracks(Default::default()),
            ),
            (Longhand::GridTemplateAreas, Value::GridAreas(Vec::new())),
        ]);
    }
    // The named-areas form starts with a quoted string. Peek at the first
    // token, then always restore so the areas parser re-reads it.
    let saved = input.state();
    let starts_with_string = matches!(input.next(), Ok(Token::QuotedString(_)));
    input.reset(&saved);
    if starts_with_string {
        return parse_grid_template_areas_form(input);
    }
    let rows = parse_track_list(input)?;
    let columns = if input.try_parse(|i| i.expect_delim('/')).is_ok() {
        parse_track_list(input)?
    } else {
        Default::default()
    };
    Ok(vec![
        (Longhand::GridTemplateRows, Value::GridTracks(rows)),
        (Longhand::GridTemplateColumns, Value::GridTracks(columns)),
        (Longhand::GridTemplateAreas, Value::GridAreas(Vec::new())),
    ])
}

/// `grid-template` named-areas form:
/// `[ <string> <track-size>? ]+ [ / <track-list> ]?`
fn parse_grid_template_areas_form(input: &mut Parser<'_>) -> ParseResult<Vec<(Longhand, Value)>> {
    let mut rows: Vec<Vec<Atom>> = Vec::new();
    let mut row_sizes: Vec<SpecifiedTrackSize> = Vec::new();
    loop {
        let saved = input.state();
        let row = match input.next() {
            Ok(Token::QuotedString(s)) => s.split_whitespace().map(Atom::from).collect::<Vec<_>>(),
            _ => {
                input.reset(&saved);
                break;
            }
        };
        if row.is_empty() {
            return invalid();
        }
        if let Some(first) = rows.first()
            && first.len() != row.len()
        {
            return invalid();
        }
        rows.push(row);
        let size = input
            .try_parse(parse_track_size)
            .unwrap_or(SpecifiedTrackSize::Breadth(SpecifiedBreadth::Auto));
        row_sizes.push(size);
    }
    if rows.is_empty() {
        return invalid();
    }
    // Build and validate the named areas, mirroring grid-template-areas.
    let mut areas: Vec<GridArea> = Vec::new();
    for (row_index, cells) in rows.iter().enumerate() {
        for (column_index, cell) in cells.iter().enumerate() {
            if cell.chars().all(|c| c == '.') {
                continue;
            }
            if let Some(area) = areas.iter_mut().find(|area| area.name == *cell) {
                area.row_start = area.row_start.min(row_index);
                area.row_end = area.row_end.max(row_index + 1);
                area.col_start = area.col_start.min(column_index);
                area.col_end = area.col_end.max(column_index + 1);
            } else {
                areas.push(GridArea {
                    name: cell.clone(),
                    row_start: row_index,
                    row_end: row_index + 1,
                    col_start: column_index,
                    col_end: column_index + 1,
                });
            }
        }
    }
    for area in &areas {
        for row in rows
            .iter()
            .skip(area.row_start)
            .take(area.row_end.saturating_sub(area.row_start))
        {
            for cell in row
                .iter()
                .skip(area.col_start)
                .take(area.col_end.saturating_sub(area.col_start))
            {
                if *cell != area.name {
                    return invalid();
                }
            }
        }
    }
    let rows_tracks = SpecifiedTrackList {
        entries: row_sizes
            .into_iter()
            .map(SpecifiedTrackEntry::Size)
            .collect(),
    };
    let columns = if input.try_parse(|i| i.expect_delim('/')).is_ok() {
        parse_track_list(input)?
    } else {
        Default::default()
    };
    Ok(vec![
        (Longhand::GridTemplateRows, Value::GridTracks(rows_tracks)),
        (Longhand::GridTemplateColumns, Value::GridTracks(columns)),
        (Longhand::GridTemplateAreas, Value::GridAreas(areas)),
    ])
}

/// `grid`: `none`, `<rows> / <columns>`, or an `auto-flow` form. Auto-flow
/// fills the axis it names from the tracks on the other side.
fn parse_grid(input: &mut Parser<'_>) -> ParseResult<Vec<(Longhand, Value)>> {
    let initial = |flow: GridAutoFlow,
                   rows: SpecifiedTrackList,
                   columns: SpecifiedTrackList,
                   auto_rows: Vec<SpecifiedTrackSize>,
                   auto_columns: Vec<SpecifiedTrackSize>| {
        vec![
            (Longhand::GridTemplateRows, Value::GridTracks(rows)),
            (Longhand::GridTemplateColumns, Value::GridTracks(columns)),
            (Longhand::GridTemplateAreas, Value::GridAreas(Vec::new())),
            (Longhand::GridAutoRows, Value::GridAutoTracks(auto_rows)),
            (
                Longhand::GridAutoColumns,
                Value::GridAutoTracks(auto_columns),
            ),
            (Longhand::GridAutoFlow, Value::GridAutoFlow(flow)),
        ]
    };
    if input.try_parse(|i| i.expect_ident_matching("none")).is_ok() {
        return Ok(initial(
            GridAutoFlow::default(),
            Default::default(),
            Default::default(),
            Vec::new(),
            Vec::new(),
        ));
    }
    let auto_flow = |input: &mut Parser<'_>| -> ParseResult<GridAutoFlow> {
        input.expect_ident_matching("auto-flow")?;
        Ok(GridAutoFlow {
            column: false,
            dense: input
                .try_parse(|i| i.expect_ident_matching("dense"))
                .is_ok(),
        })
    };
    // `auto-flow` first: `[ auto-flow && grid-auto-rows? ] / grid-template-columns`.
    // Bare `auto-flow` defaults to row flow; only `column auto-flow` means column.
    if let Ok(flow) = input.try_parse(auto_flow) {
        let auto_rows = input.try_parse(parse_track_size_list).unwrap_or_default();
        input.expect_delim('/')?;
        let columns = parse_track_list(input)?;
        return Ok(initial(
            flow,
            Default::default(),
            columns,
            auto_rows,
            Vec::new(),
        ));
    }
    let rows = parse_track_list(input)?;
    input.expect_delim('/')?;
    // `auto-flow` second: rows are a template, columns are implicit.
    if let Ok(mut flow) = input.try_parse(auto_flow) {
        flow.column = false;
        let auto_columns = input.try_parse(parse_track_size_list).unwrap_or_default();
        return Ok(initial(
            flow,
            rows,
            Default::default(),
            Vec::new(),
            auto_columns,
        ));
    }
    let columns = parse_track_list(input)?;
    Ok(initial(
        GridAutoFlow::default(),
        rows,
        columns,
        Vec::new(),
        Vec::new(),
    ))
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
            ] => Some((*grow, *shrink, basis.clone())),
            _ => None,
        }
    }

    #[test]
    fn the_flex_shorthand_fills_grow_shrink_and_basis() {
        // Spec 0006, AC-1
        let zero = FlexBasis::Length(SpecifiedLength::Percent(0.0));
        assert_eq!(flex("none"), Some((0.0, 0.0, FlexBasis::Auto)));
        assert_eq!(flex("auto"), Some((1.0, 1.0, FlexBasis::Auto)));
        assert_eq!(flex("2"), Some((2.0, 1.0, zero.clone())));
        assert_eq!(flex("1 0"), Some((1.0, 0.0, zero.clone())));
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
            ("normal", JustifyContent::Normal),
            ("space-evenly", JustifyContent::SpaceEvenly),
            ("end", JustifyContent::End),
            ("safe center", JustifyContent::Center),
            ("stretch", JustifyContent::Stretch),
        ] {
            assert_eq!(
                value(&format!("justify-content: {text}")),
                Value::JustifyContent(expected),
                "justify-content: {text}"
            );
        }
        for (text, expected) in [
            ("normal", AlignItems::Normal),
            ("center", AlignItems::Center),
            ("baseline", AlignItems::Baseline),
            ("last baseline", AlignItems::Baseline),
            ("self-end", AlignItems::End),
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
            DeclaredValue::Value(Value::Overflow(Overflow::Auto)),
            "auto computes to itself"
        );
        assert_eq!(
            parse("overflow: visible")[1].1,
            parse("overflow: visible")[0].1
        );
        assert!(parse("justify-content: space").is_empty());
    }

    #[test]
    fn overflow_longhands_compute_every_keyword_to_itself() {
        // Spec 0009, child 0002, AC-6 and AC-20: no overflow keyword maps
        // away anymore. The shorthand sets both axes to the same value.
        for (text, expected) in [
            ("overflow-x: visible", Overflow::Visible),
            ("overflow-x: hidden", Overflow::Hidden),
            ("overflow-x: clip", Overflow::Clip),
            ("overflow-x: scroll", Overflow::Scroll),
            ("overflow-x: auto", Overflow::Auto),
            ("overflow-y: visible", Overflow::Visible),
            ("overflow-y: hidden", Overflow::Hidden),
            ("overflow-y: clip", Overflow::Clip),
            ("overflow-y: scroll", Overflow::Scroll),
            ("overflow-y: auto", Overflow::Auto),
        ] {
            assert_eq!(
                value(text),
                Value::Overflow(expected),
                "{text} should compute to itself"
            );
        }
        let shorthand = parse("overflow: scroll auto");
        assert_eq!(
            shorthand
                .iter()
                .map(|(n, _, _)| n.as_str())
                .collect::<Vec<_>>(),
            ["overflow-x", "overflow-y"]
        );
        assert_eq!(
            shorthand[0].1,
            DeclaredValue::Value(Value::Overflow(Overflow::Scroll)),
        );
        assert_eq!(
            shorthand[1].1,
            DeclaredValue::Value(Value::Overflow(Overflow::Auto)),
        );
    }

    #[test]
    fn positioning_longhands_parse_fixed_and_sticky() {
        // Spec 0009, child 0003, AC-20: `fixed` and `sticky` compute to
        // themselves, not to `absolute` and `relative`.
        assert_eq!(
            value("position: absolute"),
            Value::Position(Position::Absolute)
        );
        assert_eq!(value("position: fixed"), Value::Position(Position::Fixed));
        assert_eq!(value("position: sticky"), Value::Position(Position::Sticky));
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
    fn overflow_wrap_takes_break_word_and_anywhere() {
        // Spec 0035, AC-1.
        assert_eq!(
            value("overflow-wrap: break-word"),
            Value::OverflowWrap(OverflowWrap::BreakWord)
        );
        assert_eq!(
            value("overflow-wrap: anywhere"),
            Value::OverflowWrap(OverflowWrap::BreakWord)
        );
        assert_eq!(
            value("overflow-wrap: normal"),
            Value::OverflowWrap(OverflowWrap::Normal)
        );
        assert!(parse("overflow-wrap: wrap").is_empty());
    }

    #[test]
    fn cursor_and_user_select_keep_the_profile_and_map_the_rest() {
        // Spec 0006, AC-1 and Unsupported keywords
        assert_eq!(value("cursor: pointer"), Value::Cursor(Cursor::Pointer));
        assert_eq!(value("cursor: TEXT"), Value::Cursor(Cursor::Text));
        assert_eq!(value("cursor: grab"), Value::Cursor(Cursor::Auto));
        // Spec 0035, AC-5.
        assert_eq!(value("cursor: zoom-in"), Value::Cursor(Cursor::ZoomIn));
        assert_eq!(value("cursor: zoom-out"), Value::Cursor(Cursor::ZoomOut));
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
        assert_eq!(value("display: grid"), Value::Display(Display::Grid));
        assert_eq!(
            value("display: inline-grid"),
            Value::Display(Display::InlineGrid),
            "an inline grid lays out as an inline level grid (spec 0010, AC-1)"
        );
        assert_eq!(
            value("display: inline-flex"),
            Value::Display(Display::InlineBlock),
            "inline flex lays out as an inline block (spec 0006, AC-1)"
        );
    }

    #[test]
    fn clip_parses_auto_and_rect_edges() {
        // Spec 0009, child 0003, AC-12: `clip: auto` is no clip, and
        // `clip: rect(...)` clips absolute and fixed boxes to the rectangle.
        assert_eq!(
            value("clip: auto"),
            Value::Clip(None),
            "`clip: auto` is no clip"
        );
        assert_eq!(
            value("clip: rect(10px, 20px, 30px, 4px)"),
            Value::Clip(Some(ClipRect {
                top: Some(Au::from_px(10.0)),
                right: Some(Au::from_px(20.0)),
                bottom: Some(Au::from_px(30.0)),
                left: Some(Au::from_px(4.0)),
            })),
            "each edge is a definite length"
        );
        assert_eq!(
            value("clip: rect(auto, 5px, auto, auto)"),
            Value::Clip(Some(ClipRect {
                top: None,
                right: Some(Au::from_px(5.0)),
                bottom: None,
                left: None,
            })),
            "auto edges read as None"
        );
        assert!(parse("clip: rect(1px 2px 3px 4px)").is_empty());
        assert!(parse("clip: banana").is_empty());
    }

    #[test]
    fn vertical_align_parses_cell_keywords() {
        // Spec 0009, child 0004, AC-16: baseline, top, middle, bottom; any
        // other keyword acts as baseline.
        assert_eq!(
            value("vertical-align: baseline"),
            Value::VerticalAlign(VerticalAlign::Baseline)
        );
        assert_eq!(
            value("vertical-align: top"),
            Value::VerticalAlign(VerticalAlign::Top)
        );
        assert_eq!(
            value("vertical-align: middle"),
            Value::VerticalAlign(VerticalAlign::Middle)
        );
        assert_eq!(
            value("vertical-align: bottom"),
            Value::VerticalAlign(VerticalAlign::Bottom)
        );
        assert_eq!(
            value("vertical-align: text-top"),
            Value::VerticalAlign(VerticalAlign::Baseline),
            "unknown keyword acts as baseline"
        );
        assert!(!Longhand::VerticalAlign.is_inherited());
    }

    #[test]
    fn animation_shorthand_expands_every_longhand() {
        // Spec 0010, AC-24: `animation: <name> <duration> ...` sets every
        // longhand, whether the parts come in the usual order or not.
        let declarations = parse("animation: kf-slide 2s infinite alternate");
        let names: Vec<_> = declarations.iter().map(|(n, _, _)| n.as_str()).collect();
        assert_eq!(
            names,
            [
                "animation-name",
                "animation-duration",
                "animation-delay",
                "animation-timing-function",
                "animation-iteration-count",
                "animation-direction",
                "animation-fill-mode",
                "animation-play-state",
            ],
            "the shorthand expands to the eight animation longhands"
        );
        let values: Vec<_> = declarations
            .iter()
            .map(|(_, value, _)| value.clone())
            .collect();
        assert_eq!(
            values[0],
            DeclaredValue::Value(Value::AnimationName(Atom::from("kf-slide")))
        );
        assert_eq!(
            values[1],
            DeclaredValue::Value(Value::AnimationDuration(2.0))
        );
        assert_eq!(values[2], DeclaredValue::Value(Value::AnimationDelay(0.0)));
        assert_eq!(
            values[3],
            DeclaredValue::Value(Value::AnimationTimingFunction(TimingFunction::Ease))
        );
        assert_eq!(
            values[4],
            DeclaredValue::Value(Value::AnimationIterationCount(None)),
            "infinite is an endless iteration count"
        );
        assert_eq!(
            values[5],
            DeclaredValue::Value(Value::AnimationDirection(AnimationDirection::Alternate))
        );
    }

    #[test]
    fn grid_template_areas_parses_named_areas() {
        let areas = parse_template_areas("\"header header\" \". main\"");
        assert_eq!(
            areas,
            vec![
                GridArea {
                    name: Atom::from("header"),
                    row_start: 0,
                    row_end: 1,
                    col_start: 0,
                    col_end: 2,
                },
                GridArea {
                    name: Atom::from("main"),
                    row_start: 1,
                    row_end: 2,
                    col_start: 1,
                    col_end: 2,
                },
            ]
        );
        // A name that is not a rectangle is invalid.
        assert!(parse_template_areas("\"a b\" \"b a\"").is_empty());
    }

    fn parse_template_areas(text: &str) -> Vec<GridArea> {
        let mut parser = Parser::new(text);
        parse_grid_template_areas(&mut parser).unwrap_or_default()
    }

    #[test]
    fn syntax_matches_registered_custom_property_types() {
        use super::PropertySyntax;
        // `<length>` accepts lengths and percentages, rejects colors and keywords.
        assert!(syntax_matches("12px", &PropertySyntax::Length));
        assert!(syntax_matches("50%", &PropertySyntax::Length));
        assert!(!syntax_matches("red", &PropertySyntax::Length));
        assert!(!syntax_matches("auto", &PropertySyntax::Length));
        // `<number>` accepts numbers only.
        assert!(syntax_matches("3.5", &PropertySyntax::Number));
        assert!(!syntax_matches("3px", &PropertySyntax::Number));
        // `<integer>` accepts integers only.
        assert!(syntax_matches("-2", &PropertySyntax::Integer));
        assert!(!syntax_matches("2.5", &PropertySyntax::Integer));
        // `<color>` accepts colors only.
        assert!(syntax_matches("red", &PropertySyntax::Color));
        assert!(syntax_matches("#ff0000", &PropertySyntax::Color));
        assert!(!syntax_matches("12px", &PropertySyntax::Color));
        // `<angle>` accepts angles.
        assert!(syntax_matches("45deg", &PropertySyntax::Angle));
        assert!(!syntax_matches("45", &PropertySyntax::Angle));
        // `*` accepts anything.
        assert!(syntax_matches("anything at all", &PropertySyntax::Star));
        // `|` accepts any alternative.
        assert!(syntax_matches(
            "red",
            &PropertySyntax::Or(vec![PropertySyntax::Length, PropertySyntax::Color,])
        ));
        assert!(syntax_matches(
            "12px",
            &PropertySyntax::Or(vec![PropertySyntax::Length, PropertySyntax::Color,])
        ));
        assert!(!syntax_matches(
            "auto",
            &PropertySyntax::Or(vec![PropertySyntax::Length, PropertySyntax::Color,])
        ));
        // `+` / `#` accept one or more comma separated repetitions.
        assert!(syntax_matches(
            "12px",
            &PropertySyntax::Repeated(Box::new(PropertySyntax::Length))
        ));
        assert!(syntax_matches(
            "12px, 4em",
            &PropertySyntax::Repeated(Box::new(PropertySyntax::Length))
        ));
        assert!(!syntax_matches(
            "",
            &PropertySyntax::Repeated(Box::new(PropertySyntax::Length))
        ));
    }

    #[test]
    fn transform_functions_parse() {
        let transform = |text: &str| match parse(text).as_slice() {
            [(_, DeclaredValue::Value(Value::Transform(t)), _)] => t.clone(),
            other => panic!("{text} gave {other:?}"),
        };
        // `scale(s)` uses unitless numbers.
        assert_eq!(
            transform("transform: scale(1.5)").transform.as_ref(),
            &[TransformFn::Scale(1.5, None)]
        );
        // `translate(tx, ty)` takes lengths.
        assert_eq!(
            transform("transform: translate(10px, 20px)")
                .transform
                .as_ref(),
            &[TransformFn::Translate(
                Length::px(10.0),
                Some(Length::px(20.0))
            )]
        );
        // `rotate(a)` takes an angle.
        assert_eq!(
            transform("transform: rotate(45deg)").transform.as_ref(),
            &[TransformFn::Rotate(Angle::Deg(45.0))]
        );
        // `perspective(d)` takes a length.
        assert_eq!(
            transform("transform: perspective(1000px)")
                .transform
                .as_ref(),
            &[TransformFn::Perspective(Length::px(1000.0))]
        );
        // `none` computes to the identity.
        assert_eq!(
            transform("transform: none").transform.as_ref(),
            &[] as &[TransformFn]
        );
        // An unknown function drops the declaration.
        assert!(parse("transform: skewz(45deg)").is_empty());
    }

    #[test]
    fn scale_property_parses_unitless_numbers() {
        assert_eq!(value("scale: 1.5"), Value::Scale(1.5, None));
        assert_eq!(value("scale: 1.5 2.0"), Value::Scale(1.5, Some(2.0)));
        assert_eq!(value("scale: -1"), Value::Scale(-1.0, None));
        // `px` is not a unitless number, so the declaration drops.
        assert!(parse("scale: 1px").is_empty());
    }

    #[test]
    fn transform_origin_and_style_parse() {
        assert_eq!(
            value("transform-origin: 10px 20%"),
            Value::TransformOrigin(Length::px(10.0), Length::Percent(20.0))
        );
        assert_eq!(
            value("transform-style: preserve-3d"),
            Value::TransformStyle(TransformStyle::Preserve3d)
        );
        assert_eq!(
            value("transform-style: flat"),
            Value::TransformStyle(TransformStyle::Flat)
        );
        assert!(parse("transform-style: preserve3d").is_empty());
    }

    #[test]
    fn translate_and_perspective_properties_parse() {
        assert_eq!(
            value("translate: 10px 20px"),
            Value::Translate(Length::px(10.0), Some(Length::px(20.0)))
        );
        assert_eq!(
            value("perspective: 1000px"),
            Value::Perspective(Length::px(1000.0))
        );
    }
}
