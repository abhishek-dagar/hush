//! Every format decodes the same small picture (spec 0004, AC-29): a 4 by
//! 2 image, red on the left half and blue on the right. The fixtures were
//! made with ImageMagick and cwebp.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

use image::{Decoded, ImageError, Limits, decode, decode_icon};

const RED: [u8; 4] = [255, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const CLEAR: [u8; 4] = [0, 0, 0, 0];
/// Per channel, for the lossy formats.
const LOSSY: i32 = 12;

fn load(name: &str) -> Decoded {
    let path = format!("{}/tests/images/{name}", env!("CARGO_MANIFEST_DIR"));
    let bytes = std::fs::read(&path).unwrap();
    decode(&bytes, &Limits::default()).unwrap_or_else(|err| panic!("{name}: {err}"))
}

fn pixel(image: &Decoded, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * image.width + x) * 4) as usize;
    image.pixels[i..i + 4].try_into().unwrap()
}

fn near(actual: [u8; 4], expected: [u8; 4], tolerance: i32) -> bool {
    actual
        .iter()
        .zip(expected)
        .all(|(a, e)| (i32::from(*a) - i32::from(e)).abs() <= tolerance)
}

fn red_then_blue(name: &str, tolerance: i32) {
    let image = load(name);
    assert_eq!((image.width, image.height), (4, 2), "{name}");
    assert_eq!(image.pixels.len(), 4 * 2 * 4, "{name}");
    for y in 0..2 {
        assert!(near(pixel(&image, 0, y), RED, tolerance), "{name} left");
        assert!(near(pixel(&image, 3, y), BLUE, tolerance), "{name} right");
    }
}

#[test]
fn png_jpeg_and_webp_decode_to_rgba() {
    red_then_blue("two.png", 0);
    red_then_blue("two.webp", 0);
    red_then_blue("two.jpg", LOSSY);
    red_then_blue("two-lossy.webp", LOSSY);
}

#[test]
fn an_animated_webp_shows_its_first_frame() {
    let image = load("anim.webp");
    assert_eq!((image.width, image.height), (4, 2));
    assert!(image.pixels.chunks_exact(4).all(|p| p == RED));
}

#[test]
fn a_gif_frame_lands_on_its_logical_screen() {
    // The 4 by 2 frame sits at (1, 1) on a 6 by 3 screen.
    let image = load("two.gif");
    assert_eq!((image.width, image.height), (6, 3));
    assert_eq!(pixel(&image, 0, 0), CLEAR);
    assert_eq!(pixel(&image, 1, 1), RED);
    assert_eq!(pixel(&image, 4, 2), BLUE);
    assert_eq!(pixel(&image, 5, 2), CLEAR);
}

#[test]
fn alpha_comes_out_premultiplied() {
    let image = load("half.png");
    assert!(near(pixel(&image, 0, 0), [128, 0, 0, 128], 1));
}

#[test]
fn limits_and_broken_files_fail_cleanly() {
    let path = format!("{}/tests/images/two.png", env!("CARGO_MANIFEST_DIR"));
    let bytes = std::fs::read(path).unwrap();
    let tight = Limits {
        max_side: 3,
        max_pixels: 100,
    };
    assert_eq!(
        decode(&bytes, &tight),
        Err(ImageError::TooLarge {
            width: 4,
            height: 2
        })
    );
    for name in ["two.png", "two.jpg", "two.gif", "two.webp"] {
        let path = format!("{}/tests/images/{name}", env!("CARGO_MANIFEST_DIR"));
        let bytes = std::fs::read(path).unwrap();
        // Every prefix fails or decodes; none panics.
        for end in 0..bytes.len() {
            let _ = decode(&bytes[..end], &Limits::default());
        }
    }
}

/// An ICO file around `entries`, each `(listed size, data)`.
fn ico(entries: &[(u8, &[u8])]) -> Vec<u8> {
    let mut out = vec![0, 0, 1, 0];
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * entries.len();
    for (size, data) in entries {
        out.extend_from_slice(&[*size, *size, 0, 0, 1, 0, 32, 0]);
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += data.len();
    }
    for (_, data) in entries {
        out.extend_from_slice(data);
    }
    out
}

#[test]
fn favicons_come_straight_alpha_from_ico_png_entries_or_plain_images() {
    // AC-37
    let dir = format!("{}/tests/images", env!("CARGO_MANIFEST_DIR"));
    let two = std::fs::read(format!("{dir}/two.png")).unwrap();
    let half = std::fs::read(format!("{dir}/half.png")).unwrap();
    // The 32 px entry wins over the 16 px one, whatever the order.
    let file = ico(&[(16, &half), (32, &two)]);
    let icon = decode_icon(&file, &Limits::default()).unwrap();
    assert_eq!((icon.width, icon.height), (4, 2));
    assert_eq!(&icon.rgba[..4], RED);
    // A plain PNG works too, and its alpha comes back straight.
    let plain = decode_icon(&half, &Limits::default()).unwrap();
    let premultiplied = decode(&half, &Limits::default()).unwrap();
    assert_eq!(plain.rgba[3], premultiplied.pixels[3]);
    assert!(plain.rgba[..3] >= premultiplied.pixels[..3]);
    assert!(decode_icon(b"<html>", &Limits::default()).is_err());
}
