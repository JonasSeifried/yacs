//! `yacs-core` for the PWA. The browser only ever sees opaque envelopes and
//! plaintext clips; keys stay inside this module's memory except when a
//! pairing is exported with `secret()` to be stored.
//!
//! Clips cross into JS through serde, so a clip looks like
//! `{ created_at_ms, device_name, items: [{ Text: "…" }, { Image: { mime, data: Uint8Array } }] }`.
//! Big files are a `{ Stream: { salt: Uint8Array, chunk_size, files: [{ name, mime, size }] } }`
//! item, whose chunks a [`WasmStreamCipher`] seals and opens one at a time.

use serde::Serialize;
use wasm_bindgen::prelude::*;
use yacs_core::{Clip, Envelope, Invite, InviteSecret, Payload, Stream, StreamCipher, StreamFile};

#[wasm_bindgen(js_name = Pairing)]
pub struct WasmPairing(yacs_core::Pairing);

#[wasm_bindgen(js_class = Pairing)]
impl WasmPairing {
    /// A new space with a random key.
    pub fn generate() -> Result<WasmPairing, JsError> {
        Ok(Self(yacs_core::Pairing::generate()?))
    }

    /// From `secret()` or a pairing link: instant.
    #[wasm_bindgen(js_name = fromSecret)]
    pub fn from_secret(secret: &str) -> Result<WasmPairing, JsError> {
        Ok(Self(yacs_core::Pairing::from_secret(secret)?))
    }

    /// For API paths. Not secret from the relay, but don't publish it.
    #[wasm_bindgen(getter, js_name = channelId)]
    pub fn channel_id(&self) -> String {
        self.0.channel_id.to_string()
    }

    /// Channel id and key, to store the space.
    pub fn secret(&self) -> String {
        self.0.to_secret()
    }

    /// Encrypt a clip into envelope bytes, ready to POST.
    pub fn seal(&self, clip: JsValue) -> Result<Vec<u8>, JsError> {
        let clip: Clip = serde_wasm_bindgen::from_value(clip)?;
        Ok(Envelope::seal(&self.0, &Payload::Clip(clip))?.to_bytes())
    }

    /// Decrypt envelope bytes as fetched from the relay.
    pub fn open(&self, envelope: &[u8]) -> Result<JsValue, JsError> {
        let Payload::Clip(clip) = Envelope::from_bytes(envelope)?.open(&self.0)?;
        Ok(serde_wasm_bindgen::to_value(&clip)?)
    }

    /// A new one-time invite to this space: `{ secret, slot, sealed }`. PUT
    /// `sealed` to the relay under `slot`; the link carries `secret`.
    pub fn invite(
        &self,
        space_name: &str,
        inviter: &str,
        token: Option<String>,
    ) -> Result<JsValue, JsError> {
        let secret = InviteSecret::generate()?;
        let invite = Invite::new(space_name, inviter, token.as_deref(), &self.0);
        let sealed = NewInvite {
            secret: secret.to_string(),
            slot: secret.slot().to_string(),
            sealed: secret.seal(&invite)?,
        };
        Ok(serde_wasm_bindgen::to_value(&sealed)?)
    }

    /// For a clip's `Stream` item: seals the chunks to upload, or opens the
    /// downloaded ones.
    #[wasm_bindgen(js_name = streamCipher)]
    pub fn stream_cipher(&self, stream: JsValue) -> Result<WasmStreamCipher, JsError> {
        let stream: Stream = serde_wasm_bindgen::from_value(stream)?;
        Ok(WasmStreamCipher(stream.cipher(&self.0)?))
    }
}

#[derive(Serialize)]
struct NewInvite {
    secret: String,
    slot: String,
    #[serde(with = "serde_bytes")]
    sealed: Vec<u8>,
}

/// What a taken invite holds: `{ space, name, inviter, token }`, `space`
/// being the space's secret for `Pairing.fromSecret`.
#[derive(Serialize)]
struct OpenedInvite {
    space: String,
    name: String,
    inviter: String,
    token: Option<String>,
}

/// Where the relay keeps the invite behind `secret` (from a `#join=` link).
#[wasm_bindgen(js_name = inviteSlot)]
pub fn invite_slot(secret: &str) -> Result<String, JsError> {
    Ok(secret.parse::<InviteSecret>()?.slot().to_string())
}

/// Opens the sealed invite fetched from the relay.
#[wasm_bindgen(js_name = openInvite)]
pub fn open_invite(secret: &str, sealed: &[u8]) -> Result<JsValue, JsError> {
    let invite = secret.parse::<InviteSecret>()?.open(sealed)?;
    let opened = OpenedInvite {
        space: invite.pairing().to_secret(),
        name: invite.space_name.clone(),
        inviter: invite.inviter.clone(),
        token: invite.token.clone(),
    };
    Ok(serde_wasm_bindgen::to_value(&opened)?)
}

/// A new `Stream` item for `files` (`[{ name, mime, size }]`, in the order
/// their bytes follow each other), with a fresh salt.
#[wasm_bindgen(js_name = newStream)]
pub fn new_stream(files: JsValue, chunk_size: u32) -> Result<JsValue, JsError> {
    let files: Vec<StreamFile> = serde_wasm_bindgen::from_value(files)?;
    let mut stream = Stream::new(files)?;
    stream.chunk_size = chunk_size;
    Ok(serde_wasm_bindgen::to_value(&stream)?)
}

#[wasm_bindgen(js_name = StreamCipher)]
pub struct WasmStreamCipher(StreamCipher);

#[wasm_bindgen(js_class = StreamCipher)]
impl WasmStreamCipher {
    /// Chunk `index` (exactly its plaintext length) to upload. Seal each
    /// chunk once: sealing it again with other bytes would reuse a nonce.
    pub fn seal(&self, index: u32, chunk: Vec<u8>) -> Result<Vec<u8>, JsError> {
        Ok(self.0.seal(index.into(), chunk)?)
    }

    /// Chunk `index` as downloaded, decrypted. Throws if it was tampered
    /// with, reordered or cut short.
    pub fn open(&self, index: u32, chunk: Vec<u8>) -> Result<Vec<u8>, JsError> {
        Ok(self.0.open(index.into(), chunk)?)
    }
}
