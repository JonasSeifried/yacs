//! The pairing (channel id + key) and the relay's access token, stored in
//! `pairing.json` in the app's config dir, readable only by the user. The
//! `yacs` command keeps its pairing the same way.
//!
//! Versions up to 0.2.3 kept release pairings in the OS keychain and debug
//! pairings in `dev-pairing.json`; `migrate` moves them over.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use yacs_core::{ChannelKey, Pairing};

const FILE: &str = "pairing.json";

#[derive(Debug, thiserror::Error)]
pub enum SecretsError {
    #[error("can't access the pairing file: {0}")]
    File(#[from] std::io::Error),
    #[error("the stored pairing is corrupt; pair this device again")]
    Corrupt,
    #[error("keychain error: {0}")]
    Keychain(#[from] keyring::Error),
}

pub struct Stored {
    pub pairing: Pairing,
    pub token: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Blob {
    channel_id: String,
    key: String,
    token: Option<String>,
}

impl Blob {
    fn encode(stored: &Stored) -> String {
        let blob = Blob {
            channel_id: stored.pairing.channel_id.to_string(),
            key: hex::encode(stored.pairing.key.as_bytes()),
            token: stored.token.clone(),
        };
        serde_json::to_string(&blob).expect("blob always serializes")
    }

    fn decode(s: &str) -> Option<Stored> {
        let blob: Blob = serde_json::from_str(s).ok()?;
        let key: [u8; 32] = hex::decode(&blob.key).ok()?.try_into().ok()?;
        Some(Stored {
            pairing: Pairing {
                channel_id: blob.channel_id.parse().ok()?,
                key: ChannelKey::from_bytes(key),
            },
            token: blob.token,
        })
    }
}

pub struct Secrets {
    path: PathBuf,
}

impl Secrets {
    pub fn new(config_dir: &Path) -> Self {
        Self {
            path: config_dir.join(FILE),
        }
    }

    pub fn load(&self) -> Result<Option<Stored>, SecretsError> {
        let blob = match fs::read_to_string(&self.path) {
            Ok(s) => s,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        Blob::decode(&blob).map(Some).ok_or(SecretsError::Corrupt)
    }

    pub fn save(&self, stored: &Stored) -> Result<(), SecretsError> {
        Ok(write_private(&self.path, &Blob::encode(stored))?)
    }

    pub fn delete(&self) -> Result<(), SecretsError> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Moves the pairing from where versions up to 0.2.3 kept it, unless
    /// there's a pairing file already. Only the builds that wrote a location
    /// read it, so debug builds never trigger a keychain prompt. Failures are
    /// logged; the user can pair again.
    pub fn migrate(&self, keychain_service: &str) {
        if self.path.exists() {
            return;
        }
        let result = if cfg!(debug_assertions) {
            self.migrate_dev_file()
        } else {
            self.migrate_keychain(keychain_service)
        };
        match result {
            Ok(true) => {
                tracing::info!(path = %self.path.display(), "moved the pairing to its file")
            }
            Ok(false) => {}
            Err(e) => tracing::warn!(error = %e, "couldn't move the old pairing"),
        }
    }

    fn migrate_dev_file(&self) -> Result<bool, SecretsError> {
        match fs::rename(self.path.with_file_name(DEV_FILE), &self.path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// The keychain entry holds the same blob as the file. It's deleted only
    /// once the file is written.
    fn migrate_keychain(&self, service: &str) -> Result<bool, SecretsError> {
        let entry = keyring::Entry::new(service, KEYCHAIN_ACCOUNT)?;
        let blob = match entry.get_password() {
            Ok(blob) => blob,
            Err(keyring::Error::NoEntry) => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        if Blob::decode(&blob).is_none() {
            return Err(SecretsError::Corrupt);
        }
        write_private(&self.path, &blob)?;
        if let Err(e) = entry.delete_credential() {
            tracing::warn!(error = %e, "moved the pairing, but couldn't delete it from the keychain");
        }
        Ok(true)
    }
}

/// Up to 0.2.3: the debug builds' file, and the release builds' keychain
/// entry (service: the app identifier).
const DEV_FILE: &str = "dev-pairing.json";
const KEYCHAIN_ACCOUNT: &str = "pairing";

/// Readable only by the current user (on Unix), written atomically. On
/// Windows the app's config dir is already private to the user.
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
    std::io::Write::write_all(&mut options.open(&tmp)?, contents.as_bytes())?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use yacs_core::ChannelId;

    use super::*;

    fn stored() -> Stored {
        Stored {
            pairing: Pairing {
                channel_id: ChannelId::from_bytes([3; 32]),
                key: ChannelKey::from_bytes([4; 32]),
            },
            token: Some("s3cret".into()),
        }
    }

    #[test]
    fn blob_round_trips() {
        let stored = stored();
        let decoded = Blob::decode(&Blob::encode(&stored)).unwrap();
        assert_eq!(decoded.pairing, stored.pairing);
        assert_eq!(decoded.token, stored.token);
    }

    #[test]
    fn file_store_round_trips_and_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path().join("nested");
        let secrets = Secrets::new(&config_dir);
        assert!(secrets.load().unwrap().is_none());
        secrets.delete().unwrap();

        let stored = stored();
        secrets.save(&stored).unwrap();
        let loaded = secrets.load().unwrap().unwrap();
        assert_eq!(loaded.pairing, stored.pairing);
        assert_eq!(loaded.token, stored.token);
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
        assert!(matches!(secrets.load(), Err(SecretsError::Corrupt)));
        secrets.delete().unwrap();
        assert!(secrets.load().unwrap().is_none());
    }

    #[test]
    fn migrates_the_dev_file_once() {
        let dir = tempfile::tempdir().unwrap();
        let secrets = Secrets::new(dir.path());
        assert!(!secrets.migrate_dev_file().unwrap());

        write_private(&dir.path().join(DEV_FILE), &Blob::encode(&stored())).unwrap();
        assert!(secrets.migrate_dev_file().unwrap());
        assert!(!dir.path().join(DEV_FILE).exists());
        assert_eq!(secrets.load().unwrap().unwrap().pairing, stored().pairing);
    }

    /// Uses the real keychain: `cargo test -p yacs-desktop -- --ignored keychain`.
    #[test]
    #[ignore]
    fn migrates_the_keychain_entry() {
        let service = "com.jonasseifried.yacs.test";
        let entry = keyring::Entry::new(service, KEYCHAIN_ACCOUNT).unwrap();
        entry.set_password(&Blob::encode(&stored())).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let secrets = Secrets::new(dir.path());
        assert!(secrets.migrate_keychain(service).unwrap());
        let loaded = secrets.load().unwrap().unwrap();
        assert_eq!(loaded.pairing, stored().pairing);
        assert_eq!(loaded.token, stored().token);
        assert!(matches!(entry.get_password(), Err(keyring::Error::NoEntry)));
        assert!(!secrets.migrate_keychain(service).unwrap());
    }

    #[test]
    fn rejects_corrupt_blobs() {
        assert!(Blob::decode("nope").is_none());
        assert!(Blob::decode(r#"{"channel_id":"x","key":"00","token":null}"#).is_none());
    }
}
