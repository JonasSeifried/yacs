//! Small sealed messages under a derived key: invites and the code exchange.
//!
//! ```text
//! format (1 byte) || nonce (24 bytes) || XChaCha20-Poly1305 ciphertext + tag
//! ```

use chacha20poly1305::aead::{Aead, Generate, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};

use crate::error::{Error, Result};

const FORMAT: u8 = 1;
const NONCE_LEN: usize = 24;

pub(crate) fn seal(key: &[u8; 32], aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
    let nonce = XNonce::try_generate().map_err(|_| Error::Rng)?;
    let ciphertext = cipher(key)
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| Error::Encrypt)?;
    let mut sealed = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
    sealed.push(FORMAT);
    sealed.extend_from_slice(&nonce);
    sealed.extend_from_slice(&ciphertext);
    Ok(sealed)
}

pub(crate) fn open(key: &[u8; 32], aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>> {
    let (&format, rest) = sealed.split_first().ok_or(Error::Truncated)?;
    if format != FORMAT {
        return Err(Error::UnsupportedVersion(format));
    }
    if rest.len() < NONCE_LEN {
        return Err(Error::Truncated);
    }
    let (nonce, ciphertext) = rest.split_at(NONCE_LEN);
    let nonce: [u8; NONCE_LEN] = nonce.try_into().expect("split at NONCE_LEN");
    cipher(key)
        .decrypt(
            &XNonce::from(nonce),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| Error::Decrypt)
}

fn cipher(key: &[u8; 32]) -> XChaCha20Poly1305 {
    XChaCha20Poly1305::new(&Key::from(*key))
}
