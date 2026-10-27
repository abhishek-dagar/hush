//! Computed values: the winning declaration of every property, turned into
//! the values `ComputedStyle` stores (spec 0004, child 0003, Cascade,
//! AC-18, AC-20).
//!
//! The caller hands the declarations over in cascade order, lowest
//! priority first, so for each property the last one wins.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use base::Au;
use base::atoms::Atom;
use cssparser::{ParseError, Parser, Token};

use crate::CalcExpr;
use crate::media::Device;
use crate::properties::{
    CssWide, Declaration, DeclaredValue, FlexBasis as SpecifiedFlexBasis, FontSize, FontWeight,
    LineHeight as SpecifiedLineHeight, Longhand, ParseContext, PropertyKey, SpecifiedBreadth,
    SpecifiedTrackEntry, SpecifiedTrackList, SpecifiedTrackSize, Unparsed, Value, parse_values,
    syntax_matches,
};
use crate::values::{SpecifiedCalcExpr, SpecifiedLength};
use crate::{
    BorderStyle, Breadth, Color, ComputedStyle, CustomProperties, Display, FlexBasis, Float, Font,
    Length, LineHeight, OutlineStyle, Overflow, Position, PropertyRule, TrackEntry, TrackList,
    TrackSize,
};

/// The longest text a `var()` substitution may produce. Past it the
/// property is invalid, so a page cannot blow up memory with variables
/// that double at each step.
const MAX_SUBSTITUTION_LEN: usize = 64 * 1024;

/// How deep custom properties may reference each other.
const MAX_VAR_DEPTH: usize = 64;

/// The default size of the generic `monospace` family, where every other
/// family gets 16 px (spec 0004, AC-25).
const MONOSPACE_PX: f32 = 13.0;
/// `medium` for every other family.
const MEDIUM_PX: f32 = 16.0;

/// What computing a style reads besides the declarations.
pub(crate) struct CascadeInputs<'a> {
    /// The parent element's style, `None` for the root element.
    pub(crate) parent: Option<&'a ComputedStyle>,
    /// The root element's font size, for `rem`. Ignored for the root itself.
    pub(crate) root_font_size: Au,
    /// The viewport, for `vw` and `vh`.
    pub(crate) device: &'a Device,
    /// Registered custom properties from `@property`, for validating declared
    /// values at computed value time (spec 0010, AC-10).
    pub(crate) properties: &'a HashMap<Atom, PropertyRule>,
}

/// Shorthand and longhand texts already parsed after `var()`
/// substitution, keyed by the declaration they came from.
type Reparsed = Vec<(*const Unparsed, Option<Vec<(Longhand, Value)>>)>;

/// A longhand's winning value once `var()` is substituted.
enum Applied {
    Value(Value),
    Wide(CssWide),
}

/// Computes the style for `declarations`, given in cascade order.
pub(crate) fn compute<'a>(
    declarations: impl IntoIterator<Item = &'a Declaration>,
    inputs: &CascadeInputs<'_>,
) -> ComputedStyle {
    let mut winners: [Option<&DeclaredValue>; Longhand::COUNT] = [None; Longhand::COUNT];
    let mut custom: HashMap<&Atom, &DeclaredValue> = HashMap::new();
    for declaration in declarations {
        match &declaration.property {
            PropertyKey::Longhand(longhand) => winners[longhand.index()] = Some(&declaration.value),
            PropertyKey::Custom(name) => {
                custom.insert(name, &declaration.value);
            }
        }
    }

    let initial = ComputedStyle::initial();
    let parent = inputs.parent.unwrap_or(&initial);
    let mut computer = Computer {
        parent,
        initial: &initial,
        style: ComputedStyle::inherit_from(parent),
        is_root: inputs.parent.is_none(),
        root_font_size: inputs.root_font_size,
        device: inputs.device,
    };
    computer.style.custom = compute_custom(&custom, &parent.custom, inputs.properties);

    let mut reparsed: Reparsed = Vec::new();
    let mut applied: Vec<(Longhand, Applied)> = Vec::new();
    for &longhand in Longhand::ALL {
        let Some(value) = winners[longhand.index()] else {
            continue;
        };
        let value = match value {
            DeclaredValue::Value(value) => Applied::Value(value.clone()),
            DeclaredValue::Wide(wide) => Applied::Wide(*wide),
            DeclaredValue::Unparsed(unparsed) => {
                substituted(longhand, unparsed, &computer.style.custom, &mut reparsed)
            }
            // A custom value never lands on a longhand.
            DeclaredValue::Custom(_) => continue,
        };
        applied.push((longhand, value));
    }

    computer.compute_font(&applied);
    // `color` before everything that may say `currentcolor`.
    for (longhand, value) in &applied {
        if *longhand == Longhand::Color {
            computer.apply(*longhand, value);
        }
    }
    let mut border = BorderInputs::default();
    let mut outline = OutlineInputs::default();
    for (longhand, value) in &applied {
        if is_font(*longhand) || *longhand == Longhand::Color {
            continue;
        }
        match (longhand, value) {
            (Longhand::OutlineWidth, value) => {
                outline.touched = true;
                outline.width = match value {
                    Applied::Value(Value::Length(width)) => Some(computer.absolute(width)),
                    Applied::Wide(CssWide::Inherit) => Some(computer.parent.outline.width),
                    _ => None,
                };
                continue;
            }
            (Longhand::OutlineStyle, _) => outline.touched = true,
            (Longhand::OutlineColor, value) => {
                outline.touched = true;
                // `initial` and `unset` are `currentcolor`, settled below.
                outline.color_set =
                    matches!(value, Applied::Value(_) | Applied::Wide(CssWide::Inherit));
            }
            _ => {}
        }
        if let Some(side) = border_width_side(*longhand) {
            border.touched = true;
            border.widths[side] = Some(match value {
                Applied::Value(Value::Length(width)) => computer.absolute(width),
                Applied::Value(_) => continue,
                Applied::Wide(CssWide::Inherit) => computer.parent_border_width(side),
                Applied::Wide(_) => computer.absolute(&MEDIUM_BORDER),
            });
            continue;
        }
        if border_style_side(*longhand).is_some() {
            border.touched = true;
        }
        computer.apply(*longhand, value);
    }
    if border.touched {
        computer.finish_border(&border);
    }
    if outline.touched {
        computer.finish_outline(&outline);
    }
    computer.fix_up();
    computer.style
}

/// The initial border width, `medium`.
const MEDIUM_BORDER: SpecifiedLength = SpecifiedLength::Px(3.0);

#[derive(Default)]
struct BorderInputs {
    touched: bool,
    widths: [Option<Au>; 4],
}

/// The outline longhands that settle together once the rest is computed:
/// the width hangs on the style, and the color on `color`.
#[derive(Default)]
struct OutlineInputs {
    touched: bool,
    /// `None` is `medium`.
    width: Option<Au>,
    /// An `outline-color` other than its initial `currentcolor` won.
    color_set: bool,
}

fn is_font(longhand: Longhand) -> bool {
    matches!(
        longhand,
        Longhand::FontFamily
            | Longhand::FontSize
            | Longhand::FontWeight
            | Longhand::FontStyle
            | Longhand::LineHeight
    )
}

fn border_width_side(longhand: Longhand) -> Option<usize> {
    Some(match longhand {
        Longhand::BorderTopWidth => 0,
        Longhand::BorderRightWidth => 1,
        Longhand::BorderBottomWidth => 2,
        Longhand::BorderLeftWidth => 3,
        _ => return None,
    })
}

fn border_style_side(longhand: Longhand) -> Option<usize> {
    Some(match longhand {
        Longhand::BorderTopStyle => 0,
        Longhand::BorderRightStyle => 1,
        Longhand::BorderBottomStyle => 2,
        Longhand::BorderLeftStyle => 3,
        _ => return None,
    })
}

/// Substitutes `var()` in `unparsed` and parses the result for `longhand`.
/// A shorthand's text is parsed once for all its longhands. Anything that
/// fails makes the property `unset` (AC-20).
fn substituted(
    longhand: Longhand,
    unparsed: &Rc<Unparsed>,
    custom: &CustomProperties,
    reparsed: &mut Reparsed,
) -> Applied {
    let key = Rc::as_ptr(unparsed);
    let index = match reparsed.iter().position(|(k, _)| *k == key) {
        Some(index) => index,
        None => {
            let mut lookup = |name: &str| custom.get(&Atom::from(name)).cloned();
            let values = substitute(&unparsed.css, &mut lookup).and_then(|text| {
                let context = ParseContext {
                    base: unparsed.base.clone(),
                    ..ParseContext::default()
                };
                parse_values(&text, Some(longhand), unparsed.shorthand, &context)
            });
            reparsed.push((key, values));
            reparsed.len() - 1
        }
    };
    reparsed[index]
        .1
        .as_ref()
        .and_then(|values| values.iter().find(|(l, _)| *l == longhand))
        .map_or(Applied::Wide(CssWide::Unset), |(_, value)| {
            Applied::Value(value.clone())
        })
}

/// The element's custom properties: the parent's, with this element's
/// declarations applied and their `var()` references resolved. A cycle or
/// a missing variable without a fallback leaves that property `unset`,
/// which for an inherited property keeps the parent's value.
fn compute_custom(
    declared: &HashMap<&Atom, &DeclaredValue>,
    parent: &Rc<CustomProperties>,
    properties: &HashMap<Atom, PropertyRule>,
) -> Rc<CustomProperties> {
    if declared.is_empty() {
        return Rc::clone(parent);
    }
    let mut map = (**parent).clone();
    let mut raw: HashMap<Atom, Rc<str>> = HashMap::new();
    for (&name, value) in declared {
        match value {
            DeclaredValue::Wide(CssWide::Initial) => {
                map.remove(name);
            }
            DeclaredValue::Custom(text) => {
                raw.insert(name.clone(), Rc::clone(text));
            }
            _ => {}
        }
    }
    let mut resolver = Resolver {
        raw: &raw,
        parent,
        done: HashMap::new(),
        stack: Vec::new(),
        cyclic: HashSet::new(),
    };
    for name in raw.keys() {
        resolver.resolve(name);
    }
    let done = resolver.done;
    for (name, value) in &done {
        if let Some(value) = value {
            map.insert(name.clone(), value.clone());
        }
    }
    // Validate custom properties declared on this element that are
    // registered with `@property`. A value that does not match the registered
    // syntax falls back to the initial value when the property does not
    // inherit, and to the inherited value (already in `map`) when it does
    // (spec 0010, AC-10).
    for name in declared.keys() {
        let Some(rule) = properties.get(name) else {
            continue;
        };
        if rule.inherits {
            continue;
        }
        let valid = done
            .get(name)
            .and_then(|v| v.as_deref())
            .is_some_and(|text| syntax_matches(text, &rule.syntax));
        if !valid {
            // A custom value that fails validation lands as the initial value.
            map.insert((*name).clone(), Rc::clone(&rule.initial));
        }
    }
    Rc::new(map)
}

struct Resolver<'a> {
    raw: &'a HashMap<Atom, Rc<str>>,
    parent: &'a CustomProperties,
    /// Resolved names: `None` is invalid at computed value time.
    done: HashMap<Atom, Option<Rc<str>>>,
    /// The names being resolved, outermost first, to spot cycles.
    stack: Vec<Atom>,
    /// Every name found on a cycle.
    cyclic: HashSet<Atom>,
}

impl Resolver<'_> {
    fn resolve(&mut self, name: &Atom) -> Option<Rc<str>> {
        if let Some(done) = self.done.get(name) {
            return done.clone();
        }
        let Some(text) = self.raw.get(name) else {
            return self.parent.get(name).cloned();
        };
        if let Some(start) = self.stack.iter().position(|n| n == name) {
            self.cyclic.extend(self.stack[start..].iter().cloned());
            return None;
        }
        if self.stack.len() >= MAX_VAR_DEPTH {
            return None;
        }
        if !mentions_var(text) {
            let text = Rc::clone(text);
            self.done.insert(name.clone(), Some(Rc::clone(&text)));
            return Some(text);
        }
        self.stack.push(name.clone());
        let text = Rc::clone(text);
        let mut lookup = |reference: &str| self.resolve(&Atom::from(reference));
        let result = substitute(&text, &mut lookup);
        self.stack.pop();
        let result = if self.cyclic.contains(name) {
            None
        } else {
            result.map(Rc::from)
        };
        self.done.insert(name.clone(), result.clone());
        result
    }
}

fn mentions_var(text: &str) -> bool {
    text.to_ascii_lowercase().contains("var(")
}

/// Replaces every `var(--name, fallback)` in `text` with the value
/// `lookup` gives for `name` (without its `--`), else the fallback. `None`
/// when a reference has neither, or the result grows too long.
pub(crate) fn substitute(
    text: &str,
    lookup: &mut dyn FnMut(&str) -> Option<Rc<str>>,
) -> Option<String> {
    let mut parser = Parser::new(text);
    let mut out = String::new();
    substitute_into(&mut parser, &mut out, lookup).ok()?;
    Some(out)
}

fn substitute_into(
    input: &mut Parser<'_>,
    out: &mut String,
    lookup: &mut dyn FnMut(&str) -> Option<Rc<str>>,
) -> Result<(), ParseError<()>> {
    loop {
        let start = input.position();
        let token = match input.next_including_whitespace_and_comments() {
            Ok(token) => token.clone(),
            Err(_) => return Ok(()),
        };
        match token {
            Token::Function(ref name) if name.eq_ignore_ascii_case("var") => {
                let value = input.parse_nested_block(|input| var_value(input, lookup))?;
                // Comments keep the substituted tokens from joining their
                // neighbors, as token level substitution would.
                out.push_str("/**/");
                out.push_str(&value);
                out.push_str("/**/");
            }
            Token::Function(_)
            | Token::ParenthesisBlock
            | Token::SquareBracketBlock
            | Token::CurlyBracketBlock => {
                out.push_str(input.slice_from(start));
                let close = match token {
                    Token::SquareBracketBlock => ']',
                    Token::CurlyBracketBlock => '}',
                    _ => ')',
                };
                input.parse_nested_block(|input| substitute_into(input, out, lookup))?;
                out.push(close);
            }
            _ => out.push_str(input.slice_from(start)),
        }
        if out.len() > MAX_SUBSTITUTION_LEN {
            return Err(ParseError::unexpected_token());
        }
    }
}

/// The inside of `var()`.
fn var_value(
    input: &mut Parser<'_>,
    lookup: &mut dyn FnMut(&str) -> Option<Rc<str>>,
) -> Result<String, ParseError<()>> {
    let name = input.expect_ident_cloned()?;
    let Some(name) = name.strip_prefix("--") else {
        return Err(ParseError::unexpected_token());
    };
    let value = lookup(name);
    if input.is_exhausted() {
        return value
            .map(|v| v.to_string())
            .ok_or(ParseError::unexpected_token());
    }
    input.expect_comma()?;
    if let Some(value) = value {
        while input.next().is_ok() {}
        return Ok(value.to_string());
    }
    let mut fallback = String::new();
    substitute_into(input, &mut fallback, lookup)?;
    Ok(fallback.trim().to_owned())
}

/// Computes values onto `style`.
struct Computer<'a> {
    parent: &'a ComputedStyle,
    initial: &'a ComputedStyle,
    style: ComputedStyle,
    is_root: bool,
    root_font_size: Au,
    device: &'a Device,
}

impl Computer<'_> {
    /// The font group and `line-height`, which every length after them reads.
    fn compute_font(&mut self, applied: &[(Longhand, Applied)]) {
        let find = |wanted| {
            applied
                .iter()
                .find(|(longhand, _)| *longhand == wanted)
                .map(|(_, value)| value)
        };
        let any = [
            Longhand::FontFamily,
            Longhand::FontSize,
            Longhand::FontWeight,
            Longhand::FontStyle,
        ]
        .into_iter()
        .any(|l| find(l).is_some());
        if any {
            let parent = &self.parent.font;
            let mut font = (**parent).clone();
            match find(Longhand::FontFamily) {
                Some(Applied::Value(Value::FontFamily(family))) => font.family = family.to_vec(),
                Some(Applied::Wide(CssWide::Initial)) => {
                    font.family = self.initial.font.family.clone()
                }
                _ => {}
            }
            self.compute_font_size(&mut font, find(Longhand::FontSize));
            match find(Longhand::FontWeight) {
                Some(Applied::Value(Value::FontWeight(weight))) => {
                    font.weight = match *weight {
                        FontWeight::Absolute(weight) => weight,
                        FontWeight::Bolder => bolder(parent.weight),
                        FontWeight::Lighter => lighter(parent.weight),
                    }
                }
                Some(Applied::Wide(CssWide::Initial)) => font.weight = self.initial.font.weight,
                _ => {}
            }
            match find(Longhand::FontStyle) {
                Some(Applied::Value(Value::FontStyle(style))) => font.style = *style,
                Some(Applied::Wide(CssWide::Initial)) => font.style = self.initial.font.style,
                _ => {}
            }
            if font != **parent {
                self.style.font = Rc::new(font);
            }
        }
        if let Some(value) = find(Longhand::LineHeight) {
            self.apply(Longhand::LineHeight, value);
        }
    }

    fn compute_font_size(&self, font: &mut Font, value: Option<&Applied>) {
        let parent = &self.parent.font;
        let base_px = if font.family.as_slice() == [Atom::from("monospace")] {
            MONOSPACE_PX
        } else {
            MEDIUM_PX
        };
        let from_keyword = |factor: f32| (Au::from_px(base_px * factor), Some(factor));
        let relative = |factor: f32| match parent.keyword_size {
            Some(keyword) => from_keyword(keyword * factor),
            None => (parent.size.scale_by(factor), None),
        };
        let (size, keyword_size) = match value {
            Some(Applied::Value(Value::FontSize(size))) => match size {
                FontSize::Keyword(factor) => from_keyword(*factor),
                FontSize::Smaller => relative(1.0 / 1.2),
                FontSize::Larger => relative(1.2),
                FontSize::Length(SpecifiedLength::Em(em)) => relative(*em),
                FontSize::Length(SpecifiedLength::Percent(percent)) => relative(*percent / 100.0),
                FontSize::Length(SpecifiedLength::Rem(rem)) => {
                    let root = if self.is_root {
                        Au::from_px(MEDIUM_PX)
                    } else {
                        self.root_font_size
                    };
                    (root.scale_by(*rem), None)
                }
                FontSize::Length(length) => (self.viewport_or_px(length), None),
            },
            Some(Applied::Wide(CssWide::Initial)) => from_keyword(1.0),
            // Inherited: a keyword size follows a change of family.
            _ => relative(1.0),
        };
        font.size = size;
        font.keyword_size = keyword_size;
    }

    /// Resolves an absolute or viewport length (never `em` or `%`).
    fn viewport_or_px(&self, length: &SpecifiedLength) -> Au {
        let (width, height) = (self.device.width, self.device.height);
        Au::from_px(match length {
            SpecifiedLength::Px(px) => *px,
            SpecifiedLength::Vw(v) => width * *v / 100.0,
            SpecifiedLength::Vh(v) => height * *v / 100.0,
            SpecifiedLength::Vmin(v) => width.min(height) * *v / 100.0,
            SpecifiedLength::Vmax(v) => width.max(height) * *v / 100.0,
            SpecifiedLength::Em(_) | SpecifiedLength::Rem(_) | SpecifiedLength::Percent(_) => 0.0,
            SpecifiedLength::Calc(_) => 0.0,
        })
    }

    /// A computed length: `em` against this element's font size, `rem`
    /// against the root's, percentages kept for layout.
    fn length(&self, length: &SpecifiedLength) -> Length {
        match length {
            SpecifiedLength::Percent(percent) => Length::Percent(*percent),
            SpecifiedLength::Em(em) => Length::Au(self.style.font.size.scale_by(*em)),
            SpecifiedLength::Rem(rem) => {
                let root = if self.is_root {
                    self.style.font.size
                } else {
                    self.root_font_size
                };
                Length::Au(root.scale_by(*rem))
            }
            SpecifiedLength::Calc(expr) => Length::Calc(Rc::new(self.calc_expr_to_css(expr))),
            other => Length::Au(self.viewport_or_px(other)),
        }
    }

    /// Resolves a specified track list into computed tracks (spec 0010,
    /// AC-1).
    fn track_list(&self, list: &SpecifiedTrackList) -> TrackList {
        TrackList {
            entries: list
                .entries
                .iter()
                .map(|entry| self.track_entry(entry))
                .collect(),
        }
    }

    /// Resolves one track entry, keeping `repeat()` and line names.
    fn track_entry(&self, entry: &SpecifiedTrackEntry) -> TrackEntry {
        match entry {
            SpecifiedTrackEntry::Size(size) => TrackEntry::Size(self.track_size(size)),
            SpecifiedTrackEntry::LineNames(names) => TrackEntry::LineNames(names.clone()),
            SpecifiedTrackEntry::Repeat { count, tracks } => TrackEntry::Repeat {
                count: *count,
                tracks: tracks.iter().map(|entry| self.track_entry(entry)).collect(),
            },
        }
    }

    /// Resolves one track size.
    fn track_size(&self, size: &SpecifiedTrackSize) -> TrackSize {
        match size {
            SpecifiedTrackSize::Breadth(breadth) => TrackSize::Breadth(self.breadth(breadth)),
            SpecifiedTrackSize::MinMax(min, max) => {
                TrackSize::MinMax(self.breadth(min), self.breadth(max))
            }
            SpecifiedTrackSize::FitContent(length) => TrackSize::FitContent(self.length(length)),
        }
    }

    /// Resolves one track breadth.
    fn breadth(&self, breadth: &SpecifiedBreadth) -> Breadth {
        match breadth {
            SpecifiedBreadth::Length(length) => Breadth::Length(self.length(length)),
            SpecifiedBreadth::Fr(fr) => Breadth::Fr(*fr),
            SpecifiedBreadth::Auto => Breadth::Auto,
            SpecifiedBreadth::MinContent => Breadth::MinContent,
            SpecifiedBreadth::MaxContent => Breadth::MaxContent,
        }
    }

    /// Turns a parse-time calc expression into a computed one: `em`, `rem`
    /// and viewport units resolve against this element's font, the root's,
    /// and the viewport; percentages stay for layout until used value time
    /// (spec 0010, AC-8).
    fn calc_expr_to_css(&self, expr: &SpecifiedCalcExpr) -> CalcExpr {
        match expr {
            SpecifiedCalcExpr::Leaf(length) => CalcExpr::Leaf(self.length(length)),
            SpecifiedCalcExpr::Sum(terms) => CalcExpr::Sum(
                terms
                    .iter()
                    .map(|term| self.calc_expr_to_css(term))
                    .collect(),
            ),
            SpecifiedCalcExpr::Product(term, factor) => {
                CalcExpr::Product(Box::new(self.calc_expr_to_css(term)), *factor)
            }
            SpecifiedCalcExpr::Min(terms) => CalcExpr::Min(
                terms
                    .iter()
                    .map(|term| self.calc_expr_to_css(term))
                    .collect(),
            ),
            SpecifiedCalcExpr::Max(terms) => CalcExpr::Max(
                terms
                    .iter()
                    .map(|term| self.calc_expr_to_css(term))
                    .collect(),
            ),
            SpecifiedCalcExpr::Clamp(a, b, c) => CalcExpr::Clamp(
                Box::new(self.calc_expr_to_css(a)),
                Box::new(self.calc_expr_to_css(b)),
                Box::new(self.calc_expr_to_css(c)),
            ),
        }
    }

    /// A length that may not be a percentage, such as a border width.
    fn absolute(&self, length: &SpecifiedLength) -> Au {
        self.length(length).resolve(None)
    }

    fn current_color(&self) -> Color {
        self.style.inherited_text.color
    }

    fn parent_border_width(&self, side: usize) -> Au {
        let border = &self.parent.border;
        [border.top, border.right, border.bottom, border.left][side].width
    }

    /// Applies one longhand's winning value.
    fn apply(&mut self, longhand: Longhand, value: &Applied) {
        match value {
            Applied::Value(value) => self.apply_value(longhand, value),
            Applied::Wide(wide) => {
                let inherit = match wide {
                    CssWide::Inherit => true,
                    CssWide::Initial => false,
                    CssWide::Unset => longhand.is_inherited(),
                };
                let source = if inherit { self.parent } else { self.initial };
                copy_longhand(&mut self.style, source, longhand);
            }
        }
    }

    fn apply_value(&mut self, longhand: Longhand, value: &Value) {
        use Longhand as L;
        let style = &mut self.style;
        match (longhand, value) {
            (L::Display, Value::Display(display)) => {
                Rc::make_mut(&mut style.display_box).display = *display;
            }
            (L::Width, Value::Size(size)) => {
                let size = size.as_ref().map(|l| self.length(l));
                Rc::make_mut(&mut self.style.size).width = size;
            }
            (L::Height, Value::Size(size)) => {
                let size = size.as_ref().map(|l| self.length(l));
                Rc::make_mut(&mut self.style.size).height = size;
            }
            (L::MinWidth, Value::Size(size)) => {
                let size = size.as_ref().map(|l| self.length(l));
                Rc::make_mut(&mut self.style.size).min_width = size;
            }
            (L::MinHeight, Value::Size(size)) => {
                let size = size.as_ref().map(|l| self.length(l));
                Rc::make_mut(&mut self.style.size).min_height = size;
            }
            (L::MaxWidth, Value::Size(size)) => {
                let size = size.as_ref().map(|l| self.length(l));
                Rc::make_mut(&mut self.style.size).max_width = size;
            }
            (L::MaxHeight, Value::Size(size)) => {
                let size = size.as_ref().map(|l| self.length(l));
                Rc::make_mut(&mut self.style.size).max_height = size;
            }
            (L::BoxSizing, Value::BoxSizing(sizing)) => {
                Rc::make_mut(&mut style.size).box_sizing = *sizing;
            }
            (L::AspectRatio, Value::AspectRatio(ratio)) => {
                Rc::make_mut(&mut style.size).aspect_ratio = *ratio;
            }
            (L::MarginTop, Value::Size(m)) => {
                let m = m.as_ref().map(|l| self.length(l));
                Rc::make_mut(&mut self.style.margin).top = m;
            }
            (L::MarginRight, Value::Size(m)) => {
                let m = m.as_ref().map(|l| self.length(l));
                Rc::make_mut(&mut self.style.margin).right = m;
            }
            (L::MarginBottom, Value::Size(m)) => {
                let m = m.as_ref().map(|l| self.length(l));
                Rc::make_mut(&mut self.style.margin).bottom = m;
            }
            (L::MarginLeft, Value::Size(m)) => {
                let m = m.as_ref().map(|l| self.length(l));
                Rc::make_mut(&mut self.style.margin).left = m;
            }
            (L::PaddingTop, Value::Length(p)) => {
                let p = self.length(p);
                Rc::make_mut(&mut self.style.padding).top = p;
            }
            (L::PaddingRight, Value::Length(p)) => {
                let p = self.length(p);
                Rc::make_mut(&mut self.style.padding).right = p;
            }
            (L::PaddingBottom, Value::Length(p)) => {
                let p = self.length(p);
                Rc::make_mut(&mut self.style.padding).bottom = p;
            }
            (L::PaddingLeft, Value::Length(p)) => {
                let p = self.length(p);
                Rc::make_mut(&mut self.style.padding).left = p;
            }
            (L::BorderTopStyle, Value::BorderStyle(s)) => {
                Rc::make_mut(&mut style.border).top.style = *s;
            }
            (L::BorderRightStyle, Value::BorderStyle(s)) => {
                Rc::make_mut(&mut style.border).right.style = *s;
            }
            (L::BorderBottomStyle, Value::BorderStyle(s)) => {
                Rc::make_mut(&mut style.border).bottom.style = *s;
            }
            (L::BorderLeftStyle, Value::BorderStyle(s)) => {
                Rc::make_mut(&mut style.border).left.style = *s;
            }
            (L::BorderTopColor, Value::Color(c)) => {
                let c = c.resolve(self.current_color());
                Rc::make_mut(&mut self.style.border).top.color = c;
            }
            (L::BorderRightColor, Value::Color(c)) => {
                let c = c.resolve(self.current_color());
                Rc::make_mut(&mut self.style.border).right.color = c;
            }
            (L::BorderBottomColor, Value::Color(c)) => {
                let c = c.resolve(self.current_color());
                Rc::make_mut(&mut self.style.border).bottom.color = c;
            }
            (L::BorderLeftColor, Value::Color(c)) => {
                let c = c.resolve(self.current_color());
                Rc::make_mut(&mut self.style.border).left.color = c;
            }
            (L::BorderTopLeftRadius, Value::Length(r)) => {
                let r = self.length(r);
                Rc::make_mut(&mut self.style.border).radius.top_left = r;
            }
            (L::BorderTopRightRadius, Value::Length(r)) => {
                let r = self.length(r);
                Rc::make_mut(&mut self.style.border).radius.top_right = r;
            }
            (L::BorderBottomRightRadius, Value::Length(r)) => {
                let r = self.length(r);
                Rc::make_mut(&mut self.style.border).radius.bottom_right = r;
            }
            (L::BorderBottomLeftRadius, Value::Length(r)) => {
                let r = self.length(r);
                Rc::make_mut(&mut self.style.border).radius.bottom_left = r;
            }
            (L::BackgroundColor, Value::Color(c)) => {
                let c = c.resolve(self.current_color());
                Rc::make_mut(&mut self.style.background).color = c;
            }
            (L::BackgroundImage, Value::Image(image)) => {
                Rc::make_mut(&mut style.background).image = image.clone();
            }
            (L::Opacity, Value::Number(opacity)) => {
                Rc::make_mut(&mut style.effects).opacity = *opacity;
            }
            (L::Color, Value::Color(c)) => {
                // `currentcolor` on `color` itself means the parent's color.
                let c = c.resolve(self.parent.inherited_text.color);
                Rc::make_mut(&mut self.style.inherited_text).color = c;
            }
            (L::LineHeight, Value::LineHeight(height)) => {
                let height = match height {
                    SpecifiedLineHeight::Normal => LineHeight::Normal,
                    SpecifiedLineHeight::Number(n) => LineHeight::Number(*n),
                    SpecifiedLineHeight::Length(SpecifiedLength::Percent(p)) => {
                        LineHeight::Length(Length::Au(self.style.font.size.scale_by(*p / 100.0)))
                    }
                    SpecifiedLineHeight::Length(l) => LineHeight::Length(self.length(l)),
                };
                Rc::make_mut(&mut self.style.inherited_text).line_height = height;
            }
            (L::TextAlign, Value::TextAlign(align)) => {
                Rc::make_mut(&mut style.inherited_text).text_align = *align;
            }
            (L::WhiteSpace, Value::WhiteSpace(white_space)) => {
                Rc::make_mut(&mut style.inherited_text).white_space = *white_space;
            }
            (L::TextDecorationLine, Value::TextDecorationLine(line)) => {
                Rc::make_mut(&mut style.inherited_text).text_decoration_line = *line;
            }
            (L::Visibility, Value::Visibility(visibility)) => {
                Rc::make_mut(&mut style.inherited_box).visibility = *visibility;
            }
            (L::ListStyleType, Value::ListStyleType(kind)) => {
                Rc::make_mut(&mut style.list).style_type = *kind;
            }
            (L::ListStylePosition, Value::ListStylePosition(position)) => {
                Rc::make_mut(&mut style.list).position = *position;
            }
            (L::FlexDirection, Value::FlexDirection(direction)) => {
                Rc::make_mut(&mut style.flex).direction = *direction;
            }
            (L::RowGap, Value::Length(gap)) => {
                let gap = self.length(gap);
                Rc::make_mut(&mut self.style.alignment).row_gap = gap;
            }
            (L::ColumnGap, Value::Length(gap)) => {
                let gap = self.length(gap);
                Rc::make_mut(&mut self.style.alignment).column_gap = gap;
            }
            (L::FlexWrap, Value::FlexWrap(wrap)) => {
                Rc::make_mut(&mut style.flex).wrap = *wrap;
            }
            (L::FlexGrow, Value::Number(grow)) => {
                Rc::make_mut(&mut style.flex).grow = *grow;
            }
            (L::FlexShrink, Value::Number(shrink)) => {
                Rc::make_mut(&mut style.flex).shrink = *shrink;
            }
            (L::FlexBasis, Value::FlexBasis(basis)) => {
                let basis = match basis {
                    SpecifiedFlexBasis::Auto => FlexBasis::Auto,
                    SpecifiedFlexBasis::Content => FlexBasis::Content,
                    SpecifiedFlexBasis::Length(length) => FlexBasis::Length(self.length(length)),
                };
                Rc::make_mut(&mut self.style.flex).basis = basis;
            }
            (L::JustifyContent, Value::JustifyContent(justify)) => {
                Rc::make_mut(&mut self.style.alignment).justify_content = *justify;
            }
            (L::AlignContent, Value::JustifyContent(align)) => {
                Rc::make_mut(&mut self.style.alignment).align_content = *align;
            }
            (L::JustifyItems, Value::AlignItems(align)) => {
                Rc::make_mut(&mut self.style.alignment).justify_items = *align;
            }
            (L::AlignItems, Value::AlignItems(align)) => {
                Rc::make_mut(&mut self.style.alignment).align_items = *align;
            }
            (L::JustifySelf, Value::AlignSelf(align)) => {
                Rc::make_mut(&mut self.style.alignment).justify_self = *align;
            }
            (L::AlignSelf, Value::AlignSelf(align)) => {
                Rc::make_mut(&mut self.style.alignment).align_self = *align;
            }
            (L::Order, Value::Integer(order)) => {
                Rc::make_mut(&mut style.flex).order = *order;
            }
            (L::Transform, Value::Transform(transform)) => {
                let t = Rc::make_mut(&mut self.style.transform);
                t.scale = transform.scale;
                t.transform = Rc::clone(&transform.transform);
                t.origin = transform.origin;
                t.binding_id = None;
            }
            (L::GridTemplateColumns, Value::GridTracks(tracks)) => {
                let tracks = self.track_list(tracks);
                Rc::make_mut(&mut self.style.grid).template_columns = tracks;
            }
            (L::GridTemplateRows, Value::GridTracks(tracks)) => {
                let tracks = self.track_list(tracks);
                Rc::make_mut(&mut self.style.grid).template_rows = tracks;
            }
            (L::GridAutoColumns, Value::GridAutoTracks(tracks)) => {
                let tracks = tracks.iter().map(|size| self.track_size(size)).collect();
                Rc::make_mut(&mut self.style.grid).auto_columns = tracks;
            }
            (L::GridAutoRows, Value::GridAutoTracks(tracks)) => {
                let tracks = tracks.iter().map(|size| self.track_size(size)).collect();
                Rc::make_mut(&mut self.style.grid).auto_rows = tracks;
            }
            (L::GridAutoFlow, Value::GridAutoFlow(flow)) => {
                Rc::make_mut(&mut self.style.grid).auto_flow = *flow;
            }
            (L::GridTemplateAreas, Value::GridAreas(areas)) => {
                Rc::make_mut(&mut self.style.grid).template_areas = areas.clone();
            }
            (L::GridColumnStart, Value::GridLine(line)) => {
                Rc::make_mut(&mut self.style.grid).column_start = line.clone();
            }
            (L::GridColumnEnd, Value::GridLine(line)) => {
                Rc::make_mut(&mut self.style.grid).column_end = line.clone();
            }
            (L::GridRowStart, Value::GridLine(line)) => {
                Rc::make_mut(&mut self.style.grid).row_start = line.clone();
            }
            (L::GridRowEnd, Value::GridLine(line)) => {
                Rc::make_mut(&mut self.style.grid).row_end = line.clone();
            }
            (L::OverflowX, Value::Overflow(overflow)) => {
                Rc::make_mut(&mut style.display_box).overflow_x = *overflow;
            }
            (L::OverflowY, Value::Overflow(overflow)) => {
                Rc::make_mut(&mut style.display_box).overflow_y = *overflow;
            }
            (L::Float, Value::Float(float)) => {
                Rc::make_mut(&mut style.display_box).float = *float;
            }
            (L::Clear, Value::Clear(clear)) => {
                Rc::make_mut(&mut style.display_box).clear = *clear;
            }
            (L::Position, Value::Position(position)) => {
                Rc::make_mut(&mut style.display_box).position = *position;
            }
            (L::Top, Value::Size(inset)) => {
                let inset = inset.as_ref().map(|l| self.length(l));
                Rc::make_mut(&mut self.style.position_offsets).top = inset;
            }
            (L::Right, Value::Size(inset)) => {
                let inset = inset.as_ref().map(|l| self.length(l));
                Rc::make_mut(&mut self.style.position_offsets).right = inset;
            }
            (L::Bottom, Value::Size(inset)) => {
                let inset = inset.as_ref().map(|l| self.length(l));
                Rc::make_mut(&mut self.style.position_offsets).bottom = inset;
            }
            (L::Left, Value::Size(inset)) => {
                let inset = inset.as_ref().map(|l| self.length(l));
                Rc::make_mut(&mut self.style.position_offsets).left = inset;
            }
            (L::ZIndex, Value::ZIndex(z_index)) => {
                Rc::make_mut(&mut style.effects).z_index = *z_index;
            }
            (L::Clip, Value::Clip(clip)) => {
                Rc::make_mut(&mut style.effects).clip = *clip;
            }
            (L::OutlineStyle, Value::OutlineStyle(outline_style)) => {
                Rc::make_mut(&mut style.outline).style = *outline_style;
            }
            (L::OutlineColor, Value::Color(c)) => {
                let c = c.resolve(self.current_color());
                Rc::make_mut(&mut self.style.outline).color = c;
            }
            (L::OutlineOffset, Value::Length(offset)) => {
                let offset = self.absolute(offset);
                Rc::make_mut(&mut self.style.outline).offset = offset;
            }
            (L::TextOverflow, Value::TextOverflow(text_overflow)) => {
                Rc::make_mut(&mut style.display_box).text_overflow = *text_overflow;
            }
            (L::Cursor, Value::Cursor(cursor)) => {
                Rc::make_mut(&mut style.inherited_box).cursor = *cursor;
            }
            (L::UserSelect, Value::UserSelect(user_select)) => {
                Rc::make_mut(&mut style.inherited_box).user_select = *user_select;
            }
            (L::VerticalAlign, Value::VerticalAlign(align)) => {
                Rc::make_mut(&mut style.inherited_box).vertical_align = *align;
            }
            (L::TableLayout, Value::TableLayout(table_layout)) => {
                Rc::make_mut(&mut self.style.table).table_layout = *table_layout;
            }
            (L::BorderCollapse, Value::BorderCollapse(border_collapse)) => {
                Rc::make_mut(&mut self.style.inherited_table).border_collapse = *border_collapse;
            }
            (L::BorderSpacing, Value::BorderSpacing(spacing)) => {
                Rc::make_mut(&mut self.style.inherited_table).border_spacing = *spacing;
            }
            (L::CaptionSide, Value::CaptionSide(caption_side)) => {
                Rc::make_mut(&mut self.style.inherited_table).caption_side = *caption_side;
            }
            (L::EmptyCells, Value::EmptyCells(empty_cells)) => {
                Rc::make_mut(&mut self.style.inherited_table).empty_cells = *empty_cells;
            }
            // Font properties are computed together first; a value of the
            // wrong kind cannot come out of the parsers.
            _ => {}
        }
    }

    /// Border widths compute to zero where the style is `none` or `hidden`.
    fn finish_border(&mut self, inputs: &BorderInputs) {
        let medium = self.absolute(&MEDIUM_BORDER);
        let border = Rc::make_mut(&mut self.style.border);
        let sides = [
            &mut border.top,
            &mut border.right,
            &mut border.bottom,
            &mut border.left,
        ];
        for (side, width) in sides.into_iter().zip(inputs.widths) {
            side.width = match side.style {
                BorderStyle::None | BorderStyle::Hidden => Au::ZERO,
                _ => width.unwrap_or(medium),
            };
        }
    }

    /// The outline width computes to zero where the style is `none`, and
    /// the color to `currentcolor` unless one was given.
    fn finish_outline(&mut self, inputs: &OutlineInputs) {
        let medium = self.absolute(&MEDIUM_BORDER);
        let color = self.current_color();
        let outline = Rc::make_mut(&mut self.style.outline);
        outline.width = match outline.style {
            OutlineStyle::None => Au::ZERO,
            OutlineStyle::Solid => inputs.width.unwrap_or(medium),
        };
        if !inputs.color_set {
            outline.color = color;
        }
    }

    fn fix_up(&mut self) {
        // The root element is always a block box (CSS 2.2, 9.7), and so are
        // absolute boxes (section 9.7), the children of a flex container,
        // which are flex items (CSS Flexbox 4; spec 0006, AC-3), and a box
        // with `float` other than `none` that is not itself absolutely
        // positioned (CSS 2.2, 9.7; spec 0009, child 0001).
        let floated = self.style.display_box.float != Float::None
            && self.style.display_box.position != Position::Absolute;
        // Absolute, fixed and sticky boxes are positioned, so they blockify
        // (CSS 2.2 section 9.7); relative boxes do not.
        let positioned = matches!(
            self.style.display_box.position,
            Position::Absolute | Position::Fixed | Position::Sticky
        );
        let blockify = self.is_root
            || matches!(
                self.parent.display_box.display,
                Display::Flex | Display::Grid | Display::InlineGrid
            )
            || positioned
            || floated;
        if blockify
            && matches!(
                self.style.display_box.display,
                Display::Inline | Display::InlineBlock | Display::InlineTable | Display::InlineGrid
            )
        {
            // `inline-table` blockifies to `table` (spec 0009, child 0004,
            // AC-14) and `inline-grid` to `grid` (spec 0010, AC-1).
            let blockified = match self.style.display_box.display {
                Display::InlineTable => Display::Table,
                Display::InlineGrid => Display::Grid,
                _ => Display::Block,
            };
            Rc::make_mut(&mut self.style.display_box).display = blockified;
        }
        // CSS Overflow 3: when one axis is `visible` or `clip` and the other
        // is not, `visible` computes to `auto` and `clip` to `hidden`
        // (spec 0009, child 0002, AC-6).
        let display_box = Rc::make_mut(&mut self.style.display_box);
        let (x, y) = (display_box.overflow_x, display_box.overflow_y);
        let normalize = |axis: Overflow, other: Overflow| match (axis, other) {
            (Overflow::Visible, other) if other != Overflow::Visible => Overflow::Auto,
            (Overflow::Clip, other) if other != Overflow::Clip => Overflow::Hidden,
            (axis, _) => axis,
        };
        display_box.overflow_x = normalize(x, y);
        display_box.overflow_y = normalize(y, x);
    }
}

/// Copies one longhand's computed value from `source`, for `inherit` and
/// `initial`.
fn copy_longhand(style: &mut ComputedStyle, source: &ComputedStyle, longhand: Longhand) {
    use Longhand as L;
    macro_rules! copy {
        ($group:ident . $($field:tt).+) => {
            if style.$group.$($field).+ != source.$group.$($field).+ {
                Rc::make_mut(&mut style.$group).$($field).+ = source.$group.$($field).+.clone();
            }
        };
    }
    match longhand {
        L::Display => copy!(display_box.display),
        L::Width => copy!(size.width),
        L::Height => copy!(size.height),
        L::MinWidth => copy!(size.min_width),
        L::MinHeight => copy!(size.min_height),
        L::MaxWidth => copy!(size.max_width),
        L::MaxHeight => copy!(size.max_height),
        L::BoxSizing => copy!(size.box_sizing),
        L::AspectRatio => copy!(size.aspect_ratio),
        L::MarginTop => copy!(margin.top),
        L::MarginRight => copy!(margin.right),
        L::MarginBottom => copy!(margin.bottom),
        L::MarginLeft => copy!(margin.left),
        L::PaddingTop => copy!(padding.top),
        L::PaddingRight => copy!(padding.right),
        L::PaddingBottom => copy!(padding.bottom),
        L::PaddingLeft => copy!(padding.left),
        // Widths are settled in `finish_border`.
        L::BorderTopWidth | L::BorderRightWidth | L::BorderBottomWidth | L::BorderLeftWidth => {}
        L::BorderTopStyle => copy!(border.top.style),
        L::BorderRightStyle => copy!(border.right.style),
        L::BorderBottomStyle => copy!(border.bottom.style),
        L::BorderLeftStyle => copy!(border.left.style),
        L::BorderTopColor => copy!(border.top.color),
        L::BorderRightColor => copy!(border.right.color),
        L::BorderBottomColor => copy!(border.bottom.color),
        L::BorderLeftColor => copy!(border.left.color),
        L::BorderTopLeftRadius => copy!(border.radius.top_left),
        L::BorderTopRightRadius => copy!(border.radius.top_right),
        L::BorderBottomRightRadius => copy!(border.radius.bottom_right),
        L::BorderBottomLeftRadius => copy!(border.radius.bottom_left),
        L::BackgroundColor => copy!(background.color),
        L::BackgroundImage => copy!(background.image),
        L::Opacity => copy!(effects.opacity),
        L::FontFamily => copy!(font.family),
        L::FontSize => {
            copy!(font.size);
            copy!(font.keyword_size);
        }
        L::FontWeight => copy!(font.weight),
        L::FontStyle => copy!(font.style),
        L::Color => copy!(inherited_text.color),
        L::LineHeight => copy!(inherited_text.line_height),
        L::TextAlign => copy!(inherited_text.text_align),
        L::WhiteSpace => copy!(inherited_text.white_space),
        L::TextDecorationLine => copy!(inherited_text.text_decoration_line),
        L::Visibility => copy!(inherited_box.visibility),
        L::ListStyleType => copy!(list.style_type),
        L::ListStylePosition => copy!(list.position),
        L::FlexDirection => copy!(flex.direction),
        L::RowGap => copy!(alignment.row_gap),
        L::ColumnGap => copy!(alignment.column_gap),
        L::FlexWrap => copy!(flex.wrap),
        L::FlexGrow => copy!(flex.grow),
        L::FlexShrink => copy!(flex.shrink),
        L::FlexBasis => copy!(flex.basis),
        L::JustifyContent => copy!(alignment.justify_content),
        L::AlignContent => copy!(alignment.align_content),
        L::JustifyItems => copy!(alignment.justify_items),
        L::AlignItems => copy!(alignment.align_items),
        L::JustifySelf => copy!(alignment.justify_self),
        L::AlignSelf => copy!(alignment.align_self),
        L::Order => copy!(flex.order),
        L::Transform => {
            copy!(transform.transform);
            copy!(transform.binding_id);
        }
        L::GridTemplateColumns => copy!(grid.template_columns),
        L::GridTemplateRows => copy!(grid.template_rows),
        L::GridAutoColumns => copy!(grid.auto_columns),
        L::GridAutoRows => copy!(grid.auto_rows),
        L::GridAutoFlow => copy!(grid.auto_flow),
        L::GridTemplateAreas => copy!(grid.template_areas),
        L::GridColumnStart => copy!(grid.column_start),
        L::GridColumnEnd => copy!(grid.column_end),
        L::GridRowStart => copy!(grid.row_start),
        L::GridRowEnd => copy!(grid.row_end),
        L::OverflowX => copy!(display_box.overflow_x),
        L::OverflowY => copy!(display_box.overflow_y),
        L::Position => copy!(display_box.position),
        L::Top => copy!(position_offsets.top),
        L::Right => copy!(position_offsets.right),
        L::Bottom => copy!(position_offsets.bottom),
        L::Left => copy!(position_offsets.left),
        L::ZIndex => copy!(effects.z_index),
        L::Clip => copy!(effects.clip),
        // The width is settled in `finish_outline`.
        L::OutlineWidth => {}
        L::OutlineStyle => copy!(outline.style),
        L::OutlineColor => copy!(outline.color),
        L::OutlineOffset => copy!(outline.offset),
        L::Float => copy!(display_box.float),
        L::Clear => copy!(display_box.clear),
        L::TextOverflow => copy!(display_box.text_overflow),
        L::Cursor => copy!(inherited_box.cursor),
        L::UserSelect => copy!(inherited_box.user_select),
        L::VerticalAlign => copy!(inherited_box.vertical_align),
        L::TableLayout => copy!(table.table_layout),
        L::BorderCollapse => copy!(inherited_table.border_collapse),
        L::BorderSpacing => copy!(inherited_table.border_spacing),
        L::CaptionSide => copy!(inherited_table.caption_side),
        L::EmptyCells => copy!(inherited_table.empty_cells),
    }
}

/// `bolder`, from CSS Fonts 4's relative weight table.
fn bolder(parent: u16) -> u16 {
    match parent {
        0..350 => 400,
        350..550 => 700,
        550..900 => 900,
        _ => parent,
    }
}

/// `lighter`, from the same table.
fn lighter(parent: u16) -> u16 {
    match parent {
        0..100 => parent,
        100..550 => 100,
        550..750 => 400,
        _ => 700,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(text: &str, vars: &[(&str, &str)]) -> Option<String> {
        let mut lookup = |name: &str| {
            vars.iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| Rc::from(*v))
        };
        substitute(text, &mut lookup)
    }

    #[test]
    fn var_substitutes_values_fallbacks_and_nested_blocks() {
        // AC-20
        let vars = [("gap", "4px"), ("c", "10, 20, 30")];
        assert_eq!(resolve("var(--gap)", &vars).as_deref(), Some("/**/4px/**/"));
        assert_eq!(
            resolve("rgb(var(--c))", &vars).as_deref(),
            Some("rgb(/**/10, 20, 30/**/)")
        );
        assert_eq!(
            resolve("var(--missing, 1px 2px)", &vars).as_deref(),
            Some("/**/1px 2px/**/")
        );
        assert_eq!(
            resolve("var(--missing, var(--gap))", &vars).as_deref(),
            Some("/**//**/4px/**//**/")
        );
        assert_eq!(resolve("var(--missing)", &vars), None);
        assert_eq!(resolve("var(gap)", &vars), None);
    }

    #[test]
    fn doubling_variables_hit_the_size_cap() {
        let big = "x".repeat(40 * 1024);
        assert!(resolve("var(--a) var(--a)", &[("a", &big)]).is_none());
    }

    /// The style `css` (a `style` attribute's text) computes to, under a
    /// parent with blue text.
    fn computed(css: &str) -> ComputedStyle {
        let mut parent = ComputedStyle::default();
        Rc::make_mut(&mut parent.inherited_text).color = Color::rgb8(0, 0, 255);
        let declarations = crate::parse_declarations(css, None);
        let device = Device {
            width: 800.0,
            height: 600.0,
        };
        compute(
            &declarations,
            &CascadeInputs {
                parent: Some(&parent),
                root_font_size: Au::from_px(16.0),
                device: &device,
                properties: &HashMap::new(),
            },
        )
    }

    /// Like [`computed`], but with registered custom properties from
    /// `@property` available for validation (spec 0010, AC-10).
    fn computed_with_properties(
        css: &str,
        properties: &HashMap<Atom, PropertyRule>,
    ) -> ComputedStyle {
        let parent = ComputedStyle::default();
        let declarations = crate::parse_declarations(css, None);
        let device = Device {
            width: 800.0,
            height: 600.0,
        };
        compute(
            &declarations,
            &CascadeInputs {
                parent: Some(&parent),
                root_font_size: Au::from_px(16.0),
                device: &device,
                properties,
            },
        )
    }

    #[test]
    fn a_registered_custom_property_validates_against_its_syntax() {
        // --foo: syntax "<length>", inherits false, initial 8px.
        let mut properties = HashMap::new();
        properties.insert(
            Atom::from("foo"),
            PropertyRule {
                name: Atom::from("foo"),
                syntax: crate::properties::PropertySyntax::Length,
                inherits: false,
                initial: Rc::from("8px"),
            },
        );
        // A valid length is kept.
        let ok = computed_with_properties("--foo: 16px;", &properties);
        assert_eq!(
            ok.custom.get(&Atom::from("foo")).map(|v| v.as_ref()),
            Some("16px")
        );
        // An invalid value falls back to the initial value (not inherited).
        let bad = computed_with_properties("--foo: red;", &properties);
        assert_eq!(
            bad.custom.get(&Atom::from("foo")).map(|v| v.as_ref()),
            Some("8px")
        );
    }

    #[test]
    fn outline_width_follows_its_style_and_its_color_follows_color() {
        // Spec 0006, Data model: `Outline`
        let ring = computed("color: red; outline: 2px solid; outline-offset: 0.25em");
        assert_eq!(ring.outline.width, Au::from_px(2.0));
        assert_eq!(ring.outline.color, Color::rgb8(255, 0, 0), "currentcolor");
        assert_eq!(ring.outline.offset, Au::from_px(4.0));
        assert_eq!(
            computed("outline-style: solid").outline.width,
            Au::from_px(3.0)
        );
        assert_eq!(
            computed("outline: 5px").outline.width,
            Au::ZERO,
            "style none"
        );
        let colored = computed("outline: thin solid green");
        assert_eq!(colored.outline.color, Color::rgb8(0, 128, 0));
        assert_eq!(colored.outline.width, Au::from_px(1.0));
        let initial = computed("outline: 1px solid green; outline-color: initial");
        assert_eq!(initial.outline.color, Color::rgb8(0, 0, 255));
    }

    #[test]
    fn insets_z_index_and_absolute_blockification_compute() {
        // Spec 0006, AC-1
        let style = computed("position: fixed; top: 10%; left: 2em; z-index: 4");
        assert_eq!(style.display_box.position, Position::Fixed);
        assert_eq!(style.display_box.display, Display::Block, "blockified");
        assert_eq!(style.position_offsets.top, Some(Length::Percent(10.0)));
        assert_eq!(style.position_offsets.left, Some(Length::px(32.0)));
        assert_eq!(style.position_offsets.right, None);
        assert_eq!(style.effects.z_index, Some(4));
        let relative = computed("position: relative; display: inline");
        assert_eq!(relative.display_box.display, Display::Inline);
        let clipped = computed("overflow: hidden; text-overflow: ellipsis");
        assert_eq!(
            clipped.display_box.text_overflow,
            crate::TextOverflow::Ellipsis
        );
    }

    #[test]
    fn overflow_cross_axis_normalizes_visible_and_clip() {
        // Spec 0009, child 0002, AC-6: when one axis is `visible` or `clip`
        // and the other is not, `visible` becomes `auto` and `clip` becomes
        // `hidden`.
        let mixed = computed("overflow-x: visible; overflow-y: scroll");
        assert_eq!(mixed.display_box.overflow_x, Overflow::Auto);
        assert_eq!(mixed.display_box.overflow_y, Overflow::Scroll);

        let reversed = computed("overflow-x: scroll; overflow-y: visible");
        assert_eq!(reversed.display_box.overflow_x, Overflow::Scroll);
        assert_eq!(reversed.display_box.overflow_y, Overflow::Auto);

        let clip = computed("overflow-x: clip; overflow-y: auto");
        assert_eq!(clip.display_box.overflow_x, Overflow::Hidden);
        assert_eq!(clip.display_box.overflow_y, Overflow::Auto);

        // Two `visible`s or two `clip`s stay as they are.
        let both_visible = computed("overflow: visible");
        assert_eq!(both_visible.display_box.overflow_x, Overflow::Visible);
        assert_eq!(both_visible.display_box.overflow_y, Overflow::Visible);
        let both_clip = computed("overflow: clip");
        assert_eq!(both_clip.display_box.overflow_x, Overflow::Clip);
        assert_eq!(both_clip.display_box.overflow_y, Overflow::Clip);
    }

    #[test]
    fn relative_weights_follow_the_table() {
        assert_eq!(bolder(400), 700);
        assert_eq!(bolder(700), 900);
        assert_eq!(bolder(100), 400);
        assert_eq!(lighter(400), 100);
        assert_eq!(lighter(700), 400);
        assert_eq!(lighter(900), 700);
    }

    #[test]
    fn float_clear_and_flow_root_compute() {
        // Spec 0009, child 0001: `float`, `clear` and `flow-root` compute.
        let floated = computed("float: left; clear: both; display: flow-root");
        assert_eq!(floated.display_box.float, Float::Left);
        assert_eq!(floated.display_box.clear, crate::Clear::Both);
        assert_eq!(floated.display_box.display, Display::FlowRoot);

        // A float is blockified even when its display is inline (CSS 2.2 9.7).
        let inline_float = computed("display: inline; float: right");
        assert_eq!(inline_float.display_box.display, Display::Block);
        assert_eq!(inline_float.display_box.float, Float::Right);

        // `inherit` on float copies from the parent.
        let mut parent = ComputedStyle::default();
        Rc::make_mut(&mut parent.display_box).float = Float::Left;
        let inherited = compute(
            &[Declaration {
                property: PropertyKey::Longhand(Longhand::Float),
                value: DeclaredValue::Wide(CssWide::Inherit),
                important: false,
            }],
            &CascadeInputs {
                parent: Some(&parent),
                root_font_size: Au::from_px(16.0),
                device: &Device {
                    width: 800.0,
                    height: 600.0,
                },
                properties: &HashMap::new(),
            },
        );
        assert_eq!(inherited.display_box.float, Float::Left);
    }
}
