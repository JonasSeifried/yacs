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
    /// A copied file. One clip can carry several.
    File(File),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Image {
    pub mime: String,
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
}

/// A file as the sender had it. `name` comes from another device: use
/// [`File::safe_name`] before putting it on a filesystem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct File {
    pub name: String,
    /// Guessed by the sender, `application/octet-stream` if unknown.
    pub mime: String,
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
}

/// Longest file name written to disk, in bytes; most filesystems allow 255.
const MAX_NAME_BYTES: usize = 200;

impl File {
    /// The name, made safe to create in a folder on any OS: no path parts,
    /// no characters Windows rejects, no reserved device names, not hidden,
    /// and short enough (keeping the extension).
    pub fn safe_name(&self) -> String {
        let base = self.name.rsplit(['/', '\\']).next().unwrap_or_default();
        let cleaned: String = base
            .chars()
            .map(|c| match c {
                '<' | '>' | ':' | '"' | '|' | '?' | '*' => '_',
                c if c.is_control() => '_',
                c => c,
            })
            .collect();
        let mut name = cleaned
            .trim_start_matches(['.', ' '])
            .trim_end_matches(['.', ' '])
            .to_owned();
        if name.is_empty() {
            name = "file".into();
        }
        let stem = name.split('.').next().unwrap_or_default();
        const RESERVED: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];
        let reserved = RESERVED.iter().any(|r| stem.eq_ignore_ascii_case(r))
            || (stem.len() == 4
                && (stem[..3].eq_ignore_ascii_case("COM")
                    || stem[..3].eq_ignore_ascii_case("LPT"))
                && stem.as_bytes()[3].is_ascii_digit());
        if reserved {
            name.insert(0, '_');
        }
        if name.len() > MAX_NAME_BYTES {
            let ext = match name.rfind('.') {
                Some(dot) if name.len() - dot <= 16 => name[dot..].to_owned(),
                _ => String::new(),
            };
            let mut cut = MAX_NAME_BYTES - ext.len();
            while !name.is_char_boundary(cut) {
                cut -= 1;
            }
            name = format!("{}{ext}", &name[..cut]);
        }
        name
    }

    /// Whether browsers and the desktop can show it as a picture.
    pub fn is_image(&self) -> bool {
        matches!(
            self.mime.as_str(),
            "image/png" | "image/jpeg" | "image/gif" | "image/webp"
        )
    }
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
                ClipItem::File(File {
                    name: "report.pdf".into(),
                    mime: "application/pdf".into(),
                    data: b"%PDF-1.7".to_vec(),
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

    /// Clips from before `File` existed must still decode the same.
    #[test]
    fn earlier_variants_keep_their_encoding() {
        let text = Payload::Clip(Clip {
            created_at_ms: 1,
            device_name: "a".into(),
            items: vec![ClipItem::Text("x".into())],
        });
        assert_eq!(text.to_bytes(), [0, 2, 1, b'a', 1, 0, 1, b'x']);
    }

    #[test]
    fn safe_names() {
        let name = |n: &str| {
            File {
                name: n.into(),
                mime: String::new(),
                data: vec![],
            }
            .safe_name()
        };
        assert_eq!(name("report.pdf"), "report.pdf");
        assert_eq!(name("Präsentation (final).key"), "Präsentation (final).key");
        assert_eq!(name("../../etc/passwd"), "passwd");
        assert_eq!(name("C:\\Users\\x\\evil.exe"), "evil.exe");
        assert_eq!(name("a:b*c?.txt"), "a_b_c_.txt");
        assert_eq!(name(".bashrc"), "bashrc");
        assert_eq!(name("notes. "), "notes");
        assert_eq!(name(".."), "file");
        assert_eq!(name(""), "file");
        assert_eq!(name("tab\there"), "tab_here");
        assert_eq!(name("con.txt"), "_con.txt");
        assert_eq!(name("COM1"), "_COM1");
        assert_eq!(name("console.log"), "console.log");
        let long = name(&format!("{}.tar.gz", "ü".repeat(300)));
        assert!(
            long.len() <= MAX_NAME_BYTES && long.ends_with(".gz"),
            "{long}"
        );
    }

    #[test]
    fn rejects_unknown_variant() {
        assert_eq!(Payload::from_bytes(&[42]), Err(Error::Malformed));
    }
}
