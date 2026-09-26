//! YACS protocol core: space keys, one-time invites and codes, clip payloads and the
//! end-to-end encrypted envelope. Shared by the server, desktop, CLI and (via
//! WASM) the PWA, so it has no OS, clock or async dependencies.

pub mod api;
mod code;
mod envelope;
mod error;
mod invite;
mod pairing;
mod payload;
mod sealed;
mod stream;

pub use code::{
    CODE_TTL_SECS, Code, CodeInviter, CodeJoiner, CodeKey, MAX_NAMEPLATE, looks_like_code,
};
pub use envelope::{Envelope, PROTOCOL_VERSION};
pub use error::{Error, Result};
pub use invite::{Invite, InviteSecret, InviteSlot, MAX_INVITE_TTL_SECS, MAX_SEALED_INVITE};
pub use pairing::{ChannelId, ChannelKey, Pairing};
pub use payload::{Clip, ClipItem, File, Image, Payload};
pub use stream::{
    CHUNK_TAG_LEN, DEFAULT_CHUNK_SIZE, MAX_CHUNK_SIZE, MIN_CHUNK_SIZE, Stream, StreamCipher,
    StreamFile,
};
