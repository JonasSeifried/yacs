//! The pairing (channel id + key) and the relay's access token, stored as one
//! keychain entry: macOS Keychain or Windows Credential Manager. One entry
//! means at most one keychain prompt.

use serde::{Deserialize, Serialize};
use yacs_core::{ChannelKey, Pairing};

const ACCOUNT: &str = "pairing";

#[derive(Debug, thiserror::Error)]
pub enum SecretsError {
    #[error("keychain error: {0}")]
    Keyring(#[from] keyring::Error),
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

/// `service` is the app identifier, so dev and release builds with different
/// identifiers don't overwrite each other's pairing.
pub struct Secrets {
    service: String,
}

impl Secrets {
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn entry(&self) -> Result<keyring::Entry, SecretsError> {
        Ok(keyring::Entry::new(&self.service, ACCOUNT)?)
    }

    pub fn load(&self) -> Result<Option<Stored>, SecretsError> {
        match self.entry()?.get_password() {
            Ok(s) => Blob::decode(&s).map(Some).ok_or(SecretsError::Corrupt),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, stored: &Stored) -> Result<(), SecretsError> {
        Ok(self.entry()?.set_password(&Blob::encode(stored))?)
    }

    pub fn delete(&self) -> Result<(), SecretsError> {
        match self.entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
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
    fn rejects_corrupt_blobs() {
        assert!(Blob::decode("nope").is_none());
        assert!(Blob::decode(r#"{"channel_id":"x","key":"00","token":null}"#).is_none());
    }
}
