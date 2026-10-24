//! The scrolling rules (spec 0004, child 0005, Scrolling, AC-30): which
//! keys scroll the root viewport and how far, and where the overlay
//! indicator sits and how it fades. Plain data in, data out: the main
//! thread maps keys, the render thread applies the rest.

use std::time::{Duration, Instant};

use display_list::{ColorF, DisplayList, PointF, RectF, ScrollId, ScrollOffset, SizeF, SpatialId};
use ipc::KeyScroll;
use render::ScrollIndicator;

/// CSS px one arrow key or one wheel line scrolls.
pub(crate) const LINE: f32 = 40.0;
/// What a page step keeps in view: it moves the viewport's height minus
/// this.
pub(crate) const PAGE_OVERLAP: f32 = 40.0;
/// The indicator's width, and its inset from the viewport's edges, in CSS
/// px.
pub(crate) const INDICATOR_WIDTH: f32 = 6.0;
pub(crate) const INDICATOR_INSET: f32 = 3.0;
/// Its shortest length, so a very long page still shows a bar to see.
pub(crate) const INDICATOR_MIN_LENGTH: f32 = 24.0;
/// Black at 45%.
pub(crate) const INDICATOR_ALPHA: f32 = 0.45;
/// How long the indicator stays after the last scroll, then how long it
/// takes to fade.
pub(crate) const INDICATOR_HOLD: Duration = Duration::from_millis(800);
pub(crate) const INDICATOR_FADE: Duration = Duration::from_millis(200);
/// A smooth scroll takes this long, ease out.
pub(crate) const SMOOTH_SCROLL: Duration = Duration::from_millis(150);
/// How close the pointer must be to an edge for that edge's bar to show,
/// and how far the hover band reaches past the frame's edge.
pub(crate) const HOVER_BAND: f32 = 12.0;
/// A hovered bar widens from its normal width to this.
pub(crate) const HOVER_WIDTH: f32 = 10.0;
/// How long the page waits with no wheel event before a new gesture starts
/// (spec 0009, child 0002, Value sourcing, Gesture phase).
pub(crate) const GESTURE_GAP: Duration = Duration::from_millis(100);
/// How far the pointer is inside a thumb when a drag began, so dragging
/// keeps that relative position (spec 0009, child 0002, Value sourcing,
/// Thumb drag).
#[derive(Debug, Clone, Copy)]
#[allow(dead_code, reason = "kept to record where a thumb drag began")]
pub(crate) struct Drag {
    pub grab: f32,
}

/// One axis a scroll bar runs along (spec 0009, child 0002, AC-8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Axis {
    X,
    Y,
}

/// The phase of a wheel gesture: where in a Start / Move / End sequence
/// this event sits. A mouse wheel has no phases, so the browser synthesizes
/// `None` events and starts a new gesture after a quiet gap
/// (spec 0009, child 0002, Value sourcing, Gesture phase).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GesturePhase {
    Start,
    Move,
    End,
    None,
}

/// One overlay bar's geometry, in page CSS px (spec 0009, child 0002,
/// Value sourcing, Bar geometry).
#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(
    dead_code,
    reason = "kept as the bar geometry model; bar_geometry returns a tuple for now"
)]
pub(crate) struct Bar {
    /// Which axis the bar scrolls.
    pub axis: Axis,
    /// The track: the full length the thumb can travel in.
    pub track: RectF,
    /// The thumb: the draggable part.
    pub thumb: RectF,
}

/// What the render thread reports to the main thread so it can decide
/// whether a press lands on a bar before forwarding it to the page
/// (spec 0009, child 0002, Messages, `RenderEvent::Bars`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct BarHit {
    /// Which scroll container.
    pub scroll_id: ScrollId,
    /// Which axis.
    pub axis: Axis,
    /// The bar's track.
    pub track: RectF,
    /// The bar's thumb.
    pub thumb: RectF,
}

/// The geometry of every overlay bar one frame shows (spec 0009, child
/// 0002, Value sourcing, Bar geometry).
#[derive(Debug, Clone, Default)]
pub(crate) struct Bars {
    pub hits: Vec<BarHit>,
}

/// The geometry of one bar: its track and thumb, or `None` when the content
/// fits and there is nothing to scroll (spec 0009, child 0002, AC-8).
/// `width` is the bar's thickness: the normal width, or the wider hovered
/// width.
pub(crate) fn bar_geometry(
    frame: RectF,
    content: SizeF,
    offset: ScrollOffset,
    axis: Axis,
    width: f32,
) -> Option<(RectF, RectF)> {
    match axis {
        Axis::Y => {
            let overhang = content.height - frame.size.height;
            if overhang < 0.5 {
                return None;
            }
            let inset = INDICATOR_INSET;
            let track = RectF {
                origin: PointF {
                    x: frame.origin.x + frame.size.width - inset - width,
                    y: frame.origin.y + inset,
                },
                size: SizeF {
                    width,
                    height: frame.size.height - 2.0 * inset,
                },
            };
            let length = (frame.size.height / content.height * track.size.height)
                .max(INDICATOR_MIN_LENGTH)
                .min(track.size.height);
            let progress = (offset.y / overhang).clamp(0.0, 1.0);
            let thumb = RectF {
                origin: PointF {
                    x: track.origin.x,
                    y: track.origin.y + progress * (track.size.height - length),
                },
                size: SizeF {
                    width: track.size.width,
                    height: length,
                },
            };
            Some((track, thumb))
        }
        Axis::X => {
            let overhang = content.width - frame.size.width;
            if overhang < 0.5 {
                return None;
            }
            let inset = INDICATOR_INSET;
            let track = RectF {
                origin: PointF {
                    x: frame.origin.x + inset,
                    y: frame.origin.y + frame.size.height - inset - width,
                },
                size: SizeF {
                    width: frame.size.width - 2.0 * inset,
                    height: width,
                },
            };
            let length = (frame.size.width / content.width * track.size.width)
                .max(INDICATOR_MIN_LENGTH)
                .min(track.size.width);
            let progress = (offset.x / overhang).clamp(0.0, 1.0);
            let thumb = RectF {
                origin: PointF {
                    x: track.origin.x + progress * (track.size.width - length),
                    y: track.origin.y,
                },
                size: SizeF {
                    width: length,
                    height: track.size.height,
                },
            };
            Some((track, thumb))
        }
    }
}

/// The root-space top-left corner of a scroll frame's scrollport, where its
/// overlay scrollbar track is anchored. This stays fixed as the frame scrolls:
/// `to_parent(offset) = frame_rect.origin + offset - offset = frame_rect.origin`,
/// so walking `offset` up to root space yields `frame_rect.origin` in root
/// space — independent of the scroll offset.
pub(crate) fn scroll_port_origin_in_root(
    list: &DisplayList,
    frame_id: SpatialId,
    offset: ScrollOffset,
) -> Option<PointF> {
    list.to_root(
        frame_id,
        PointF {
            x: offset.x,
            y: offset.y,
        },
    )
}

/// Ease out cubic: starts fast, ends slow, used for smooth scrolling
/// (spec 0009, child 0002, Value sourcing, Smooth scroll curve).
pub(crate) fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

/// One smooth scroll in progress: where it started, where it is going, and
/// when it began (spec 0009, child 0002, Value sourcing, Smooth scroll).
#[derive(Debug, Clone, Copy)]
pub(crate) struct SmoothScroll {
    /// Which frame is moving.
    pub frame: ScrollId,
    /// Where the scroll started.
    pub from: PointF,
    /// Where it ends.
    pub to: PointF,
    /// When it began.
    pub start: Instant,
}

/// The gesture in progress: which frame it latched to, and when its last
/// event was, so a quiet gap starts a new gesture
/// (spec 0009, child 0002, Value sourcing, Gesture).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Gesture {
    /// Which frame the gesture latched to, or `None` for the viewport.
    pub frame: Option<ScrollId>,
    /// When the last gesture event arrived.
    pub last_event: Instant,
}

/// Advances a smooth scroll to `now`. Returns the offset at this moment and
/// whether it has finished. `None` once the duration is up and the scroll
/// should stop (spec 0009, child 0002, AC-9).
pub(crate) fn smooth_step(smooth: &SmoothScroll, now: Instant) -> Option<(PointF, bool)> {
    let elapsed = now.saturating_duration_since(smooth.start).as_secs_f32();
    let t = (elapsed / SMOOTH_SCROLL.as_secs_f32()).min(1.0);
    let eased = ease_out(t);
    let offset = PointF {
        x: smooth.from.x + (smooth.to.x - smooth.from.x) * eased,
        y: smooth.from.y + (smooth.to.y - smooth.from.y) * eased,
    };
    Some((offset, t >= 1.0))
}

/// Where a scroll key moves a frame, before the caller clamps it to the
/// scroll range (spec 0009, child 0002, AC-9). The mapping from a winit key
/// to a [`KeyScroll`] now lives in `page::input::scroll_key`, so the page
/// gets the first say on whether the key scrolls at all.
pub(crate) fn key_scroll_target(
    key: KeyScroll,
    offset: ScrollOffset,
    frame: RectF,
    content: SizeF,
) -> (f32, f32) {
    let page = (frame.size.height - PAGE_OVERLAP).max(LINE);
    let (x, y) = (offset.x, offset.y);
    match key {
        KeyScroll::LineUp => (x, y - LINE),
        KeyScroll::LineDown => (x, y + LINE),
        KeyScroll::LineLeft => (x - LINE, y),
        KeyScroll::LineRight => (x + LINE, y),
        KeyScroll::PageUp => (x, y - page),
        KeyScroll::PageDown => (x, y + page),
        KeyScroll::Home => (x, 0.0),
        KeyScroll::End => (x, content.height - frame.size.height),
    }
}

/// The indicator for a viewport `frame` over `content` at `offset`, last
/// scrolled at `scrolled_at`, as it looks at `now`: on the right edge, its
/// length and place in proportion to the viewport over the content. `None`
/// when the content fits or it has faded out.
#[allow(
    dead_code,
    reason = "kept as the single-root indicator helper; per-frame bars use bar_geometry"
)]
pub(crate) fn indicator(
    frame: RectF,
    content: SizeF,
    offset: ScrollOffset,
    scrolled_at: Instant,
    now: Instant,
) -> Option<ScrollIndicator> {
    let alpha = indicator_alpha(scrolled_at, now)?;
    let overhang = content.height - frame.size.height;
    if overhang < 0.5 {
        return None;
    }
    let track = frame.size.height - 2.0 * INDICATOR_INSET;
    let length = (frame.size.height / content.height * track)
        .max(INDICATOR_MIN_LENGTH)
        .min(track);
    let progress = (offset.y / overhang).clamp(0.0, 1.0);
    let rect = RectF {
        origin: PointF {
            x: frame.origin.x + frame.size.width - INDICATOR_INSET - INDICATOR_WIDTH,
            y: frame.origin.y + INDICATOR_INSET + progress * (track - length),
        },
        size: SizeF {
            width: INDICATOR_WIDTH,
            height: length,
        },
    };
    let color = ColorF {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: INDICATOR_ALPHA * alpha,
    };
    Some(ScrollIndicator { rect, color })
}

/// How visible the indicator is at `now`, 0 to 1: whole for
/// [`INDICATOR_HOLD`] after the scroll, then fading out over
/// [`INDICATOR_FADE`]. `None` once gone.
pub(crate) fn indicator_alpha(scrolled_at: Instant, now: Instant) -> Option<f32> {
    let since = now.saturating_duration_since(scrolled_at);
    let fading = since.checked_sub(INDICATOR_HOLD).unwrap_or_default();
    (fading < INDICATOR_FADE).then(|| 1.0 - fading.as_secs_f32() / INDICATOR_FADE.as_secs_f32())
}

/// When the render thread must draw next for the indicator alone: when the
/// hold ends, then every frame, until it draws one after
/// [`indicator_gone`] and stops asking.
pub(crate) fn indicator_wake(scrolled_at: Instant, now: Instant) -> Instant {
    (scrolled_at + INDICATOR_HOLD).max(now)
}

/// Whether the indicator has faded out completely by `now`.
pub(crate) fn indicator_gone(scrolled_at: Instant, now: Instant) -> bool {
    now >= scrolled_at + INDICATOR_HOLD + INDICATOR_FADE
}

#[cfg(test)]
mod tests {
    use super::*;
    use display_list::{Epoch, PageId, SpatialKind};

    fn rect(x: f32, y: f32, width: f32, height: f32) -> RectF {
        RectF {
            origin: PointF { x, y },
            size: SizeF { width, height },
        }
    }

    #[test]
    fn keys_move_by_a_line_a_page_or_to_an_end() {
        // AC-9: 40 px a line, the viewport minus 40 a page.
        let frame = rect(0.0, 0.0, 800.0, 600.0);
        let content = SizeF {
            width: 800.0,
            height: 5000.0,
        };
        let at = ScrollOffset {
            x: 0.0,
            y: 1000.0,
            generation: 0,
        };
        let target = |key: KeyScroll| key_scroll_target(key, at, frame, content);
        assert_eq!(target(KeyScroll::LineDown), (0.0, 1040.0));
        assert_eq!(target(KeyScroll::LineUp), (0.0, 960.0));
        assert_eq!(target(KeyScroll::LineRight), (40.0, 1000.0));
        assert_eq!(target(KeyScroll::PageDown), (0.0, 1560.0));
        assert_eq!(target(KeyScroll::PageUp), (0.0, 440.0));
        assert_eq!(target(KeyScroll::Home), (0.0, 0.0));
        assert_eq!(target(KeyScroll::End), (0.0, 4400.0));
    }

    #[test]
    fn the_indicator_tracks_the_offset_then_fades_after_800_ms() {
        // AC-30
        let frame = rect(0.0, 0.0, 800.0, 600.0);
        let content = SizeF {
            width: 800.0,
            height: 1200.0,
        };
        let start = Instant::now();
        let at = |y: f32| ScrollOffset {
            x: 0.0,
            y,
            generation: 0,
        };
        let top = indicator(frame, content, at(0.0), start, start).unwrap();
        // Half the content is visible: half the 594 px track.
        assert_eq!(top.rect, rect(791.0, 3.0, 6.0, 297.0));
        assert_eq!(top.color.a, 0.45);
        let bottom = indicator(frame, content, at(600.0), start, start).unwrap();
        assert_eq!(bottom.rect.origin.y + bottom.rect.size.height, 597.0);

        let ms = |ms: u64| start + Duration::from_millis(ms);
        assert_eq!(
            indicator(frame, content, at(0.0), start, ms(800))
                .unwrap()
                .color
                .a,
            0.45
        );
        let fading = indicator(frame, content, at(0.0), start, ms(900)).unwrap();
        assert!((fading.color.a - 0.225).abs() < 1e-3, "halfway faded");
        assert_eq!(indicator(frame, content, at(0.0), start, ms(1000)), None);

        assert_eq!(indicator_wake(start, ms(100)), ms(800));
        assert_eq!(indicator_wake(start, ms(900)), ms(900), "every frame");
        assert!(!indicator_gone(start, ms(999)));
        assert!(indicator_gone(start, ms(1000)));

        let fits = SizeF {
            width: 800.0,
            height: 600.0,
        };
        assert_eq!(indicator(frame, fits, at(0.0), start, start), None);
    }

    #[test]
    fn bar_geometry_vertical_track_and_thumb() {
        let frame = RectF {
            origin: PointF { x: 0.0, y: 0.0 },
            size: SizeF {
                width: 800.0,
                height: 600.0,
            },
        };
        let content = SizeF {
            width: 800.0,
            height: 2400.0,
        };
        let offset = ScrollOffset {
            x: 0.0,
            y: 600.0,
            generation: 0,
        };
        let (track, thumb) = bar_geometry(frame, content, offset, Axis::Y, INDICATOR_WIDTH)
            .expect("a tall page shows a bar");
        // Track runs down the right edge, inset 3 px, 6 px wide.
        assert!((track.origin.x - 791.0).abs() < 1e-3);
        assert!((track.origin.y - 3.0).abs() < 1e-3);
        assert!((track.size.width - 6.0).abs() < 1e-3);
        assert!((track.size.height - 594.0).abs() < 1e-3);
        // Progress is 600 / 1800 = one third of the overhang.
        let expected = 3.0 + (594.0 - thumb.size.height) / 3.0;
        assert!(
            (thumb.origin.y - expected).abs() < 1e-3,
            "thumb at one third"
        );
    }

    #[test]
    fn bar_geometry_horizontal_track_and_thumb() {
        let frame = RectF {
            origin: PointF { x: 0.0, y: 0.0 },
            size: SizeF {
                width: 800.0,
                height: 600.0,
            },
        };
        let content = SizeF {
            width: 2400.0,
            height: 600.0,
        };
        let offset = ScrollOffset {
            x: 600.0,
            y: 0.0,
            generation: 0,
        };
        let (track, _thumb) = bar_geometry(frame, content, offset, Axis::X, INDICATOR_WIDTH)
            .expect("a wide page shows a bar");
        // Track runs along the bottom edge, inset 3 px, 6 px tall.
        assert!((track.origin.y - 591.0).abs() < 1e-3);
        assert!((track.size.height - 6.0).abs() < 1e-3);
        assert!((track.size.width - 794.0).abs() < 1e-3);
    }

    #[test]
    fn bar_geometry_none_when_content_fits() {
        let frame = RectF {
            origin: PointF::default(),
            size: SizeF {
                width: 800.0,
                height: 600.0,
            },
        };
        let content = SizeF {
            width: 800.0,
            height: 600.0,
        };
        let offset = ScrollOffset::default();
        assert!(
            bar_geometry(frame, content, offset, Axis::Y, INDICATOR_WIDTH).is_none(),
            "a page that fits shows no bar"
        );
    }

    #[test]
    fn ease_out_starts_flat_ends_at_one_and_is_smooth() {
        assert!((ease_out(0.0) - 0.0).abs() < 1e-3);
        assert!((ease_out(1.0) - 1.0).abs() < 1e-3);
        assert!(ease_out(0.5) > 0.5, "ease out covers most distance early");
        assert!(ease_out(0.9) > 0.5, "ease out is fast at the start");
    }

    #[test]
    fn smooth_step_interpolates_and_finishes() {
        let start = Instant::now();
        let smooth = SmoothScroll {
            frame: ScrollId::new(0, 0, 0),
            from: PointF { x: 0.0, y: 0.0 },
            to: PointF { x: 0.0, y: 1000.0 },
            start,
        };
        // At the start the offset is the from offset.
        let Some((offset, done)) = smooth_step(&smooth, start) else {
            panic!("smooth step always returns Some");
        };
        assert!(!done, "not finished at the start");
        assert!((offset.y - 0.0).abs() < 1e-3);
        // At the end the offset is the to offset.
        let end = start + SMOOTH_SCROLL;
        let Some((offset, done)) = smooth_step(&smooth, end) else {
            panic!("smooth step always returns Some");
        };
        assert!(done, "finished at the end");
        assert!((offset.y - 1000.0).abs() < 1e-3);
        // Past the end it clamps to the target and stays finished.
        let Some((offset, done)) = smooth_step(&smooth, end + Duration::from_secs(1)) else {
            panic!("smooth step always returns Some");
        };
        assert!(done);
        assert!((offset.y - 1000.0).abs() < 1e-3);
    }

    #[test]
    fn scrollbar_track_stays_fixed_when_scrolling() {
        // A scroll frame whose scrollport sits at (100, 50) in root space.
        let make_list = |y: f32| {
            let mut list = DisplayList::new(PageId(1), Epoch(1), SizeF::default());
            let frame_id = list.push_spatial(
                SpatialId::ROOT,
                SpatialKind::ScrollFrame {
                    scroll_id: ScrollId::new(0, 0, 0),
                    frame_rect: RectF {
                        origin: PointF { x: 100.0, y: 50.0 },
                        size: SizeF {
                            width: 800.0,
                            height: 600.0,
                        },
                    },
                    content_size: SizeF {
                        width: 800.0,
                        height: 2000.0,
                    },
                    offset: ScrollOffset {
                        x: 0.0,
                        y,
                        generation: 0,
                    },
                    user_scrollable: [true, true],
                },
            );
            (list, frame_id)
        };

        // Before scrolling: the track anchors at the scrollport's top-left.
        let (list, frame_id) = make_list(0.0);
        let origin_before = scroll_port_origin_in_root(
            &list,
            frame_id,
            ScrollOffset {
                x: 0.0,
                y: 0.0,
                generation: 0,
            },
        )
        .unwrap();
        assert_eq!(origin_before, PointF { x: 100.0, y: 50.0 });

        // After scrolling down 300px: the track stays put, it does not move up
        // off the screen. The old code computed `to_root(frame, PointF::default())`
        // then added `frame_rect.origin`, which gave `-offset` for a root frame
        // — so the track slid up as the page scrolled down.
        let (list, frame_id) = make_list(300.0);
        let origin_after = scroll_port_origin_in_root(
            &list,
            frame_id,
            ScrollOffset {
                x: 0.0,
                y: 300.0,
                generation: 0,
            },
        )
        .unwrap();
        assert_eq!(origin_after, PointF { x: 100.0, y: 50.0 });
    }
}
