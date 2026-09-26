//! Non-secret preferences, stored as JSON in the app's config dir.
//! Spaces (keys, access tokens) live in their own file, see `spaces`.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// `CommandOrControl` is Cmd on macOS and Ctrl on Windows.
pub const DEFAULT_HOTKEY: &str = "CommandOrControl+Shift+Space";
pub const DEFAULT_TTL_SECS: u64 = 15 * 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub device_name: String,
    pub hotkey: String,
    /// Preselected expiry in the Spotlight TTL dropdown.
    pub default_ttl_secs: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            device_name: gethostname::gethostname().to_string_lossy().into_owned(),
            hotkey: DEFAULT_HOTKEY.into(),
            default_ttl_secs: DEFAULT_TTL_SECS,
        }
    }
}

pub struct SettingsFile {
    path: PathBuf,
}

impl SettingsFile {
    pub fn new(config_dir: &Path) -> Self {
        Self {
            path: config_dir.join("settings.json"),
        }
    }

    /// Missing or unreadable settings fall back to defaults rather than
    /// keeping the app from starting.
    pub fn load(&self) -> Settings {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!(path = %self.path.display(), error = %e, "ignoring corrupt settings");
                Settings::default()
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Settings::default(),
            Err(e) => {
                tracing::warn!(path = %self.path.display(), error = %e, "can't read settings");
                Settings::default()
            }
        }
    }

    /// Written to a temp file and renamed, so a crash can't leave half a file.
    pub fn save(&self, settings: &Settings) -> io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(settings)?)?;
        std::fs::rename(&tmp, &self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_defaults_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let file = SettingsFile::new(dir.path());
        assert_eq!(file.load(), Settings::default());

        let settings = Settings {
            device_name: "MacBook".into(),
            hotkey: "Alt+Space".into(),
            default_ttl_secs: 3600,
        };
        file.save(&settings).unwrap();
        assert_eq!(file.load(), settings);
    }

    #[test]
    fn fills_in_fields_added_by_newer_versions() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("settings.json"), r#"{"device_name":"PC"}"#).unwrap();
        let settings = SettingsFile::new(dir.path()).load();
        assert_eq!(settings.device_name, "PC");
        assert_eq!(settings.hotkey, DEFAULT_HOTKEY);
    }

    #[test]
    fn corrupt_file_falls_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("settings.json"), b"{not json").unwrap();
        assert_eq!(SettingsFile::new(dir.path()).load(), Settings::default());
    }
}
