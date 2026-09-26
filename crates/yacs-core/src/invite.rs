//! One-time invites: a space's key, sealed and parked on the relay until the
//! invited device fetches it once.
//!
//! ```text
//! secret (32 random bytes, only in the link: https://relay/#join=v2.<secret>)
//! secret ── HKDF-SHA256(info = HKDF_INFO_SLOT) ── slot (where the relay keeps it)
//! secret ── HKDF-SHA256(info = HKDF_INFO_WRAP) ── key the invite is sealed with
//! ```
//!
//! The relay sees the slot and the sealed bytes, never the secret, so it can
//! neither open an invite nor find its slot from the link.

use core::fmt;
use core::str::FromStr;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use zeroize::Zeroize;

use crate::error::{Error, Result};
use crate::pairing::{ChannelId, ChannelKey, Pairing};
use crate::sealed;

pub const HKDF_INFO_SLOT: &[u8] = b"yacs/v2/invite/slot";
pub const HKDF_INFO_WRAP: &[u8] = b"yacs/v2/invite/key";
/// Bound into every sealed invite, with the slot.
const AAD_PREFIX: &[u8] = b"yacs/v2/invite";
/// What the relay takes for one invite: a name, a device name and a token fit easily.
pub const MAX_SEALED_INVITE: usize = 4096;
/// How long a relay keeps an unused invite at most.
pub const MAX_INVITE_TTL_SECS: u64 = 24 * 60 * 60;
/// In the link, before the secret.
const SECRET_PREFIX: &str = "v2.";

/// What an invite link carries. Anyone who has it can take the invite, once.
#[derive(Clone, PartialEq, Eq)]
pub struct InviteSecret([u8; 32]);

impl InviteSecret {
    pub fn generate() -> Result<Self> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|_| Error::Rng)?;
        Ok(Self(bytes))
    }

    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn slot(&self) -> InviteSlot {
        InviteSlot(self.expand(HKDF_INFO_SLOT))
    }

    pub fn seal(&self, invite: &Invite) -> Result<Vec<u8>> {
        seal_invite(&self.expand(HKDF_INFO_WRAP), &self.aad(), invite)
    }

    pub fn open(&self, sealed: &[u8]) -> Result<Invite> {
        open_invite(&self.expand(HKDF_INFO_WRAP), &self.aad(), sealed)
    }

    fn expand(&self, info: &[u8]) -> [u8; 32] {
        let mut out = [0u8; 32];
        Hkdf::<Sha256>::new(None, &self.0)
            .expand(info, &mut out)
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        out
    }

    fn aad(&self) -> Vec<u8> {
        [AAD_PREFIX, self.slot().as_bytes()].concat()
    }
}

/// An invite, sealed under `key`: by an invite secret, or by the key a code
/// exchange agreed on.
pub(crate) fn seal_invite(key: &[u8; 32], aad: &[u8], invite: &Invite) -> Result<Vec<u8>> {
    let mut plaintext = postcard::to_allocvec(invite).map_err(|_| Error::Malformed)?;
    let sealed = sealed::seal(key, aad, &plaintext);
    plaintext.zeroize();
    sealed
}

pub(crate) fn open_invite(key: &[u8; 32], aad: &[u8], sealed: &[u8]) -> Result<Invite> {
    let mut plaintext = sealed::open(key, aad, sealed)?;
    let invite = postcard::from_bytes(&plaintext).map_err(|_| Error::Malformed);
    plaintext.zeroize();
    invite
}

impl Drop for InviteSecret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for InviteSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InviteSecret(<redacted>)")
    }
}

/// `v2.<secret>`, base64url: the part of the link after `#join=`.
impl fmt::Display for InviteSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{SECRET_PREFIX}{}", URL_SAFE_NO_PAD.encode(self.0))
    }
}

impl FromStr for InviteSecret {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let encoded = s.strip_prefix(SECRET_PREFIX).ok_or(Error::InvalidInvite)?;
        let mut bytes = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| Error::InvalidInvite)?;
        let secret = <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| Error::InvalidInvite);
        bytes.zeroize();
        Ok(Self(secret?))
    }
}

/// Where the relay keeps an invite. Knowing it is enough to take the invite
/// (still sealed), so it stays between the inviter, the relay and the link.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct InviteSlot([u8; 32]);

impl InviteSlot {
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Base64url without padding, 43 characters, as in URLs.
impl fmt::Display for InviteSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&URL_SAFE_NO_PAD.encode(self.0))
    }
}

impl fmt::Debug for InviteSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "InviteSlot({self})")
    }
}

impl FromStr for InviteSlot {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let bytes = URL_SAFE_NO_PAD
            .decode(s)
            .map_err(|_| Error::InvalidInvite)?;
        Ok(Self(bytes.try_into().map_err(|_| Error::InvalidInvite)?))
    }
}

/// What the invited device gets: the space, and what to call it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invite {
    /// The inviter's name for the space, suggested to the invited device.
    pub space_name: String,
    /// The inviting device's name.
    pub inviter: String,
    /// The relay's access token, which the invited device needs as well
    /// until relays let members in without one.
    pub token: Option<String>,
    channel_id: [u8; 32],
    key: [u8; 32],
}

impl Invite {
    pub fn new(space_name: &str, inviter: &str, token: Option<&str>, pairing: &Pairing) -> Self {
        Self {
            space_name: space_name.to_owned(),
            inviter: inviter.to_owned(),
            token: token.map(str::to_owned),
            channel_id: *pairing.channel_id.as_bytes(),
            key: *pairing.key.as_bytes(),
        }
    }

    pub fn pairing(&self) -> Pairing {
        Pairing {
            channel_id: ChannelId::from_bytes(self.channel_id),
            key: ChannelKey::from_bytes(self.key),
        }
    }
}

impl Drop for Invite {
    fn drop(&mut self) {
        self.key.zeroize();
        if let Some(token) = &mut self.token {
            token.zeroize();
        }
    }
}

impl fmt::Debug for Invite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Invite")
            .field("space_name", &self.space_name)
            .field("inviter", &self.inviter)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invite() -> Invite {
        let pairing = Pairing::from_root(&[5; 32]);
        Invite::new("Anna & me", "MacBook", Some("s3cret"), &pairing)
    }

    #[test]
    fn seals_and_opens() {
        let secret = InviteSecret::generate().unwrap();
        let sealed = secret.seal(&invite()).unwrap();
        assert!(sealed.len() < MAX_SEALED_INVITE);
        assert_eq!(secret.open(&sealed).unwrap(), invite());
        assert_eq!(invite().pairing(), Pairing::from_root(&[5; 32]));
        assert_ne!(
            sealed,
            secret.seal(&invite()).unwrap(),
            "nonce must be fresh"
        );
    }

    #[test]
    fn only_its_secret_opens_it() {
        let secret = InviteSecret::from_bytes([1; 32]);
        let sealed = secret.seal(&invite()).unwrap();
        let other = InviteSecret::from_bytes([2; 32]);
        assert_eq!(other.open(&sealed), Err(Error::Decrypt));
        assert_ne!(other.slot(), secret.slot());

        let mut tampered = sealed.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert_eq!(secret.open(&tampered), Err(Error::Decrypt));
        assert_eq!(secret.open(&[]), Err(Error::Truncated));
        assert_eq!(secret.open(&[1, 1, 2]), Err(Error::Truncated));
        assert_eq!(secret.open(&[9; 40]), Err(Error::UnsupportedVersion(9)));
    }

    #[test]
    fn secret_and_slot_round_trip_through_strings() {
        let secret = InviteSecret::generate().unwrap();
        let s = secret.to_string();
        assert_eq!(s.len(), 3 + 43);
        assert_eq!(s.parse::<InviteSecret>(), Ok(secret.clone()));
        let slot = secret.slot();
        assert_eq!(slot.to_string().parse::<InviteSlot>(), Ok(slot));
        for bad in ["", "v2.", "v1.AAAA", &s[3..], &format!("{s}A")] {
            assert_eq!(
                bad.parse::<InviteSecret>(),
                Err(Error::InvalidInvite),
                "{bad}"
            );
        }
        assert_eq!("AAAA".parse::<InviteSlot>(), Err(Error::InvalidInvite));
        assert_eq!(format!("{secret:?}"), "InviteSecret(<redacted>)");
        assert!(!format!("{:?}", invite()).contains("s3cret"));
    }
}
