//! A stable one line summary of a computed style, for the style dump
//! (spec 0002, AC-17). It lists a chosen set of properties, and only those
//! that differ from their initial value, so dumps stay short and readable.

use crate::{BorderSide, Color, ComputedStyle, FlexBasis, Length, LineHeight};

impl ComputedStyle {
    /// The chosen properties that differ from the initial style, as
    /// `name: value` pairs joined by `; `. Empty for the initial style.
    pub fn dump(&self) -> String {
        let initial = properties(&Self::initial());
        properties(self)
            .into_iter()
            .filter(|own| !initial.contains(own))
            .map(|(name, value)| format!("{name}: {value}"))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// The properties a dump shows, in a fixed order. Custom properties come
/// last, sorted by name.
fn properties(style: &ComputedStyle) -> Vec<(String, String)> {
    let display_box = &style.display_box;
    let size = &style.size;
    let margin = &style.margin;
    let padding = &style.padding;
    let border = &style.border;
    let font = &style.font;
    let text = &style.inherited_text;
    let lower = |value: &dyn std::fmt::Debug| kebab(&format!("{value:?}"));
    let auto = |length: Option<Length>| length.map_or("auto".into(), self::length);
    let none = |length: Option<Length>| length.map_or("none".into(), self::length);
    let radius = &border.radius;
    let mut list: Vec<(&str, String)> = vec![
        ("display", lower(&display_box.display)),
        ("position", lower(&display_box.position)),
        ("float", lower(&display_box.float)),
        ("overflow-x", lower(&display_box.overflow_x)),
        ("overflow-y", lower(&display_box.overflow_y)),
        ("text-overflow", lower(&display_box.text_overflow)),
        ("inset", {
            let offsets = &style.position_offsets;
            [offsets.top, offsets.right, offsets.bottom, offsets.left]
                .map(auto)
                .join(" ")
        }),
        ("box-sizing", lower(&size.box_sizing)),
        ("width", auto(size.width)),
        ("height", auto(size.height)),
        ("min-width", auto(size.min_width)),
        ("min-height", auto(size.min_height)),
        ("max-width", none(size.max_width)),
        ("max-height", none(size.max_height)),
        (
            "margin",
            [margin.top, margin.right, margin.bottom, margin.left]
                .map(auto)
                .join(" "),
        ),
        (
            "padding",
            [padding.top, padding.right, padding.bottom, padding.left]
                .map(length)
                .join(" "),
        ),
        ("border-top", side(&border.top)),
        ("border-right", side(&border.right)),
        ("border-bottom", side(&border.bottom)),
        ("border-left", side(&border.left)),
        (
            "border-radius",
            [
                radius.top_left,
                radius.top_right,
                radius.bottom_right,
                radius.bottom_left,
            ]
            .map(length)
            .join(" "),
        ),
        (
            "font-family",
            font.family
                .iter()
                .map(|family| family.to_string())
                .collect::<Vec<_>>()
                .join(", "),
        ),
        ("font-size", format!("{}px", number(font.size.to_f32_px()))),
        ("font-weight", font.weight.to_string()),
        ("font-style", lower(&font.style)),
        ("color", color(text.color)),
        (
            "line-height",
            match text.line_height {
                LineHeight::Normal => "normal".into(),
                LineHeight::Number(n) => number(n),
                LineHeight::Length(l) => length(l),
            },
        ),
        ("text-align", lower(&text.text_align)),
        ("white-space", lower(&text.white_space)),
        ("text-decoration-line", lower(&text.text_decoration_line)),
        ("background-color", color(style.background.color)),
        (
            "background-image",
            style
                .background
                .image
                .as_ref()
                .map_or("none".into(), |url| format!("url({url})")),
        ),
        ("visibility", lower(&style.inherited_box.visibility)),
        ("list-style-type", lower(&style.list.style_type)),
        ("list-style-position", lower(&style.list.position)),
        ("flex-direction", lower(&style.flex.direction)),
        ("row-gap", length(style.flex.row_gap)),
        ("column-gap", length(style.flex.column_gap)),
        ("flex-wrap", lower(&style.flex.wrap)),
        ("justify-content", lower(&style.flex.justify_content)),
        ("align-items", lower(&style.flex.align_items)),
        ("flex-grow", number(style.flex.grow)),
        ("flex-shrink", number(style.flex.shrink)),
        (
            "flex-basis",
            match style.flex.basis {
                FlexBasis::Auto => "auto".into(),
                FlexBasis::Content => "content".into(),
                FlexBasis::Length(l) => length(l),
            },
        ),
        (
            "align-self",
            style.flex.align_self.map_or("auto".into(), |a| lower(&a)),
        ),
        ("order", style.flex.order.to_string()),
        ("opacity", number(style.effects.opacity)),
        (
            "z-index",
            style
                .effects
                .z_index
                .map_or("auto".into(), |z| z.to_string()),
        ),
        (
            "outline",
            format!(
                "{}px {} {}",
                number(style.outline.width.to_f32_px()),
                lower(&style.outline.style),
                color(style.outline.color)
            ),
        ),
        (
            "outline-offset",
            format!("{}px", number(style.outline.offset.to_f32_px())),
        ),
    ];
    let mut custom: Vec<_> = style.custom.iter().collect();
    custom.sort_by(|a, b| a.0.cmp(b.0));
    let mut list: Vec<(String, String)> = list
        .drain(..)
        .map(|(name, value)| (name.to_owned(), value))
        .collect();
    list.extend(
        custom
            .into_iter()
            .map(|(name, value)| (format!("--{name}"), value.to_string())),
    );
    list
}

/// `PreWrap` as `pre-wrap`, the way CSS spells keywords.
fn kebab(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            out.push('-');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

fn length(length: Length) -> String {
    match length {
        Length::Au(au) => format!("{}px", number(au.to_f32_px())),
        Length::Percent(percent) => format!("{}%", number(percent)),
    }
}

fn side(side: &BorderSide) -> String {
    format!(
        "{}px {} {}",
        number(side.width.to_f32_px()),
        format!("{:?}", side.style).to_lowercase(),
        color(side.color)
    )
}

/// At most two decimals, no trailing zeros.
fn number(value: f32) -> String {
    let text = format!("{value:.2}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" {
        "0".into()
    } else {
        text.into()
    }
}

fn color(color: Color) -> String {
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}{:02x}",
        byte(color.r),
        byte(color.g),
        byte(color.b),
        byte(color.a)
    )
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use crate::{Display, Margin, Overflow};

    use super::*;

    #[test]
    fn only_properties_away_from_their_initial_value_are_listed() {
        assert_eq!(ComputedStyle::default().dump(), "");
        let mut style = ComputedStyle::default();
        Rc::make_mut(&mut style.display_box).display = Display::Block;
        Rc::make_mut(&mut style.display_box).overflow_y = Overflow::Auto;
        style.margin = Rc::new(Margin {
            left: Some(Length::Percent(10.0)),
            ..Margin::all(Length::px(8.0))
        });
        style.background = Rc::new(crate::Background {
            color: Color::rgb8(0xe0, 0x6c, 0x4f),
            image: None,
        });
        assert_eq!(
            style.dump(),
            "display: block; overflow-y: auto; margin: 8px 8px 8px 10%; \
             background-color: #e06c4fff"
        );
    }
}
