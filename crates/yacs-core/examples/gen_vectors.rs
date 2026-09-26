//! Regenerates `tests/vectors.json`. Only needed after an intentional protocol
//! change, which also means bumping the protocol version:
//!
//! ```sh
//! cargo run -p yacs-core --example gen_vectors > crates/yacs-core/tests/vectors.json
//! ```

use serde_json::json;
use sha2::{Digest, Sha256};
use yacs_core::{
    Clip, ClipItem, Envelope, Image, MIN_CHUNK_SIZE, Pairing, Payload, Stream, StreamFile,
};

/// What Argon2id made of three phrases up to 0.3 ("correct horse battery
/// staple", "Ｃafé ﬁle naïve", "tundra velvet anchor pickle orbit meadow"), so
/// the vectors keep their channel ids and keys.
const ROOTS: &[&str] = &[
    "e9794e1b5cb23afeb84cdd3a67cde1babd7c2474808a6eee9dfe8289a0e8d91d",
    "0144c1bbae38c08c5fafaeb39a60990c3fd494ec654210946931d98ee56b2ece",
    "d50a42466f0649b68344ed84894633b51a49f498c572802df637b6d488d95a12",
];

fn main() {
    let pairings: Vec<_> = ROOTS
        .iter()
        .map(|root| Pairing::from_root(&hex::decode(root).unwrap().try_into().unwrap()))
        .collect();
    let roots: Vec<_> = ROOTS
        .iter()
        .zip(&pairings)
        .map(|(root, pairing)| {
            json!({
                "root": root,
                "channel_id": pairing.channel_id.to_string(),
                "key": hex::encode(pairing.key.as_bytes()),
            })
        })
        .collect();

    let pairing = &pairings[2];
    let payloads = [
        Payload::Clip(Clip {
            created_at_ms: 1_758_600_000_000,
            device_name: "MacBook".into(),
            items: vec![
                ClipItem::Text("Meeting notes for Thursday".into()),
                ClipItem::Html("<p><b>Meeting</b> notes for Thursday</p>".into()),
                ClipItem::Rtf(r"{\rtf1\ansi {\b Meeting} notes for Thursday}".into()),
            ],
        }),
        Payload::Clip(Clip {
            created_at_ms: 1_758_600_060_000,
            device_name: "PC 🖥️".into(),
            items: vec![ClipItem::Image(Image {
                mime: "image/png".into(),
                data: vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 1, 2, 255],
            })],
        }),
    ];
    let envelopes: Vec<_> = payloads
        .iter()
        .map(|payload| {
            json!({
                "channel_id": pairing.channel_id.to_string(),
                "key": hex::encode(pairing.key.as_bytes()),
                "envelope": hex::encode(Envelope::seal(pairing, payload).unwrap().to_bytes()),
                "payload": payload,
            })
        })
        .collect();

    // Two files across three chunks: sealing is deterministic, so every
    // client must produce these exact chunks. Full chunks are stored as
    // SHA-256 hashes to keep the file small.
    let stream = Stream {
        salt: core::array::from_fn(|i| i as u8),
        chunk_size: MIN_CHUNK_SIZE,
        files: vec![
            StreamFile {
                name: "a.bin".into(),
                mime: "application/octet-stream".into(),
                size: u64::from(MIN_CHUNK_SIZE) + 100,
            },
            StreamFile {
                name: "b.txt".into(),
                mime: "text/plain".into(),
                size: u64::from(MIN_CHUNK_SIZE),
            },
        ],
    };
    let plaintext = stream_plaintext(stream.total());
    let cipher = stream.cipher(pairing).unwrap();
    let chunks: Vec<_> = (0..stream.chunk_count())
        .map(|i| {
            let start = (i * u64::from(stream.chunk_size)) as usize;
            let chunk = plaintext[start..start + stream.chunk_len(i)].to_vec();
            let sealed = cipher.seal(i, chunk).unwrap();
            json!({
                "sha256": hex::encode(Sha256::digest(&sealed)),
                "hex": (sealed.len() <= 256).then(|| hex::encode(&sealed)),
            })
        })
        .collect();
    let streams = [json!({
        "channel_id": pairing.channel_id.to_string(),
        "key": hex::encode(pairing.key.as_bytes()),
        "stream": stream,
        "plaintext": "byte i is i * 31 % 251",
        "chunks": chunks,
    })];

    let vectors = json!({
        "version": yacs_core::PROTOCOL_VERSION,
        "roots": roots,
        "envelopes": envelopes,
        "streams": streams,
    });
    println!("{}", serde_json::to_string_pretty(&vectors).unwrap());
}

fn stream_plaintext(len: u64) -> Vec<u8> {
    (0..len).map(|i| (i * 31 % 251) as u8).collect()
}
