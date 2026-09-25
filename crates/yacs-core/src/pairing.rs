//! Turning a shared pairing phrase into a public channel id and a private key.
//!
//! ```text
//! phrase ── normalize ── Argon2id(salt = ARGON2_SALT) ── root
//! root   ── HKDF-SHA256(info = HKDF_INFO_CHANNEL)     ── channel id  (sent to the server)
//! root   ── HKDF-SHA256(info = HKDF_INFO_KEY)         ── channel key (never leaves the device)
//! ```
//!
//! Every client must derive byte-identical values, so all parameters here are
//! part of protocol version 1. Changing any of them requires a new version.

use core::fmt;
use core::str::FromStr;

use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hkdf::Hkdf;
use sha2::Sha256;
use unicode_normalization::UnicodeNormalization;
use zeroize::Zeroize;

use crate::error::{Error, Result};

/// Argon2id memory cost in KiB (64 MiB).
pub const ARGON2_M_COST: u32 = 64 * 1024;
/// Argon2id iterations.
pub const ARGON2_T_COST: u32 = 3;
/// Argon2id lanes.
pub const ARGON2_P_COST: u32 = 1;
/// Fixed on purpose: both devices must derive the same root without talking to each other.
pub const ARGON2_SALT: &[u8] = b"yacs/v1/argon2id";
pub const HKDF_INFO_CHANNEL: &[u8] = b"yacs/v1/channel";
pub const HKDF_INFO_KEY: &[u8] = b"yacs/v1/key";

/// Canonical form of a phrase: NFKC, lowercase, words separated by single spaces.
///
/// This makes `"  Correct HORSE\tbattery "` and `"correct horse battery"` pair
/// with each other, which matters when a phrase is typed on a phone keyboard.
pub fn normalize_phrase(phrase: &str) -> String {
    let normalized: String = phrase.nfkc().collect::<String>().to_lowercase();
    normalized.split_whitespace().collect::<Vec<_>>().join(" ")
}

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
    /// Derive a pairing from a phrase. Deliberately slow (Argon2id, ~64 MiB),
    /// so run it once at pairing time and store the result.
    pub fn from_phrase(phrase: &str) -> Result<Self> {
        let phrase = normalize_phrase(phrase);
        if phrase.is_empty() {
            return Err(Error::EmptyPhrase);
        }

        let params = Params::new(ARGON2_M_COST, ARGON2_T_COST, ARGON2_P_COST, Some(32))
            .expect("argon2 params are valid constants");
        let mut root = [0u8; 32];
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password_into(phrase.as_bytes(), ARGON2_SALT, &mut root)
            .expect("argon2 salt and output length are valid constants");

        let hkdf = Hkdf::<Sha256>::new(None, &root);
        root.zeroize();

        let mut channel_id = [0u8; 32];
        let mut key = [0u8; 32];
        hkdf.expand(HKDF_INFO_CHANNEL, &mut channel_id)
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        hkdf.expand(HKDF_INFO_KEY, &mut key)
            .expect("32 bytes is a valid HKDF-SHA256 output length");

        Ok(Self {
            channel_id: ChannelId(channel_id),
            key: ChannelKey(key),
        })
    }

    /// The derived pairing as one string, `v1.<channel id>.<key>` (base64url),
    /// for pairing links: a device given this skips the phrase and Argon2id.
    /// As secret as the phrase itself.
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
    fn normalizes_case_whitespace_and_unicode() {
        assert_eq!(
            normalize_phrase("  Correct   HORSE\tbattery\nstaple "),
            "correct horse battery staple"
        );
        // Fullwidth letters and ligatures fold to their plain forms under NFKC.
        assert_eq!(normalize_phrase("Ｃafé ﬁle"), "café file");
    }

    #[test]
    fn rejects_empty_phrase() {
        assert_eq!(Pairing::from_phrase(" \t "), Err(Error::EmptyPhrase));
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
