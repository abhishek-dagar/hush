//! Bundle size measurement (spec 0007, AC-2, AC-3).
//!
//! Bundle size is the executable plus every file shipped beside it (today,
//! the executable alone, since assets are embedded). MB = bytes / 10^6.

use std::path::Path;

use crate::budgets::{Os, Target};
use crate::build::{cross_target_dir, exe_name, native_target_dir};

/// The size of one app on one OS, in MB, or `None` when it could not be
/// measured (cross tool missing or build failed).
pub fn measure(app: Target, os: Os) -> Option<f64> {
    let dir = match os {
        Os::Macos => native_target_dir(),
        _ => cross_target_dir(os)?,
    };
    let exe = dir.join(exe_name(app));
    let bytes = dir_size(&exe)?;
    Some(bytes as f64 / 1_000_000.0)
}

/// The executable's bytes; assets are embedded, so there is nothing beside
/// it to count today.
fn dir_size(path: &Path) -> Option<u64> {
    if !path.is_file() {
        return None;
    }
    std::fs::metadata(path).ok().map(|m| m.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_size_returns_none_for_missing() {
        assert!(dir_size(Path::new("does-not-exist")).is_none());
    }
}
