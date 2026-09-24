//! The pairing `yacs pair` saves, so later commands need no flags or phrase.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use yacs_core::Pairing;

#[derive(Serialize, Deserialize)]
pub struct Saved {
    pub server: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// `Pairing::to_secret`: the derived key, so no Argon2id on every run.
    pairing: String,
}

impl Saved {
    pub fn new(server: String, token: Option<String>, pairing: &Pairing) -> Self {
        Self {
            server,
            token,
            pairing: pairing.to_secret(),
        }
    }

    pub fn pairing(&self) -> Result<Pairing> {
        Pairing::from_secret(&self.pairing)
            .context("the saved pairing is damaged; run `yacs pair` again")
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

pub fn load(path: &Path) -> Result<Option<Saved>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let saved = serde_json::from_slice(&bytes)
        .with_context(|| format!("{} is damaged; run `yacs pair` again", path.display()))?;
    Ok(Some(saved))
}

/// Readable by the owner only: the file holds the channel key and token.
pub fn save(path: &Path, saved: &Saved) -> Result<()> {
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

pub fn remove(path: &Path) -> Result<bool> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
    }
}
