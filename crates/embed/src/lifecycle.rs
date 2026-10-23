//! The navigation lifecycle's rules on the browser main thread (spec 0004,
//! Navigation lifecycle, child 0001, Reload, shortcuts and crash relaunch):
//! which keys reload or quit, how long the old frame is held, and when a
//! crashing content process stops being relaunched. Plain data in, data
//! out; `browser` applies the results.

use std::time::{Duration, Instant};

use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};

/// How long the previous frame stays on screen after a navigation starts,
/// before the window turns white (AC-12).
pub(crate) const PAINT_HOLD: Duration = Duration::from_millis(500);
/// Crashes are counted over this window (AC-14).
pub(crate) const CRASH_WINDOW: Duration = Duration::from_secs(60);
/// The crash within [`CRASH_WINDOW`] that stops relaunching (AC-14).
pub(crate) const MAX_CRASHES: usize = 3;

/// A shortcut the browser itself handles (AC-13; spec 0008, AC-29).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shortcut {
    /// Load the current URL again.
    Reload,
    /// Go back one entry.
    Back,
    /// Go forward one entry.
    Forward,
    /// Focus the address, or open the palette (spec 0008, AC-5, AC-6).
    FocusAddress,
    /// Stop the load in progress (AC-26).
    Stop,
    /// F6, or Shift+F6 with `backward`: focus to the next region, the
    /// sidebar or the page (AC-31).
    Region {
        /// Shift was held.
        backward: bool,
    },
    /// Shut the content process down and close.
    Quit,
}

/// The shortcut `event` is, if any: the platform's primary modifier (Cmd on
/// macOS, Ctrl elsewhere) with R or Q, or F5; back and forward with Cmd+[
/// and Cmd+] on macOS, Alt+Left and Alt+Right elsewhere; the address with
/// the primary modifier and L, or Alt+D off macOS; stop with Cmd+. on
/// macOS (spec 0008, AC-29). Keys go by their physical position, as the chrome's shortcuts
/// do. Repeats are ignored, so holding a key acts once.
pub(crate) fn shortcut(event: &KeyEvent, modifiers: ModifiersState) -> Option<Shortcut> {
    if event.state != ElementState::Pressed || event.repeat {
        return None;
    }
    let PhysicalKey::Code(code) = event.physical_key else {
        return None;
    };
    shortcut_for(code, modifiers)
}

fn shortcut_for(code: KeyCode, modifiers: ModifiersState) -> Option<Shortcut> {
    let primary = if cfg!(target_os = "macos") {
        modifiers.super_key()
    } else {
        modifiers.control_key()
    };
    let mac = cfg!(target_os = "macos");
    // Alt alone: Alt+Ctrl+Left is a different shortcut on some desktops.
    let alt = modifiers.alt_key() && !modifiers.control_key() && !modifiers.super_key();
    match code {
        KeyCode::F5 => Some(Shortcut::Reload),
        KeyCode::KeyR if primary => Some(Shortcut::Reload),
        KeyCode::KeyQ if primary => Some(Shortcut::Quit),
        KeyCode::BracketLeft if mac && primary => Some(Shortcut::Back),
        KeyCode::BracketRight if mac && primary => Some(Shortcut::Forward),
        KeyCode::ArrowLeft if !mac && alt => Some(Shortcut::Back),
        KeyCode::ArrowRight if !mac && alt => Some(Shortcut::Forward),
        KeyCode::KeyL if primary => Some(Shortcut::FocusAddress),
        KeyCode::KeyD if !mac && alt => Some(Shortcut::FocusAddress),
        KeyCode::Period if mac && primary => Some(Shortcut::Stop),
        KeyCode::F6 if !primary && !modifiers.alt_key() => Some(Shortcut::Region {
            backward: modifiers.shift_key(),
        }),
        _ => None,
    }
}

/// When the content process crashed lately (AC-14).
#[derive(Debug, Default)]
pub(crate) struct CrashLog {
    times: Vec<Instant>,
}

impl CrashLog {
    /// Records a crash at `now` and says whether to relaunch: yes until the
    /// [`MAX_CRASHES`]th crash within [`CRASH_WINDOW`].
    pub(crate) fn record(&mut self, now: Instant) -> bool {
        self.times
            .retain(|&at| now.saturating_duration_since(at) < CRASH_WINDOW);
        self.times.push(now);
        self.times.len() < MAX_CRASHES
    }
}

/// The paint holding timer (AC-12): while it runs, the previous frame stays
/// on screen; when it ends before the new page painted, the window turns
/// white.
#[derive(Debug, Default)]
pub(crate) struct PaintHold {
    until: Option<Instant>,
}

impl PaintHold {
    /// A navigation started at `now`. With nothing on screen (the first
    /// launch) there is nothing to hold, so the window stays white.
    pub(crate) fn start(&mut self, now: Instant, frame_on_screen: bool) {
        self.until = frame_on_screen.then(|| now + PAINT_HOLD);
    }

    /// The new page painted: the hold is over.
    pub(crate) fn painted(&mut self) {
        self.until = None;
    }

    /// When the event loop must wake to end the hold, if one is running.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.until
    }

    /// Whether the hold ran out at `now`, ending it if so. True means the
    /// window should show white until the first paint.
    pub(crate) fn expired(&mut self, now: Instant) -> bool {
        match self.until {
            Some(until) if now >= until => {
                self.until = None;
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn primary() -> ModifiersState {
        if cfg!(target_os = "macos") {
            ModifiersState::SUPER
        } else {
            ModifiersState::CONTROL
        }
    }

    #[test]
    fn reload_and_quit_take_the_platform_modifier_and_f5_needs_none() {
        // AC-13
        let none = ModifiersState::empty();
        assert_eq!(
            shortcut_for(KeyCode::KeyR, primary()),
            Some(Shortcut::Reload)
        );
        assert_eq!(
            shortcut_for(KeyCode::KeyR, primary() | ModifiersState::SHIFT),
            Some(Shortcut::Reload)
        );
        assert_eq!(shortcut_for(KeyCode::F5, none), Some(Shortcut::Reload));
        assert_eq!(shortcut_for(KeyCode::KeyQ, primary()), Some(Shortcut::Quit));
        assert_eq!(shortcut_for(KeyCode::KeyR, none), None, "typing r");
        assert_eq!(shortcut_for(KeyCode::KeyQ, ModifiersState::ALT), None);
        assert_eq!(shortcut_for(KeyCode::KeyW, primary()), None);
        // The other platform's modifier does nothing.
        let other = if cfg!(target_os = "macos") {
            ModifiersState::CONTROL
        } else {
            ModifiersState::SUPER
        };
        assert_eq!(shortcut_for(KeyCode::KeyR, other), None);
    }

    #[test]
    fn back_and_forward_take_the_platform_keys() {
        // Spec 0008, AC-29
        let none = ModifiersState::empty();
        let (back, forward, modifier) = if cfg!(target_os = "macos") {
            (
                KeyCode::BracketLeft,
                KeyCode::BracketRight,
                ModifiersState::SUPER,
            )
        } else {
            (KeyCode::ArrowLeft, KeyCode::ArrowRight, ModifiersState::ALT)
        };
        assert_eq!(shortcut_for(back, modifier), Some(Shortcut::Back));
        assert_eq!(shortcut_for(forward, modifier), Some(Shortcut::Forward));
        assert_eq!(shortcut_for(back, none), None);
        assert_eq!(
            shortcut_for(KeyCode::Backspace, none),
            None,
            "never navigates"
        );
        if !cfg!(target_os = "macos") {
            let with_ctrl = ModifiersState::ALT | ModifiersState::CONTROL;
            assert_eq!(shortcut_for(back, with_ctrl), None);
        }
    }

    #[test]
    fn the_address_and_stop_take_the_platform_keys() {
        // Spec 0008, AC-29
        assert_eq!(
            shortcut_for(KeyCode::KeyL, primary()),
            Some(Shortcut::FocusAddress)
        );
        assert_eq!(shortcut_for(KeyCode::KeyL, ModifiersState::empty()), None);
        if cfg!(target_os = "macos") {
            assert_eq!(
                shortcut_for(KeyCode::Period, primary()),
                Some(Shortcut::Stop)
            );
            assert_eq!(shortcut_for(KeyCode::KeyD, ModifiersState::ALT), None);
        } else {
            assert_eq!(
                shortcut_for(KeyCode::KeyD, ModifiersState::ALT),
                Some(Shortcut::FocusAddress)
            );
            assert_eq!(shortcut_for(KeyCode::Period, primary()), None);
        }
    }

    #[test]
    fn f6_and_shift_f6_cycle_the_regions() {
        // Spec 0008, AC-31
        assert_eq!(
            shortcut_for(KeyCode::F6, ModifiersState::empty()),
            Some(Shortcut::Region { backward: false })
        );
        assert_eq!(
            shortcut_for(KeyCode::F6, ModifiersState::SHIFT),
            Some(Shortcut::Region { backward: true })
        );
        assert_eq!(shortcut_for(KeyCode::F6, primary()), None);
    }

    #[test]
    fn the_third_crash_within_a_minute_stops_relaunching() {
        // AC-14
        let start = Instant::now();
        let mut log = CrashLog::default();
        assert!(log.record(start));
        assert!(log.record(start + Duration::from_secs(10)));
        assert!(!log.record(start + Duration::from_secs(59)));

        // Crashes spread over more than a minute keep relaunching.
        let mut log = CrashLog::default();
        assert!(log.record(start));
        assert!(log.record(start + Duration::from_secs(30)));
        assert!(log.record(start + Duration::from_secs(61)));
        assert!(log.record(start + Duration::from_secs(95)));
    }

    #[test]
    fn the_hold_lasts_500_ms_and_ends_early_on_paint() {
        // AC-12
        let start = Instant::now();
        let mut hold = PaintHold::default();
        hold.start(start, false);
        assert_eq!(hold.deadline(), None, "first launch starts white");

        hold.start(start, true);
        assert_eq!(hold.deadline(), Some(start + PAINT_HOLD));
        assert!(!hold.expired(start + Duration::from_millis(499)));
        assert!(hold.expired(start + PAINT_HOLD));
        assert!(!hold.expired(start + PAINT_HOLD), "only once");

        hold.start(start, true);
        hold.painted();
        assert!(!hold.expired(start + Duration::from_secs(1)));
        assert_eq!(hold.deadline(), None);
    }
}
