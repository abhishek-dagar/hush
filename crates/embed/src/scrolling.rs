//! The scrolling rules (spec 0004, child 0005, Scrolling, AC-30): which
//! keys scroll the root viewport and how far, and where the overlay
//! indicator sits and how it fades. Plain data in, data out: the main
//! thread maps keys, the render thread applies the rest.

use std::time::{Duration, Instant};

use display_list::{ColorF, PointF, RectF, ScrollOffset, SizeF};
use render::ScrollIndicator;
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};

/// CSS px one arrow key or one wheel line scrolls.
pub(crate) const LINE: f32 = 40.0;
/// What a page step keeps in view: it moves the viewport's height minus
/// this.
const PAGE_OVERLAP: f32 = 40.0;
/// The indicator's width, and its inset from the viewport's edges, in CSS
/// px.
const INDICATOR_WIDTH: f32 = 6.0;
const INDICATOR_INSET: f32 = 3.0;
/// Its shortest length, so a very long page still shows a bar to see.
const INDICATOR_MIN_LENGTH: f32 = 24.0;
/// Black at 45%.
const INDICATOR_ALPHA: f32 = 0.45;
/// How long the indicator stays after the last scroll, then how long it
/// takes to fade.
pub(crate) const INDICATOR_HOLD: Duration = Duration::from_millis(800);
pub(crate) const INDICATOR_FADE: Duration = Duration::from_millis(200);

/// A key that scrolls the root viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyScroll {
    LineUp,
    LineDown,
    LineLeft,
    LineRight,
    PageUp,
    PageDown,
    Home,
    End,
}

/// The viewport scroll `event` asks for, if any. Holding a key repeats it.
pub(crate) fn key_scroll(event: &KeyEvent, modifiers: ModifiersState) -> Option<KeyScroll> {
    if event.state != ElementState::Pressed {
        return None;
    }
    let PhysicalKey::Code(code) = event.physical_key else {
        return None;
    };
    key_scroll_for(code, modifiers)
}

fn key_scroll_for(code: KeyCode, modifiers: ModifiersState) -> Option<KeyScroll> {
    // Cmd+Up and Cmd+Down are Home and End on macOS.
    if cfg!(target_os = "macos") && modifiers == ModifiersState::SUPER {
        return match code {
            KeyCode::ArrowUp => Some(KeyScroll::Home),
            KeyCode::ArrowDown => Some(KeyScroll::End),
            _ => None,
        };
    }
    // Other chords belong to the app and the OS.
    if modifiers.intersects(ModifiersState::SUPER | ModifiersState::CONTROL | ModifiersState::ALT) {
        return None;
    }
    let shift = modifiers.shift_key();
    match code {
        KeyCode::ArrowUp => Some(KeyScroll::LineUp),
        KeyCode::ArrowDown => Some(KeyScroll::LineDown),
        KeyCode::ArrowLeft => Some(KeyScroll::LineLeft),
        KeyCode::ArrowRight => Some(KeyScroll::LineRight),
        KeyCode::Space if shift => Some(KeyScroll::PageUp),
        KeyCode::Space | KeyCode::PageDown => Some(KeyScroll::PageDown),
        KeyCode::PageUp => Some(KeyScroll::PageUp),
        KeyCode::Home => Some(KeyScroll::Home),
        KeyCode::End => Some(KeyScroll::End),
        _ => None,
    }
}

impl KeyScroll {
    /// Where the key moves a viewport at `offset` with scrollport `frame`
    /// over `content`; the caller clamps it.
    pub(crate) fn target(self, offset: ScrollOffset, frame: RectF, content: SizeF) -> (f32, f32) {
        let page = (frame.size.height - PAGE_OVERLAP).max(LINE);
        let (x, y) = (offset.x, offset.y);
        match self {
            Self::LineUp => (x, y - LINE),
            Self::LineDown => (x, y + LINE),
            Self::LineLeft => (x - LINE, y),
            Self::LineRight => (x + LINE, y),
            Self::PageUp => (x, y - page),
            Self::PageDown => (x, y + page),
            Self::Home => (x, 0.0),
            Self::End => (x, content.height - frame.size.height),
        }
    }
}

/// The indicator for a viewport `frame` over `content` at `offset`, last
/// scrolled at `scrolled_at`, as it looks at `now`: on the right edge, its
/// length and place in proportion to the viewport over the content. `None`
/// when the content fits or it has faded out.
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
fn indicator_alpha(scrolled_at: Instant, now: Instant) -> Option<f32> {
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

    fn rect(x: f32, y: f32, width: f32, height: f32) -> RectF {
        RectF {
            origin: PointF { x, y },
            size: SizeF { width, height },
        }
    }

    #[test]
    fn keys_map_to_viewport_scrolls() {
        // AC-30
        let none = ModifiersState::empty();
        let shift = ModifiersState::SHIFT;
        assert_eq!(
            key_scroll_for(KeyCode::ArrowDown, none),
            Some(KeyScroll::LineDown)
        );
        assert_eq!(
            key_scroll_for(KeyCode::Space, none),
            Some(KeyScroll::PageDown)
        );
        assert_eq!(
            key_scroll_for(KeyCode::Space, shift),
            Some(KeyScroll::PageUp)
        );
        assert_eq!(
            key_scroll_for(KeyCode::PageUp, none),
            Some(KeyScroll::PageUp)
        );
        assert_eq!(key_scroll_for(KeyCode::End, none), Some(KeyScroll::End));
        assert_eq!(key_scroll_for(KeyCode::KeyA, none), None);
        assert_eq!(
            key_scroll_for(KeyCode::ArrowDown, ModifiersState::ALT),
            None,
            "chords are the app's"
        );
        if cfg!(target_os = "macos") {
            assert_eq!(
                key_scroll_for(KeyCode::ArrowDown, ModifiersState::SUPER),
                Some(KeyScroll::End)
            );
        }
    }

    #[test]
    fn keys_move_by_a_line_a_page_or_to_an_end() {
        // AC-30: 40 px a line, the viewport minus 40 a page.
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
        let target = |key: KeyScroll| key.target(at, frame, content);
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
}
