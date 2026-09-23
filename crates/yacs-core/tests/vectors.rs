//! Fixed test vectors shared by every client. If these fail, a device running
//! this build can no longer pair or decrypt with devices running older builds.
//!
//! Runs natively and under WASM:
//! `cargo test -p yacs-core --target wasm32-unknown-unknown --test vectors`

use serde::Deserialize;
use yacs_core::{ChannelId, ChannelKey, Envelope, Error, Pairing, Payload, normalize_phrase};

#[cfg(target_arch = "wasm32")]
use wasm_bindgen_test::wasm_bindgen_test;

#[derive(Deserialize)]
struct Vectors {
    version: u8,
    kdf: Vec<KdfVector>,
    envelopes: Vec<EnvelopeVector>,
}

#[derive(Deserialize)]
struct KdfVector {
    phrase: String,
    normalized: String,
    channel_id: String,
    key: String,
}

#[derive(Deserialize)]
struct EnvelopeVector {
    channel_id: String,
    key: String,
    envelope: String,
    payload: Payload,
}

fn vectors() -> Vectors {
    serde_json::from_str(include_str!("vectors.json")).unwrap()
}

fn pairing(channel_id: &str, key: &str) -> Pairing {
    Pairing {
        channel_id: channel_id.parse().unwrap(),
        key: ChannelKey::from_bytes(hex::decode(key).unwrap().try_into().unwrap()),
    }
}

#[cfg_attr(not(target_arch = "wasm32"), test)]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
fn protocol_version_matches() {
    assert_eq!(vectors().version, yacs_core::PROTOCOL_VERSION);
}

#[cfg_attr(not(target_arch = "wasm32"), test)]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
fn phrase_derivation_matches() {
    for v in vectors().kdf {
        assert_eq!(normalize_phrase(&v.phrase), v.normalized, "{:?}", v.phrase);
        assert_eq!(
            Pairing::from_phrase(&v.phrase).unwrap(),
            pairing(&v.channel_id, &v.key),
            "{:?}",
            v.phrase
        );
    }
}

#[cfg_attr(not(target_arch = "wasm32"), test)]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
fn equivalent_phrases_pair() {
    let kdf = vectors().kdf;
    assert_eq!(kdf[0].normalized, kdf[1].normalized);
    assert_eq!(kdf[0].channel_id, kdf[1].channel_id);
    assert_eq!(kdf[0].key, kdf[1].key);
}

#[cfg_attr(not(target_arch = "wasm32"), test)]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
fn stored_envelopes_open() {
    for v in vectors().envelopes {
        let p = pairing(&v.channel_id, &v.key);
        let env = Envelope::from_bytes(&hex::decode(&v.envelope).unwrap()).unwrap();
        assert_eq!(env.open(&p).unwrap(), v.payload);
    }
}

#[cfg_attr(not(target_arch = "wasm32"), test)]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
fn fresh_envelopes_round_trip() {
    for v in vectors().envelopes {
        let p = pairing(&v.channel_id, &v.key);
        let bytes = Envelope::seal(&p, &v.payload).unwrap().to_bytes();
        assert_ne!(hex::encode(&bytes), v.envelope, "nonce must be fresh");
        assert_eq!(
            Envelope::from_bytes(&bytes).unwrap().open(&p).unwrap(),
            v.payload
        );
    }
}

#[cfg_attr(not(target_arch = "wasm32"), test)]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
fn envelope_is_bound_to_its_channel() {
    let v = &vectors().envelopes[0];
    let env = Envelope::from_bytes(&hex::decode(&v.envelope).unwrap()).unwrap();
    let other_channel = Pairing {
        channel_id: ChannelId::from_bytes([0; 32]),
        key: pairing(&v.channel_id, &v.key).key,
    };
    assert_eq!(env.open(&other_channel), Err(Error::Decrypt));
}
