//! `ComputedStyle` and its groups, split the way CSS splits properties
//! (spec 0002, Computed style, AC-6).
//!
//! Inherited groups pass from parent to child by pointer; reset groups
//! start as the shared initial groups. A group the element does not touch
//! stays that very `Rc`, so most elements own almost no style memory.

use std::collections::HashMap;
use std::rc::Rc;

use base::atoms::Atom;
use base::{Au, Point};

use crate::sheet::KeyframesRule;
use crate::{Color, Length};

/// `font-style`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FontStyle {
    /// `normal`, the initial value.
    #[default]
    Normal,
    /// `italic`.
    Italic,
    /// `oblique`.
    Oblique,
}

/// Inherited font properties. A family is just its name here; `text`
/// maps names, including the generic `serif` and `monospace`, to real fonts.
#[derive(Debug, Clone, PartialEq)]
pub struct Font {
    /// `font-family`, in order of preference. Empty means the default font.
    pub family: Vec<Atom>,
    /// `font-size`.
    pub size: Au,
    /// When the size came from a keyword (`medium`, `small`, ...), directly
    /// or through `em` and `%` only: that keyword's factor of the default
    /// size. A change of family then rescales it, so `monospace` text gets
    /// 13 px where other text gets 16 px (spec 0004, AC-25). `None` once an
    /// absolute length set the size.
    pub keyword_size: Option<f32>,
    /// `font-weight`, 1 to 1000.
    pub weight: u16,
    /// `font-style`.
    pub style: FontStyle,
}

impl Default for Font {
    fn default() -> Self {
        Self {
            family: Vec::new(),
            // `medium`.
            size: Au::from_px(16.0),
            keyword_size: Some(1.0),
            weight: 400,
            style: FontStyle::Normal,
        }
    }
}

/// `line-height`.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum LineHeight {
    /// `normal`, the initial value: the font decides.
    #[default]
    Normal,
    /// A multiple of the font size.
    Number(f32),
    /// A fixed length.
    Length(Length),
}

/// `text-align`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TextAlign {
    /// `start`, the initial value.
    #[default]
    Start,
    /// `end`.
    End,
    /// `left`.
    Left,
    /// `right`.
    Right,
    /// `center`.
    Center,
    /// `justify`.
    Justify,
}

/// `white-space`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WhiteSpace {
    /// `normal`, the initial value.
    #[default]
    Normal,
    /// `pre`.
    Pre,
    /// `nowrap`.
    Nowrap,
    /// `pre-wrap`.
    PreWrap,
    /// `pre-line`.
    PreLine,
}

/// `overflow-wrap` (spec 0035, AC-1): whether a word wider than its line
/// may break anywhere.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OverflowWrap {
    /// `normal`, the initial value: words break only at opportunities.
    #[default]
    Normal,
    /// `break-word`, and `anywhere` for now: a word too wide for its line
    /// breaks between any two clusters.
    BreakWord,
}

/// `direction`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Direction {
    /// `ltr`, the initial value.
    #[default]
    Ltr,
    /// `rtl`.
    Rtl,
}

/// `writing-mode`. Layout keys its cache on it, so it is `Hash`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum WritingMode {
    /// `horizontal-tb`, the initial value.
    #[default]
    HorizontalTb,
    /// `vertical-rl`.
    VerticalRl,
    /// `vertical-lr`.
    VerticalLr,
}

/// `text-decoration-line`. Only underlines are drawn in slice 1.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TextDecorationLine {
    /// `none`, the initial value.
    #[default]
    None,
    /// `underline`.
    Underline,
}

/// Inherited text properties.
#[derive(Debug, Clone, PartialEq)]
pub struct InheritedText {
    /// `color`.
    pub color: Color,
    /// `line-height`.
    pub line_height: LineHeight,
    /// `text-align`.
    pub text_align: TextAlign,
    /// `white-space`.
    pub white_space: WhiteSpace,
    /// `overflow-wrap`.
    pub overflow_wrap: OverflowWrap,
    /// `direction`.
    pub direction: Direction,
    /// `writing-mode`.
    pub writing_mode: WritingMode,
    /// `text-decoration-line`. CSS does not inherit it, but decorations
    /// reach the text of every descendant, so slice 1 keeps it here
    /// (spec 0004, child 0003, property table).
    pub text_decoration_line: TextDecorationLine,
}

impl InheritedText {
    /// Whether every property but `color` and the decoration matches, so
    /// only a repaint differs.
    pub(crate) fn same_layout(&self, other: &Self) -> bool {
        self.line_height == other.line_height
            && self.text_align == other.text_align
            && self.white_space == other.white_space
            && self.overflow_wrap == other.overflow_wrap
            && self.direction == other.direction
            && self.writing_mode == other.writing_mode
    }
}

impl Default for InheritedText {
    fn default() -> Self {
        Self {
            color: Color::BLACK,
            line_height: LineHeight::default(),
            text_align: TextAlign::default(),
            white_space: WhiteSpace::default(),
            overflow_wrap: OverflowWrap::default(),
            direction: Direction::default(),
            writing_mode: WritingMode::default(),
            text_decoration_line: TextDecorationLine::default(),
        }
    }
}

/// `visibility`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Visibility {
    /// `visible`, the initial value.
    #[default]
    Visible,
    /// `hidden`.
    Hidden,
    /// `collapse`.
    Collapse,
}

/// `vertical-align`: `baseline`, `top`, `middle`, `bottom`. Other values
/// act as `baseline` (spec 0009, child 0004, AC-16). Does not inherit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum VerticalAlign {
    /// `baseline`, the initial value.
    #[default]
    Baseline,
    /// `top`: aligns the top of the cell's content with the top of the row.
    Top,
    /// `middle`: centers the cell's content in the row.
    Middle,
    /// `bottom`: aligns the bottom of the cell's content with the bottom of the row.
    Bottom,
}

/// `cursor`: the chrome CSS profile's keywords. Every other keyword
/// computes to `auto` (spec 0006, Unsupported keywords).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Cursor {
    /// `auto`, the initial value: `text` over selectable text, else
    /// `default` (spec 0006, AC-18).
    #[default]
    Auto,
    /// `default`.
    Default,
    /// `pointer`.
    Pointer,
    /// `text`.
    Text,
    /// `zoom-in`: the image page's fit view (spec 0035, AC-5).
    ZoomIn,
    /// `zoom-out`: the image page's actual size view (spec 0035, AC-5).
    ZoomOut,
}

/// `user-select`. It inherits here because the used value of `auto`
/// follows the parent's (spec 0006, Data model).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum UserSelect {
    /// `auto`, the initial value.
    #[default]
    Auto,
    /// `none`: the text cannot be selected, and `cursor: auto` over it
    /// is `default`.
    None,
}

/// Inherited box properties.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InheritedBox {
    /// `visibility`.
    pub visibility: Visibility,
    /// `cursor`.
    pub cursor: Cursor,
    /// `user-select`.
    pub user_select: UserSelect,
}

/// `list-style-type`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ListStyleType {
    /// `disc`, the initial value.
    #[default]
    Disc,
    /// `circle`.
    Circle,
    /// `square`.
    Square,
    /// `decimal`.
    Decimal,
    /// `none`.
    None,
}

/// `list-style-position`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ListStylePosition {
    /// `outside`, the initial value.
    #[default]
    Outside,
    /// `inside`.
    Inside,
}

/// Inherited list properties.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct List {
    /// `list-style-type`.
    pub style_type: ListStyleType,
    /// `list-style-position`.
    pub position: ListStylePosition,
}

/// The `display` property. Other keywords compute to `Block` or `Inline`
/// until their slices (spec 0004, child 0003, property table), and
/// `inline-flex` to `InlineBlock` (spec 0006, AC-1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Display {
    /// `display: inline`, the initial value.
    #[default]
    Inline,
    /// `display: block`.
    Block,
    /// `display: inline-block`.
    InlineBlock,
    /// `display: list-item`: a block with a marker.
    ListItem,
    /// `display: flex`: a block level flex container (spec 0006, AC-3).
    Flex,
    /// `display: grid`: a block level grid container (spec 0010, AC-1).
    Grid,
    /// `display: inline-grid`: an inline level grid container (spec 0010,
    /// AC-1, AC-3).
    InlineGrid,
    /// `display: flow-root`: a block that always establishes a block
    /// formatting context, so its `height: auto` holds its floats
    /// (spec 0009, child 0001).
    FlowRoot,
    /// `display: table`: a block level table wrapper (spec 0009, child 0004,
    /// AC-14).
    Table,
    /// `display: inline-table`: an inline level table wrapper (AC-14).
    InlineTable,
    /// `display: table-caption`: a table's caption (AC-14).
    TableCaption,
    /// `display: table-column-group`: a `<colgroup>` (AC-14).
    TableColumnGroup,
    /// `display: table-column`: a `<col>` (AC-14).
    TableColumn,
    /// `display: table-row-group`: a `<thead>`, `<tbody>` or `<tfoot>`
    /// (AC-14).
    TableRowGroup,
    /// `display: table-header-group`: a `<thead>` (AC-14).
    TableHeaderGroup,
    /// `display: table-footer-group`: a `<tfoot>` (AC-14).
    TableFooterGroup,
    /// `display: table-row`: a `<tr>` (AC-14).
    TableRow,
    /// `display: table-cell`: a `<td>` or `<th>` (AC-14).
    TableCell,
    /// `display: none`: no box at all.
    None,
}

/// `position` (spec 0006, AC-5; spec 0009, child 0003). `fixed` and
/// `sticky` are real values now, not mapped to `absolute` and `relative`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Position {
    /// `static`, the initial value.
    #[default]
    Static,
    /// `relative`.
    Relative,
    /// `absolute`.
    Absolute,
    /// `fixed`: laid out against the viewport, scrolled with it (spec 0009,
    /// child 0003, AC-10).
    Fixed,
    /// `sticky`: laid out in flow, then shifted to stay inside its nearest
    /// scroll container while it scrolls (spec 0009, child 0003, AC-11).
    Sticky,
}

/// `text-overflow`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TextOverflow {
    /// `clip`, the initial value.
    #[default]
    Clip,
    /// `ellipsis`: an overflowing line ends in `…` (spec 0006, AC-9).
    Ellipsis,
}

/// `float`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Float {
    /// `none`, the initial value.
    #[default]
    None,
    /// `left`.
    Left,
    /// `right`.
    Right,
}

/// `clear`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Clear {
    /// `none`, the initial value.
    #[default]
    None,
    /// `left`.
    Left,
    /// `right`.
    Right,
    /// `both`.
    Both,
}

/// `overflow-x` or `overflow-y`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Overflow {
    /// `visible`, the initial value.
    #[default]
    Visible,
    /// `hidden`.
    Hidden,
    /// `clip`.
    Clip,
    /// `scroll`.
    Scroll,
    /// `auto`.
    Auto,
}

/// Reset properties that decide which boxes exist and how they are placed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DisplayBox {
    /// `display`.
    pub display: Display,
    /// `position`.
    pub position: Position,
    /// `float`.
    pub float: Float,
    /// `clear`.
    pub clear: Clear,
    /// `overflow-x`.
    pub overflow_x: Overflow,
    /// `overflow-y`.
    pub overflow_y: Overflow,
    /// `text-overflow`.
    pub text_overflow: TextOverflow,
}

impl DisplayBox {
    /// Whether the box is positioned: `position` is not `static`, so its
    /// insets and `z-index` apply and it holds its absolute descendants.
    pub fn is_positioned(&self) -> bool {
        self.position != Position::Static
    }

    /// Whether the box is a scroll container: `hidden`, `scroll` or `auto`
    /// on either axis. `hidden` counts because script can still scroll it.
    pub fn is_scroll_container(&self) -> bool {
        let scrolls =
            |o: Overflow| matches!(o, Overflow::Hidden | Overflow::Scroll | Overflow::Auto);
        scrolls(self.overflow_x) || scrolls(self.overflow_y)
    }

    /// Whether the box cuts off content that overflows its padding box:
    /// anything but `visible` on either axis. Both axes clip together until
    /// per axis clipping is needed.
    pub fn clips_overflow(&self) -> bool {
        self.overflow_x != Overflow::Visible || self.overflow_y != Overflow::Visible
    }
}

/// `box-sizing`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BoxSizing {
    /// `content-box`, the initial value.
    #[default]
    ContentBox,
    /// `border-box`.
    BorderBox,
}

/// `width`, `height` and their limits. `None` is `auto` for the sizes and
/// minimums, and `none` for the maximums.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sizes {
    /// `width`.
    pub width: Option<Length>,
    /// `height`.
    pub height: Option<Length>,
    /// `min-width`.
    pub min_width: Option<Length>,
    /// `min-height`.
    pub min_height: Option<Length>,
    /// `max-width`.
    pub max_width: Option<Length>,
    /// `max-height`.
    pub max_height: Option<Length>,
    /// `box-sizing`.
    pub box_sizing: BoxSizing,
    /// `aspect-ratio`: the preferred width-to-height ratio (spec 0010, AC-7).
    pub aspect_ratio: AspectRatio,
    /// `container-type`: `normal`, `inline-size` or `size` (spec 0010, AC-16).
    pub container_type: ContainerType,
    /// `container-name`: custom-ident names (spec 0010, AC-16).
    pub container_name: ContainerName,
}

/// `aspect-ratio`: the preferred width-to-height ratio of a box (spec 0010,
/// AC-7). The initial value is `auto`: replaced elements use their natural
/// ratio, other boxes are unaffected. An explicit ratio (with or without
/// `auto`) sizes the box when exactly one dimension is automatic.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AspectRatio {
    /// `auto` was specified: replaced elements fall back to their natural
    /// ratio; other boxes are unaffected unless a ratio is also given.
    pub auto: bool,
    /// The explicit ratio, width divided by height. A single number means
    /// width over 1.
    pub ratio: Option<(f32, f32)>,
}

/// `container-type`: `normal`, `inline-size` or `size` (spec 0010, AC-16).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ContainerType {
    /// The element is not a query container (the initial value).
    #[default]
    Normal,
    /// The element is a query container, contained on its inline axis.
    InlineSize,
    /// The element is a query container, contained on both axes.
    Size,
}

/// `container-name`: a list of custom-ident names (spec 0010, AC-16).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContainerName(pub Vec<Atom>);

/// One length per side. `None` is `auto` (margins only).
#[derive(Debug, Clone, PartialEq)]
pub struct Margin {
    /// `margin-top`.
    pub top: Option<Length>,
    /// `margin-right`.
    pub right: Option<Length>,
    /// `margin-bottom`.
    pub bottom: Option<Length>,
    /// `margin-left`.
    pub left: Option<Length>,
}

impl Margin {
    /// The same margin on every side.
    pub fn all(length: Length) -> Self {
        Self {
            top: Some(length.clone()),
            right: Some(length.clone()),
            bottom: Some(length.clone()),
            left: Some(length.clone()),
        }
    }
}

impl Default for Margin {
    fn default() -> Self {
        Self::all(Length::ZERO)
    }
}

/// `padding` on each side.
#[derive(Debug, Clone, PartialEq)]
pub struct Padding {
    /// `padding-top`.
    pub top: Length,
    /// `padding-right`.
    pub right: Length,
    /// `padding-bottom`.
    pub bottom: Length,
    /// `padding-left`.
    pub left: Length,
}

impl Padding {
    /// The same padding on every side.
    pub fn all(length: Length) -> Self {
        Self {
            top: length.clone(),
            right: length.clone(),
            bottom: length.clone(),
            left: length.clone(),
        }
    }
}

impl Default for Padding {
    fn default() -> Self {
        Self::all(Length::ZERO)
    }
}

/// `border-style` for one side.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BorderStyle {
    /// `none`, the initial value.
    #[default]
    None,
    /// `hidden`.
    Hidden,
    /// `solid`.
    Solid,
    /// `dashed`.
    Dashed,
    /// `dotted`.
    Dotted,
    /// `double`.
    Double,
}

/// One side of a border.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BorderSide {
    /// The computed width: zero when the style is `none` or `hidden`.
    pub width: Au,
    /// `border-*-style`.
    pub style: BorderStyle,
    /// `border-*-color`, with `currentcolor` already resolved.
    pub color: Color,
}

impl BorderSide {
    fn same_layout(&self, other: &Self) -> bool {
        self.width == other.width && self.style == other.style
    }
}

impl Default for BorderSide {
    fn default() -> Self {
        Self {
            width: Au::ZERO,
            style: BorderStyle::None,
            color: Color::BLACK,
        }
    }
}

/// The radius of each corner. One value per corner: elliptical corners
/// arrive with a later slice.
#[derive(Debug, Clone, PartialEq)]
pub struct BorderRadius {
    /// `border-top-left-radius`.
    pub top_left: Length,
    /// `border-top-right-radius`.
    pub top_right: Length,
    /// `border-bottom-right-radius`.
    pub bottom_right: Length,
    /// `border-bottom-left-radius`.
    pub bottom_left: Length,
}

impl Default for BorderRadius {
    fn default() -> Self {
        Self {
            top_left: Length::ZERO,
            top_right: Length::ZERO,
            bottom_right: Length::ZERO,
            bottom_left: Length::ZERO,
        }
    }
}

/// The border on each side.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Border {
    /// `border-top`.
    pub top: BorderSide,
    /// `border-right`.
    pub right: BorderSide,
    /// `border-bottom`.
    pub bottom: BorderSide,
    /// `border-left`.
    pub left: BorderSide,
    /// `border-radius`.
    pub radius: BorderRadius,
}

impl Border {
    /// Whether widths and styles match, so only colors and radii (a
    /// repaint) differ.
    pub(crate) fn same_layout(&self, other: &Self) -> bool {
        self.top.same_layout(&other.top)
            && self.right.same_layout(&other.right)
            && self.bottom.same_layout(&other.bottom)
            && self.left.same_layout(&other.left)
    }
}

/// `top`, `right`, `bottom` and `left` for positioned boxes. `None` is `auto`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PositionOffsets {
    /// `top`.
    pub top: Option<Length>,
    /// `right`.
    pub right: Option<Length>,
    /// `bottom`.
    pub bottom: Option<Length>,
    /// `left`.
    pub left: Option<Length>,
}

/// Background properties.
#[derive(Debug, Clone, PartialEq)]
pub struct Background {
    /// `background-color`.
    pub color: Color,
    /// `background-image`: the absolute URL of its one `url()`, `None` for
    /// `none`.
    pub image: Option<Rc<str>>,
}

impl Default for Background {
    fn default() -> Self {
        Self {
            color: Color::TRANSPARENT,
            image: None,
        }
    }
}

/// `flex-direction`. `row-reverse` and `column-reverse` compute to these
/// (spec 0006, Unsupported keywords).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FlexDirection {
    /// `row`, the initial value.
    #[default]
    Row,
    /// `column`.
    Column,
}

/// `flex-wrap`. `wrap-reverse` computes to `Wrap`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FlexWrap {
    /// `nowrap`, the initial value.
    #[default]
    Nowrap,
    /// `wrap`.
    Wrap,
}

/// `justify-content` on a flex container.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum JustifyContent {
    /// `flex-start`, the initial value (`normal` behaves as it in flex
    /// layout and as `stretch` in grid layout).
    #[default]
    FlexStart,
    /// `flex-end`.
    FlexEnd,
    /// `normal`.
    Normal,
    /// `start`.
    Start,
    /// `end`.
    End,
    /// `center`.
    Center,
    /// `stretch`.
    Stretch,
    /// `space-between`.
    SpaceBetween,
    /// `space-around`.
    SpaceAround,
    /// `space-evenly`.
    SpaceEvenly,
}

/// `align-items`, `justify-items`, `align-self` and `justify-self`
/// (spec 0010, AC-4).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AlignItems {
    /// `stretch`, which `normal` (the initial value) behaves as.
    #[default]
    Stretch,
    /// `flex-start`.
    FlexStart,
    /// `flex-end`.
    FlexEnd,
    /// `normal`.
    Normal,
    /// `start`.
    Start,
    /// `end`.
    End,
    /// `center`.
    Center,
    /// `baseline`, which behaves as `start` in this feature.
    Baseline,
}

/// `flex-basis`.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum FlexBasis {
    /// `auto`, the initial value: the main size property decides.
    #[default]
    Auto,
    /// `content`: the item's max content size.
    Content,
    /// A length or percentage.
    Length(Length),
}

/// Flex container and flex item properties, one reset group (spec 0006,
/// Data model). Most elements keep the shared initial group.
#[derive(Debug, Clone, PartialEq)]
pub struct Flex {
    /// `flex-direction`.
    pub direction: FlexDirection,
    /// `flex-wrap`.
    pub wrap: FlexWrap,
    /// `flex-grow`.
    pub grow: f32,
    /// `flex-shrink`.
    pub shrink: f32,
    /// `flex-basis`.
    pub basis: FlexBasis,
    /// `order`.
    pub order: i32,
}

impl Default for Flex {
    fn default() -> Self {
        Self {
            direction: FlexDirection::default(),
            wrap: FlexWrap::default(),
            grow: 0.0,
            shrink: 1.0,
            basis: FlexBasis::default(),
            order: 0,
        }
    }
}

/// Box alignment shared by flex and grid containers and items: the gap and
/// the content / item / self alignment (spec 0010, AC-4). It is a reset
/// group: an element that sets none of it shares the initial `Rc`.
#[derive(Debug, Clone, PartialEq)]
pub struct Alignment {
    /// `row-gap`.
    pub row_gap: Length,
    /// `column-gap`.
    pub column_gap: Length,
    /// `justify-content`.
    pub justify_content: JustifyContent,
    /// `align-content`.
    pub align_content: JustifyContent,
    /// `justify-items`.
    pub justify_items: AlignItems,
    /// `align-items`.
    pub align_items: AlignItems,
    /// `justify-self`; `None` is `auto`, which takes the parent's
    /// `justify-items`.
    pub justify_self: Option<AlignItems>,
    /// `align-self`; `None` is `auto`, which takes the parent's
    /// `align-items`.
    pub align_self: Option<AlignItems>,
}

impl Default for Alignment {
    fn default() -> Self {
        Self {
            row_gap: Length::ZERO,
            column_gap: Length::ZERO,
            justify_content: JustifyContent::default(),
            // `align-content` initial is `normal`, which stretches `auto`
            // tracks in grid layout (columns use `justify-content` instead).
            align_content: JustifyContent::Normal,
            justify_items: AlignItems::default(),
            align_items: AlignItems::default(),
            justify_self: None,
            align_self: None,
        }
    }
}

/// One grid template's entries (spec 0010, AC-1): track sizes, groups of
/// line names, and `repeat()`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrackList {
    /// The entries in source order.
    pub entries: Vec<TrackEntry>,
}

/// One entry of a [`TrackList`].
#[derive(Debug, Clone, PartialEq)]
pub enum TrackEntry {
    /// A single track size.
    Size(TrackSize),
    /// Names for the line at this position (`[name]`).
    LineNames(Vec<Atom>),
    /// `repeat(count, tracks)`.
    Repeat {
        /// How many times to repeat.
        count: RepeatCount,
        /// The tracks to repeat.
        tracks: Vec<TrackEntry>,
    },
}

/// How many times a `repeat()` repeats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepeatCount {
    /// An integer count.
    Int(u16),
    /// `auto-fill`: as many as fit.
    AutoFill,
    /// `auto-fit`: as many as fit, empty ones collapsed.
    AutoFit,
}

/// One track's size (spec 0010, AC-1).
#[derive(Debug, Clone, PartialEq)]
pub enum TrackSize {
    /// A single breadth.
    Breadth(Breadth),
    /// `minmax(min, max)`.
    MinMax(Breadth, Breadth),
    /// `fit-content(<length>)`.
    FitContent(Length),
}

impl Default for TrackSize {
    fn default() -> Self {
        Self::Breadth(Breadth::Length(Length::ZERO))
    }
}

/// A track breadth (spec 0010, AC-1).
#[derive(Debug, Clone, PartialEq)]
pub enum Breadth {
    /// A fixed length or percentage.
    Length(Length),
    /// A fraction of the free space.
    Fr(f32),
    /// `auto`: the content's size, stretched by `justify-content` /
    /// `align-content`.
    Auto,
    /// `min-content`.
    MinContent,
    /// `max-content`.
    MaxContent,
}

/// The direction auto-placed items flow in and whether packing is dense
/// (spec 0010, AC-2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GridAutoFlow {
    /// Place items column by column instead of row by row.
    pub column: bool,
    /// Fill the earliest empty cell instead of keeping source order.
    pub dense: bool,
}

/// Grid container and grid item properties (spec 0010, AC-1 to AC-5).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Grid {
    /// `grid-template-columns`.
    pub template_columns: TrackList,
    /// `grid-template-rows`.
    pub template_rows: TrackList,
    /// `grid-auto-columns`: sizes for implicitly created columns.
    pub auto_columns: Vec<TrackSize>,
    /// `grid-auto-rows`: sizes for implicitly created rows.
    pub auto_rows: Vec<TrackSize>,
    /// `grid-auto-flow`.
    pub auto_flow: GridAutoFlow,
    /// `grid-column-start`.
    pub column_start: GridLine,
    /// `grid-column-end`.
    pub column_end: GridLine,
    /// `grid-row-start`.
    pub row_start: GridLine,
    /// `grid-row-end`.
    pub row_end: GridLine,
    /// Named areas from `grid-template-areas` (spec 0010, AC-2).
    pub template_areas: Vec<GridArea>,
}

/// One named grid area from `grid-template-areas`: its name and the cell
/// range it spans (row start/end, col start/end, ends exclusive)
/// (spec 0010, AC-2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridArea {
    /// The area name.
    pub name: Atom,
    /// Row start index (0-based).
    pub row_start: usize,
    /// Row end index (exclusive).
    pub row_end: usize,
    /// Column start index (0-based).
    pub col_start: usize,
    /// Column end index (exclusive).
    pub col_end: usize,
}

/// One side of grid item placement (spec 0010, AC-2).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum GridLine {
    /// Auto-placed: let the grid algorithm decide.
    #[default]
    Auto,
    /// A line: `index` counts from the start when positive and from the end
    /// when negative (Grid §8), optionally naming a line.
    Line {
        /// The line number, never zero.
        index: i32,
        /// The line name to select among equal indices, if given.
        name: Option<Atom>,
    },
    /// `span <count>` tracks, optionally up to a named line.
    Span {
        /// How many tracks to span.
        count: u16,
        /// The named line to span to, if given.
        name: Option<Atom>,
    },
}

/// `table-layout`: `auto` sizes columns from cell content; `fixed` sizes
/// them from `<col>` widths and the first row only (spec 0009, child 0004,
/// AC-16).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TableLayout {
    /// `auto`, the initial value.
    #[default]
    Auto,
    /// `fixed`: columns are sized from `<col>` widths and the first row only
    /// (spec 0009, child 0004, AC-16).
    Fixed,
}

/// `border-collapse` (AC-17), `border-spacing` (AC-17), `caption_side`
/// (AC-14) and `empty_cells` (AC-17). Inherited so a table's cells and rows
/// read them from the table.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BorderCollapse {
    /// `separate`, the initial value: each cell has its own borders, with
    /// `border-spacing` between them.
    #[default]
    Separate,
    /// `collapse`: adjoining cell borders merge into one (CSS 2.2 §17.6.2).
    Collapse,
}

/// `caption-side`: `top` puts captions above the table, `bottom` below it
/// (spec 0009, child 0004, AC-14).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CaptionSide {
    /// `top`, the initial value: captions above the table.
    #[default]
    Top,
    /// `bottom`: captions below the table.
    Bottom,
}

/// `empty-cells`: `show` paints borders and backgrounds of empty cells;
/// `hide` leaves them unpainted (spec 0009, child 0004, AC-17).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EmptyCells {
    /// `show`, the initial value: borders and backgrounds of empty cells
    /// paint.
    #[default]
    Show,
    /// `hide`: empty cells paint nothing.
    Hide,
}

/// Reset group: `table-layout` (spec 0009, child 0004, Data model). Most
/// elements share the initial `Rc`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Table {
    /// `table-layout`.
    pub table_layout: TableLayout,
}

/// Inherited group: `border_collapse`, `border_spacing`, `caption_side`,
/// `empty_cells` (spec 0009, child 0004, Data model). Cells and rows read
/// these from the table they sit in.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InheritedTable {
    /// `border-collapse`.
    pub border_collapse: BorderCollapse,
    /// `border-spacing`, one length per axis (or one for both).
    pub border_spacing: (Au, Au),
    /// `caption-side`.
    pub caption_side: CaptionSide,
    /// `empty-cells`.
    pub empty_cells: EmptyCells,
}

/// `outline-style`. Every style but `none` draws as `Solid` (spec 0006,
/// Unsupported keywords).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OutlineStyle {
    /// `none`, the initial value.
    #[default]
    None,
    /// `solid`.
    Solid,
}

/// The outline, one reset group (spec 0006, Data model). It takes no
/// layout space, so a change only repaints.
#[derive(Debug, Clone, PartialEq)]
pub struct Outline {
    /// The computed width: zero when the style is `none`.
    pub width: Au,
    /// `outline-style`.
    pub style: OutlineStyle,
    /// `outline-color`, with `currentcolor` (the initial value) resolved.
    pub color: Color,
    /// `outline-offset`: how far outside the border box it starts.
    pub offset: Au,
}

impl Default for Outline {
    fn default() -> Self {
        Self {
            width: Au::ZERO,
            style: OutlineStyle::None,
            color: Color::BLACK,
            offset: Au::ZERO,
        }
    }
}

/// One transform function, in declaration order (spec 0010, AC-19).
#[derive(Debug, Clone, PartialEq)]
pub enum TransformFn {
    /// `translate(tx, ty?)`.
    Translate(Length, Option<Length>),
    /// `translateX(tx)`.
    TranslateX(Length),
    /// `translateY(ty)`.
    TranslateY(Length),
    /// `scale(sx, sy?)`: unitless scale factors.
    Scale(f32, Option<f32>),
    /// `scaleX(sx)`.
    ScaleX(f32),
    /// `scaleY(sy)`.
    ScaleY(f32),
    /// `rotate(a)`.
    Rotate(Angle),
    /// `skew(ax, ay?)`.
    Skew(Angle, Option<Angle>),
    /// `skewX(ax)`.
    SkewX(Angle),
    /// `skewY(ay)`.
    SkewY(Angle),
    /// `translate3d(tx, ty, tz)`.
    Translate3d(Length, Length, Length),
    /// `translateZ(tz)`.
    TranslateZ(Length),
    /// `scale3d(sx, sy, sz)`.
    Scale3d(f32, f32, f32),
    /// `scaleZ(sz)`.
    ScaleZ(f32),
    /// `rotateX(ax)`.
    RotateX(Angle),
    /// `rotateY(ay)`.
    RotateY(Angle),
    /// `rotateZ(az)`.
    RotateZ(Angle),
    /// `rotate3d(x, y, z, angle)`.
    Rotate3d(f32, f32, f32, Angle),
    /// `matrix(a, b, c, d, tx, ty)`.
    Matrix(f32, f32, f32, f32, f32, f32),
    /// `matrix3d(...)` 16 values.
    Matrix3d([f32; 16]),
    /// `perspective(d)`.
    Perspective(Length),
}

/// `transform` plus the separate `scale`, `translate` and `rotate`
/// properties, and `transform-origin`. Computes to the identity when
/// nothing is set (spec 0010, AC-19, AC-20).
#[derive(Debug, Clone, PartialEq)]
pub struct Transform {
    /// `transform`: the functions, in order. Empty means no transform.
    pub transform: Rc<[TransformFn]>,
    /// `translate`; `None` means `auto`, i.e. take `transform`.
    pub translate: Option<(Length, Option<Length>)>,
    /// `rotate`; `None` means `auto`, i.e. take `transform`.
    pub rotate: Option<Angle>,
    /// `scale`; `None` means `auto`, i.e. take `transform`.
    pub scale: Option<(f32, Option<f32>)>,
    /// `transform-origin`, in border box coordinates.
    pub origin: Point,
    /// `transform-style`: `flat` in this slice (`preserve-3d` parses but
    /// renders flat, spec 0010, AC-19).
    pub style: TransformStyle,
    /// `perspective` and `perspective-origin` on the parent (spec 0010,
    /// AC-19). Stored on the box that sets them; read by paint for its
    /// children.
    pub perspective: Option<Length>,
    /// `perspective-origin`: origin of the perspective view (spec 0010,
    /// AC-19).
    pub perspective_origin: Option<(Length, Length)>,
    /// The display-list binding id when this transform is animated, `None`
    /// when it is static (spec 0010, AC-28).
    pub binding_id: Option<u64>,
}

impl Transform {
    /// Whether this box has any transform at all: a `transform` list, one of
    /// the separate `translate`/`rotate`/`scale` properties, or a
    /// `perspective` (spec 0010, AC-19, AC-20).
    pub fn has_transform(&self) -> bool {
        !self.transform.is_empty()
            || self.translate.is_some()
            || self.rotate.is_some()
            || self.scale.is_some()
    }
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            transform: Rc::new([]),
            translate: None,
            rotate: None,
            scale: None,
            origin: Point::default(),
            style: TransformStyle::Flat,
            perspective: None,
            perspective_origin: None,
            binding_id: None,
        }
    }
}

/// `transform-style`: `preserve-3d` parses but renders `flat` in this
/// slice (spec 0010, AC-19).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TransformStyle {
    /// Flatten 3D: this slice's behavior.
    #[default]
    Flat,
    /// `preserve-3d`: accepted but ignored for painting (later feature).
    Preserve3d,
}

/// An angle as written, resolved to degrees at computed value time
/// (spec 0010, AC-19).
#[derive(Debug, Clone, PartialEq)]
pub enum Angle {
    Deg(f32),
    Rad(f32),
    Grad(f32),
    Turn(f32),
}

impl Angle {
    /// Degrees as an f32.
    pub fn degrees(&self) -> f32 {
        match self {
            Self::Deg(d) => *d,
            Self::Rad(r) => *r * 180.0 / std::f32::consts::PI,
            Self::Grad(g) => *g * 0.9,
            Self::Turn(t) => *t * 360.0,
        }
    }
}

/// How a transition or animation interpolates over time (spec 0010,
/// AC-23, AC-24).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimingFunction {
    /// `linear`: constant speed.
    Linear,
    /// `ease`: slow start, fast middle, slow end.
    Ease,
    /// `ease-in`: slow start.
    EaseIn,
    /// `ease-out`: slow end.
    EaseOut,
    /// `ease-in-out`: slow start and end.
    EaseInOut,
    /// `steps(n)`: `n` discrete steps.
    Steps(usize),
}

/// One transition spec: which property, how long, how it eases (spec
/// 0010, AC-23).
#[derive(Debug, Clone, PartialEq)]
pub struct TransitionSpec {
    /// The property being transitioned; `None` means `all`.
    pub property: Option<Atom>,
    /// Duration in seconds.
    pub duration: f64,
    /// Delay in seconds.
    pub delay: f64,
    /// The timing function.
    pub timing: TimingFunction,
}

/// `transition-property` / `transition-duration` / `transition-delay` /
/// `transition-timing-function` / `transition` shorthand (spec 0010,
/// AC-23).
#[derive(Debug, Clone, PartialEq)]
pub struct Motion {
    /// The transitions to run.
    pub transitions: Rc<[TransitionSpec]>,
    /// The `@keyframes` animations to run.
    pub animations: Rc<[AnimationSpec]>,
}

/// One `@keyframes` animation spec (spec 0010, AC-24).
#[derive(Debug, Clone, PartialEq)]
pub struct AnimationSpec {
    /// The animation name.
    pub name: Atom,
    /// The `@keyframes` rule this animation plays, resolved at style time.
    pub keyframes: Option<Rc<KeyframesRule>>,
    /// Duration in seconds.
    pub duration: f64,
    /// Delay in seconds.
    pub delay: f64,
    /// Iteration count; `None` means `infinite`.
    pub iteration_count: Option<f64>,
    /// The timing function.
    pub timing: TimingFunction,
}

impl Default for Motion {
    fn default() -> Self {
        Self {
            transitions: Rc::new([]),
            animations: Rc::new([]),
        }
    }
}

/// Custom properties (`--*`) by name, with every `var()` inside already
/// substituted (spec 0004, child 0003, Cascade).
pub type CustomProperties = HashMap<Atom, Rc<str>>;

/// Visual effects. `transform` and filters arrive with their slices.
#[derive(Debug, Clone, PartialEq)]
pub struct Effects {
    /// `opacity`, 0 to 1.
    pub opacity: f32,
    /// `z-index`; `None` is `auto`.
    pub z_index: Option<i32>,
    /// `clip: rect(...)`, or `None` for `auto` / no clip. Applied only to
    /// absolute and fixed boxes (spec 0009, child 0003, AC-12).
    pub clip: Option<ClipRect>,
}

/// One edge of a `clip: rect(...)` rectangle, measured from the box's border
/// box top left corner. `None` means `auto`, i.e. the border box's edge
/// (spec 0009, child 0003, AC-12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClipRect {
    /// The top edge, or `None` for the border box's top edge (`auto`).
    pub top: Option<Au>,
    /// The right edge, or `None` for the border box's right edge (`auto`).
    pub right: Option<Au>,
    /// The bottom edge, or `None` for the border box's bottom edge (`auto`).
    pub bottom: Option<Au>,
    /// The left edge, or `None` for the border box's left edge (`auto`).
    pub left: Option<Au>,
}

impl Default for Effects {
    fn default() -> Self {
        Self {
            opacity: 1.0,
            z_index: None,
            clip: None,
        }
    }
}

/// The computed values of one element, as shared groups.
///
/// A group the element does not change is the very same `Rc` as its parent's
/// (inherited groups) or the shared initial one (reset groups). Change a
/// group with `Rc::make_mut`, which copies it only when it is shared.
#[derive(Debug, Clone, PartialEq)]
pub struct ComputedStyle {
    /// Inherited: font family, size, weight and style.
    pub font: Rc<Font>,
    /// Inherited: color, line height, text align, white space, direction,
    /// writing mode.
    pub inherited_text: Rc<InheritedText>,
    /// Inherited: visibility, cursor.
    pub inherited_box: Rc<InheritedBox>,
    /// Inherited: list style.
    pub list: Rc<List>,
    /// Reset: display, position, float, clear, overflow, text overflow.
    pub display_box: Rc<DisplayBox>,
    /// Reset: width, height and their limits.
    pub size: Rc<Sizes>,
    /// Reset: margins.
    pub margin: Rc<Margin>,
    /// Reset: padding.
    pub padding: Rc<Padding>,
    /// Reset: borders.
    pub border: Rc<Border>,
    /// Reset: `top`, `right`, `bottom`, `left`.
    pub position_offsets: Rc<PositionOffsets>,
    /// Reset: background.
    pub background: Rc<Background>,
    /// Reset: opacity, z index.
    pub effects: Rc<Effects>,
    /// Reset: flex container and flex item properties not shared with grid.
    pub flex: Rc<Flex>,
    /// Reset: box alignment shared by flex and grid (spec 0010, AC-4).
    pub alignment: Rc<Alignment>,
    /// Reset: `table-layout`.
    pub table: Rc<Table>,
    /// Reset: `grid-template-*`, `grid-auto-*` and item placement.
    pub grid: Rc<Grid>,
    /// Reset: the outline.
    pub outline: Rc<Outline>,
    /// Reset: `transform` and the separate `scale` property (spec 0010,
    /// AC-19).
    pub transform: Rc<Transform>,
    /// Reset: `transition-*` and `animation-*` (spec 0010, AC-23, AC-24).
    pub motion: Rc<Motion>,
    /// Reset: `vertical-align`. It does not inherit (spec 0009, child 0004),
    /// so every element starts from its own value, defaulting to `baseline`.
    pub vertical_align: VerticalAlign,
    /// Inherited: custom properties, for `var()`.
    pub custom: Rc<CustomProperties>,
    /// Inherited: `border-collapse`, `border-spacing`, `caption-side`,
    /// `empty-cells` (spec 0009, child 0004).
    pub inherited_table: Rc<InheritedTable>,
}

thread_local! {
    // Built once per thread on first use and never changed afterwards, so it
    // is a constant, not mutable global state. It has to be per thread
    // because `Rc` cannot be shared across threads, and every style on the
    // script thread points into it (AC-6).
    static INITIAL: Rc<ComputedStyle> = Rc::new(ComputedStyle {
        font: Rc::default(),
        inherited_text: Rc::default(),
        inherited_box: Rc::default(),
        list: Rc::default(),
        display_box: Rc::default(),
        size: Rc::default(),
        margin: Rc::default(),
        padding: Rc::default(),
        border: Rc::default(),
        position_offsets: Rc::default(),
        background: Rc::default(),
        effects: Rc::default(),
        flex: Rc::default(),
        alignment: Rc::default(),
        table: Rc::default(),
        grid: Rc::default(),
        outline: Rc::default(),
        transform: Rc::default(),
        motion: Rc::default(),
        vertical_align: VerticalAlign::default(),
        custom: Rc::default(),
        inherited_table: Rc::default(),
    });
}

impl ComputedStyle {
    /// The shared initial style: every property at its initial value. The
    /// root element starts from it, and every reset group nobody sets
    /// points into it.
    pub fn initial() -> Rc<Self> {
        INITIAL.with(Rc::clone)
    }

    /// The style of a child that sets nothing itself: inherited groups are
    /// `parent`'s, reset groups are the shared initial ones.
    pub fn inherit_from(parent: &Self) -> Self {
        Self {
            font: Rc::clone(&parent.font),
            inherited_text: Rc::clone(&parent.inherited_text),
            inherited_box: Rc::clone(&parent.inherited_box),
            list: Rc::clone(&parent.list),
            custom: Rc::clone(&parent.custom),
            inherited_table: Rc::clone(&parent.inherited_table),
            ..Self::default()
        }
    }
}

impl Default for ComputedStyle {
    /// The initial style, sharing every group with [`ComputedStyle::initial`].
    fn default() -> Self {
        (*Self::initial()).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_child_shares_its_parents_inherited_groups_and_the_initial_reset_groups() {
        // AC-6
        let mut parent = ComputedStyle::default();
        Rc::make_mut(&mut parent.inherited_text).color = Color::rgb8(255, 0, 0);
        Rc::make_mut(&mut parent.margin).top = Some(Length::px(4.0));
        Rc::make_mut(&mut parent.font).weight = 700;

        let child = ComputedStyle::inherit_from(&parent);
        let initial = ComputedStyle::initial();
        assert!(Rc::ptr_eq(&child.inherited_text, &parent.inherited_text));
        assert!(Rc::ptr_eq(&child.font, &parent.font));
        assert!(Rc::ptr_eq(&child.inherited_box, &initial.inherited_box));
        assert!(Rc::ptr_eq(&child.margin, &initial.margin));
        assert!(Rc::ptr_eq(&child.display_box, &initial.display_box));
        assert!(
            !Rc::ptr_eq(&parent.margin, &initial.margin),
            "setting a group copies it away from the initial one"
        );
    }

    #[test]
    fn default_styles_share_every_group() {
        let (a, b) = (ComputedStyle::default(), ComputedStyle::default());
        assert!(Rc::ptr_eq(&a.background, &b.background));
        assert!(Rc::ptr_eq(&a.effects, &b.effects));
        assert_eq!(a, b);
    }
}
