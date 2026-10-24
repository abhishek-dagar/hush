//! Building the release binaries the gate measures (spec 0007, AC-1, AC-2).
//!
//! Size is measured on exactly the `release` profile that ships (fat LTO,
//! stripped). Cross builds delegate to `cargo zigbuild` (Linux) and
//! `cargo xwin` (Windows); when either is missing or fails, that OS's size
//! rows become `not_measured` with a hint (AC-3).

use std::path::PathBuf;
use std::process::Command;

use crate::budgets::{Os, Target};

/// Where release binaries land for the native triple.
pub fn native_target_dir() -> PathBuf {
    PathBuf::from("target/release")
}

/// The executable name for an app on this host.
pub fn exe_name(app: Target) -> String {
    match app {
        Target::Browser => "browser",
        Target::Shell => "shell",
    }
    .to_owned()
        + exe_suffix()
}

fn exe_suffix() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        ".exe"
    }
    #[cfg(not(target_os = "windows"))]
    {
        ""
    }
}

/// Builds `browser` and `shell` with the real `release` profile for the
/// native triple. Returns an error if the build fails (exit 2).
pub fn build_native() -> anyhow::Result<()> {
    let status = Command::new("cargo")
        .args(["build", "--release", "-p", "browser", "-p", "shell"])
        .status()?;
    if !status.success() {
        anyhow::bail!("native release build failed");
    }
    Ok(())
}

/// The path to a natively built release executable.
#[allow(dead_code, reason = "used by cross-size measurement in later tasks")]
pub fn native_exe(app: Target) -> PathBuf {
    native_target_dir().join(exe_name(app))
}

/// Tries a cross build for `os`, returning the target directory it writes
/// into, or `None` when the cross tool is missing or the build fails
/// (AC-3: that OS's rows become `not_measured`).
pub fn cross_target_dir(os: Os) -> Option<PathBuf> {
    match os {
        Os::Linux => cross_zigbuild(),
        Os::Windows => cross_xwin(),
        Os::Macos => None, // macOS is native, handled by build_native
    }
}

/// Linux: `cargo zigbuild -p browser -p shell --release --target
/// x86_64-unknown-linux-gnu`.
fn cross_zigbuild() -> Option<PathBuf> {
    let tool = which("cargo-zigbuild")?;
    let status = Command::new(tool)
        .args([
            "build",
            "--release",
            "-p",
            "browser",
            "-p",
            "shell",
            "--target",
            "x86_64-unknown-linux-gnu",
        ])
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    Some(PathBuf::from("target/x86_64-unknown-linux-gnu/release"))
}

/// Windows: `cargo xwin build -p browser -p shell --release --target
/// x86_64-pc-windows-msvc`.
fn cross_xwin() -> Option<PathBuf> {
    let tool = which("cargo-xwin")?;
    let status = Command::new(tool)
        .args([
            "build",
            "--release",
            "-p",
            "browser",
            "-p",
            "shell",
            "--target",
            "x86_64-pc-windows-msvc",
        ])
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    Some(PathBuf::from("target/x86_64-pc-windows-msvc/release"))
}

/// Finds an executable on PATH, mirroring `cargo` alias resolution.
fn which(name: &str) -> Option<PathBuf> {
    // `cargo xwin` and `cargo zigbuild` are cargo subcommands: look for
    // `cargo-<name>` on PATH.
    for dir in std::env::split_paths(&std::env::var("PATH").ok()?) {
        let exe = dir.join(format!("cargo-{name}"));
        if exe.is_file() {
            return Some(exe);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exe_names_include_suffix() {
        assert_eq!(exe_name(Target::Browser), "browser");
        assert_eq!(exe_name(Target::Shell), "shell");
    }

    #[test]
    fn cross_target_dir_is_none_for_macos() {
        assert!(cross_target_dir(Os::Macos).is_none());
    }
}
