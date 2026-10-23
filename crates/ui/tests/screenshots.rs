//! Reference screenshots of the chrome, drawn offscreen by the real
//! renderer in a 1280 by 800 window and compared with
//! `tests/reference/<os>/<name>.png`.
//!
//! - The control gallery (spec 0003, AC-14): each of its four sections
//!   (light and dark, normal and high contrast) at 100% and 200%.
//! - The browser frame (spec 0008, AC-36): the sidebar navigation row with
//!   the loading bar and the status bubble, and the open palette with its
//!   intent row, in light and dark, normal and high contrast.
//!
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

use ipc::{InputEvent, ToBrowser};
use render::{Chrome, PageSlot, Renderer};
use ui::{
    ChromeDocument, ChromeFrame, ChromeSurface, NavView, Os, OsAppearance, RectF, Settings, SizeF,
};
use url::Url;

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

/// An offscreen renderer that keeps up with one chrome document's frames,
/// the way the browser's render thread does.
struct Camera {
    renderer: Renderer,
    last: Option<ChromeFrame>,
}

impl Camera {
    /// `None` without a GPU.
    fn new() -> Option<Self> {
        match Renderer::new_headless(WIDTH, HEIGHT) {
            Ok(renderer) => Some(Self {
                renderer,
                last: None,
            }),
            Err(err) => {
                eprintln!("no GPU adapter, screenshot test skipped: {err}");
                None
            }
        }
    }

    /// Takes the document's next frame, if anything changed: its fonts and
    /// images go to the renderer, and it becomes the one drawn.
    fn take(&mut self, doc: &mut ChromeDocument) {
        if let Some(frame) = doc.frame() {
            apply(&mut self.renderer, frame.adds.clone());
            apply(&mut self.renderer, frame.deletes.clone());
            self.last = Some(frame);
        }
    }

    /// The last frame at `zoom`, with an empty page card where the
    /// document has one, as RGBA rows.
    fn shoot(&mut self, doc: &mut ChromeDocument, zoom: f32) -> Vec<u8> {
        self.take(doc);
        let last = self.last.as_ref().expect("the document drew");
        let layout = doc.layout();
        let slot = layout.page.map(|page| PageSlot {
            rect: scaled(page, zoom),
            radius: layout.page_radius * zoom,
        });
        let chrome = Chrome {
            list: &last.list,
            scale: zoom,
            slot,
            overlay: last.overlay.as_ref(),
        };
        self.renderer
            .render(None, 1.0, Some(&chrome), None)
            .unwrap();
        self.renderer.read_pixels().unwrap()
    }
}

/// `rect` in chrome px, as the renderer's physical pixels.
fn scaled(rect: RectF, scale: f32) -> display_list::RectF {
    display_list::RectF {
        origin: display_list::PointF {
            x: rect.origin.x * scale,
            y: rect.origin.y * scale,
        },
        size: display_list::SizeF {
            width: rect.size.width * scale,
            height: rect.size.height * scale,
        },
    }
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

/// Compares `pixels` with reference `name`, or saves them as the new one
/// with `UPDATE_REFERENCES=1`. A mismatch is added to `failures`, with
/// the render beside the reference as `<name>.actual.png`.
fn check(name: &str, pixels: &[u8], failures: &mut Vec<String>) {
    let path = reference(name);
    if std::env::var_os("UPDATE_REFERENCES").is_some() {
        save(&path, pixels);
        return;
    }
    match load(&path) {
        None => failures.push(format!(
            "{name}: no reference at {}; run with UPDATE_REFERENCES=1",
            path.display()
        )),
        Some(expected) => {
            let share = changed(pixels, &expected);
            if share > MAX_CHANGED {
                let actual = path.with_extension("actual.png");
                save(&actual, pixels);
                failures.push(format!(
                    "{name}: {:.2}% of pixels changed, see {}",
                    share * 100.0,
                    actual.display()
                ));
            }
        }
    }
}

#[test]
fn the_gallery_matches_its_reference_screenshots() {
    // AC-14: light and dark, normal and high contrast, 100% and 200%.
    let mut failures = Vec::new();
    for theme in ["light", "dark"] {
        for contrast in ["normal", "more"] {
            for zoom in [1.0, 2.0] {
                let Some(mut camera) = Camera::new() else {
                    return;
                };
                let viewport = SizeF {
                    width: WIDTH as f32 / zoom,
                    height: HEIGHT as f32 / zoom,
                };
                let mut doc = ChromeDocument::load(
                    ChromeSurface::Gallery,
                    Os::current(),
                    viewport,
                    zoom,
                    None,
                )
                .unwrap();
                camera.take(&mut doc);
                let section = format!("mode-{theme}-{contrast}");
                let top = doc
                    .element_box(&section)
                    .unwrap_or_else(|| panic!("no #{section} in the gallery"))
                    .origin
                    .y;
                doc.scroll_by(0.0, top);
                let pixels = camera.shoot(&mut doc, zoom);
                let name = format!("{theme}-{contrast}-{}", (zoom * 100.0) as u32);
                check(&name, &pixels, &mut failures);
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The browser frame at 100% in the theme the OS asks for: dark or not,
/// high contrast or not, the default accent and address style.
fn frame(dark: bool, high_contrast: bool) -> ChromeDocument {
    let os = Os::current();
    let viewport = SizeF {
        width: WIDTH as f32,
        height: HEIGHT as f32,
    };
    let mut doc = ChromeDocument::load(ChromeSurface::Frame, os, viewport, 1.0, None).unwrap();
    let mut appearance = OsAppearance::fallback(os);
    appearance.dark = dark;
    appearance.high_contrast = high_contrast;
    doc.set_theme(&ui::resolve(&Settings::default(), &appearance));
    doc.set_caption(&appearance.button_layout);
    doc
}

fn docs_page() -> Url {
    Url::parse("https://www.example.com/docs").unwrap()
}

#[test]
fn the_frame_matches_its_reference_screenshots() {
    // Spec 0008, AC-36: the sidebar navigation row, the loading bar and the
    // status bubble; then the palette and its intent row. Light and dark,
    // normal and high contrast.
    let mut failures = Vec::new();
    for (theme, dark) in [("light", false), ("dark", true)] {
        for (contrast, high) in [("normal", false), ("more", true)] {
            let Some(mut camera) = Camera::new() else {
                return;
            };
            // Back to go to, nothing ahead, a load running, and a link
            // under the pointer. The shimmer is held at one place.
            let mut doc = frame(dark, high);
            doc.set_navigation(&NavView {
                can_back: true,
                can_forward: false,
                url: Some(docs_page()),
                pending: None,
                loading: true,
            });
            doc.tick_loading(300.0);
            doc.set_link_hover(Some("https://www.example.com/docs/getting-started"));
            let pixels = camera.shoot(&mut doc, 1.0);
            check(
                &format!("frame-browsing-{theme}-{contrast}"),
                &pixels,
                &mut failures,
            );

            // The palette over the card, a search typed into it.
            let Some(mut camera) = Camera::new() else {
                return;
            };
            let mut doc = frame(dark, high);
            doc.set_navigation(&NavView {
                can_back: true,
                url: Some(docs_page()),
                ..NavView::default()
            });
            camera.take(&mut doc);
            doc.open_address();
            camera.take(&mut doc);
            doc.input(InputEvent::Text {
                text: "rust borrow checker".into(),
            });
            assert!(doc.palette_open());
            let pixels = camera.shoot(&mut doc, 1.0);
            check(
                &format!("frame-palette-{theme}-{contrast}"),
                &pixels,
                &mut failures,
            );
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
