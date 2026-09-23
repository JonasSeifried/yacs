//! Regenerates `tests/vectors.json`. Only needed after an intentional protocol
//! change, which also means bumping the protocol version:
//!
//! ```sh
//! cargo run -p yacs-core --example gen_vectors > crates/yacs-core/tests/vectors.json
//! ```

use serde_json::json;
use yacs_core::{Clip, ClipItem, Envelope, Image, Pairing, Payload, normalize_phrase};

const PHRASES: &[&str] = &[
    "correct horse battery staple",
    "  Correct   HORSE\tbattery\nstaple ",
    "Ｃafé ﬁle naïve",
    "tundra velvet anchor pickle orbit meadow",
];

fn main() {
    let kdf: Vec<_> = PHRASES
        .iter()
        .map(|phrase| {
            let pairing = Pairing::from_phrase(phrase).unwrap();
            json!({
                "phrase": phrase,
                "normalized": normalize_phrase(phrase),
                "channel_id": pairing.channel_id.to_string(),
                "key": hex::encode(pairing.key.as_bytes()),
            })
        })
        .collect();

    let pairing = Pairing::from_phrase(PHRASES[3]).unwrap();
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
                "envelope": hex::encode(Envelope::seal(&pairing, payload).unwrap().to_bytes()),
                "payload": payload,
            })
        })
        .collect();

    let vectors =
        json!({ "version": yacs_core::PROTOCOL_VERSION, "kdf": kdf, "envelopes": envelopes });
    println!("{}", serde_json::to_string_pretty(&vectors).unwrap());
}
