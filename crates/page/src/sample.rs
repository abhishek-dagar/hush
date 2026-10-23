//! A small hand built document with directly assigned styles. It stands in
//! for real pages until the HTML parser and the cascade arrive (feature 5).
//! It has a bordered scroll box and a translucent footer, so scroll frames,
//! clips, borders and stacking contexts all reach the window.

use std::rc::Rc;

use base::atoms::{LocalName, QualName};
use base::local_name;
use css::{
    Background, Border, BorderSide, BorderStyle, Color, ComputedStyle, Display, Length, Margin,
    Overflow, Padding,
};
use dom::{Document, NodeId};

/// Header, a row of colored cards stacked as blocks, a scroll box of rows,
/// and a footer. Some margins are percentages, so resizing the window
/// visibly relays it out.
pub fn document() -> Document {
    let mut doc = Document::new("about:sample");
    let root = doc.root();
    let html = add(&mut doc, root, local_name!("html"), Style::default());
    let body = add(
        &mut doc,
        html,
        local_name!("body"),
        Style {
            background: Color::rgb8(0xf4, 0xf1, 0xea),
            padding: Length::px(24.0),
            ..Style::default()
        },
    );
    add(
        &mut doc,
        body,
        local_name!("header"),
        Style {
            background: Color::rgb8(0x1f, 0x3a, 0x5f),
            height: Some(Length::px(64.0)),
            ..Style::default()
        },
    );

    let main = add(
        &mut doc,
        body,
        local_name!("main"),
        Style {
            background: Color::rgb8(0xff, 0xff, 0xff),
            margin: Margin::all(Length::px(16.0)),
            padding: Length::px(16.0),
            ..Style::default()
        },
    );
    let cards = [
        (Color::rgb8(0xe0, 0x6c, 0x4f), Length::ZERO),
        (Color::rgb8(0xf2, 0xb1, 0x34), Length::Percent(10.0)),
        (Color::rgb8(0x3f, 0x8f, 0x6b), Length::Percent(20.0)),
    ];
    for (color, indent) in cards {
        add(
            &mut doc,
            main,
            local_name!("section"),
            Style {
                background: color,
                margin: Margin {
                    left: Some(indent),
                    ..Margin::all(Length::px(8.0))
                },
                height: Some(Length::px(72.0)),
                ..Style::default()
            },
        );
    }

    // A scroll box: rows taller than the box, so the wheel scrolls it.
    let scroller = add(
        &mut doc,
        main,
        local_name!("aside"),
        Style {
            background: Color::rgb8(0xfa, 0xf8, 0xf4),
            margin: Margin::all(Length::px(8.0)),
            padding: Length::px(4.0),
            height: Some(Length::px(120.0)),
            border: Some((2.0, Color::rgb8(0x1f, 0x3a, 0x5f))),
            scrolls: true,
            ..Style::default()
        },
    );
    for row in 0..6u8 {
        let shade = 0xc8 - row * 0x14;
        add(
            &mut doc,
            scroller,
            local_name!("div"),
            Style {
                background: Color::rgb8(shade, shade, 0xe8),
                margin: Margin::all(Length::px(4.0)),
                height: Some(Length::px(40.0)),
                ..Style::default()
            },
        );
    }

    add(
        &mut doc,
        body,
        local_name!("footer"),
        Style {
            background: Color::rgb8(0x6b, 0x6b, 0x6b),
            height: Some(Length::px(32.0)),
            opacity: 0.8,
            ..Style::default()
        },
    );
    doc
}

struct Style {
    background: Color,
    margin: Margin,
    padding: Length,
    height: Option<Length>,
    /// A solid border of this width (px) and color on every side.
    border: Option<(f32, Color)>,
    /// `overflow-y: auto`.
    scrolls: bool,
    opacity: f32,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            background: Color::TRANSPARENT,
            margin: Margin::default(),
            padding: Length::ZERO,
            height: None,
            border: None,
            scrolls: false,
            opacity: 1.0,
        }
    }
}

fn add(doc: &mut Document, parent: NodeId, name: LocalName, style: Style) -> NodeId {
    let id = doc.create_element(QualName::html(name));
    let mut computed = ComputedStyle::default();
    Rc::make_mut(&mut computed.display_box).display = Display::Block;
    computed.background = Rc::new(Background {
        color: style.background,
        image: None,
    });
    computed.margin = Rc::new(style.margin);
    computed.padding = Rc::new(Padding::all(style.padding));
    Rc::make_mut(&mut computed.size).height = style.height;
    if style.scrolls {
        Rc::make_mut(&mut computed.display_box).overflow_y = Overflow::Auto;
    }
    if let Some((width, color)) = style.border {
        let side = BorderSide {
            width: base::Au::from_px(width),
            style: BorderStyle::Solid,
            color,
        };
        computed.border = Rc::new(Border {
            top: side,
            right: side,
            bottom: side,
            left: side,
            radius: Default::default(),
        });
    }
    // Only touch the group when it changes, so the others keep sharing it.
    if style.opacity < 1.0 {
        Rc::make_mut(&mut computed.effects).opacity = style.opacity;
    }
    // `parent` and `id` both come from this document and an element under
    // an element is always valid, so these cannot fail.
    let _ = doc.append_child(parent, id);
    let _ = doc.set_style(id, Some(Rc::new(computed)));
    id
}
