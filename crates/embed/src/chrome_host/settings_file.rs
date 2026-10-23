//! `settings.toml` on disk (spec 0003, AC-11): load with fallbacks, save
//! atomically. Browser process only; the content process never sees it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process;

use tracing::{info, warn};
use ui::{MAX_SETTINGS_BYTES, Settings, SettingsDocument};

/// The file name inside the app's config directory.
const FILE_NAME: &str = "settings.toml";

/// The settings file and its last read or written contents.
pub(crate) struct SettingsFile {
    /// `None` when the OS has no config directory: settings live in memory.
    path: Option<PathBuf>,
    document: SettingsDocument,
}

impl SettingsFile {
    /// Loads `<config dir>/<app_id>/settings.toml`.
    pub(crate) fn load(app_id: &str) -> (Self, Settings) {
        let path = dirs::config_dir().map(|dir| dir.join(app_id).join(FILE_NAME));
        if path.is_none() {
            warn!("the OS gave no config directory, settings will not be saved");
        }
        Self::load_from(path)
    }

    /// Loads the file at `path`. Never fails: a missing file gives
    /// defaults, an unreadable or unparsable one is renamed to
    /// `settings.toml.bak` first, and a bad value falls back on its own.
    pub(crate) fn load_from(path: Option<PathBuf>) -> (Self, Settings) {
        let document = match &path {
            Some(path) => read(path),
            None => SettingsDocument::default(),
        };
        let (settings, warnings) = document.settings();
        for warning in warnings {
            warn!(%warning, "settings value ignored");
        }
        (Self { path, document }, settings)
    }

    /// Saves `settings`, keeping the keys this build does not know. The
    /// write goes to a temp file renamed over the old one, so the file is
    /// never half written. Last write wins between two instances.
    pub(crate) fn save(&mut self, settings: &Settings) {
        self.document.store(settings);
        let Some(path) = &self.path else {
            return;
        };
        if let Err(err) = write_atomic(path, &self.document.to_toml()) {
            warn!(%err, path = %path.display(), "could not save settings");
        }
    }
}

fn read(path: &Path) -> SettingsDocument {
    let text = match fs::metadata(path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return SettingsDocument::default();
        }
        Ok(meta) if meta.len() > MAX_SETTINGS_BYTES as u64 => {
            Err(format!("it is over {MAX_SETTINGS_BYTES} bytes"))
        }
        Ok(_) => fs::read_to_string(path).map_err(|err| err.to_string()),
        Err(err) => Err(err.to_string()),
    };
    match text.and_then(|text| SettingsDocument::parse(&text).map_err(|err| err.to_string())) {
        Ok(document) => {
            info!(path = %path.display(), "settings loaded");
            document
        }
        Err(reason) => {
            let backup = path.with_extension("toml.bak");
            match fs::rename(path, &backup) {
                Ok(()) => {
                    warn!(%reason, backup = %backup.display(), "settings file unusable, moved aside, using defaults")
                }
                Err(err) => {
                    warn!(%reason, %err, "settings file unusable and could not be moved aside, using defaults")
                }
            }
            SettingsDocument::default()
        }
    }
}

fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    // One temp name per process, so two instances never share a half written file.
    let temp = path.with_extension(format!("toml.{}.tmp", process::id()));
    fs::write(&temp, text)?;
    fs::rename(&temp, path).inspect_err(|_| {
        let _ = fs::remove_file(&temp);
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use ui::{Theme, UiZoom};

    use super::*;

    /// A fresh, empty directory under the system temp dir.
    fn temp_dir() -> PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "embed-settings-test-{}-{}",
            process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_file_gives_defaults_and_save_creates_it() {
        let dir = temp_dir();
        let path = dir.join("app").join(FILE_NAME);
        let (mut file, settings) = SettingsFile::load_from(Some(path.clone()));
        assert_eq!(settings, Settings::default());

        let dark = Settings {
            theme: Theme::Dark,
            ..settings
        };
        file.save(&dark);
        assert_eq!(
            SettingsFile::load_from(Some(path)).1,
            dark,
            "survives a restart"
        );
    }

    #[test]
    fn corrupt_file_is_backed_up_and_defaults_apply() {
        let dir = temp_dir();
        let path = dir.join(FILE_NAME);
        fs::write(&path, "appearance = [").unwrap();
        let (_, settings) = SettingsFile::load_from(Some(path.clone()));
        assert_eq!(settings, Settings::default());
        assert!(!path.exists());
        assert_eq!(
            fs::read_to_string(dir.join("settings.toml.bak")).unwrap(),
            "appearance = ["
        );
    }

    #[test]
    fn oversized_file_is_backed_up() {
        let dir = temp_dir();
        let path = dir.join(FILE_NAME);
        fs::write(&path, format!("# {}\n", "x".repeat(MAX_SETTINGS_BYTES))).unwrap();
        let (_, settings) = SettingsFile::load_from(Some(path.clone()));
        assert_eq!(settings, Settings::default());
        assert!(dir.join("settings.toml.bak").exists());
    }

    #[test]
    fn invalid_utf8_is_backed_up() {
        let dir = temp_dir();
        let path = dir.join(FILE_NAME);
        fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
        SettingsFile::load_from(Some(path.clone()));
        assert!(dir.join("settings.toml.bak").exists());
    }

    #[test]
    fn save_keeps_unknown_keys_and_leaves_no_temp_file() {
        let dir = temp_dir();
        let path = dir.join(FILE_NAME);
        fs::write(
            &path,
            "[appearance]\nui_zoom = 125\n[future]\nkey = \"kept\"\n",
        )
        .unwrap();
        let (mut file, settings) = SettingsFile::load_from(Some(path.clone()));
        assert_eq!(settings.ui_zoom, UiZoom::nearest(125));
        file.save(&Settings {
            theme: Theme::Light,
            ..settings
        });

        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("key = \"kept\""), "{text}");
        assert!(text.contains("theme = \"light\""), "{text}");
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn no_config_dir_keeps_settings_in_memory() {
        let (mut file, settings) = SettingsFile::load_from(None);
        assert_eq!(settings, Settings::default());
        file.save(&settings); // no panic, nothing written
    }
}
