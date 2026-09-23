//! The encrypted wire format. This is all the server ever sees.
//!
//! ```text
//! version (1 byte) || nonce (24 bytes) || XChaCha20-Poly1305 ciphertext + tag
//! AAD = version || channel id
//! ```
//!
//! Binding the channel id into the AAD means a ciphertext copied into another
//! channel fails to decrypt, even if both channels happened to share a key.

use chacha20poly1305::aead::{Aead, Generate, KeyInit, Payload as AeadPayload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use zeroize::Zeroize;

use crate::error::{Error, Result};
use crate::pairing::{ChannelId, Pairing};
use crate::payload::Payload;

pub const PROTOCOL_VERSION: u8 = 1;
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;
const HEADER_LEN: usize = 1 + NONCE_LEN;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    nonce: [u8; NONCE_LEN],
    ciphertext: Vec<u8>,
}

impl Envelope {
    /// Encrypt a payload for the pairing's channel, with a fresh random nonce.
    pub fn seal(pairing: &Pairing, payload: &Payload) -> Result<Self> {
        let nonce = XNonce::try_generate().map_err(|_| Error::Rng)?;
        let mut plaintext = payload.to_bytes();
        let ciphertext = cipher(pairing).encrypt(
            &nonce,
            AeadPayload {
                msg: &plaintext,
                aad: &aad(&pairing.channel_id),
            },
        );
        plaintext.zeroize();
        Ok(Self {
            nonce: nonce.into(),
            ciphertext: ciphertext.map_err(|_| Error::Encrypt)?,
        })
    }

    /// Decrypt and decode. Fails with [`Error::Decrypt`] on a wrong key, a wrong
    /// channel, or any modification of the envelope.
    pub fn open(&self, pairing: &Pairing) -> Result<Payload> {
        let mut plaintext = cipher(pairing)
            .decrypt(
                &XNonce::from(self.nonce),
                AeadPayload {
                    msg: &self.ciphertext,
                    aad: &aad(&pairing.channel_id),
                },
            )
            .map_err(|_| Error::Decrypt)?;
        let payload = Payload::from_bytes(&plaintext);
        plaintext.zeroize();
        payload
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(HEADER_LEN + self.ciphertext.len());
        bytes.push(PROTOCOL_VERSION);
        bytes.extend_from_slice(&self.nonce);
        bytes.extend_from_slice(&self.ciphertext);
        bytes
    }

    /// Parse the wire format. Only checks structure; authenticity is checked by [`open`](Self::open).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let (&version, rest) = bytes.split_first().ok_or(Error::Truncated)?;
        if version != PROTOCOL_VERSION {
            return Err(Error::UnsupportedVersion(version));
        }
        if rest.len() < NONCE_LEN + TAG_LEN {
            return Err(Error::Truncated);
        }
        let (nonce, ciphertext) = rest.split_at(NONCE_LEN);
        Ok(Self {
            nonce: nonce.try_into().expect("split at NONCE_LEN"),
            ciphertext: ciphertext.to_vec(),
        })
    }
}

fn cipher(pairing: &Pairing) -> XChaCha20Poly1305 {
    XChaCha20Poly1305::new(&Key::from(*pairing.key.as_bytes()))
}

fn aad(channel_id: &ChannelId) -> [u8; 33] {
    let mut aad = [0u8; 33];
    aad[0] = PROTOCOL_VERSION;
    aad[1..].copy_from_slice(channel_id.as_bytes());
    aad
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pairing::{ChannelKey, Pairing};
    use crate::payload::{Clip, ClipItem};

    fn pairing(id: u8, key: u8) -> Pairing {
        Pairing {
            channel_id: ChannelId::from_bytes([id; 32]),
            key: ChannelKey::from_bytes([key; 32]),
        }
    }

    fn payload() -> Payload {
        Payload::Clip(Clip {
            created_at_ms: 0,
            device_name: "test".into(),
            items: vec![ClipItem::Text("secret clipboard".into())],
        })
    }

    #[test]
    fn seal_open_round_trip_through_bytes() {
        let p = pairing(1, 2);
        let bytes = Envelope::seal(&p, &payload()).unwrap().to_bytes();
        assert_eq!(bytes[0], PROTOCOL_VERSION);
        assert_eq!(
            Envelope::from_bytes(&bytes).unwrap().open(&p),
            Ok(payload())
        );
    }

    #[test]
    fn nonces_are_fresh() {
        let p = pairing(1, 2);
        let a = Envelope::seal(&p, &payload()).unwrap();
        let b = Envelope::seal(&p, &payload()).unwrap();
        assert_ne!(a.nonce, b.nonce);
        assert_ne!(a.ciphertext, b.ciphertext);
    }

    #[test]
    fn ciphertext_hides_plaintext() {
        let bytes = Envelope::seal(&pairing(1, 2), &payload())
            .unwrap()
            .to_bytes();
        assert!(!bytes.windows(6).any(|w| w == b"secret"));
    }

    #[test]
    fn wrong_key_fails() {
        let env = Envelope::seal(&pairing(1, 2), &payload()).unwrap();
        assert_eq!(env.open(&pairing(1, 3)), Err(Error::Decrypt));
    }

    #[test]
    fn wrong_channel_fails_even_with_same_key() {
        let env = Envelope::seal(&pairing(1, 2), &payload()).unwrap();
        assert_eq!(env.open(&pairing(9, 2)), Err(Error::Decrypt));
    }

    #[test]
    fn any_flipped_bit_fails() {
        let p = pairing(1, 2);
        let bytes = Envelope::seal(&p, &payload()).unwrap().to_bytes();
        for i in 1..bytes.len() {
            let mut tampered = bytes.clone();
            tampered[i] ^= 0x01;
            let env = Envelope::from_bytes(&tampered).unwrap();
            assert_eq!(env.open(&p), Err(Error::Decrypt), "byte {i}");
        }
    }

    #[test]
    fn from_bytes_rejects_bad_structure() {
        assert_eq!(Envelope::from_bytes(&[]), Err(Error::Truncated));
        assert_eq!(
            Envelope::from_bytes(&[2; 64]),
            Err(Error::UnsupportedVersion(2))
        );
        assert_eq!(
            Envelope::from_bytes(&[PROTOCOL_VERSION; HEADER_LEN + TAG_LEN - 1]),
            Err(Error::Truncated)
        );
    }
}
