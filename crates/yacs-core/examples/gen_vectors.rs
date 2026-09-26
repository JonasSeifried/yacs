//! Regenerates `tests/vectors.json`. Only needed after an intentional protocol
//! change, which also means bumping the protocol version:
//!
//! ```sh
//! cargo run -p yacs-core --example gen_vectors > crates/yacs-core/tests/vectors.json
//! ```

use serde_json::json;
use sha2::{Digest, Sha256};
use yacs_core::{
    Clip, ClipItem, Code, CodeInviter, CodeJoiner, Envelope, Image, Invite, InviteSecret,
    MIN_CHUNK_SIZE, Pairing, Payload, Stream, StreamFile,
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

    // Sealing picks a fresh nonce, so clients check that these open and that
    // the secret leads to the slot, not that they seal to the same bytes.
    let secret = InviteSecret::from_bytes(core::array::from_fn(|i| (i * 7) as u8));
    let invites: Vec<_> = [
        Invite::new("Anna & me", "MacBook", Some("s3cret"), pairing),
        Invite::new("My devices", "PC 🖥️", None, pairing),
    ]
    .iter()
    .map(|invite| {
        json!({
            "secret": secret.to_string(),
            "slot": secret.slot().to_string(),
            "sealed": hex::encode(secret.seal(invite).unwrap()),
            "space_name": invite.space_name,
            "inviter": invite.inviter,
            "token": invite.token,
            "channel_id": pairing.channel_id.to_string(),
            "key": hex::encode(pairing.key.as_bytes()),
        })
    })
    .collect();

    // SPAKE2 with fixed RNGs: the messages must come out exactly like this;
    // the sealed parts only have to open.
    let code: Code = "7-tulip-apple".parse().unwrap();
    let invite = Invite::new("Anna & me", "MacBook", Some("s3cret"), pairing);
    let (inviter, message) = CodeInviter::start_with_rng(&code, Counter(1));
    let joiner = CodeJoiner::start_with_rng(&code, Counter(101));
    let (answer, _) = joiner.answer(&message, "Anna's iPhone").unwrap();
    let (_, key) = inviter.finish(code.nameplate(), &answer).unwrap();
    let codes = [json!({
        "code": code.to_string(),
        "inviter_rng": 1,
        "joiner_rng": 101,
        "message": hex::encode(&message),
        "answer": hex::encode(&answer),
        "device_name": "Anna's iPhone",
        "sealed_invite": hex::encode(key.seal_invite(&invite).unwrap()),
        "space_name": invite.space_name,
        "channel_id": pairing.channel_id.to_string(),
        "key": hex::encode(pairing.key.as_bytes()),
    })];

    let vectors = json!({
        "codes": codes,
        "invites": invites,
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

/// A fixed "random" byte stream, so SPAKE2 messages come out the same everywhere.
struct Counter(u8);

impl rand_core::TryRng for Counter {
    type Error = core::convert::Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        let mut bytes = [0; 4];
        self.try_fill_bytes(&mut bytes)?;
        Ok(u32::from_le_bytes(bytes))
    }

    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        let mut bytes = [0; 8];
        self.try_fill_bytes(&mut bytes)?;
        Ok(u64::from_le_bytes(bytes))
    }

    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Self::Error> {
        for byte in dst {
            *byte = self.0;
            self.0 = self.0.wrapping_add(1);
        }
        Ok(())
    }
}

impl rand_core::TryCryptoRng for Counter {}
