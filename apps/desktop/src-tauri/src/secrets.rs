//! The pairing (channel id + key) and the relay's access token, stored in
//! `pairing.json` in the app's config dir, readable only by the user. The
//! `yacs` command keeps its pairing the same way.

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
}

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
    fn rejects_corrupt_blobs() {
        assert!(Blob::decode("nope").is_none());
        assert!(Blob::decode(r#"{"channel_id":"x","key":"00","token":null}"#).is_none());
    }
}
