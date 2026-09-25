//! Big files, encrypted in chunks that travel (and can be retried) one by one.
//!
//! The age payload construction (STREAM, Hoang–Reyhanitabar–Rogaway–Vizár):
//!
//! ```text
//! file key = HKDF-SHA256(ikm = channel key, salt, info = "yacs/v1/stream" || channel id)
//! chunk i  = ChaCha20-Poly1305(file key, nonce = i as 11 bytes BE || last, AAD = version || channel id)
//! ```
//!
//! A clip's files are concatenated into one stream and cut into chunks of
//! `chunk_size` plaintext bytes; `last` is 1 on the final chunk. The final
//! chunk may be short but is never empty, unless the whole stream is (then
//! it's the only chunk). Reordering fails (the index is in the nonce),
//! truncation fails (the last flag) and mixing clips fails (a key per salt).
//!
//! Never seal chunk `i` of a salt twice with different data: that reuses a
//! nonce. A restarted upload needs a new [`Stream`], so a new salt.

use chacha20poly1305::aead::{AeadInOut, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use zeroize::Zeroize;

use crate::envelope::aad;
use crate::error::{Error, Result};
use crate::pairing::Pairing;
use crate::payload::{is_image_mime, safe_file_name};

pub const HKDF_INFO_STREAM: &[u8] = b"yacs/v1/stream";
pub const SALT_LEN: usize = 32;
/// Poly1305 tag at the end of every sealed chunk.
pub const CHUNK_TAG_LEN: usize = 16;
/// What senders use: fits every proxy we know of, and keeps memory flat.
pub const DEFAULT_CHUNK_SIZE: u32 = 4 * 1024 * 1024;
/// Bounds a receiver accepts, so a header can't make it allocate huge chunks
/// (or the relay track billions of tiny ones).
pub const MIN_CHUNK_SIZE: u32 = 64 * 1024;
pub const MAX_CHUNK_SIZE: u32 = 16 * 1024 * 1024;

/// The files of a clip that travel as chunks. It sits in the clip (the
/// "header"), so only devices with the key learn names, sizes and the salt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stream {
    #[serde(with = "serde_bytes")]
    pub salt: [u8; SALT_LEN],
    /// Plaintext bytes per chunk (the last one may be shorter).
    pub chunk_size: u32,
    /// In stream order: each file starts where the one before it ends.
    pub files: Vec<StreamFile>,
}

/// One file in a [`Stream`]. `name` comes from another device: use
/// [`StreamFile::safe_name`] before putting it on a filesystem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamFile {
    pub name: String,
    /// Guessed by the sender, `application/octet-stream` if unknown.
    pub mime: String,
    pub size: u64,
}

impl StreamFile {
    /// See [`File::safe_name`](crate::File::safe_name).
    pub fn safe_name(&self) -> String {
        safe_file_name(&self.name)
    }

    pub fn is_image(&self) -> bool {
        is_image_mime(&self.mime)
    }
}

impl Stream {
    /// A stream with a fresh random salt and the default chunk size.
    pub fn new(files: Vec<StreamFile>) -> Result<Self> {
        let mut salt = [0u8; SALT_LEN];
        getrandom::fill(&mut salt).map_err(|_| Error::Rng)?;
        Ok(Self {
            salt,
            chunk_size: DEFAULT_CHUNK_SIZE,
            files,
        })
    }

    /// Plaintext bytes of all files together.
    pub fn total(&self) -> u64 {
        self.files
            .iter()
            .fold(0, |sum, f| sum.saturating_add(f.size))
    }

    /// At least one: an empty stream is one empty chunk.
    pub fn chunk_count(&self) -> u64 {
        self.total()
            .div_ceil(u64::from(self.chunk_size.max(1)))
            .max(1)
    }

    /// Plaintext bytes in chunk `index`.
    pub fn chunk_len(&self, index: u64) -> usize {
        let start = index.saturating_mul(u64::from(self.chunk_size));
        (self.total().saturating_sub(start)).min(u64::from(self.chunk_size)) as usize
    }

    /// Bytes of chunk `index` as stored on the relay.
    pub fn sealed_chunk_len(&self, index: u64) -> usize {
        self.chunk_len(index) + CHUNK_TAG_LEN
    }

    /// Everything the relay stores for the chunks: what an upload reserves.
    pub fn sealed_len(&self) -> u64 {
        self.total() + self.chunk_count() * CHUNK_TAG_LEN as u64
    }

    /// Where file `index` starts in the stream.
    pub fn file_offset(&self, index: usize) -> u64 {
        self.files[..index]
            .iter()
            .fold(0, |sum, f| sum.saturating_add(f.size))
    }

    /// The chunks that hold any of file `index`, as `first..end`.
    pub fn file_chunks(&self, index: usize) -> core::ops::Range<u64> {
        let size = u64::from(self.chunk_size.max(1));
        let start = self.file_offset(index);
        let len = self.files[index].size;
        if len == 0 {
            let at = (start / size).min(self.chunk_count() - 1);
            return at..at + 1;
        }
        start / size..start.saturating_add(len).div_ceil(size)
    }

    /// Checks the header's parameters, then derives the chunk key.
    pub fn cipher(&self, pairing: &Pairing) -> Result<StreamCipher> {
        if !(MIN_CHUNK_SIZE..=MAX_CHUNK_SIZE).contains(&self.chunk_size) {
            return Err(Error::Malformed);
        }
        self.files
            .iter()
            .try_fold(0u64, |sum, f| sum.checked_add(f.size))
            .ok_or(Error::Malformed)?;

        let mut info = Vec::with_capacity(HKDF_INFO_STREAM.len() + 32);
        info.extend_from_slice(HKDF_INFO_STREAM);
        info.extend_from_slice(pairing.channel_id.as_bytes());
        let mut key = [0u8; 32];
        Hkdf::<Sha256>::new(Some(&self.salt), pairing.key.as_bytes())
            .expand(&info, &mut key)
            .expect("32 bytes is a valid HKDF-SHA256 output length");
        let cipher = ChaCha20Poly1305::new(&Key::from(key));
        key.zeroize();
        Ok(StreamCipher {
            cipher,
            aad: aad(&pairing.channel_id),
            chunks: self.chunk_count(),
            stream: self.clone(),
        })
    }
}

/// Seals and opens the chunks of one [`Stream`].
pub struct StreamCipher {
    cipher: ChaCha20Poly1305,
    aad: [u8; 33],
    chunks: u64,
    stream: Stream,
}

impl StreamCipher {
    pub fn stream(&self) -> &Stream {
        &self.stream
    }

    /// Encrypts chunk `index` in place: `chunk` must hold exactly
    /// [`Stream::chunk_len`] bytes, and gains the tag.
    pub fn seal(&self, index: u64, mut chunk: Vec<u8>) -> Result<Vec<u8>> {
        if index >= self.chunks || chunk.len() != self.stream.chunk_len(index) {
            return Err(Error::Encrypt);
        }
        self.cipher
            .encrypt_in_place(&self.nonce(index), &self.aad, &mut chunk)
            .map_err(|_| Error::Encrypt)?;
        Ok(chunk)
    }

    /// Decrypts chunk `index` in place, checking it has the length the
    /// header promises. Fails with [`Error::Decrypt`] on any tampering,
    /// reordering or truncation.
    pub fn open(&self, index: u64, mut chunk: Vec<u8>) -> Result<Vec<u8>> {
        if index >= self.chunks || chunk.len() != self.stream.sealed_chunk_len(index) {
            return Err(Error::Decrypt);
        }
        let opened = self
            .cipher
            .decrypt_in_place(&self.nonce(index), &self.aad, &mut chunk);
        if opened.is_err() {
            chunk.zeroize();
            return Err(Error::Decrypt);
        }
        Ok(chunk)
    }

    fn nonce(&self, index: u64) -> Nonce {
        let mut nonce = [0u8; 12];
        nonce[3..11].copy_from_slice(&index.to_be_bytes());
        nonce[11] = u8::from(index + 1 == self.chunks);
        Nonce::from(nonce)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pairing::{ChannelId, ChannelKey};

    fn pairing(id: u8, key: u8) -> Pairing {
        Pairing {
            channel_id: ChannelId::from_bytes([id; 32]),
            key: ChannelKey::from_bytes([key; 32]),
        }
    }

    fn stream(sizes: &[u64]) -> Stream {
        Stream {
            salt: [7; SALT_LEN],
            chunk_size: MIN_CHUNK_SIZE,
            files: sizes
                .iter()
                .enumerate()
                .map(|(i, &size)| StreamFile {
                    name: format!("f{i}"),
                    mime: "application/octet-stream".into(),
                    size,
                })
                .collect(),
        }
    }

    fn data(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 31 % 251) as u8).collect()
    }

    const C: u64 = MIN_CHUNK_SIZE as u64;

    #[test]
    fn chunk_arithmetic() {
        let s = stream(&[C, C / 2, 0, C + 1]);
        assert_eq!(s.total(), 3 * C + C / 2 + 1 - C);
        assert_eq!(s.chunk_count(), 3);
        assert_eq!(s.chunk_len(0), C as usize);
        assert_eq!(s.chunk_len(2), (C / 2 + 1) as usize);
        assert_eq!(s.sealed_len(), s.total() + 3 * 16);
        assert_eq!(s.file_chunks(0), 0..1);
        assert_eq!(s.file_chunks(1), 1..2);
        assert_eq!(s.file_chunks(2), 1..2);
        assert_eq!(s.file_chunks(3), 1..3);

        let empty = stream(&[0]);
        assert_eq!((empty.chunk_count(), empty.chunk_len(0)), (1, 0));
        assert_eq!(empty.file_chunks(0), 0..1);
        assert_eq!(empty.sealed_len(), 16);
        assert_eq!(stream(&[C]).chunk_count(), 1);
    }

    #[test]
    fn seal_open_round_trip() {
        let p = pairing(1, 2);
        let s = stream(&[C * 2 + 5]);
        let cipher = s.cipher(&p).unwrap();
        let all = data(s.total() as usize);
        for i in 0..s.chunk_count() {
            let start = (i * C) as usize;
            let chunk = all[start..start + s.chunk_len(i)].to_vec();
            let sealed = cipher.seal(i, chunk.clone()).unwrap();
            assert_eq!(sealed.len(), s.sealed_chunk_len(i));
            assert_ne!(&sealed[..chunk.len()], &chunk[..]);
            assert_eq!(cipher.open(i, sealed).unwrap(), chunk);
        }
    }

    #[test]
    fn rejects_reordering_truncation_and_other_keys() {
        let p = pairing(1, 2);
        let s = stream(&[C * 2]);
        let cipher = s.cipher(&p).unwrap();
        let first = cipher.seal(0, data(C as usize)).unwrap();
        let second = cipher.seal(1, data(C as usize)).unwrap();

        assert_eq!(cipher.open(1, first.clone()), Err(Error::Decrypt));
        // Truncated to one chunk: the first chunk isn't marked last.
        let truncated = stream(&[C]).cipher(&p).unwrap();
        assert_eq!(truncated.open(0, first.clone()), Err(Error::Decrypt));
        // Another salt, channel or key.
        let mut salted = s.clone();
        salted.salt[0] ^= 1;
        assert_eq!(
            salted.cipher(&p).unwrap().open(0, first.clone()),
            Err(Error::Decrypt)
        );
        let other = s.cipher(&pairing(9, 2)).unwrap();
        assert_eq!(other.open(0, first.clone()), Err(Error::Decrypt));
        let other = s.cipher(&pairing(1, 3)).unwrap();
        assert_eq!(other.open(0, first.clone()), Err(Error::Decrypt));

        let mut flipped = second;
        flipped[100] ^= 1;
        assert_eq!(cipher.open(1, flipped), Err(Error::Decrypt));
        assert_eq!(cipher.open(2, first.clone()), Err(Error::Decrypt));
        assert_eq!(cipher.open(0, first[..100].to_vec()), Err(Error::Decrypt));
    }

    #[test]
    fn seal_wants_exact_chunks() {
        let s = stream(&[C + 1]);
        let cipher = s.cipher(&pairing(1, 2)).unwrap();
        assert_eq!(cipher.seal(0, data(C as usize - 1)), Err(Error::Encrypt));
        assert_eq!(cipher.seal(1, data(2)), Err(Error::Encrypt));
        assert_eq!(cipher.seal(2, data(0)), Err(Error::Encrypt));
        assert!(cipher.seal(1, data(1)).is_ok());
    }

    #[test]
    fn rejects_bad_headers() {
        let p = pairing(1, 2);
        let mut s = stream(&[1]);
        s.chunk_size = MIN_CHUNK_SIZE - 1;
        assert!(s.cipher(&p).is_err());
        s.chunk_size = MAX_CHUNK_SIZE + 1;
        assert!(s.cipher(&p).is_err());
        let overflow = stream(&[u64::MAX, 1]);
        assert!(overflow.cipher(&p).is_err());
    }

    #[test]
    fn new_streams_get_fresh_salts() {
        let a = Stream::new(vec![]).unwrap();
        let b = Stream::new(vec![]).unwrap();
        assert_ne!(a.salt, b.salt);
        assert_eq!(a.chunk_size, DEFAULT_CHUNK_SIZE);
    }
}
