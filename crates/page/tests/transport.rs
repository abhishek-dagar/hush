//! AC-11: a display list encoded into shared memory, sent through `ipc` and
//! decoded on the other side equals the original.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

use std::thread;
use std::time::Duration;

use display_list::{
    BlendMode, BorderStyle, ClipNode, ColorF, DisplayItem, DisplayList, Epoch, FontKey,
    GlyphInstance, ImageKey, ItemKind, PageId, PointF, RectF, ScrollId, ScrollOffset, SizeF,
    SpatialId, SpatialKind,
};
use ipc::{BootstrapServer, BrowserChannel, NavigationId, ToBrowser};

const TIMEOUT: Duration = Duration::from_secs(10);

fn rect(f: f32) -> RectF {
    RectF {
        origin: PointF {
            x: f * 0.5,
            y: f * 1.25,
        },
        size: SizeF {
            width: 10.0 + f,
            height: 4.0,
        },
    }
}

/// A list with a scroll frame, a reference frame, two clips, and `items`
/// items cycling through every item kind.
fn list_with(items: usize) -> DisplayList {
    let mut list = DisplayList::new(
        PageId(3),
        Epoch(9),
        SizeF {
            width: 1280.0,
            height: 720.0,
        },
    );
    let scrolled = list.push_spatial(
        SpatialId::ROOT,
        SpatialKind::ScrollFrame {
            scroll_id: ScrollId::new(0, 12, 3),
            frame_rect: rect(1.0),
            content_size: SizeF {
                width: 10.0,
                height: 900.0,
            },
            offset: ScrollOffset {
                x: 0.0,
                y: 12.5,
                generation: 2,
            },
        },
    );
    let mut transform = [0.0; 16];
    (transform[0], transform[5], transform[10], transform[15]) = (1.0, 1.0, 1.0, 1.0);
    transform[12] = 40.0;
    let moved = list.push_spatial(scrolled, SpatialKind::ReferenceFrame { transform });
    let outer = list.push_clip(ClipNode {
        parent: None,
        spatial: SpatialId::ROOT,
        rect: rect(2.0),
        radii: None,
    });
    let inner = list.push_clip(ClipNode {
        parent: Some(outer),
        spatial: scrolled,
        rect: rect(3.0),
        radii: Some([2.0; 8]),
    });
    list.items = (0..items)
        .map(|i| {
            let f = i as f32;
            let color = ColorF {
                r: 0.1,
                g: (i % 255) as f32 / 255.0,
                b: 0.9,
                a: 1.0,
            };
            let kind = match i % 6 {
                0 => ItemKind::Rect {
                    rect: rect(f),
                    color,
                    radii: None,
                },
                1 => ItemKind::Border {
                    rect: rect(f),
                    widths: [1.0, 2.0, 3.0, 4.0],
                    colors: [color; 4],
                    styles: [
                        BorderStyle::Solid,
                        BorderStyle::Dashed,
                        BorderStyle::Dotted,
                        BorderStyle::Double,
                    ],
                    radii: Some([f; 8]),
                },
                2 => ItemKind::Text {
                    font: FontKey(1),
                    size: 16.0,
                    color,
                    glyphs: vec![GlyphInstance {
                        index: i as u32,
                        point: PointF { x: f, y: 0.0 },
                    }],
                },
                3 => ItemKind::Image {
                    key: ImageKey(2),
                    rect: rect(f),
                },
                4 => ItemKind::PushStackingContext {
                    opacity: 0.5,
                    blend_mode: BlendMode::Normal,
                },
                _ => ItemKind::PopStackingContext,
            };
            let (spatial, clip) = match i % 3 {
                0 => (SpatialId::ROOT, None),
                1 => (scrolled, Some(inner)),
                _ => (moved, Some(outer)),
            };
            DisplayItem {
                spatial,
                clip,
                kind,
            }
        })
        .collect();
    list
}

/// Sends each list from a "content" thread and decodes it on the "browser" side.
fn send_and_decode(lists: Vec<DisplayList>) -> Vec<DisplayList> {
    let server = BootstrapServer::new().unwrap();
    let name = server.name().to_owned();
    let count = lists.len();
    let content = thread::spawn(move || {
        let channel = BrowserChannel::connect(&name).unwrap();
        for list in lists {
            let data = page::encode(&list).unwrap();
            channel
                .send(ToBrowser::DisplayList {
                    page: list.page,
                    navigation: NavigationId(1),
                    epoch: list.epoch,
                    data,
                })
                .unwrap();
        }
    });

    let channel = server.accept(TIMEOUT).unwrap();
    let decoded = (0..count)
        .map(|_| match channel.recv_timeout(TIMEOUT).unwrap() {
            ToBrowser::DisplayList { data, .. } => postcard::from_bytes(&data).unwrap(),
            other => panic!("expected a display list, got {other:?}"),
        })
        .collect();
    content.join().unwrap();
    decoded
}

#[test]
fn lists_survive_the_trip_through_shared_memory() {
    let lists = vec![list_with(0), list_with(50_000)];
    assert_eq!(send_and_decode(lists.clone()), lists);
}
