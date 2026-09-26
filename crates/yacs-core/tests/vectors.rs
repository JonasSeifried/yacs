//! Fixed test vectors shared by every client. If these fail, a device running
//! this build can no longer join spaces or decrypt with devices running older builds.
//!
//! Runs natively and under WASM:
//! `cargo test -p yacs-core --target wasm32-unknown-unknown --test vectors`

use serde::Deserialize;
use sha2::{Digest, Sha256};
use yacs_core::{
    ChannelId, ChannelKey, Code, CodeInviter, CodeJoiner, Envelope, Error, InviteSecret, Pairing,
    Payload, Stream,
};

#[cfg(target_arch = "wasm32")]
use wasm_bindgen_test::wasm_bindgen_test;

#[derive(Deserialize)]
struct Vectors {
    version: u8,
    roots: Vec<RootVector>,
    invites: Vec<InviteVector>,
    codes: Vec<CodeVector>,
    envelopes: Vec<EnvelopeVector>,
    streams: Vec<StreamVector>,
}

#[derive(Deserialize)]
struct RootVector {
    root: String,
    channel_id: String,
    key: String,
}

#[derive(Deserialize)]
struct InviteVector {
    secret: String,
    slot: String,
    sealed: String,
    space_name: String,
    inviter: String,
    token: Option<String>,
    channel_id: String,
    key: String,
}

#[derive(Deserialize)]
struct CodeVector {
    code: String,
    inviter_rng: u8,
    joiner_rng: u8,
    message: String,
    answer: String,
    device_name: String,
    sealed_invite: String,
    space_name: String,
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

#[derive(Deserialize)]
struct StreamVector {
    channel_id: String,
    key: String,
    stream: Stream,
    chunks: Vec<ChunkVector>,
}

#[derive(Deserialize)]
struct ChunkVector {
    sha256: String,
    /// Only for short chunks.
    hex: Option<String>,
}

/// What `gen_vectors` encrypts: byte i is i * 31 % 251.
fn stream_plaintext(len: u64) -> Vec<u8> {
    (0..len).map(|i| (i * 31 % 251) as u8).collect()
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

/// The first three roots are what phrases derived with Argon2id up to 0.3,
/// so spaces made then keep their channel id and key.
#[cfg_attr(not(target_arch = "wasm32"), test)]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
fn root_derivation_matches() {
    for v in vectors().roots {
        let root: [u8; 32] = hex::decode(&v.root).unwrap().try_into().unwrap();
        assert_eq!(
            Pairing::from_root(&root),
            pairing(&v.channel_id, &v.key),
            "{}",
            v.root
        );
    }
}

#[cfg_attr(not(target_arch = "wasm32"), test)]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
fn stored_invites_open() {
    for v in vectors().invites {
        let secret: InviteSecret = v.secret.parse().unwrap();
        assert_eq!(secret.slot().to_string(), v.slot);
        let invite = secret.open(&hex::decode(&v.sealed).unwrap()).unwrap();
        assert_eq!(invite.space_name, v.space_name);
        assert_eq!(invite.inviter, v.inviter);
        assert_eq!(invite.token, v.token);
        assert_eq!(invite.pairing(), pairing(&v.channel_id, &v.key));
    }
}

#[cfg_attr(not(target_arch = "wasm32"), test)]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
fn code_exchange_matches() {
    for v in vectors().codes {
        let code: Code = v.code.parse().unwrap();
        let answer = hex::decode(&v.answer).unwrap();
        let (inviter, message) = CodeInviter::start_with_rng(&code, Counter(v.inviter_rng));
        assert_eq!(hex::encode(&message), v.message);

        // The joiner's SPAKE2 message is the answer's first part.
        let joiner = CodeJoiner::start_with_rng(&code, Counter(v.joiner_rng));
        let (fresh, joiner_key) = joiner.answer(&message, &v.device_name).unwrap();
        assert_eq!(fresh[..34], answer[..34]);

        let (device_name, _) = inviter.finish(code.nameplate(), &answer).unwrap();
        assert_eq!(device_name, v.device_name);
        let invite = joiner_key
            .open_invite(&hex::decode(&v.sealed_invite).unwrap())
            .unwrap();
        assert_eq!(invite.space_name, v.space_name);
        assert_eq!(invite.pairing(), pairing(&v.channel_id, &v.key));
    }
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

#[cfg_attr(not(target_arch = "wasm32"), test)]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
fn stream_chunks_match() {
    for v in vectors().streams {
        let p = pairing(&v.channel_id, &v.key);
        let cipher = v.stream.cipher(&p).unwrap();
        let plaintext = stream_plaintext(v.stream.total());
        assert_eq!(v.chunks.len() as u64, v.stream.chunk_count());
        for (i, expected) in v.chunks.iter().enumerate() {
            let i = i as u64;
            let start = (i * u64::from(v.stream.chunk_size)) as usize;
            let chunk = plaintext[start..start + v.stream.chunk_len(i)].to_vec();
            let sealed = cipher.seal(i, chunk.clone()).unwrap();
            assert_eq!(
                hex::encode(Sha256::digest(&sealed)),
                expected.sha256,
                "chunk {i}"
            );
            if let Some(stored) = &expected.hex {
                let stored = hex::decode(stored).unwrap();
                assert_eq!(cipher.open(i, stored).unwrap(), chunk, "chunk {i}");
            }
        }
    }
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
