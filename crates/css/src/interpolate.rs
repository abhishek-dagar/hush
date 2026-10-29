//! Interpolation of computed values, for transitions and `@keyframes`
//! (spec 0010, AC-26).
//!
//! Lengths, percentages, `calc()` mixes, numbers, integers, colors (in
//! premultiplied sRGB), angles and transform lists interpolate; every other
//! property animates discretely, flipping at 50%. `visibility` is visible
//! for any progress strictly between 0 and 1 when either end is visible.

use std::rc::Rc;

use base::Au;
use display_list::Binding;

use crate::properties::{Declaration, DeclaredValue, Longhand, PropertyKey, Shorthand, Value};
use crate::style::Angle;
use crate::transform::transform_fn_matrix;
use crate::{CalcExpr, Color, ComputedStyle, FlexBasis, Length, LineHeight, TimingFunction};
use crate::{TransformFn, Visibility};

/// An animatable longhand property (spec 0010, AC-23, AC-26): every
/// longhand except `display`, the container properties and the
/// `transition-*` and `animation-*` ones themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PropertyId(Longhand);

impl PropertyId {
    /// The longhands a `transition-property` or keyframe name stands for:
    /// `all` is every animatable longhand, a shorthand its animatable
    /// longhands, a longhand itself, and anything else nothing.
    pub fn expand(name: &str) -> Vec<Self> {
        if name.eq_ignore_ascii_case("all") {
            return Self::all().collect();
        }
        if let Some(longhand) = Longhand::from_name(name) {
            return Self::new(longhand).into_iter().collect();
        }
        Shorthand::from_name(name)
            .map(|shorthand| {
                shorthand
                    .longhands()
                    .iter()
                    .filter_map(|&l| Self::new(l))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Every animatable longhand, in table order.
    pub fn all() -> impl Iterator<Item = Self> {
        Longhand::ALL.iter().filter_map(|&l| Self::new(l))
    }

    fn new(longhand: Longhand) -> Option<Self> {
        use Longhand as L;
        let animatable = !matches!(
            longhand,
            L::Display
                | L::ContainerType
                | L::ContainerName
                | L::TransitionProperty
                | L::TransitionDuration
                | L::TransitionDelay
                | L::TransitionTimingFunction
                | L::AnimationName
                | L::AnimationDuration
                | L::AnimationDelay
                | L::AnimationTimingFunction
                | L::AnimationIterationCount
                | L::AnimationDirection
                | L::AnimationFillMode
                | L::AnimationPlayState
        );
        animatable.then_some(Self(longhand))
    }

    /// The property name, as `transitionend`'s `propertyName` gives it.
    pub fn name(self) -> &'static str {
        self.0.name()
    }

    /// Whether the property is `transform` or one of the separate
    /// `translate`, `rotate` and `scale`, which run as a bound transform
    /// (spec 0010, AC-28).
    pub fn is_transform(self) -> bool {
        matches!(
            self.0,
            Longhand::Transform | Longhand::Translate | Longhand::Rotate | Longhand::Scale
        )
    }

    /// Whether the property is `opacity`, which runs as a bound opacity
    /// (spec 0010, AC-28).
    pub fn is_opacity(self) -> bool {
        self.0 == Longhand::Opacity
    }

    /// Whether `a` and `b` hold different computed values for it.
    pub fn differs(self, a: &ComputedStyle, b: &ComputedStyle) -> bool {
        differs(self.0, a, b)
    }

    /// Writes the value `progress` of the way from `from`'s value to
    /// `to`'s into `out` (spec 0010, AC-26). Progress below 0 or above 1
    /// extrapolates where the type allows it, as easing overshoot does.
    pub fn interpolate(
        self,
        from: &ComputedStyle,
        to: &ComputedStyle,
        progress: f32,
        out: &mut ComputedStyle,
    ) {
        interpolate(self.0, from, to, progress, out);
    }
}

/// The animatable longhands `declarations` set, as one keyframe declares
/// them (spec 0010, AC-24).
pub fn declared_properties(declarations: &[Declaration]) -> Vec<PropertyId> {
    let mut out: Vec<PropertyId> = Vec::new();
    for declaration in declarations {
        if let PropertyKey::Longhand(longhand) = declaration.property
            && let Some(id) = PropertyId::new(longhand)
            && !out.contains(&id)
        {
            out.push(id);
        }
    }
    out
}

/// The `animation-timing-function` a keyframe declares, which eases from it
/// to the next keyframe (spec 0010, AC-24).
pub fn keyframe_timing(declarations: &[Declaration]) -> Option<TimingFunction> {
    declarations
        .iter()
        .rev()
        .find_map(|d| match (&d.property, &d.value) {
            (
                PropertyKey::Longhand(Longhand::AnimationTimingFunction),
                DeclaredValue::Value(Value::AnimationTimingFunction(timing)),
            ) => Some(*timing),
            _ => None,
        })
}

/// One table from longhand to computed field and how it animates, so
/// `differs` and `interpolate` cannot disagree.
macro_rules! table {
    ($($longhand:ident => $how:ident($group:ident $(. $field:tt)+);)*) => {
        fn differs(longhand: Longhand, a: &ComputedStyle, b: &ComputedStyle) -> bool {
            match longhand {
                $(Longhand::$longhand => a.$group$(.$field)+ != b.$group$(.$field)+,)*
                Longhand::FontSize => a.font.size != b.font.size,
                Longhand::VerticalAlign => a.vertical_align != b.vertical_align,
                _ => false,
            }
        }

        fn interpolate(
            longhand: Longhand,
            from: &ComputedStyle,
            to: &ComputedStyle,
            p: f32,
            out: &mut ComputedStyle,
        ) {
            match longhand {
                $(Longhand::$longhand => {
                    let value = $how(&from.$group$(.$field)+, &to.$group$(.$field)+, p);
                    if out.$group$(.$field)+ != value {
                        Rc::make_mut(&mut out.$group)$(.$field)+ = value;
                    }
                })*
                Longhand::FontSize => {
                    let size = au(&from.font.size, &to.font.size, p);
                    if out.font.size != size {
                        let font = Rc::make_mut(&mut out.font);
                        font.size = size;
                        font.keyword_size = None;
                    }
                }
                Longhand::VerticalAlign => {
                    out.vertical_align = discrete(&from.vertical_align, &to.vertical_align, p);
                }
                _ => {}
            }
        }
    };
}

table! {
    Width => option_length(size.width);
    Height => option_length(size.height);
    MinWidth => option_length(size.min_width);
    MinHeight => option_length(size.min_height);
    MaxWidth => option_length(size.max_width);
    MaxHeight => option_length(size.max_height);
    BoxSizing => discrete(size.box_sizing);
    AspectRatio => discrete(size.aspect_ratio);
    MarginTop => option_length(margin.top);
    MarginRight => option_length(margin.right);
    MarginBottom => option_length(margin.bottom);
    MarginLeft => option_length(margin.left);
    PaddingTop => length(padding.top);
    PaddingRight => length(padding.right);
    PaddingBottom => length(padding.bottom);
    PaddingLeft => length(padding.left);
    BorderTopWidth => au(border.top.width);
    BorderRightWidth => au(border.right.width);
    BorderBottomWidth => au(border.bottom.width);
    BorderLeftWidth => au(border.left.width);
    BorderTopStyle => discrete(border.top.style);
    BorderRightStyle => discrete(border.right.style);
    BorderBottomStyle => discrete(border.bottom.style);
    BorderLeftStyle => discrete(border.left.style);
    BorderTopColor => color(border.top.color);
    BorderRightColor => color(border.right.color);
    BorderBottomColor => color(border.bottom.color);
    BorderLeftColor => color(border.left.color);
    BorderTopLeftRadius => length(border.radius.top_left);
    BorderTopRightRadius => length(border.radius.top_right);
    BorderBottomRightRadius => length(border.radius.bottom_right);
    BorderBottomLeftRadius => length(border.radius.bottom_left);
    BackgroundColor => color(background.color);
    BackgroundImage => discrete(background.image);
    Opacity => opacity(effects.opacity);
    FontFamily => discrete(font.family);
    FontWeight => weight(font.weight);
    FontStyle => discrete(font.style);
    Color => color(inherited_text.color);
    LineHeight => line_height(inherited_text.line_height);
    TextAlign => discrete(inherited_text.text_align);
    WhiteSpace => discrete(inherited_text.white_space);
    OverflowWrap => discrete(inherited_text.overflow_wrap);
    TextDecorationLine => discrete(inherited_text.text_decoration_line);
    Visibility => visibility(inherited_box.visibility);
    ListStyleType => discrete(list.style_type);
    ListStylePosition => discrete(list.position);
    FlexDirection => discrete(flex.direction);
    RowGap => length(alignment.row_gap);
    ColumnGap => length(alignment.column_gap);
    FlexWrap => discrete(flex.wrap);
    FlexGrow => number(flex.grow);
    FlexShrink => number(flex.shrink);
    FlexBasis => flex_basis(flex.basis);
    JustifyContent => discrete(alignment.justify_content);
    AlignContent => discrete(alignment.align_content);
    JustifyItems => discrete(alignment.justify_items);
    AlignItems => discrete(alignment.align_items);
    JustifySelf => discrete(alignment.justify_self);
    AlignSelf => discrete(alignment.align_self);
    Order => integer(flex.order);
    Transform => transform_list(transform.transform);
    Translate => translate(transform.translate);
    Rotate => rotate(transform.rotate);
    Scale => scale(transform.scale);
    TransformOrigin => origin(transform.origin);
    TransformStyle => discrete(transform.style);
    Perspective => discrete(transform.perspective);
    PerspectiveOrigin => discrete(transform.perspective_origin);
    GridTemplateColumns => discrete(grid.template_columns);
    GridTemplateRows => discrete(grid.template_rows);
    GridAutoColumns => discrete(grid.auto_columns);
    GridAutoRows => discrete(grid.auto_rows);
    GridAutoFlow => discrete(grid.auto_flow);
    GridTemplateAreas => discrete(grid.template_areas);
    GridColumnStart => discrete(grid.column_start);
    GridColumnEnd => discrete(grid.column_end);
    GridRowStart => discrete(grid.row_start);
    GridRowEnd => discrete(grid.row_end);
    OverflowX => discrete(display_box.overflow_x);
    OverflowY => discrete(display_box.overflow_y);
    Float => discrete(display_box.float);
    Clear => discrete(display_box.clear);
    Position => discrete(display_box.position);
    Top => option_length(position_offsets.top);
    Right => option_length(position_offsets.right);
    Bottom => option_length(position_offsets.bottom);
    Left => option_length(position_offsets.left);
    Clip => discrete(effects.clip);
    ZIndex => option_integer(effects.z_index);
    OutlineWidth => au(outline.width);
    OutlineStyle => discrete(outline.style);
    OutlineColor => color(outline.color);
    OutlineOffset => au(outline.offset);
    TextOverflow => discrete(display_box.text_overflow);
    Cursor => discrete(inherited_box.cursor);
    UserSelect => discrete(inherited_box.user_select);
    TableLayout => discrete(table.table_layout);
    BorderCollapse => discrete(inherited_table.border_collapse);
    BorderSpacing => spacing(inherited_table.border_spacing);
    CaptionSide => discrete(inherited_table.caption_side);
    EmptyCells => discrete(inherited_table.empty_cells);
}

fn lerp(a: f32, b: f32, p: f32) -> f32 {
    a + (b - a) * p
}

/// The from value before 50%, the to value from there on.
fn discrete<T: Clone>(a: &T, b: &T, p: f32) -> T {
    if p < 0.5 { a.clone() } else { b.clone() }
}

fn number(a: &f32, b: &f32, p: f32) -> f32 {
    lerp(*a, *b, p)
}

fn integer(a: &i32, b: &i32, p: f32) -> i32 {
    lerp(*a as f32, *b as f32, p).round() as i32
}

fn option_integer(a: &Option<i32>, b: &Option<i32>, p: f32) -> Option<i32> {
    match (a, b) {
        (Some(a), Some(b)) => Some(integer(a, b, p)),
        _ => discrete(a, b, p),
    }
}

/// `font-weight` is a number from 1 to 1000.
fn weight(a: &u16, b: &u16, p: f32) -> u16 {
    lerp(f32::from(*a), f32::from(*b), p)
        .round()
        .clamp(1.0, 1000.0) as u16
}

fn au(a: &Au, b: &Au, p: f32) -> Au {
    Au(lerp(a.0 as f32, b.0 as f32, p).round() as i32)
}

fn spacing(a: &(Au, Au), b: &(Au, Au), p: f32) -> (Au, Au) {
    (au(&a.0, &b.0, p), au(&a.1, &b.1, p))
}

/// Same units blend directly; a mix of absolute, percentage and `calc()`
/// blends as `calc(a * (1 - p) + b * p)` (spec 0010, AC-26).
pub(crate) fn length(a: &Length, b: &Length, p: f32) -> Length {
    match (a, b) {
        (Length::Au(a), Length::Au(b)) => Length::Au(au(a, b, p)),
        (Length::Percent(a), Length::Percent(b)) => Length::Percent(lerp(*a, *b, p)),
        _ if p == 0.0 => a.clone(),
        _ if p == 1.0 => b.clone(),
        _ => Length::Calc(Rc::new(CalcExpr::Sum(vec![
            CalcExpr::Product(Box::new(CalcExpr::Leaf(a.clone())), 1.0 - p),
            CalcExpr::Product(Box::new(CalcExpr::Leaf(b.clone())), p),
        ]))),
    }
}

/// `auto` (and `none`) against a length is discrete.
fn option_length(a: &Option<Length>, b: &Option<Length>, p: f32) -> Option<Length> {
    match (a, b) {
        (Some(a), Some(b)) => Some(length(a, b, p)),
        _ => discrete(a, b, p),
    }
}

fn line_height(a: &LineHeight, b: &LineHeight, p: f32) -> LineHeight {
    match (a, b) {
        (LineHeight::Number(a), LineHeight::Number(b)) => LineHeight::Number(lerp(*a, *b, p)),
        (LineHeight::Length(a), LineHeight::Length(b)) => LineHeight::Length(length(a, b, p)),
        _ => discrete(a, b, p),
    }
}

fn flex_basis(a: &FlexBasis, b: &FlexBasis, p: f32) -> FlexBasis {
    match (a, b) {
        (FlexBasis::Length(a), FlexBasis::Length(b)) => FlexBasis::Length(length(a, b, p)),
        _ => discrete(a, b, p),
    }
}

/// Running opacity is bound by the page once it applies the value (spec
/// 0010, AC-28); interpolation gives the plain number.
fn opacity(a: &Binding<f32>, b: &Binding<f32>, p: f32) -> Binding<f32> {
    let value = |binding: &Binding<f32>| match binding {
        Binding::Static(v) | Binding::Bound { value: v, .. } => *v,
    };
    Binding::Static(lerp(value(a), value(b), p).clamp(0.0, 1.0))
}

fn visibility(a: &Visibility, b: &Visibility, p: f32) -> Visibility {
    let visible = |v: &Visibility| *v == Visibility::Visible;
    if p > 0.0 && p < 1.0 && (visible(a) || visible(b)) {
        Visibility::Visible
    } else {
        discrete(a, b, p)
    }
}

/// Two colors blended in premultiplied sRGB (spec 0010, AC-26).
pub(crate) fn color(a: &Color, b: &Color, p: f32) -> Color {
    let alpha = lerp(a.a, b.a, p).clamp(0.0, 1.0);
    if alpha <= 0.0 {
        return Color::TRANSPARENT;
    }
    let channel = |x: f32, y: f32| (lerp(x * a.a, y * b.a, p) / alpha).clamp(0.0, 1.0);
    Color::rgba(
        channel(a.r, b.r),
        channel(a.g, b.g),
        channel(a.b, b.b),
        alpha,
    )
}

fn angle(a: &Angle, b: &Angle, p: f32) -> Angle {
    Angle::Deg(lerp(a.degrees(), b.degrees(), p))
}

fn origin(a: &(Length, Length), b: &(Length, Length), p: f32) -> (Length, Length) {
    (length(&a.0, &b.0, p), length(&a.1, &b.1, p))
}

/// `none` is no translation.
fn translate(
    a: &Option<(Length, Option<Length>)>,
    b: &Option<(Length, Option<Length>)>,
    p: f32,
) -> Option<(Length, Option<Length>)> {
    if a.is_none() && b.is_none() {
        return None;
    }
    let parts = |t: &Option<(Length, Option<Length>)>| {
        t.clone().map_or((Length::ZERO, Length::ZERO), |(x, y)| {
            (x, y.unwrap_or(Length::ZERO))
        })
    };
    let (ax, ay) = parts(a);
    let (bx, by) = parts(b);
    Some((length(&ax, &bx, p), Some(length(&ay, &by, p))))
}

/// `none` is no rotation.
fn rotate(a: &Option<Angle>, b: &Option<Angle>, p: f32) -> Option<Angle> {
    if a.is_none() && b.is_none() {
        return None;
    }
    let zero = Angle::Deg(0.0);
    Some(angle(
        a.as_ref().unwrap_or(&zero),
        b.as_ref().unwrap_or(&zero),
        p,
    ))
}

/// `none` is a scale of 1.
fn scale(
    a: &Option<(f32, Option<f32>)>,
    b: &Option<(f32, Option<f32>)>,
    p: f32,
) -> Option<(f32, Option<f32>)> {
    if a.is_none() && b.is_none() {
        return None;
    }
    let parts = |s: &Option<(f32, Option<f32>)>| s.map_or((1.0, 1.0), |(x, y)| (x, y.unwrap_or(x)));
    let (ax, ay) = parts(a);
    let (bx, by) = parts(b);
    let (x, y) = (lerp(ax, bx, p), lerp(ay, by, p));
    Some((x, (x != y).then_some(y)))
}

/// Two transform lists blended function by function when their kinds match
/// pairwise, the shorter (or `none`) padded with identity functions; else
/// through their 2D matrices, decomposed and recomposed. Two 3D lists that
/// do not match flip at 50% (spec 0010, AC-26).
pub(crate) fn transform_list(
    a: &Rc<[TransformFn]>,
    b: &Rc<[TransformFn]>,
    p: f32,
) -> Rc<[TransformFn]> {
    if p == 0.0 {
        return Rc::clone(a);
    }
    if p == 1.0 {
        return Rc::clone(b);
    }
    let len = a.len().max(b.len());
    let mut out = Vec::with_capacity(len);
    let mut matched = true;
    for i in 0..len {
        let blended = match (a.get(i), b.get(i)) {
            (Some(x), Some(y)) => transform_fn(x, y, p),
            (Some(x), None) => identity_of(x).and_then(|id| transform_fn(x, &id, p)),
            (None, Some(y)) => identity_of(y).and_then(|id| transform_fn(&id, y, p)),
            (None, None) => None,
        };
        match blended {
            Some(f) => out.push(f),
            None => {
                matched = false;
                break;
            }
        }
    }
    if matched {
        return Rc::from(out);
    }
    match (matrix_2d(a), matrix_2d(b)) {
        (Some(ma), Some(mb)) => {
            let blended = recompose(&lerp_decomposed(&decompose(ma), &decompose(mb), p));
            Rc::from([TransformFn::Matrix(
                blended[0], blended[1], blended[2], blended[3], blended[4], blended[5],
            )])
        }
        _ => discrete(a, b, p),
    }
}

/// The identity function of the same kind as `f`, to pad a shorter list.
fn identity_of(f: &TransformFn) -> Option<TransformFn> {
    use TransformFn as F;
    let zero = || Length::ZERO;
    let none = || Angle::Deg(0.0);
    Some(match f {
        F::Translate(_, y) => F::Translate(zero(), y.as_ref().map(|_| zero())),
        F::TranslateX(_) => F::TranslateX(zero()),
        F::TranslateY(_) => F::TranslateY(zero()),
        F::TranslateZ(_) => F::TranslateZ(zero()),
        F::Translate3d(..) => F::Translate3d(zero(), zero(), zero()),
        F::Scale(_, y) => F::Scale(1.0, y.map(|_| 1.0)),
        F::ScaleX(_) => F::ScaleX(1.0),
        F::ScaleY(_) => F::ScaleY(1.0),
        F::ScaleZ(_) => F::ScaleZ(1.0),
        F::Scale3d(..) => F::Scale3d(1.0, 1.0, 1.0),
        F::Rotate(_) => F::Rotate(none()),
        F::RotateX(_) => F::RotateX(none()),
        F::RotateY(_) => F::RotateY(none()),
        F::RotateZ(_) => F::RotateZ(none()),
        F::Rotate3d(x, y, z, _) => F::Rotate3d(*x, *y, *z, none()),
        F::Skew(_, y) => F::Skew(none(), y.as_ref().map(|_| none())),
        F::SkewX(_) => F::SkewX(none()),
        F::SkewY(_) => F::SkewY(none()),
        F::Matrix(..) => F::Matrix(1.0, 0.0, 0.0, 1.0, 0.0, 0.0),
        F::Matrix3d(_) => {
            let mut m = [0.0; 16];
            m[0] = 1.0;
            m[5] = 1.0;
            m[10] = 1.0;
            m[15] = 1.0;
            F::Matrix3d(m)
        }
        // `perspective(none)` is the identity, which this type cannot say.
        F::Perspective(_) => return None,
    })
}

/// One pair of functions of the same kind blended by `p`, or `None` when
/// the kinds differ.
fn transform_fn(a: &TransformFn, b: &TransformFn, p: f32) -> Option<TransformFn> {
    use TransformFn as F;
    let both = |a: &Option<Length>, b: &Option<Length>| match (a, b) {
        (Some(a), Some(b)) => Some(Some(length(a, b, p))),
        (None, None) => Some(None),
        _ => None,
    };
    Some(match (a, b) {
        (F::Translate(ax, ay), F::Translate(bx, by)) => {
            F::Translate(length(ax, bx, p), both(ay, by)?)
        }
        (F::TranslateX(a), F::TranslateX(b)) => F::TranslateX(length(a, b, p)),
        (F::TranslateY(a), F::TranslateY(b)) => F::TranslateY(length(a, b, p)),
        (F::TranslateZ(a), F::TranslateZ(b)) => F::TranslateZ(length(a, b, p)),
        (F::Translate3d(ax, ay, az), F::Translate3d(bx, by, bz)) => {
            F::Translate3d(length(ax, bx, p), length(ay, by, p), length(az, bz, p))
        }
        (F::Scale(ax, ay), F::Scale(bx, by)) => F::Scale(
            lerp(*ax, *bx, p),
            match (ay, by) {
                (Some(ay), Some(by)) => Some(lerp(*ay, *by, p)),
                (None, None) => None,
                (ay, by) => Some(lerp(ay.unwrap_or(*ax), by.unwrap_or(*bx), p)),
            },
        ),
        (F::ScaleX(a), F::ScaleX(b)) => F::ScaleX(lerp(*a, *b, p)),
        (F::ScaleY(a), F::ScaleY(b)) => F::ScaleY(lerp(*a, *b, p)),
        (F::ScaleZ(a), F::ScaleZ(b)) => F::ScaleZ(lerp(*a, *b, p)),
        (F::Scale3d(ax, ay, az), F::Scale3d(bx, by, bz)) => {
            F::Scale3d(lerp(*ax, *bx, p), lerp(*ay, *by, p), lerp(*az, *bz, p))
        }
        (F::Rotate(a), F::Rotate(b)) => F::Rotate(angle(a, b, p)),
        (F::RotateX(a), F::RotateX(b)) => F::RotateX(angle(a, b, p)),
        (F::RotateY(a), F::RotateY(b)) => F::RotateY(angle(a, b, p)),
        (F::RotateZ(a), F::RotateZ(b)) => F::RotateZ(angle(a, b, p)),
        (F::Rotate3d(ax, ay, az, aa), F::Rotate3d(bx, by, bz, ba))
            if (ax, ay, az) == (bx, by, bz) =>
        {
            F::Rotate3d(*ax, *ay, *az, angle(aa, ba, p))
        }
        (F::Skew(ax, ay), F::Skew(bx, by)) => {
            let zero = Angle::Deg(0.0);
            F::Skew(
                angle(ax, bx, p),
                match (ay, by) {
                    (None, None) => None,
                    (ay, by) => Some(angle(
                        ay.as_ref().unwrap_or(&zero),
                        by.as_ref().unwrap_or(&zero),
                        p,
                    )),
                },
            )
        }
        (F::SkewX(a), F::SkewX(b)) => F::SkewX(angle(a, b, p)),
        (F::SkewY(a), F::SkewY(b)) => F::SkewY(angle(a, b, p)),
        (F::Perspective(a), F::Perspective(b)) => F::Perspective(length(a, b, p)),
        // Matrices blend through decomposition, as mismatched lists do.
        _ => return None,
    })
}

/// The list's matrix as `[a, b, c, d, e, f]` when it is 2D. Percentages
/// count as zero: interpolation has no box size.
fn matrix_2d(list: &[TransformFn]) -> Option<[f32; 6]> {
    let mut m = base::mat4_identity();
    for f in list.iter().rev() {
        m = base::mat4_mul(&m, &transform_fn_matrix(f, base::Size::default()));
    }
    let flat = m[2] == 0.0
        && m[3] == 0.0
        && m[6] == 0.0
        && m[7] == 0.0
        && m[8] == 0.0
        && m[9] == 0.0
        && m[10] == 1.0
        && m[11] == 0.0
        && m[14] == 0.0
        && m[15] == 1.0;
    flat.then_some([m[0], m[1], m[4], m[5], m[12], m[13]])
}

/// A 2D matrix as translation, scale, angle (radians) and the remaining
/// 2 by 2 part (CSS Transforms 1, "Decomposing a 2D matrix").
#[derive(Debug, Clone, Copy)]
struct Decomposed {
    translate: [f32; 2],
    scale: [f32; 2],
    angle: f32,
    m: [f32; 4],
}

fn decompose([a, b, c, d, e, f]: [f32; 6]) -> Decomposed {
    let (mut row0, mut row1) = ([a, b], [c, d]);
    let mut scale = [row0[0].hypot(row0[1]), row1[0].hypot(row1[1])];
    // A negative determinant flips one axis.
    if row0[0] * row1[1] - row0[1] * row1[0] < 0.0 {
        if row0[0] < row1[1] {
            scale[0] = -scale[0];
        } else {
            scale[1] = -scale[1];
        }
    }
    if scale[0] != 0.0 {
        row0 = [row0[0] / scale[0], row0[1] / scale[0]];
    }
    if scale[1] != 0.0 {
        row1 = [row1[0] / scale[1], row1[1] / scale[1]];
    }
    let angle = row0[1].atan2(row0[0]);
    if angle != 0.0 {
        let (sn, cs) = (-row0[1], row0[0]);
        let [m11, m12] = row0;
        let [m21, m22] = row1;
        row0 = [cs * m11 + sn * m21, cs * m12 + sn * m22];
        row1 = [-sn * m11 + cs * m21, -sn * m12 + cs * m22];
    }
    Decomposed {
        translate: [e, f],
        scale,
        angle,
        m: [row0[0], row0[1], row1[0], row1[1]],
    }
}

fn lerp_decomposed(a: &Decomposed, b: &Decomposed, p: f32) -> Decomposed {
    let (mut a, mut b) = (*a, *b);
    // Turn the shorter way, flipping both axes instead of a half turn.
    if (a.scale[0] < 0.0 && b.scale[1] < 0.0) || (a.scale[1] < 0.0 && b.scale[0] < 0.0) {
        a.scale = [-a.scale[0], -a.scale[1]];
        a.angle += if a.angle < 0.0 {
            std::f32::consts::PI
        } else {
            -std::f32::consts::PI
        };
    }
    let tau = std::f32::consts::TAU;
    if a.angle == 0.0 {
        a.angle = tau;
    }
    if b.angle == 0.0 {
        b.angle = tau;
    }
    if (a.angle - b.angle).abs() > std::f32::consts::PI {
        if a.angle > b.angle {
            a.angle -= tau;
        } else {
            b.angle -= tau;
        }
    }
    Decomposed {
        translate: [
            lerp(a.translate[0], b.translate[0], p),
            lerp(a.translate[1], b.translate[1], p),
        ],
        scale: [
            lerp(a.scale[0], b.scale[0], p),
            lerp(a.scale[1], b.scale[1], p),
        ],
        angle: lerp(a.angle, b.angle, p),
        m: [
            lerp(a.m[0], b.m[0], p),
            lerp(a.m[1], b.m[1], p),
            lerp(a.m[2], b.m[2], p),
            lerp(a.m[3], b.m[3], p),
        ],
    }
}

fn recompose(d: &Decomposed) -> [f32; 6] {
    let (sn, cs) = d.angle.sin_cos();
    // Undo `decompose`: turn the 2 by 2 part's rows back by the angle,
    // then scale each row.
    let [m11, m12, m21, m22] = d.m;
    let row0 = [cs * m11 + sn * m21, cs * m12 + sn * m22];
    let row1 = [-sn * m11 + cs * m21, -sn * m12 + cs * m22];
    [
        row0[0] * d.scale[0],
        row0[1] * d.scale[0],
        row1[0] * d.scale[1],
        row1[1] * d.scale[1],
        d.translate[0],
        d.translate[1],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-3, "{a} vs {b}");
    }

    #[test]
    fn mixed_units_blend_through_calc() {
        let mixed = length(&Length::px(100.0), &Length::Percent(50.0), 0.5);
        // Half of 100px plus half of 50% of 400px.
        assert_eq!(mixed.resolve(Some(Au::from_px(400.0))), Au::from_px(150.0));
    }

    #[test]
    fn none_blends_with_a_list_through_identities() {
        let none: Rc<[TransformFn]> = Rc::from([]);
        let to: Rc<[TransformFn]> = Rc::from([TransformFn::Scale(2.0, None)]);
        assert_eq!(
            &*transform_list(&none, &to, 0.25),
            &[TransformFn::Scale(1.25, None)]
        );
    }

    #[test]
    fn mismatched_lists_blend_through_their_matrices() {
        let from: Rc<[TransformFn]> = Rc::from([TransformFn::Rotate(Angle::Deg(0.0))]);
        let to: Rc<[TransformFn]> = Rc::from([
            TransformFn::Scale(2.0, None),
            TransformFn::Rotate(Angle::Deg(90.0)),
        ]);
        let half = transform_list(&from, &to, 0.5);
        let [TransformFn::Matrix(a, b, c, d, _, _)] = *half else {
            panic!("expected a matrix, got {half:?}");
        };
        // Scale 1.5, rotated 45 degrees.
        let (s, c45) = (45f32.to_radians().sin(), 45f32.to_radians().cos());
        close(a, 1.5 * c45);
        close(b, 1.5 * s);
        close(c, -1.5 * s);
        close(d, 1.5 * c45);
    }

    #[test]
    fn colors_blend_premultiplied() {
        let clear = Color::rgba(1.0, 0.0, 0.0, 0.0);
        let blue = Color::rgba(0.0, 0.0, 1.0, 1.0);
        let half = color(&clear, &blue, 0.5);
        close(half.a, 0.5);
        // The transparent red adds no red.
        close(half.r, 0.0);
        close(half.b, 1.0);
    }

    #[test]
    fn visibility_is_visible_in_between() {
        assert_eq!(
            visibility(&Visibility::Hidden, &Visibility::Visible, 0.1),
            Visibility::Visible
        );
        assert_eq!(
            visibility(&Visibility::Hidden, &Visibility::Visible, 0.0),
            Visibility::Hidden
        );
    }

    #[test]
    fn all_and_shorthands_expand() {
        assert!(PropertyId::expand("all").len() > 50);
        let margin: Vec<&str> = PropertyId::expand("margin")
            .iter()
            .map(|p| p.name())
            .collect();
        assert_eq!(
            margin,
            ["margin-top", "margin-right", "margin-bottom", "margin-left"]
        );
        assert!(PropertyId::expand("display").is_empty());
        assert!(PropertyId::expand("none").is_empty());
    }
}
