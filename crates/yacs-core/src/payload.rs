//! What travels inside an [`Envelope`](crate::Envelope), after decryption.
//!
//! Serialized with postcard, which encodes enum variants by index. Never reorder
//! or remove variants or fields; only append new variants at the end.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Payload {
    Clip(Clip),
    // v2: P2pOffer { .. } for large-file transfers.
}

/// One copy action: every format the source clipboard offered for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clip {
    /// Unix time in milliseconds, set by the sending device. Supplied by the
    /// caller because `std::time` is unavailable on `wasm32-unknown-unknown`.
    pub created_at_ms: i64,
    pub device_name: String,
    pub items: Vec<ClipItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipItem {
    Text(String),
    Html(String),
    Rtf(String),
    Image(Image),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Image {
    pub mime: String,
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
}

impl Payload {
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("payload types always serialize")
    }

    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self> {
        match postcard::take_from_bytes(bytes) {
            Ok((payload, [])) => Ok(payload),
            _ => Err(Error::Malformed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Payload {
        Payload::Clip(Clip {
            created_at_ms: 1_758_600_000_000,
            device_name: "MacBook".into(),
            items: vec![
                ClipItem::Text("hello".into()),
                ClipItem::Html("<b>hello</b>".into()),
                ClipItem::Image(Image {
                    mime: "image/png".into(),
                    data: vec![0x89, b'P', b'N', b'G'],
                }),
            ],
        })
    }

    #[test]
    fn round_trips() {
        let payload = sample();
        assert_eq!(Payload::from_bytes(&payload.to_bytes()), Ok(payload));
    }

    #[test]
    fn rejects_trailing_bytes() {
        let mut bytes = sample().to_bytes();
        bytes.push(0);
        assert_eq!(Payload::from_bytes(&bytes), Err(Error::Malformed));
    }

    #[test]
    fn rejects_unknown_variant() {
        assert_eq!(Payload::from_bytes(&[42]), Err(Error::Malformed));
    }
}
