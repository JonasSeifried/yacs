//! A space's public channel id and private key, both derived from 32 random
//! root bytes.
//!
//! ```text
//! root ── HKDF-SHA256(info = HKDF_INFO_CHANNEL) ── channel id  (sent to the server)
//! root ── HKDF-SHA256(info = HKDF_INFO_KEY)     ── channel key (never leaves the device)
//! ```
//!
//! Every client must derive byte-identical values, so all parameters here are
//! part of protocol version 1. Changing any of them requires a new version.
//! Spaces made up to 0.3 derived their root from a phrase with Argon2id;
//! their stored channel id and key work the same.

use core::fmt;
use core::str::FromStr;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroize;

use crate::error::{Error, Result};

pub const HKDF_INFO_CHANNEL: &[u8] = b"yacs/v1/channel";
pub const HKDF_INFO_KEY: &[u8] = b"yacs/v1/key";

/// Public identifier of a channel. Not secret from the server, but anyone who
/// knows it can list, fetch (still encrypted) and delete the channel's clips,
/// so don't publish it.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChannelId([u8; 32]);

impl ChannelId {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Base64url without padding, 43 characters. This is the form used in URLs.
impl fmt::Display for ChannelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&URL_SAFE_NO_PAD.encode(self.0))
    }
}

impl fmt::Debug for ChannelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ChannelId({self})")
    }
}

impl FromStr for ChannelId {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let bytes = URL_SAFE_NO_PAD
            .decode(s)
            .map_err(|_| Error::InvalidChannelId)?;
        let bytes: [u8; 32] = bytes.try_into().map_err(|_| Error::InvalidChannelId)?;
        Ok(Self(bytes))
    }
}

/// Secret symmetric key of a channel. Zeroed on drop, never printed.
#[derive(Clone, PartialEq, Eq)]
pub struct ChannelKey([u8; 32]);

impl ChannelKey {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Raw key bytes, e.g. for storing the pairing.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl Drop for ChannelKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for ChannelKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ChannelKey(<redacted>)")
    }
}

/// Everything a device needs to take part in a channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pairing {
    pub channel_id: ChannelId,
    pub key: ChannelKey,
}

impl Pairing {
    /// A new space: fresh random root bytes.
    pub fn generate() -> Result<Self> {
        let mut root = [0u8; 32];
        getrandom::fill(&mut root).map_err(|_| Error::Rng)?;
        let pairing = Self::from_root(&root);
        root.zeroize();
        Ok(pairing)
    }

    /// The channel id and key of the space with this root.
    pub fn from_root(root: &[u8; 32]) -> Self {
        let hkdf = Hkdf::<Sha256>::new(None, root);
        let mut channel_id = [0u8; 32];
        let mut key = [0u8; 32];
        hkdf.expand(HKDF_INFO_CHANNEL, &mut channel_id)
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        hkdf.expand(HKDF_INFO_KEY, &mut key)
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        Self {
            channel_id: ChannelId(channel_id),
            key: ChannelKey(key),
        }
    }

    /// The space as one string, `v1.<channel id>.<key>` (base64url), for
    /// storing it, pairing links and `YACS_SPACE`. Anyone with it can read
    /// and send the space's clips.
    pub fn to_secret(&self) -> String {
        format!(
            "{SECRET_PREFIX}{}.{}",
            self.channel_id,
            URL_SAFE_NO_PAD.encode(self.key.0)
        )
    }

    pub fn from_secret(secret: &str) -> Result<Self> {
        let rest = secret
            .strip_prefix(SECRET_PREFIX)
            .ok_or(Error::InvalidSecret)?;
        let (channel_id, key) = rest.split_once('.').ok_or(Error::InvalidSecret)?;
        let channel_id = channel_id.parse().map_err(|_| Error::InvalidSecret)?;
        let mut bytes = URL_SAFE_NO_PAD
            .decode(key)
            .map_err(|_| Error::InvalidSecret)?;
        let key = <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| Error::InvalidSecret);
        bytes.zeroize();
        Ok(Self {
            channel_id,
            key: ChannelKey(key?),
        })
    }
}

const SECRET_PREFIX: &str = "v1.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_spaces_differ() {
        let a = Pairing::generate().unwrap();
        let b = Pairing::generate().unwrap();
        assert_ne!(a.channel_id, b.channel_id);
        assert_ne!(a.key, b.key);
        assert_ne!(a.channel_id.as_bytes(), a.key.as_bytes());
    }

    #[test]
    fn channel_id_round_trips_through_string() {
        let id = ChannelId::from_bytes([7; 32]);
        let s = id.to_string();
        assert_eq!(s.len(), 43);
        assert_eq!(s.parse::<ChannelId>(), Ok(id));
    }

    #[test]
    fn channel_id_rejects_bad_input() {
        assert_eq!("".parse::<ChannelId>(), Err(Error::InvalidChannelId));
        assert_eq!(
            "not base64!".parse::<ChannelId>(),
            Err(Error::InvalidChannelId)
        );
        assert_eq!("AAAA".parse::<ChannelId>(), Err(Error::InvalidChannelId));
    }

    #[test]
    fn secret_round_trips_and_rejects_garbage() {
        let pairing = Pairing {
            channel_id: ChannelId::from_bytes([7; 32]),
            key: ChannelKey::from_bytes([9; 32]),
        };
        let secret = pairing.to_secret();
        assert_eq!(secret.len(), 3 + 43 + 1 + 43);
        assert_eq!(Pairing::from_secret(&secret), Ok(pairing));

        let (channel, key) = secret[3..].split_once('.').unwrap();
        for bad in [
            "",
            "v1.",
            &secret[3..],
            &format!("v2.{channel}.{key}"),
            &format!("v1.{channel}"),
            &format!("v1.{channel}.{}", &key[..40]),
            &format!("v1.{}.{key}", &channel[..40]),
            &format!("v1.{channel}.{key}.extra"),
        ] {
            assert_eq!(
                Pairing::from_secret(bad),
                Err(Error::InvalidSecret),
                "{bad}"
            );
        }
    }

    #[test]
    fn key_debug_is_redacted() {
        let key = ChannelKey::from_bytes([1; 32]);
        assert_eq!(format!("{key:?}"), "ChannelKey(<redacted>)");
    }
}
