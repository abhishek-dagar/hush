//! The script load throttle (spec 0008, AC-28): at most
//! [`MAX_SCRIPT_LOADS`] script started loads per page in any
//! [`SCRIPT_WINDOW`] are honored. Past that they are dropped, with one
//! `warn` per window. Only `Origin::Page(Script)` loads are counted; user
//! actions and link activations never are.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// The most script started loads honored in one [`SCRIPT_WINDOW`].
pub const MAX_SCRIPT_LOADS: usize = 200;
/// The window [`MAX_SCRIPT_LOADS`] counts over.
pub const SCRIPT_WINDOW: Duration = Duration::from_secs(10);

/// Whether a script started load may go ahead (spec 0008, Interface
/// surface).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Allow {
    /// It loads.
    Yes,
    /// It is dropped.
    No {
        /// Log the one `warn` of this window.
        warn: bool,
    },
}

/// One page's recent script loads (spec 0008, Data model,
/// `ScriptThrottle`).
#[derive(Debug, Default)]
pub struct ScriptThrottle {
    /// When the honored loads of the last window happened, oldest first;
    /// at most [`MAX_SCRIPT_LOADS`].
    recent: VecDeque<Instant>,
    /// Until when the `warn` of the current refusal window is spent.
    warned_until: Option<Instant>,
}

impl ScriptThrottle {
    /// Counts a script started load at `now`, or refuses it when
    /// [`MAX_SCRIPT_LOADS`] were already honored in the last
    /// [`SCRIPT_WINDOW`].
    pub fn allow(&mut self, now: Instant) -> Allow {
        while self
            .recent
            .front()
            .is_some_and(|&at| now.saturating_duration_since(at) >= SCRIPT_WINDOW)
        {
            self.recent.pop_front();
        }
        if self.recent.len() < MAX_SCRIPT_LOADS {
            self.recent.push_back(now);
            return Allow::Yes;
        }
        let warn = self.warned_until.is_none_or(|until| now >= until);
        if warn {
            self.warned_until = Some(now + SCRIPT_WINDOW);
        }
        Allow::No { warn }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_201st_load_in_a_window_is_refused_with_one_warning() {
        // AC-28
        let mut throttle = ScriptThrottle::default();
        let now = Instant::now();
        for _ in 0..MAX_SCRIPT_LOADS {
            assert_eq!(throttle.allow(now), Allow::Yes);
        }
        assert_eq!(throttle.allow(now), Allow::No { warn: true });
        let later = now + Duration::from_secs(5);
        assert_eq!(
            throttle.allow(later),
            Allow::No { warn: false },
            "one per window"
        );
    }

    #[test]
    fn the_window_slides() {
        // AC-28: "any 10 s window".
        let mut throttle = ScriptThrottle::default();
        let start = Instant::now();
        for i in 0..MAX_SCRIPT_LOADS {
            let at = start + Duration::from_millis(i as u64 * 10);
            assert_eq!(throttle.allow(at), Allow::Yes);
        }
        // The first load leaves the window 10 s after it happened, so one
        // more fits.
        assert_eq!(throttle.allow(start + SCRIPT_WINDOW), Allow::Yes);
        assert!(matches!(
            throttle.allow(start + SCRIPT_WINDOW),
            Allow::No { .. }
        ));
    }

    #[test]
    fn a_new_refusal_window_warns_again() {
        // AC-28: one `warn` per window.
        let mut throttle = ScriptThrottle::default();
        let start = Instant::now();
        for _ in 0..MAX_SCRIPT_LOADS {
            throttle.allow(start);
        }
        assert_eq!(throttle.allow(start), Allow::No { warn: true });
        let later = start + SCRIPT_WINDOW + Duration::from_secs(1);
        for _ in 0..MAX_SCRIPT_LOADS {
            assert_eq!(throttle.allow(later), Allow::Yes);
        }
        assert_eq!(throttle.allow(later), Allow::No { warn: true });
    }
}
