//! `yacs-core` for the PWA. The browser only ever sees opaque envelopes and
//! plaintext clips; keys stay inside this module's memory except when a
//! pairing is exported with `secret()` to be stored.
//!
//! Clips cross into JS through serde, so a clip looks like
//! `{ created_at_ms, device_name, items: [{ Text: "…" }, { Image: { mime, data: Uint8Array } }] }`.

use wasm_bindgen::prelude::*;
use yacs_core::{Clip, Envelope, Payload};

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
}

/// A new random 6-word pairing phrase.
#[wasm_bindgen(js_name = generatePhrase)]
pub fn generate_phrase() -> Result<String, JsError> {
    Ok(yacs_core::generate_phrase(yacs_core::DEFAULT_PHRASE_WORDS)?)
}
