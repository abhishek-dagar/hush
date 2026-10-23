//! A test's result (spec 0005, Data model sketch).

use std::fmt;

use serde::{Deserialize, Serialize};

/// What one run of a test came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Status {
    /// The test passed.
    Pass,
    /// The rendering did not satisfy its references.
    Fail,
    /// The worker exited or died mid test.
    Crash,
    /// The test was still running after its timeout.
    Timeout,
    /// The test itself is broken: a missing reference, a bad `fuzzy`.
    Error,
    /// Not run: a kind or feature the harness does not support.
    Skip,
}

impl Status {
    /// Every status, in the order the summary table shows them.
    pub const ALL: [Self; 6] = [
        Self::Pass,
        Self::Fail,
        Self::Crash,
        Self::Timeout,
        Self::Error,
        Self::Skip,
    ];

    /// The name expectations files and `results.json` use.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::Crash => "CRASH",
            Self::Timeout => "TIMEOUT",
            Self::Error => "ERROR",
            Self::Skip => "SKIP",
        }
    }

    /// The status named `name`, if any.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|status| status.as_str() == name)
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
