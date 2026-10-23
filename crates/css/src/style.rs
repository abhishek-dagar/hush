//! `ComputedStyle` and its groups, split the way CSS splits properties
//! (spec 0002, Computed style, AC-6).
//!
//! Inherited groups pass from parent to child by pointer; reset groups
//! start as the shared initial groups. A group the element does not touch
//! stays that very `Rc`, so most elements own almost no style memory.

use std::collections::HashMap;
use std::rc::Rc;

use base::Au;
use base::atoms::Atom;

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
#[derive(Debug, Clone, Copy, Default, PartialEq)]
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
    /// `display: none`: no box at all.
    None,
}

/// `position`. `fixed` computes to `Absolute` and `sticky` to `Relative`
/// (spec 0006, Unsupported keywords).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Position {
    /// `static`, the initial value.
    #[default]
    Static,
    /// `relative`.
    Relative,
    /// `absolute`.
    Absolute,
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
}

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
            top: Some(length),
            right: Some(length),
            bottom: Some(length),
            left: Some(length),
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
            top: length,
            right: length,
            bottom: length,
            left: length,
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
#[derive(Debug, Clone, Copy, PartialEq)]
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
    /// `flex-start`, the initial value (`normal` behaves as it).
    #[default]
    FlexStart,
    /// `flex-end`.
    FlexEnd,
    /// `center`.
    Center,
    /// `space-between`.
    SpaceBetween,
    /// `space-around`.
    SpaceAround,
    /// `space-evenly`.
    SpaceEvenly,
}

/// `align-items`, and `align-self` other than `auto`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AlignItems {
    /// `stretch`, which `normal` (the initial value) behaves as.
    #[default]
    Stretch,
    /// `flex-start`.
    FlexStart,
    /// `flex-end`.
    FlexEnd,
    /// `center`.
    Center,
}

/// `flex-basis`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
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
    /// `justify-content`.
    pub justify_content: JustifyContent,
    /// `align-items`.
    pub align_items: AlignItems,
    /// `row-gap`.
    pub row_gap: Length,
    /// `column-gap`.
    pub column_gap: Length,
    /// `flex-grow`.
    pub grow: f32,
    /// `flex-shrink`.
    pub shrink: f32,
    /// `flex-basis`.
    pub basis: FlexBasis,
    /// `align-self`; `None` is `auto`, which takes the parent's
    /// `align-items`.
    pub align_self: Option<AlignItems>,
    /// `order`.
    pub order: i32,
}

impl Default for Flex {
    fn default() -> Self {
        Self {
            direction: FlexDirection::default(),
            wrap: FlexWrap::default(),
            justify_content: JustifyContent::default(),
            align_items: AlignItems::default(),
            row_gap: Length::ZERO,
            column_gap: Length::ZERO,
            grow: 0.0,
            shrink: 1.0,
            basis: FlexBasis::default(),
            align_self: None,
            order: 0,
        }
    }
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
}

impl Default for Effects {
    fn default() -> Self {
        Self {
            opacity: 1.0,
            z_index: None,
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
    /// Reset: flex container and flex item properties.
    pub flex: Rc<Flex>,
    /// Reset: the outline.
    pub outline: Rc<Outline>,
    /// Inherited: custom properties, for `var()`.
    pub custom: Rc<CustomProperties>,
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
        outline: Rc::default(),
        custom: Rc::default(),
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
