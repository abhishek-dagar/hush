//! The browser side of fonts by reference (spec 0032, AC-1 to AC-3).
//!
//! The content process names a system font by its file, never by sending
//! the file's bytes. This module checks the path against the OS's font
//! roots, opens it once per canonical path, and keeps one shared handle so
//! every face of the file, on every page, uses those bytes.
//!
//! A file under a root the OS keeps read only (macOS System Integrity
//! Protection guards `/System/Library`) is memory mapped: the content and
//! browser processes then share the same file pages. A file under a user
//! writable root is read once into the heap, because a mapped file that is
//! truncated while mapped would crash the process (spec 0032, Mapped vs
//! read).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Weak;

use display_list::{FontKey, PageId};
use render::{FontBytes, FontStorage};
use tracing::warn;

/// The roots a `System` path may lie under, per OS (spec 0032, AC-3).
#[cfg(target_os = "macos")]
fn system_roots() -> Vec<PathBuf> {
    vec![
        PathBuf::from("/System/Library/Fonts"),
        PathBuf::from("/System/Library/AssetsV2"),
        PathBuf::from("/Library/Fonts"),
    ]
}

/// The roots a `System` path may lie under, per OS (spec 0032, AC-3).
#[cfg(target_os = "linux")]
fn system_roots() -> Vec<PathBuf> {
    vec![
        PathBuf::from("/usr/share/fonts"),
        PathBuf::from("/usr/local/share/fonts"),
    ]
}

/// The roots a `System` path may lie under, per OS (spec 0032, AC-3).
#[cfg(windows)]
fn system_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(windir) = std::env::var_os("WINDIR") {
        roots.push(PathBuf::from(windir).join("Fonts"));
    }
    roots
}

/// The current user's own font folder, when the OS has one (spec 0032,
/// AC-3). Read from the environment so the shipped binary needs no new
/// dependency for it.
fn user_font_root() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join("Library/Fonts"))
    }
    #[cfg(target_os = "linux")]
    {
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join(".local/share/fonts"))
    }
    #[cfg(windows)]
    {
        let local = std::env::var_os("LOCALAPPDATA")?;
        Some(PathBuf::from(local).join("Microsoft/Windows/Fonts"))
    }
}

/// The roots only the OS may write to, whose files are safe to map (spec
/// 0032, Mapped vs read). macOS System Integrity Protection guards them.
fn read_only_roots() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        vec![PathBuf::from("/System/Library")]
    }
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}

/// One file's live bytes, shared by every key of every page that uses it.
struct Cached {
    handle: Weak<dyn AsRef<[u8]> + Send + Sync>,
    storage: FontStorage,
}

/// The browser's font by reference cache (spec 0032, AC-2).
#[derive(Default)]
pub(crate) struct FontStore {
    files: HashMap<PathBuf, Cached>,
}

impl FontStore {
    /// A store with no files yet.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The bytes of `path`'s face `index`, if the content process may name
    /// it (spec 0032, AC-1, AC-3). One canonical path keeps one set of
    /// bytes, however many faces, keys and pages name it.
    pub(crate) fn system(
        &mut self,
        page: PageId,
        key: FontKey,
        path: &str,
        index: u32,
    ) -> Option<FontBytes> {
        let canonical = match std::fs::canonicalize(path) {
            Ok(canonical) => canonical,
            Err(err) => {
                warn!(page = page.0, key = key.0, path, %err, "font path refused: cannot resolve it");
                return None;
            }
        };
        let allowed = allowed_roots();
        if !is_allowed(&canonical, &allowed) {
            warn!(
                page = page.0,
                key = key.0,
                path = %canonical.display(),
                "font path refused: outside the font roots"
            );
            return None;
        }
        if let Some(cached) = self.files.get(&canonical)
            && let Some(bytes) = FontBytes::from_handle(&cached.handle, cached.storage)
        {
            return Some(bytes);
        }
        // Dead entries hold nothing; drop them before adding the new one.
        self.files
            .retain(|_, cached| cached.handle.strong_count() > 0);
        let mapped = is_read_only(&canonical);
        let bytes = if mapped {
            map_file(&canonical)
        } else {
            std::fs::read(&canonical).ok().map(FontBytes::heap)
        };
        let Some(bytes) = bytes else {
            warn!(
                page = page.0,
                key = key.0,
                path = %canonical.display(),
                "font file refused: cannot read it"
            );
            return None;
        };
        if skrifa::FontRef::from_index(&bytes, index).is_err() {
            warn!(
                page = page.0,
                key = key.0,
                path = %canonical.display(),
                index,
                "font file refused: not a readable font"
            );
            return None;
        }
        self.files.insert(
            canonical,
            Cached {
                handle: bytes.handle(),
                storage: bytes.storage(),
            },
        );
        Some(bytes)
    }
}

/// The allowed roots, canonical where they exist.
fn allowed_roots() -> Vec<PathBuf> {
    let mut roots = system_roots();
    if let Some(user) = user_font_root() {
        roots.push(user);
    }
    roots
        .into_iter()
        .filter_map(|root| std::fs::canonicalize(root).ok())
        .collect()
}

/// Whether `path` (already canonical) lies under one of `roots` (also
/// canonical). Compares whole path components, so `/System/Library/Fonts`
/// never admits `/System/Library/FontsExtra` (spec 0032, AC-3).
fn is_allowed(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

/// Whether the OS keeps `path` read only, so mapping it is safe.
fn is_read_only(path: &Path) -> bool {
    read_only_roots().iter().any(|root| path.starts_with(root))
}

/// Maps a read only font file, shared by both processes.
fn map_file(path: &Path) -> Option<FontBytes> {
    let file = std::fs::File::open(path).ok()?;
    // SAFETY: the caller checked the path lies under a root the OS keeps
    // read only (`read_only_roots`), so the file cannot be truncated or
    // rewritten while it is mapped; nothing writes through the map.
    #[allow(unsafe_code, reason = "Mmap::map is the one unsafe call; see SAFETY")]
    let map = unsafe { memmap2::Mmap::map(&file).ok()? };
    Some(FontBytes::mapped(map))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_root_admits_its_files_and_nothing_that_only_starts_like_it() {
        let roots = vec![PathBuf::from("/System/Library/Fonts")];
        assert!(is_allowed(
            Path::new("/System/Library/Fonts/SFNS.ttf"),
            &roots
        ));
        assert!(is_allowed(Path::new("/System/Library/Fonts"), &roots));
        assert!(!is_allowed(
            Path::new("/System/Library/FontsExtra/evil.ttf"),
            &roots
        ));
        assert!(!is_allowed(Path::new("/etc/passwd"), &roots));
    }

    #[test]
    fn dot_dot_is_resolved_before_the_root_check() {
        let dir = std::env::temp_dir().join("embed-font-roots-test");
        let inside = dir.join("fonts");
        std::fs::create_dir_all(&inside).ok();
        let escaped = inside.join("..").join("outside.ttf");
        std::fs::write(&escaped, b"x").ok();
        let canonical = std::fs::canonicalize(&escaped).ok();
        let root = std::fs::canonicalize(&inside).ok();
        if let (Some(canonical), Some(root)) = (canonical, root) {
            assert!(!is_allowed(&canonical, &[root]));
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_inside_a_root_pointing_outside_is_refused() {
        use std::os::unix::fs::symlink;

        let dir = std::env::temp_dir().join("embed-font-symlink-test");
        let root = dir.join("fonts");
        let outside = dir.join("outside.ttf");
        std::fs::create_dir_all(&root).ok();
        std::fs::write(&outside, b"x").ok();
        let link = root.join("link.ttf");
        let _ = std::fs::remove_file(&link);
        if symlink(&outside, &link).is_err() {
            return;
        }
        let canonical = std::fs::canonicalize(&link).ok();
        let root = std::fs::canonicalize(&root).ok();
        if let (Some(canonical), Some(root)) = (canonical, root) {
            // Canonicalizing resolves the link, so the check sees the real
            // file outside the root and refuses it (spec 0032, AC-3).
            assert!(!is_allowed(&canonical, &[root]));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn only_system_library_is_mapped() {
        assert!(is_read_only(Path::new("/System/Library/Fonts/SFNS.ttf")));
        assert!(!is_read_only(Path::new("/Library/Fonts/Someone.ttf")));
        assert!(!is_read_only(Path::new(
            "/Users/someone/Library/Fonts/Someone.ttf"
        )));
    }
}
