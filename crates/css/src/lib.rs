//! Style: CSS parsing, selector matching, the cascade and computed values,
//! stored in shared groups (spec 0002, Computed style; spec 0004, child
//! 0003).
//!
//! `cssparser` tokenizes and recovers from errors; matching, the cascade
//! and computed values are written here. `css` never names a `dom` type, so
//! the dependency stays one way: elements are seen through [`MatchElement`].

#![forbid(unsafe_code)]

mod cascade;
mod damage;
mod dump;
mod font_face;
mod hints;
mod invalidation;
mod media;
mod properties;
mod selector;
mod sharing;
mod sheet;
mod style;
mod stylist;
mod ua;
mod values;

use std::ops::BitOr;
use std::rc::Rc;

use base::Au;
use base::atoms::{Atom, QualName};

pub use damage::StyleDamage;
pub use font_face::{FontFaceRule, FontSource};
pub use invalidation::{ElementChange, InvalidationMap, restyle_hint};
pub use media::{Device, MediaList};
pub use properties::Declaration;
pub use selector::{MatchContext, PseudoElement, Selector, parse_selectors};
pub use sharing::{SHARING_CACHE_SIZE, SharingCache};
pub use sheet::{
    Origin, PropertyRule, Rule, Stylesheet, decode_stylesheet, parse_declarations, parse_stylesheet,
};
pub use style::{
    AlignItems, AspectRatio, Background, Border, BorderCollapse, BorderRadius, BorderSide,
    BorderStyle, BoxSizing, CaptionSide, Clear, ClipRect, ComputedStyle, Cursor, CustomProperties,
    Direction, Display, DisplayBox, Effects, EmptyCells, Flex, FlexBasis, FlexDirection, FlexWrap,
    Float, Font, FontStyle, Grid, GridArea, GridAutoFlow, GridLine, InheritedBox, InheritedTable,
    InheritedText, JustifyContent, LineHeight, List, ListStylePosition, ListStyleType, Margin,
    Motion, Outline, OutlineStyle, Overflow, Padding, Position, PositionOffsets, Sizes, Table,
    TableLayout, TextAlign, TextDecorationLine, TextOverflow, TimingFunction, TrackSize, Transform,
    TransformFn, UserSelect, VerticalAlign, Visibility, WhiteSpace, WritingMode,
};
pub use stylist::Stylist;
pub use ua::ua_stylesheet;

/// A color resolved to RGBA, each channel from 0 to 1, in sRGB.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Color {
    /// Red.
    pub r: f32,
    /// Green.
    pub g: f32,
    /// Blue.
    pub b: f32,
    /// Alpha (opacity).
    pub a: f32,
}

impl Color {
    /// Fully transparent.
    pub const TRANSPARENT: Self = Self::rgba(0.0, 0.0, 0.0, 0.0);
    /// Opaque black.
    pub const BLACK: Self = Self::rgba(0.0, 0.0, 0.0, 1.0);

    /// A color from float channels.
    pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    /// An opaque color from 8 bit channels, as in `#rrggbb`.
    pub fn rgb8(r: u8, g: u8, b: u8) -> Self {
        Self::rgba(
            f32::from(r) / 255.0,
            f32::from(g) / 255.0,
            f32::from(b) / 255.0,
            1.0,
        )
    }
}

/// A cascade layer, assigned in first-appearance order across sheets
/// (spec 0010, AC-9). Higher ids are later layers and win the cascade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct LayerId(pub u32);

/// A computed length: absolute, a percentage, or a `calc()` family
/// expression that resolves at used value time (spec 0010, AC-8).
#[derive(Debug, Clone, PartialEq)]
pub enum Length {
    /// An absolute length.
    Au(Au),
    /// A percentage of the containing block's size, 0 to 100.
    Percent(f32),
    /// A `calc()`, `min()`, `max()` or `clamp()` expression.
    Calc(Rc<CalcExpr>),
}

/// A `calc()` family expression, simplified at parse time.
#[derive(Debug, Clone, PartialEq)]
pub enum CalcExpr {
    /// One leaf length.
    Leaf(Length),
    /// A sum of terms.
    Sum(Vec<CalcExpr>),
    /// A term scaled by a factor.
    Product(Box<CalcExpr>, f32),
    /// The smallest of several terms.
    Min(Vec<CalcExpr>),
    /// The largest of several terms.
    Max(Vec<CalcExpr>),
    /// `clamp(min, value, max)`.
    Clamp(Box<CalcExpr>, Box<CalcExpr>, Box<CalcExpr>),
}

impl Length {
    /// Zero.
    pub const ZERO: Self = Self::Au(Au::ZERO);

    /// A length in CSS pixels.
    pub fn px(px: f32) -> Self {
        Self::Au(Au::from_px(px))
    }

    /// Resolves against `basis`; a percentage with no basis counts as zero.
    /// A `calc()` family expression evaluates its leaves against `basis`.
    pub fn resolve(&self, basis: Option<Au>) -> Au {
        match self {
            Self::Au(au) => *au,
            Self::Percent(percent) => basis.map_or(Au::ZERO, |b| b.scale_by(*percent / 100.0)),
            Self::Calc(expr) => expr.resolve(basis),
        }
    }
}

impl CalcExpr {
    /// Evaluates the expression against `basis`, in CSS px.
    pub fn resolve(&self, basis: Option<Au>) -> Au {
        match self {
            Self::Leaf(length) => length.resolve(basis),
            Self::Sum(terms) => terms.iter().map(|t| t.resolve(basis)).sum(),
            Self::Product(term, factor) => Au(term.resolve(basis).0 * factor.round() as i32),
            Self::Min(terms) => terms
                .iter()
                .map(|t| t.resolve(basis))
                .min()
                .unwrap_or(Au::ZERO),
            Self::Max(terms) => terms
                .iter()
                .map(|t| t.resolve(basis))
                .max()
                .unwrap_or(Au::ZERO),
            Self::Clamp(min, value, max) => {
                let value = value.resolve(basis);
                let min = min.resolve(basis);
                let max = max.resolve(basis);
                value.clamp(min, max)
            }
        }
    }
}

/// The element view that selector matching and restyle hints run against.
///
/// `dom` implements it, so `css` never names a `dom` type. Relatives are
/// elements in the DOM tree (not the flat tree); a node that is not an
/// element is skipped.
pub trait MatchElement {
    /// The element's qualified name.
    fn name(&self) -> &QualName;

    /// The cached `id` attribute value, if it has one.
    fn id(&self) -> Option<&Atom>;

    /// The cached class list, in attribute order.
    fn classes(&self) -> &[Atom];

    /// The value of attribute `name`, if it is set.
    fn attribute(&self, name: &QualName) -> Option<&str>;

    /// The parent, if it is an element.
    fn parent_element(&self) -> Option<Self>
    where
        Self: Sized;

    /// The closest earlier sibling that is an element.
    fn prev_sibling_element(&self) -> Option<Self>
    where
        Self: Sized;

    /// The closest later sibling that is an element.
    fn next_sibling_element(&self) -> Option<Self>
    where
        Self: Sized;

    /// Whether this is the document element (`:root`).
    fn is_root(&self) -> bool;

    /// Whether both elements carry exactly the same attributes and values.
    fn same_attributes(&self, other: &Self) -> bool;

    /// Whether both elements are in the same states (`:hover`, `:focus`, ...).
    fn same_state(&self, other: &Self) -> bool;

    /// Whether the element is in `state`, for the user action and input
    /// pseudo classes (spec 0006, AC-2).
    fn is_in_state(&self, state: PseudoState) -> bool;

    /// Whether the element hosts a shadow root.
    fn has_shadow_root(&self) -> bool;

    /// Whether a selector that depends on siblings (`+`, `~`,
    /// `:first-child`, `:last-child`) was matched against this element, so
    /// changing it can restyle its siblings. Set from [`MatchContext`].
    fn is_affected_by_sibling_selectors(&self) -> bool;
}

/// An element state a pseudo class matches (spec 0006, AC-2). `dom`
/// keeps the bits; `css` only asks about them through [`MatchElement`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PseudoState {
    /// `:hover`: the pointer is over the element or a descendant.
    Hover,
    /// `:active`: the element is being pressed.
    Active,
    /// `:focus`: the element has focus.
    Focus,
    /// `:focus-visible`: the element has focus and should show it.
    FocusVisible,
    /// `:disabled`: a disabled form control.
    Disabled,
}

/// Which elements a change may restyle (spec 0002, Mutation API, step 4).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RestyleHint(u8);

impl RestyleHint {
    /// Nothing needs restyling.
    pub const NONE: Self = Self(0);
    /// The element itself.
    pub const SELF: Self = Self(1 << 0);
    /// The element and every element below it.
    pub const SUBTREE: Self = Self(1 << 1);
    /// Every later sibling of the element, with their subtrees.
    pub const LATER_SIBLINGS: Self = Self(1 << 2);

    /// Whether every part of `other` is in `self`.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitOr for RestyleHint {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentages_resolve_against_the_basis() {
        assert_eq!(
            Length::Percent(50.0).resolve(Some(Au::from_px(200.0))),
            Au::from_px(100.0)
        );
        assert_eq!(Length::Percent(50.0).resolve(None), Au::ZERO);
        assert_eq!(Length::px(3.0).resolve(None), Au(180));
    }
}
