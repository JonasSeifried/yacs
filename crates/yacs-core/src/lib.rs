//! YACS protocol core: pairing-phrase key derivation, clip payloads and the
//! end-to-end encrypted envelope. Shared by the server, desktop, CLI and (via
//! WASM) the PWA, so it has no OS, clock or async dependencies.

mod envelope;
mod error;
mod pairing;
mod payload;

pub use envelope::{Envelope, PROTOCOL_VERSION};
pub use error::{Error, Result};
pub use pairing::{ChannelId, ChannelKey, Pairing, normalize_phrase};
pub use payload::{Clip, ClipItem, Image, Payload};
