//! The spaces `yacs join` and `yacs space new` save, so later commands need
//! no flags. The format is shared with the desktop app, see `yacs_client::spaces`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use yacs_client::spaces::{DEFAULT_SPACE_NAME, Space, Spaces};
use yacs_core::Pairing;

/// `cli.json` up to 0.3: one pairing, no name.
#[derive(Deserialize)]
struct Legacy {
    server: String,
    token: Option<String>,
    pairing: String,
}

impl Legacy {
    fn upgrade(self) -> Option<Spaces> {
        let pairing = Pairing::from_secret(&self.pairing).ok()?;
        let mut spaces = Spaces::default();
        spaces.set_current(
            Space::new(DEFAULT_SPACE_NAME, &self.server, &pairing),
            self.token,
        );
        Some(spaces)
    }
}

/// `YACS_CONFIG`, or `yacs/cli.json` in the user's config directory
/// (`~/.config` on Linux).
pub fn path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("YACS_CONFIG") {
        return Ok(path.into());
    }
    let Some(dir) = dirs::config_dir() else {
        bail!("can't find a config directory; set YACS_CONFIG to a file path");
    };
    Ok(dir.join("yacs").join("cli.json"))
}

/// Empty if nothing is saved yet. A file from 0.3 or earlier becomes a space
/// called "My devices" (and is saved that way the next time it's saved).
pub fn load(path: &Path) -> Result<Spaces> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Spaces::default()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let damaged = || format!("{} is damaged; run `yacs join` again", path.display());
    if let Ok(legacy) = serde_json::from_slice::<Legacy>(&bytes) {
        return legacy.upgrade().with_context(damaged);
    }
    let spaces: Spaces = serde_json::from_slice(&bytes).with_context(damaged)?;
    if spaces.spaces.iter().any(|s| s.pairing().is_err()) {
        bail!(damaged());
    }
    Ok(spaces)
}

/// Readable by the owner only: the file holds the spaces' keys and tokens.
pub fn save(path: &Path, saved: &Spaces) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let json = serde_json::to_vec_pretty(saved).expect("config always serializes");
    let tmp = path.with_extension("tmp");
    write_private(&tmp, &json).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let _ = std::fs::remove_file(path);
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?
        .write_all(bytes)
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    // The profile directory is already private to the user on Windows.
    std::fs::write(path, bytes)
}

/// Removes the file once no space is left, rather than leaving an empty one.
pub fn save_or_remove(path: &Path, saved: &Spaces) -> Result<()> {
    if !saved.spaces.is_empty() {
        return save(path, saved);
    }
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use yacs_core::{ChannelId, ChannelKey};

    use super::*;

    #[test]
    fn upgrades_a_pairing_saved_before_spaces() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cli.json");
        assert_eq!(load(&path).unwrap(), Spaces::default());

        let pairing = Pairing {
            channel_id: ChannelId::from_bytes([7; 32]),
            key: ChannelKey::from_bytes([9; 32]),
        };
        let legacy = format!(
            r#"{{"server":"https://clip.example.com","token":"s3cret","pairing":"{}"}}"#,
            pairing.to_secret()
        );
        std::fs::write(&path, legacy).unwrap();
        let spaces = load(&path).unwrap();
        let space = spaces.current().unwrap();
        assert_eq!(space.name, DEFAULT_SPACE_NAME);
        assert_eq!(space.relay, "https://clip.example.com");
        assert_eq!(space.pairing().unwrap(), pairing);
        assert_eq!(spaces.token("https://clip.example.com"), Some("s3cret"));

        save(&path, &spaces).unwrap();
        assert_eq!(load(&path).unwrap(), spaces);
        save_or_remove(&path, &Spaces::default()).unwrap();
        assert!(!path.exists());

        std::fs::write(&path, r#"{"server":"x","pairing":"v1.nope"}"#).unwrap();
        assert!(load(&path).is_err());
        std::fs::write(
            &path,
            r#"{"spaces":[{"name":"a","relay":"b","secret":"v1.nope"}]}"#,
        )
        .unwrap();
        assert!(load(&path).is_err());
    }
}
