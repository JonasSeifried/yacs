//! `yacs-core` for the PWA. The browser only ever sees opaque envelopes and
//! plaintext clips; keys stay inside this module's memory except when a
//! pairing is exported with `secret()` to be stored.
//!
//! Clips cross into JS through serde, so a clip looks like
//! `{ created_at_ms, device_name, items: [{ Text: "…" }, { Image: { mime, data: Uint8Array } }] }`.
//! Big files are a `{ Stream: { salt: Uint8Array, chunk_size, files: [{ name, mime, size }] } }`
//! item, whose chunks a [`WasmStreamCipher`] seals and opens one at a time.

use wasm_bindgen::prelude::*;
use yacs_core::{Clip, Envelope, Payload, Stream, StreamCipher, StreamFile};

#[wasm_bindgen(js_name = Pairing)]
pub struct WasmPairing(yacs_core::Pairing);

#[wasm_bindgen(js_class = Pairing)]
impl WasmPairing {
    /// Deliberately slow (Argon2id with 64 MiB): about a second on a phone.
    #[wasm_bindgen(js_name = fromPhrase)]
    pub fn from_phrase(phrase: &str) -> Result<WasmPairing, JsError> {
        Ok(Self(yacs_core::Pairing::from_phrase(phrase)?))
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

    /// Channel id and key, to store the pairing.
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

    /// For a clip's `Stream` item: seals the chunks to upload, or opens the
    /// downloaded ones.
    #[wasm_bindgen(js_name = streamCipher)]
    pub fn stream_cipher(&self, stream: JsValue) -> Result<WasmStreamCipher, JsError> {
        let stream: Stream = serde_wasm_bindgen::from_value(stream)?;
        Ok(WasmStreamCipher(stream.cipher(&self.0)?))
    }
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

/// A new random 6-word pairing phrase.
#[wasm_bindgen(js_name = generatePhrase)]
pub fn generate_phrase() -> Result<String, JsError> {
    Ok(yacs_core::generate_phrase(yacs_core::DEFAULT_PHRASE_WORDS)?)
}
