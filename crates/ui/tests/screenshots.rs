//! Reference screenshots of the control gallery (spec 0003, AC-14): each
//! of its four sections (light and dark, normal and high contrast) at 100%
//! and 200% in a 1280 by 800 window, drawn offscreen by the real renderer
//! and compared with `tests/reference/<os>/<theme>-<contrast>-<zoom>.png`.
//! A render fails when more than 0.5% of its pixels differ by more than 8
//! of 255 in any channel. Fonts differ per OS, so each OS keeps its own
//! references; make or refresh them with `UPDATE_REFERENCES=1`. A machine
//! without a GPU adapter skips the test with a log line.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

use std::fs::{self, File};
use std::io::BufWriter;
use std::path::PathBuf;

use ipc::ToBrowser;
use render::{Chrome, Renderer};
use ui::{ChromeDocument, ChromeFrame, ChromeSurface, Os, SizeF};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 800;
/// A channel may drift this much before the pixel counts as changed.
const CHANNEL_SLACK: u8 = 8;
/// The share of changed pixels a render may have.
const MAX_CHANGED: f64 = 0.005;

fn os_dir() -> &'static str {
    match Os::current() {
        Os::Mac => "macos",
        Os::Windows => "windows",
        Os::Linux => "linux",
    }
}

fn reference(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/reference")
        .join(os_dir())
        .join(format!("{name}.png"))
}

fn apply(renderer: &mut Renderer, messages: Vec<ToBrowser>) {
    for message in messages {
        match message {
            ToBrowser::AddFont {
                page,
                key,
                data,
                index,
                variations,
            } => renderer.add_font(page, key, &data, index, &variations),
            ToBrowser::AddImage {
                page,
                key,
                size,
                format,
                pixels,
            } => renderer.add_image(page, key, size, format, &pixels),
            ToBrowser::DeleteFont { page, key } => renderer.delete_font(page, key),
            ToBrowser::DeleteImage { page, key } => renderer.delete_image(page, key),
            _ => {}
        }
    }
}

/// The gallery's `section` scrolled to the top of the window at `zoom`,
/// as RGBA rows. `None` without a GPU.
fn shoot(section: &str, zoom: f32) -> Option<Vec<u8>> {
    let mut renderer = match Renderer::new_headless(WIDTH, HEIGHT) {
        Ok(renderer) => renderer,
        Err(err) => {
            eprintln!("no GPU adapter, screenshot test skipped: {err}");
            return None;
        }
    };
    let viewport = SizeF {
        width: WIDTH as f32 / zoom,
        height: HEIGHT as f32 / zoom,
    };
    let mut doc =
        ChromeDocument::load(ChromeSurface::Gallery, Os::current(), viewport, zoom, None).unwrap();
    let first = doc.frame().expect("the gallery draws");
    let top = doc
        .element_box(section)
        .unwrap_or_else(|| panic!("no #{section} in the gallery"))
        .origin
        .y;
    doc.scroll_by(0.0, top);
    let mut frames = vec![first];
    frames.extend(doc.frame());
    let mut last: Option<ChromeFrame> = None;
    for frame in frames {
        apply(&mut renderer, frame.adds.clone());
        apply(&mut renderer, frame.deletes.clone());
        last = Some(frame);
    }
    let last = last.unwrap();
    let chrome = Chrome {
        list: &last.list,
        scale: zoom,
        slot: None,
    };
    renderer.render(None, 1.0, Some(&chrome), None).unwrap();
    Some(renderer.read_pixels().unwrap())
}

fn save(path: &PathBuf, pixels: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut encoder = png::Encoder::new(BufWriter::new(File::create(path).unwrap()), WIDTH, HEIGHT);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(pixels)
        .unwrap();
}

fn load(path: &PathBuf) -> Option<Vec<u8>> {
    let decoder = png::Decoder::new(std::io::BufReader::new(File::open(path).ok()?));
    let mut reader = decoder.read_info().ok()?;
    let mut pixels = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut pixels).ok()?;
    let ok =
        (info.width, info.height) == (WIDTH, HEIGHT) && info.color_type == png::ColorType::Rgba;
    ok.then(|| {
        pixels.truncate(info.buffer_size());
        pixels
    })
}

/// The share of pixels that drifted past the slack.
fn changed(a: &[u8], b: &[u8]) -> f64 {
    let differ = a
        .chunks(4)
        .zip(b.chunks(4))
        .filter(|(p, q)| {
            p.iter()
                .zip(*q)
                .any(|(x, y)| x.abs_diff(*y) > CHANNEL_SLACK)
        })
        .count();
    differ as f64 / f64::from(WIDTH * HEIGHT)
}

#[test]
fn the_gallery_matches_its_reference_screenshots() {
    // AC-14: light and dark, normal and high contrast, 100% and 200%.
    let update = std::env::var_os("UPDATE_REFERENCES").is_some();
    let mut failures = Vec::new();
    for theme in ["light", "dark"] {
        for contrast in ["normal", "more"] {
            for zoom in [1.0, 2.0] {
                let section = format!("mode-{theme}-{contrast}");
                let Some(pixels) = shoot(&section, zoom) else {
                    return;
                };
                let name = format!("{theme}-{contrast}-{}", (zoom * 100.0) as u32);
                let path = reference(&name);
                if update {
                    save(&path, &pixels);
                    continue;
                }
                match load(&path) {
                    None => failures.push(format!(
                        "{name}: no reference at {}; run with UPDATE_REFERENCES=1",
                        path.display()
                    )),
                    Some(expected) => {
                        let share = changed(&pixels, &expected);
                        if share > MAX_CHANGED {
                            let actual = path.with_extension("actual.png");
                            save(&actual, &pixels);
                            failures.push(format!(
                                "{name}: {:.2}% of pixels changed, see {}",
                                share * 100.0,
                                actual.display()
                            ));
                        }
                    }
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
