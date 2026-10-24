//! Presentational hints: HTML element attributes that map to CSS
//! declarations, cascaded at the author origin with specificity 0, before
//! every author rule (spec 0009, child 0004, AC-19). The mappings follow
//! the HTML standard's Rendering section on tables.
//!
//! `Stylist::cascade` calls [`presentational_hints`] for every element and
//! chains the result into the cascade between the UA and author normal
//! declarations, which is where author origin at specificity 0 sits.

use base::Au;
use base::atoms::{LocalName, QualName};

use crate::properties::{Declaration, DeclaredValue, Longhand, PropertyKey, Value};
use crate::values::{SpecifiedLength, parse_legacy_color};
use crate::{BorderStyle, Float, MatchElement, TextAlign, VerticalAlign, WhiteSpace};

/// The presentational hint declarations for `element`, in a fixed order
/// per attribute. An attribute that is absent or parses to nothing
/// produces no declarations.
pub(crate) fn presentational_hints<E: MatchElement + Clone>(element: &E) -> Vec<Declaration> {
    let mut hints = Vec::new();
    match &*element.name().local {
        "table" => table_hints(element, &mut hints),
        "td" | "th" => cell_hints(element, &mut hints),
        "tr" | "thead" | "tbody" | "tfoot" => row_hints(element, &mut hints),
        "col" | "colgroup" => col_hints(element, &mut hints),
        _ => {}
    }
    hints
}

/// One hint declaration.
fn decl(longhand: Longhand, value: Value) -> Declaration {
    Declaration {
        property: PropertyKey::Longhand(longhand),
        value: DeclaredValue::Value(value),
        important: false,
    }
}

/// Reads `attr` as a length: a non negative number is px, a `%` suffix is
/// a percentage, anything else is no value.
fn length_attr<E: MatchElement>(element: &E, attr: &str) -> Option<SpecifiedLength> {
    let value = element.attribute(&attr_name(attr))?.trim();
    if let Some(rest) = value.strip_suffix('%') {
        let percent: f32 = rest.trim().parse().ok()?;
        return (percent >= 0.0).then_some(SpecifiedLength::Percent(percent));
    }
    let px: f32 = value.parse().ok()?;
    (px >= 0.0).then_some(SpecifiedLength::Px(px))
}

/// A `table`'s `border` attribute as a px count: a missing number parses
/// as 1, `0` gives no borders, and a missing attribute gives no hints.
fn table_border_px<E: MatchElement>(table: &E) -> Option<f32> {
    let value = table.attribute(&attr_name("border"))?.trim();
    let px = if value.is_empty() {
        1.0
    } else {
        value.parse::<f32>().ok()?
    };
    Some(px)
}

fn attr_name(local: &str) -> QualName {
    QualName::attribute(LocalName::from(local))
}

/// `width` and `height` attributes, with `0` ignored (`Size` in px or
/// percent).
fn size_hints<E: MatchElement>(element: &E, hints: &mut Vec<Declaration>) {
    for (attr, longhand) in [("width", Longhand::Width), ("height", Longhand::Height)] {
        let Some(length) = length_attr(element, attr) else {
            continue;
        };
        if matches!(length, SpecifiedLength::Px(px) if px == 0.0) {
            continue;
        }
        hints.push(decl(longhand, Value::Size(Some(length))));
    }
}

/// `bgcolor` as a background color hint.
fn bgcolor_hint<E: MatchElement>(element: &E, hints: &mut Vec<Declaration>) {
    let Some(color) = element
        .attribute(&attr_name("bgcolor"))
        .and_then(parse_legacy_color)
    else {
        return;
    };
    hints.push(decl(Longhand::BackgroundColor, Value::Color(color)));
}

/// `align` as a `text-align` hint (`left`, `right`, `center`, `justify`).
fn text_align_hint<E: MatchElement>(element: &E, hints: &mut Vec<Declaration>) {
    let Some(align) = element.attribute(&attr_name("align")) else {
        return;
    };
    let align = match align.trim().to_ascii_lowercase().as_str() {
        "left" => TextAlign::Left,
        "right" => TextAlign::Right,
        "center" => TextAlign::Center,
        "justify" => TextAlign::Justify,
        _ => return,
    };
    hints.push(decl(Longhand::TextAlign, Value::TextAlign(align)));
}

/// `valign` as a `vertical-align` hint.
fn vertical_align_hint<E: MatchElement>(element: &E, hints: &mut Vec<Declaration>) {
    let Some(valign) = element.attribute(&attr_name("valign")) else {
        return;
    };
    let align = match valign.trim().to_ascii_lowercase().as_str() {
        "top" => VerticalAlign::Top,
        "middle" => VerticalAlign::Middle,
        "bottom" => VerticalAlign::Bottom,
        "baseline" => VerticalAlign::Baseline,
        _ => return,
    };
    hints.push(decl(Longhand::VerticalAlign, Value::VerticalAlign(align)));
}

/// Hints for `<table>`: `width`, `height`, `border`, `cellspacing`,
/// `align` and `bgcolor`.
fn table_hints<E: MatchElement + Clone>(element: &E, hints: &mut Vec<Declaration>) {
    size_hints(element, hints);

    // `border`: that many px of border with an outset style. `outset`
    // computes to `solid` here, as it does for authored CSS.
    if let Some(px) = table_border_px(element)
        && px > 0.0
    {
        for side in [
            Longhand::BorderTopWidth,
            Longhand::BorderRightWidth,
            Longhand::BorderBottomWidth,
            Longhand::BorderLeftWidth,
        ] {
            hints.push(decl(side, Value::Length(SpecifiedLength::Px(px))));
        }
        for side in [
            Longhand::BorderTopStyle,
            Longhand::BorderRightStyle,
            Longhand::BorderBottomStyle,
            Longhand::BorderLeftStyle,
        ] {
            hints.push(decl(side, Value::BorderStyle(BorderStyle::Solid)));
        }
    }

    // `cellspacing` → `border-spacing` px.
    if let Some(px) = length_attr(element, "cellspacing")
        && let SpecifiedLength::Px(px) = px
        && px > 0.0
    {
        let spacing = Au::from_px(px);
        hints.push(decl(
            Longhand::BorderSpacing,
            Value::BorderSpacing((spacing, spacing)),
        ));
    }

    // `align`: `left` and `right` float the table, `center` centers it
    // with auto inline margins.
    if let Some(align) = element.attribute(&attr_name("align")) {
        match align.trim().to_ascii_lowercase().as_str() {
            "left" => hints.push(decl(Longhand::Float, Value::Float(Float::Left))),
            "right" => hints.push(decl(Longhand::Float, Value::Float(Float::Right))),
            "center" => {
                // `margin-inline: auto`, as this engine's two margins.
                hints.push(decl(Longhand::MarginLeft, Value::Size(None)));
                hints.push(decl(Longhand::MarginRight, Value::Size(None)));
            }
            _ => {}
        }
    }

    bgcolor_hint(element, hints);
}

/// Hints for `<td>` and `<th>`: `width`, `height`, `align`, `valign`,
/// `bgcolor`, `nowrap`, plus `cellpadding` and the 1 px inset borders a
/// bordered `<table>` gives its cells.
fn cell_hints<E: MatchElement + Clone>(element: &E, hints: &mut Vec<Declaration>) {
    size_hints(element, hints);
    text_align_hint(element, hints);
    vertical_align_hint(element, hints);

    // `nowrap`: no wrapping, unless the cell has a `width` attribute (the
    // HTML rule: a fixed width replaces it).
    if element.attribute(&attr_name("nowrap")).is_some()
        && element.attribute(&attr_name("width")).is_none()
    {
        hints.push(decl(
            Longhand::WhiteSpace,
            Value::WhiteSpace(WhiteSpace::Nowrap),
        ));
    }

    bgcolor_hint(element, hints);

    // A bordered table's cells get a 1 px inset border of their own.
    if let Some(table) = nearest_table(element)
        && let Some(px) = table_border_px(&table)
        && px > 0.0
    {
        for side in [
            Longhand::BorderTopWidth,
            Longhand::BorderRightWidth,
            Longhand::BorderBottomWidth,
            Longhand::BorderLeftWidth,
        ] {
            hints.push(decl(side, Value::Length(SpecifiedLength::Px(1.0))));
        }
        // `inset` computes to `solid` here, as it does for authored CSS.
        for side in [
            Longhand::BorderTopStyle,
            Longhand::BorderRightStyle,
            Longhand::BorderBottomStyle,
            Longhand::BorderLeftStyle,
        ] {
            hints.push(decl(side, Value::BorderStyle(BorderStyle::Solid)));
        }
    }

    // `cellpadding` from the nearest table → the cell's padding.
    if let Some(table) = nearest_table(element)
        && let Some(padding) = table
            .attribute(&attr_name("cellpadding"))
            .and_then(|value| value.trim().parse::<f32>().ok())
            .filter(|padding| *padding >= 0.0)
    {
        for side in [
            Longhand::PaddingTop,
            Longhand::PaddingRight,
            Longhand::PaddingBottom,
            Longhand::PaddingLeft,
        ] {
            hints.push(decl(side, Value::Length(SpecifiedLength::Px(padding))));
        }
    }
}

/// Hints for `<tr>`, `<thead>`, `<tbody>` and `<tfoot>`: `align`,
/// `valign` and `bgcolor`, which inherit down to the cells.
fn row_hints<E: MatchElement + Clone>(element: &E, hints: &mut Vec<Declaration>) {
    text_align_hint(element, hints);
    vertical_align_hint(element, hints);
    bgcolor_hint(element, hints);
}

/// Hints for `<col>` and `<colgroup>`: `width`.
fn col_hints<E: MatchElement + Clone>(element: &E, hints: &mut Vec<Declaration>) {
    if let Some(length) = length_attr(element, "width")
        && !matches!(length, SpecifiedLength::Px(px) if px == 0.0)
    {
        hints.push(decl(Longhand::Width, Value::Size(Some(length))));
    }
}

/// The nearest ancestor `table` element, or `None`.
fn nearest_table<E: MatchElement + Clone>(element: &E) -> Option<E> {
    let mut ancestor = element.parent_element();
    while let Some(current) = ancestor {
        if &*current.name().local == "table" {
            return Some(current);
        }
        ancestor = current.parent_element();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    use base::atoms::{Atom, LocalName, QualName};

    /// A minimal element view for testing hints.
    #[derive(Clone)]
    struct Elem {
        name: QualName,
        attrs: Vec<(QualName, &'static str)>,
        parent: Option<std::rc::Rc<Elem>>,
    }

    impl MatchElement for Elem {
        fn name(&self) -> &QualName {
            &self.name
        }

        fn id(&self) -> Option<&Atom> {
            None
        }

        fn classes(&self) -> &[Atom] {
            &[]
        }

        fn attribute(&self, name: &QualName) -> Option<&str> {
            self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| *v)
        }

        fn parent_element(&self) -> Option<Self> {
            self.parent.as_ref().map(|p| (**p).clone())
        }

        fn is_root(&self) -> bool {
            false
        }

        fn same_attributes(&self, _other: &Self) -> bool {
            false
        }

        fn same_state(&self, _other: &Self) -> bool {
            true
        }

        fn is_in_state(&self, _state: crate::PseudoState) -> bool {
            false
        }

        fn has_shadow_root(&self) -> bool {
            false
        }

        fn is_affected_by_sibling_selectors(&self) -> bool {
            false
        }

        fn prev_sibling_element(&self) -> Option<Self>
        where
            Self: Sized,
        {
            None
        }

        fn next_sibling_element(&self) -> Option<Self>
        where
            Self: Sized,
        {
            None
        }
    }

    fn elem(name: &'static str, attrs: &[(&'static str, &'static str)]) -> Elem {
        Elem {
            name: QualName::html(LocalName::from(name)),
            attrs: attrs
                .iter()
                .map(|(k, v)| (QualName::attribute(LocalName::from(*k)), *v))
                .collect(),
            parent: None,
        }
    }

    fn with_parent(
        name: &'static str,
        attrs: &[(&'static str, &'static str)],
        parent: Elem,
    ) -> Elem {
        Elem {
            name: QualName::html(LocalName::from(name)),
            attrs: attrs
                .iter()
                .map(|(k, v)| (QualName::attribute(LocalName::from(*k)), *v))
                .collect(),
            parent: Some(std::rc::Rc::new(parent)),
        }
    }

    fn names(hints: &[Declaration]) -> Vec<String> {
        hints.iter().map(|d| d.name()).collect()
    }

    #[test]
    fn table_hints_map_width_height_border_cellspacing_align_bgcolor() {
        let table = elem(
            "table",
            &[
                ("width", "300"),
                ("height", "200"),
                ("border", "1"),
                ("cellspacing", "4"),
                ("align", "center"),
                ("bgcolor", "#f0f0f0"),
            ],
        );
        let hints = presentational_hints(&table);
        let names = names(&hints);
        assert!(names.iter().any(|n| n == "width"), "width: {names:?}");
        assert!(names.iter().any(|n| n == "height"), "height: {names:?}");
        assert!(
            names.iter().any(|n| n == "border-top-width"),
            "border width: {names:?}"
        );
        assert!(
            names.iter().any(|n| n == "border-spacing"),
            "border-spacing: {names:?}"
        );
        assert!(
            names.iter().any(|n| n == "margin-left"),
            "center align gives auto margins: {names:?}"
        );
        assert!(
            names.iter().any(|n| n == "background-color"),
            "bgcolor: {names:?}"
        );
    }

    #[test]
    fn cell_hints_map_width_height_align_valign_bgcolor_nowrap_cellpadding() {
        let table = elem("table", &[("border", "1"), ("cellpadding", "8")]);
        let cell = with_parent(
            "td",
            &[
                ("width", "100"),
                ("align", "center"),
                ("valign", "middle"),
                ("bgcolor", "red"),
                ("nowrap", ""),
            ],
            table,
        );
        let hints = presentational_hints(&cell);
        let names = names(&hints);
        assert!(names.iter().any(|n| n == "width"), "width: {names:?}");
        assert!(
            names.iter().any(|n| n == "text-align"),
            "text-align: {names:?}"
        );
        assert!(
            names.iter().any(|n| n == "vertical-align"),
            "vertical-align: {names:?}"
        );
        assert!(
            names.iter().any(|n| n == "background-color"),
            "background-color: {names:?}"
        );
        assert!(
            names.iter().any(|n| n == "padding-top"),
            "cellpadding: {names:?}"
        );
        assert!(
            names.iter().any(|n| n == "border-top-width"),
            "border: {names:?}"
        );
    }

    #[test]
    fn nowrap_is_dropped_when_the_cell_has_a_width_attribute() {
        let table = elem("table", &[]);
        let cell = with_parent("td", &[("nowrap", ""), ("width", "50")], table);
        let hints = presentational_hints(&cell);
        let names = names(&hints);
        assert!(
            !names.iter().any(|n| n == "white-space"),
            "nowrap dropped: {names:?}"
        );
    }

    #[test]
    fn nowrap_applies_when_the_cell_has_no_width_attribute() {
        let table = elem("table", &[]);
        let cell = with_parent("td", &[("nowrap", "")], table);
        let hints = presentational_hints(&cell);
        let names = names(&hints);
        assert!(
            names.iter().any(|n| n == "white-space"),
            "nowrap present: {names:?}"
        );
    }

    #[test]
    fn non_table_elements_produce_no_hints() {
        let div = elem("div", &[("align", "center")]);
        assert!(presentational_hints(&div).is_empty());
    }
}
