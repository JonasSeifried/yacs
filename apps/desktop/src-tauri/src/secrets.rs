//! The pairing (channel id + key) and the relay's access token, stored as one
//! keychain entry: macOS Keychain or Windows Credential Manager. One entry
//! means at most one keychain prompt.
//!
//! Debug builds use a plain file instead. Each rebuild changes an unsigned
//! binary's identity, so the keychain would ask again every time, and
//! dismissing the prompt left the app unpaired.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use yacs_core::{ChannelKey, Pairing};

const ACCOUNT: &str = "pairing";

#[derive(Debug, thiserror::Error)]
pub enum SecretsError {
    #[error("keychain error: {0}")]
    Keyring(#[from] keyring::Error),
    #[error("can't access the dev pairing file: {0}")]
    File(#[from] std::io::Error),
    #[error("the stored pairing is corrupt; pair this device again")]
    Corrupt,
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

pub enum Secrets {
    /// `service` is the app identifier, so dev and release builds with
    /// different identifiers don't overwrite each other's pairing.
    Keychain { service: String },
    /// Unencrypted, for debug builds only.
    File { path: PathBuf },
}

impl Secrets {
    /// The keychain in release builds, `dev-pairing.json` in debug builds.
    pub fn for_build(service: &str, config_dir: &Path) -> Self {
        if cfg!(debug_assertions) {
            let path = config_dir.join(DEV_FILE);
            tracing::warn!(path = %path.display(), "debug build: the pairing is stored unencrypted");
            Self::File { path }
        } else {
            Self::Keychain {
                service: service.into(),
            }
        }
    }

    pub fn load(&self) -> Result<Option<Stored>, SecretsError> {
        let blob = match self {
            Self::Keychain { service } => {
                match keyring::Entry::new(service, ACCOUNT)?.get_password() {
                    Ok(s) => s,
                    Err(keyring::Error::NoEntry) => return Ok(None),
                    Err(e) => return Err(e.into()),
                }
            }
            Self::File { path } => match fs::read_to_string(path) {
                Ok(s) => s,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e.into()),
            },
        };
        Blob::decode(&blob).map(Some).ok_or(SecretsError::Corrupt)
    }

    pub fn save(&self, stored: &Stored) -> Result<(), SecretsError> {
        let blob = Blob::encode(stored);
        match self {
            Self::Keychain { service } => {
                keyring::Entry::new(service, ACCOUNT)?.set_password(&blob)?
            }
            Self::File { path } => write_private(path, &blob)?,
        }
        Ok(())
    }

    pub fn delete(&self) -> Result<(), SecretsError> {
        match self {
            Self::Keychain { service } => {
                match keyring::Entry::new(service, ACCOUNT)?.delete_credential() {
                    Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                    Err(e) => Err(e.into()),
                }
            }
            Self::File { path } => match fs::remove_file(path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.into()),
            },
        }
    }
}

const DEV_FILE: &str = "dev-pairing.json";

/// Readable only by the current user (on Unix), written atomically.
fn write_private(path: &Path, contents: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    std::io::Write::write_all(&mut options.open(&tmp)?, contents.as_bytes())?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use yacs_core::ChannelId;

    use super::*;

    #[test]
    fn blob_round_trips() {
        let stored = Stored {
            pairing: Pairing {
                channel_id: ChannelId::from_bytes([3; 32]),
                key: ChannelKey::from_bytes([4; 32]),
            },
            token: Some("s3cret".into()),
        };
        let decoded = Blob::decode(&Blob::encode(&stored)).unwrap();
        assert_eq!(decoded.pairing, stored.pairing);
        assert_eq!(decoded.token, stored.token);
    }

    #[test]
    fn file_store_round_trips_and_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join(DEV_FILE);
        let secrets = Secrets::File { path: path.clone() };
        assert!(secrets.load().unwrap().is_none());
        secrets.delete().unwrap();

        let stored = Stored {
            pairing: Pairing {
                channel_id: ChannelId::from_bytes([3; 32]),
                key: ChannelKey::from_bytes([4; 32]),
            },
            token: None,
        };
        secrets.save(&stored).unwrap();
        assert_eq!(secrets.load().unwrap().unwrap().pairing, stored.pairing);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        fs::write(&path, "garbage").unwrap();
        assert!(matches!(secrets.load(), Err(SecretsError::Corrupt)));
        secrets.delete().unwrap();
        assert!(secrets.load().unwrap().is_none());
    }

    #[test]
    fn rejects_corrupt_blobs() {
        assert!(Blob::decode("nope").is_none());
        assert!(Blob::decode(r#"{"channel_id":"x","key":"00","token":null}"#).is_none());
    }
}
