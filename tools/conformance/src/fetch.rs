//! The WPT snapshot: a partial, sparse `git` checkout of the pinned commit
//! (spec 0005, AC-2). The only network access a run makes.

use std::io::ErrorKind;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

/// Where the snapshot comes from.
pub const WPT_REMOTE: &str = "https://github.com/web-platform-tests/wpt.git";

/// Written last; a folder without it is fetched again.
const MARKER: &str = ".complete";

/// Makes sure `dir` holds `paths` of `commit`, fetching them from `remote`
/// unless a complete snapshot is already there. Every argument goes to
/// `git` as a list, never through a shell.
pub fn ensure(dir: &Path, commit: &str, paths: &[String], remote: &str) -> Result<()> {
    if dir.join(MARKER).is_file() {
        return Ok(());
    }
    git(None, &["--version"])?;
    if dir.exists() {
        std::fs::remove_dir_all(dir)
            .with_context(|| format!("could not clear the partial snapshot {}", dir.display()))?;
    }
    std::fs::create_dir_all(dir)
        .with_context(|| format!("could not create {}", dir.display()))?;
    eprintln!("fetching WPT {commit} into {}", dir.display());
    let dir = Some(dir);
    git(dir, &["init", "-q"])?;
    git(dir, &["remote", "add", "origin", remote])?;
    let patterns: Vec<String> = paths.iter().map(|p| format!("/{p}/")).collect();
    let mut sparse = vec!["sparse-checkout", "set", "--no-cone"];
    sparse.extend(patterns.iter().map(String::as_str));
    git(dir, &sparse)?;
    git(
        dir,
        &["fetch", "-q", "--depth", "1", "--filter=blob:none", "origin", commit],
    )?;
    git(dir, &["checkout", "-q", "FETCH_HEAD"])?;
    let dir = dir.unwrap_or(Path::new("."));
    std::fs::write(dir.join(MARKER), commit)
        .with_context(|| format!("could not mark {} complete", dir.display()))
}

/// Runs `git args` in `dir`, failing with its stderr.
fn git(dir: Option<&Path>, args: &[&str]) -> Result<()> {
    let mut command = Command::new("git");
    command.args(args).stdin(Stdio::null());
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    let output = match command.output() {
        Ok(output) => output,
        Err(err) if err.kind() == ErrorKind::NotFound => {
            bail!("`git` was not found on the PATH; it is needed to fetch the WPT snapshot")
        }
        Err(err) => return Err(err).context("could not run `git`"),
    };
    if !output.status.success() {
        bail!(
            "`git {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}
