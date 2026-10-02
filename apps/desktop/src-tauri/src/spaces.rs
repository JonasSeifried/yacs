//! The spaces this computer is in (see `yacs_client::spaces`), with their
//! keys and the relays' access tokens, stored in `spaces.json` in the app's
//! config dir, readable only by the user. The `yacs` command on this computer
//! uses the same file, so a space joined or left there is here too
//! (`commands::reread_spaces`).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Deserialize;
use yacs_client::spaces::{DEFAULT_SPACE_NAME, Space, Spaces};
use yacs_core::{ChannelKey, Pairing};

const FILE: &str = "spaces.json";
/// Up to 0.3: one pairing, whose relay was in `settings.json`.
const LEGACY_FILE: &str = "pairing.json";

#[derive(Debug, thiserror::Error)]
pub enum SpacesError {
    #[error("can't access the spaces file: {0}")]
    File(#[from] std::io::Error),
    #[error("the stored spaces are corrupt; join your space again")]
    Corrupt,
}

pub struct SpacesFile {
    dir: PathBuf,
    /// The last `load` failed, so the file may hold keys this app doesn't
    /// have: the next `save` moves it aside first.
    unreadable: AtomicBool,
}

impl SpacesFile {
    pub fn new(config_dir: &Path) -> Self {
        Self {
            dir: config_dir.to_owned(),
            unreadable: AtomicBool::new(false),
        }
    }

    /// Empty if there's nothing yet. A pairing from 0.3 or earlier becomes a
    /// space called "My devices", saved in the new file.
    pub fn load(&self) -> Result<Spaces, SpacesError> {
        let loaded = self.read_spaces();
        self.unreadable.store(loaded.is_err(), Ordering::SeqCst);
        loaded
    }

    /// What's in the file now, after `yacs` may have changed it; `None` if
    /// it can't be read. Unlike `load`, a failed read doesn't make the next
    /// `save` move the file aside: it may fail just while `yacs` replaces it.
    pub fn reread(&self) -> Option<Spaces> {
        let json = read(&self.dir.join(FILE)).ok()?;
        let Some(json) = json else {
            // `yacs leave` removes the file with the last space.
            return Some(Spaces::default());
        };
        let spaces: Spaces = serde_json::from_str(&json).ok()?;
        if spaces.spaces.iter().any(|s| s.pairing().is_err()) {
            return None;
        }
        // Readable again, e.g. because `yacs join` replaced a damaged file.
        self.unreadable.store(false, Ordering::SeqCst);
        Some(spaces)
    }

    fn read_spaces(&self) -> Result<Spaces, SpacesError> {
        let Some(json) = read(&self.dir.join(FILE))? else {
            return self.upgrade();
        };
        let spaces: Spaces = serde_json::from_str(&json).map_err(|_| SpacesError::Corrupt)?;
        if spaces.spaces.iter().any(|s| s.pairing().is_err()) {
            return Err(SpacesError::Corrupt);
        }
        Ok(spaces)
    }

    /// Never over a file `load` couldn't read: that one is moved to
    /// `spaces.json.corrupt-<unix secs>` first. Moving it at startup instead
    /// would lose the spaces to a read that only failed for a moment.
    pub fn save(&self, spaces: &Spaces) -> Result<(), SpacesError> {
        if self.unreadable.load(Ordering::SeqCst) {
            if let Some(aside) = yacs_client::spaces::set_aside(&self.dir.join(FILE))? {
                tracing::warn!(path = %aside.display(), "kept the spaces that couldn't be read aside");
            }
            self.unreadable.store(false, Ordering::SeqCst);
        }
        let json = serde_json::to_string_pretty(spaces).expect("spaces always serialize");
        Ok(write_private(&self.dir.join(FILE), &json)?)
    }

    fn upgrade(&self) -> Result<Spaces, SpacesError> {
        let legacy = self.dir.join(LEGACY_FILE);
        let Some(json) = read(&legacy)? else {
            return Ok(Spaces::default());
        };
        let relay = read(&self.dir.join("settings.json"))?
            .and_then(|s| serde_json::from_str::<LegacySettings>(&s).ok())
            .and_then(|s| s.server_url);
        let (Some(old), Some(relay)) = (serde_json::from_str::<Legacy>(&json).ok(), relay) else {
            return Err(SpacesError::Corrupt);
        };
        let key: [u8; 32] = hex::decode(&old.key)
            .ok()
            .and_then(|k| k.try_into().ok())
            .ok_or(SpacesError::Corrupt)?;
        let restored = Pairing {
            channel_id: old.channel_id.parse().map_err(|_| SpacesError::Corrupt)?,
            key: ChannelKey::from_bytes(key),
        };
        let mut spaces = Spaces::default();
        spaces.set_current(Space::new(DEFAULT_SPACE_NAME, &relay, &restored), old.token);
        self.save(&spaces)?;
        fs::remove_file(legacy)?;
        tracing::info!("moved the pairing to {FILE}");
        Ok(spaces)
    }
}

#[derive(Deserialize)]
struct Legacy {
    channel_id: String,
    /// Hex.
    key: String,
    token: Option<String>,
}

#[derive(Deserialize)]
struct LegacySettings {
    server_url: Option<String>,
}

fn read(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Readable only by the current user (on Unix), written atomically and on
/// disk before the rename, so a power cut leaves the old file or the new
/// one, never an empty one. On Windows the app's config dir is already
/// private to the user.
fn write_private(path: &Path, contents: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let _ = fs::remove_file(&tmp);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&tmp)?;
    std::io::Write::write_all(&mut file, contents.as_bytes())?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use yacs_core::ChannelId;

    use super::*;

    fn pairing() -> Pairing {
        Pairing {
            channel_id: ChannelId::from_bytes([3; 32]),
            key: ChannelKey::from_bytes([4; 32]),
        }
    }

    #[test]
    fn round_trips_privately() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path().join("nested");
        let file = SpacesFile::new(&config_dir);
        assert_eq!(file.load().unwrap(), Spaces::default());

        let mut spaces = Spaces::default();
        spaces.set_current(
            Space::new("Anna & me", "https://clip.example.com", &pairing()),
            Some("s3cret".into()),
        );
        file.save(&spaces).unwrap();
        assert_eq!(file.load().unwrap(), spaces);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(config_dir.join(FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        fs::write(config_dir.join(FILE), "garbage").unwrap();
        assert!(matches!(file.load(), Err(SpacesError::Corrupt)));
    }

    /// Joining again after that must not overwrite the keys that may still
    /// be in the file, and nothing moves until then.
    #[test]
    fn an_unreadable_file_is_kept_aside_when_saving() {
        let dir = tempfile::tempdir().unwrap();
        let file = SpacesFile::new(dir.path());
        fs::write(dir.path().join(FILE), "garbage").unwrap();
        assert!(matches!(file.load(), Err(SpacesError::Corrupt)));
        assert_eq!(
            fs::read_to_string(dir.path().join(FILE)).unwrap(),
            "garbage"
        );

        let mut spaces = Spaces::default();
        spaces.set_current(
            Space::new("Anna & me", "https://clip.example.com", &pairing()),
            None,
        );
        file.save(&spaces).unwrap();
        assert_eq!(file.load().unwrap(), spaces);
        let aside: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.to_string_lossy().contains("spaces.json.corrupt-"))
            .collect();
        assert_eq!(aside.len(), 1, "{aside:?}");
        assert_eq!(fs::read_to_string(&aside[0]).unwrap(), "garbage");

        // Saving again replaces the file as usual.
        file.save(&Spaces::default()).unwrap();
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    /// What the `yacs` command does to the file while the app runs.
    #[test]
    fn rereads_what_yacs_changed() {
        let dir = tempfile::tempdir().unwrap();
        let file = SpacesFile::new(dir.path());
        assert_eq!(file.reread(), Some(Spaces::default()));

        // A file `yacs` is just replacing reads as nothing new, and doesn't
        // make the next save move it aside.
        fs::write(dir.path().join(FILE), "garbage").unwrap();
        assert_eq!(file.reread(), None);

        let mut joined = Spaces::default();
        joined.set_current(
            Space::new("Server", "https://clip.example.com", &pairing()),
            None,
        );
        // As `yacs join` writes it, from a startup that found it damaged.
        assert!(file.load().is_err());
        fs::write(
            dir.path().join(FILE),
            serde_json::to_string(&joined).unwrap(),
        )
        .unwrap();
        assert_eq!(file.reread(), Some(joined.clone()));
        file.save(&joined).unwrap();
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);

        // `yacs leave` removes it with the last space.
        fs::remove_file(dir.path().join(FILE)).unwrap();
        assert_eq!(file.reread(), Some(Spaces::default()));
    }

    #[test]
    fn upgrades_the_pairing_from_before_spaces() {
        let dir = tempfile::tempdir().unwrap();
        let blob = format!(
            r#"{{"channel_id":"{}","key":"{}","token":"s3cret"}}"#,
            pairing().channel_id,
            hex::encode([4; 32])
        );
        fs::write(dir.path().join(LEGACY_FILE), blob).unwrap();
        fs::write(
            dir.path().join("settings.json"),
            r#"{"server_url":"https://clip.example.com","device_name":"PC"}"#,
        )
        .unwrap();

        let file = SpacesFile::new(dir.path());
        let spaces = file.load().unwrap();
        let space = spaces.current().unwrap();
        assert_eq!(space.name, DEFAULT_SPACE_NAME);
        assert_eq!(space.relay, "https://clip.example.com");
        assert_eq!(space.pairing().unwrap(), pairing());
        assert_eq!(spaces.token(&space.relay), Some("s3cret"));
        assert!(!dir.path().join(LEGACY_FILE).exists());
        assert_eq!(file.load().unwrap(), spaces);
    }

    #[test]
    fn a_broken_old_pairing_is_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join(LEGACY_FILE),
            r#"{"channel_id":"x","key":"00","token":null}"#,
        )
        .unwrap();
        fs::write(
            dir.path().join("settings.json"),
            r#"{"server_url":"https://a"}"#,
        )
        .unwrap();
        assert!(matches!(
            SpacesFile::new(dir.path()).load(),
            Err(SpacesError::Corrupt)
        ));
    }
}
