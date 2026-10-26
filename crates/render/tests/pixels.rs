//! The headless pixel test (spec 0004, child 0005, AC-27): a fixed display
//! list drawn offscreen at scale 1 and 2, checked at chosen pixels. A
//! machine without a GPU adapter skips it with a log line.

use display_list::{
    BlendMode, BorderStyle, ColorF, DisplayItem, DisplayList, Epoch, FontKey, GlyphInstance,
    ImageFormat, ImageKey, ItemKind, PageId, PointF, RectF, SizeF, SpatialId,
};
use render::Renderer;
use skrifa::MetadataProvider;

const PAGE: PageId = PageId(1);
const FONT: &[u8] = include_bytes!("fonts/Cantarell-Regular.ttf");
/// Per channel, out of 255.
const TOLERANCE: i32 = 2;

const WHITE: [u8; 4] = [255, 255, 255, 255];
const RED: ColorF = ColorF {
    r: 1.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};
const BLUE: ColorF = ColorF {
    r: 0.0,
    g: 0.0,
    b: 1.0,
    a: 1.0,
};
const GREEN: ColorF = ColorF {
    r: 0.0,
    g: 1.0,
    b: 0.0,
    a: 1.0,
};
const BLACK: ColorF = ColorF {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};

fn rect(x: f32, y: f32, width: f32, height: f32) -> RectF {
    RectF {
        origin: PointF { x, y },
        size: SizeF { width, height },
    }
}

fn item(kind: ItemKind) -> DisplayItem {
    DisplayItem {
        spatial: SpatialId::ROOT,
        clip: None,
        kind,
    }
}

/// One of each item kind, in CSS px on a 120 by 80 page.
fn fixture() -> DisplayList {
    let glyph = skrifa::FontRef::new(FONT)
        .ok()
        .and_then(|font| font.charmap().map('H'))
        .map_or(0, |id| id.to_u32());
    let mut list = DisplayList::new(PAGE, Epoch(1), SizeF::default());
    list.items = [
        ItemKind::Rect {
            rect: rect(10.0, 10.0, 20.0, 20.0),
            color: RED,
            radii: None,
        },
        // A rounded ring: 4 px blue, corners of radius 10.
        ItemKind::Border {
            rect: rect(40.0, 10.0, 30.0, 30.0),
            widths: [4.0; 4],
            colors: [BLUE; 4],
            styles: [BorderStyle::Solid; 4],
            radii: Some([10.0; 8]),
        },
        // A dashed green top: 6 px dashes, gaps stretched to 8.67 px.
        ItemKind::Border {
            rect: rect(10.0, 40.0, 50.0, 10.0),
            widths: [2.0, 0.0, 0.0, 0.0],
            colors: [GREEN; 4],
            styles: [BorderStyle::Dashed; 4],
            radii: None,
        },
        ItemKind::Text {
            font: FontKey(1),
            size: 20.0,
            color: BLACK,
            glyphs: vec![GlyphInstance {
                index: glyph,
                point: PointF { x: 72.0, y: 74.0 },
            }],
        },
        ItemKind::Image {
            key: ImageKey(1),
            rect: rect(90.0, 10.0, 20.0, 20.0),
        },
        // Two overlapping reds at half opacity, as one group: where they
        // overlap is still half red, not three quarters.
        ItemKind::PushStackingContext {
            opacity: 0.5,
            blend_mode: BlendMode::Normal,
        },
        ItemKind::Rect {
            rect: rect(10.0, 60.0, 20.0, 10.0),
            color: RED,
            radii: None,
        },
        ItemKind::Rect {
            rect: rect(20.0, 60.0, 20.0, 10.0),
            color: RED,
            radii: None,
        },
        ItemKind::PopStackingContext,
    ]
    .into_iter()
    .map(item)
    .collect();
    list
}

/// A 2 by 2 image: red, green / blue, white, premultiplied RGBA.
const IMAGE: [u8; 16] = [
    255, 0, 0, 255, 0, 255, 0, 255, //
    0, 0, 255, 255, 255, 255, 255, 255,
];

fn draw(scale: f32) -> Option<(Vec<u8>, u32)> {
    let width = (120.0 * scale) as u32;
    let mut renderer = match Renderer::new_headless(width, (80.0 * scale) as u32) {
        Ok(renderer) => renderer,
        Err(err) => {
            eprintln!("no GPU adapter, pixel test skipped: {err}");
            return None;
        }
    };
    renderer.add_font(
        PAGE,
        FontKey(1),
        render::FontBytes::heap(FONT.to_vec()),
        0,
        &[],
    );
    renderer.add_image(PAGE, ImageKey(1), (2, 2), ImageFormat::Rgba8, &IMAGE);
    renderer
        .render(Some(&fixture()), scale, None, None)
        .unwrap_or_else(|err| panic!("render failed: {err}"));
    let pixels = renderer
        .read_pixels()
        .unwrap_or_else(|err| panic!("readback failed: {err}"));
    assert_eq!(pixels.len(), (width * (80.0 * scale) as u32 * 4) as usize);
    Some((pixels, width))
}

/// The pixel at CSS px (`x`, `y`) at `scale`.
fn at(pixels: &[u8], width: u32, scale: f32, x: f32, y: f32) -> [u8; 4] {
    let (x, y) = ((x * scale) as usize, (y * scale) as usize);
    let i = (y * width as usize + x) * 4;
    [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
}

fn close(actual: [u8; 4], expected: [u8; 4]) -> bool {
    actual
        .iter()
        .zip(expected)
        .all(|(a, e)| (i32::from(*a) - i32::from(e)).abs() <= TOLERANCE)
}

#[test]
fn every_item_kind_draws_at_both_scales() {
    for scale in [1.0, 2.0] {
        let Some((pixels, width)) = draw(scale) else {
            return;
        };
        let check = |x: f32, y: f32, expected: [u8; 4], what: &str| {
            let actual = at(&pixels, width, scale, x, y);
            assert!(
                close(actual, expected),
                "{what} at ({x}, {y}), scale {scale}: got {actual:?}, want {expected:?}"
            );
        };

        check(5.0, 5.0, WHITE, "the blank page");
        check(15.0, 15.0, [255, 0, 0, 255], "the rect");

        check(55.0, 11.0, [0, 0, 255, 255], "the ring's top edge");
        check(41.0, 11.0, WHITE, "outside the rounded corner");
        check(55.0, 25.0, WHITE, "inside the ring");

        check(12.0, 40.5, [0, 255, 0, 255], "the first dash");
        check(19.0, 40.5, WHITE, "the gap after it");
        check(58.0, 40.5, [0, 255, 0, 255], "the last dash");

        check(90.5, 10.5, [255, 0, 0, 255], "the image's top left texel");
        check(109.5, 29.5, WHITE, "the image's bottom right texel");

        check(15.0, 65.0, [255, 128, 128, 255], "one red at half opacity");
        check(25.0, 65.0, [255, 128, 128, 255], "the overlap, still half");

        // The glyph: some ink inside its box, none above the cap height.
        let mut darkest = 255;
        for y in 60..74 {
            for x in 72..86 {
                darkest = darkest.min(at(&pixels, width, scale, x as f32, y as f32)[0]);
            }
        }
        assert!(darkest < 64, "the glyph inked, scale {scale}: {darkest}");
        check(78.0, 50.0, WHITE, "above the glyph");
    }
}
