//! JSON types of the relay's REST API, shared by server and clients.
//!
//! Clip bodies are not JSON: they travel as raw [`Envelope`](crate::Envelope)
//! bytes with content type [`ENVELOPE_CONTENT_TYPE`].

use serde::{Deserialize, Serialize};

use crate::stream::{DEFAULT_CHUNK_SIZE, MIN_CHUNK_SIZE};

pub const ENVELOPE_CONTENT_TYPE: &str = "application/octet-stream";

/// Response headers that carry a clip's metadata alongside its envelope body.
pub const HEADER_CLIP_ID: &str = "x-yacs-clip-id";
pub const HEADER_CREATED_AT: &str = "x-yacs-created-at";
pub const HEADER_EXPIRES_AT: &str = "x-yacs-expires-at";
/// Everything the clip takes on the relay, chunks included. Missing from relays before 0.3.0.
pub const HEADER_SIZE: &str = "x-yacs-size";
/// `1` when the clip has chunks (see [`ClipMeta::chunked`]).
pub const HEADER_CHUNKED: &str = "x-yacs-chunked";

/// Files up to this total go into the clip itself, even when the relay takes
/// chunks: they arrive in one request and can be previewed right away.
pub const INLINE_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// Room left in a clip's envelope for everything besides file contents.
const ENVELOPE_SLACK: u64 = 64 * 1024;

/// What the server knows about a stored clip. Everything else is encrypted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipMeta {
    /// Server-assigned ULID. Sorts by creation time.
    pub id: String,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
    /// Bytes stored: the envelope, plus its chunks if it has any.
    pub size: u64,
    /// The clip's files are stored as chunks next to it, fetched one by one
    /// (`GET …/clips/{id}/chunks/{i}`). The clip itself stays small.
    #[serde(default, skip_serializing_if = "core::ops::Not::not")]
    pub chunked: bool,
}

/// `GET /api/v1/config`: limits a client needs to build its UI (e.g. which TTL options to offer).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerConfig {
    pub default_ttl_secs: u64,
    pub max_ttl_secs: u64,
    pub max_size_bytes: u64,
    pub max_clips: usize,
    /// The relay's version, e.g. `0.2.0`. Missing from relays before 0.2.0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Set when the relay takes big files as chunks (0.3.0 and later).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunked: Option<ChunkedConfig>,
    /// Set when the relay registers spaces (0.5.0 and later): joining a
    /// space then needs no account key, and each space has its limits at
    /// `GET …/channels/{channel}/limits` ([`SpaceLimits`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accounts: Option<AccountsConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountsConfig {
    /// Anyone may create a space, on the free plan. Otherwise creating one
    /// takes the relay's account key.
    pub public: bool,
}

/// Which limits a space gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Plan {
    /// The public relay's plan for everyone.
    Free,
    /// Only the relay's own limits: spaces of its owner, and every space on
    /// a relay without accounts.
    Unlimited,
    /// A plan from a newer relay.
    #[serde(other)]
    Other,
}

/// `GET /api/v1/channels/{channel}/limits`: what this space may do, so apps
/// offer only TTLs and files that fit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceLimits {
    pub plan: Plan,
    pub default_ttl_secs: u64,
    pub max_ttl_secs: u64,
    /// Largest clip, chunks included. `None`: only the relay's disk limits it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_clip_bytes: Option<u64>,
    /// Bytes the space may send and receive per day (UTC), uploads and
    /// downloads together. `None`: no daily limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_transfer_bytes: Option<u64>,
    /// Of `daily_transfer_bytes`, used today.
    pub transfer_used_bytes: u64,
    pub max_clips: usize,
}

impl ServerConfig {
    /// The most file bytes to put into one clip. Bigger files go as chunks
    /// if [`chunked`](Self::chunked) is set, and can't be sent otherwise.
    pub fn inline_file_limit(&self) -> u64 {
        let fits = self.max_size_bytes.saturating_sub(ENVELOPE_SLACK);
        match self.chunked {
            Some(_) => fits.min(INLINE_FILE_BYTES),
            None => fits,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkedConfig {
    /// Largest plaintext chunk the relay takes.
    pub max_chunk_bytes: u64,
}

impl ChunkedConfig {
    /// Plaintext bytes per chunk for streams sent to this relay.
    pub fn chunk_size(&self) -> u32 {
        let max = u32::try_from(self.max_chunk_bytes).unwrap_or(u32::MAX);
        DEFAULT_CHUNK_SIZE.min(max).max(MIN_CHUNK_SIZE)
    }
}

/// Chunked uploads:
///
/// ```text
/// POST   …/uploads?ttl=&length=&chunk_size=   body: the clip's envelope → UploadCreated
/// PUT    …/uploads/{id}/chunks/{i}            body: sealed chunk i (any order, repeatable)
/// GET    …/uploads/{id}                       → UploadStatus
/// POST   …/uploads/{id}/complete              → ClipMeta; only now is the clip listed
/// DELETE …/uploads/{id}                       abort
/// ```
///
/// `length` is the sealed size of all chunks together and `chunk_size` the
/// sealed size of every chunk but the last (so the relay knows how many to
/// expect). The whole `length` counts against the relay's disk quota from
/// the start. Uploads idle for a day are dropped, and so are all of them
/// when the relay restarts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadCreated {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadStatus {
    /// Indexes of the chunks the relay has, ascending.
    pub received: Vec<u64>,
}

/// `GET /api/v1/channels/{channel}/events` is a server-sent event stream with
/// one of these as JSON in each message's `data`. It says what changed; clips
/// are fetched as usual. Clients should re-list after (re)connecting, since
/// events sent while they were away are gone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChannelEvent {
    /// A new clip. Older ones may have been evicted to make room.
    Added {
        clip: ClipMeta,
    },
    Deleted {
        id: String,
    },
    Cleared,
    /// Someone took one of the space's invites (`GET /api/v1/invites/{slot}`),
    /// so it's gone. Older clients see it as [`Other`](Self::Other).
    InviteUsed {
        slot: String,
    },
    /// A type from a newer relay. Treat it as "the history changed".
    #[serde(other)]
    Other,
}

// One-time invites (see [`crate::InviteSecret`]), sealed bytes of at most
// [`crate::MAX_SEALED_INVITE`]:
//
// ```text
// PUT    …/channels/{channel}/invites/{slot}?ttl=   body: the sealed invite → 201
// DELETE …/channels/{channel}/invites/{slot}        take it back → 204, or 404
// GET    /api/v1/invites/{slot}                     → 200 with the sealed invite once, then 404
// ```
//
// The `GET` needs no access token: the invited device has none yet, and the
// slot is as hard to guess as a key. Taking an invite sends
// [`ChannelEvent::InviteUsed`] to its space. `ttl` is at most
// [`crate::MAX_INVITE_TTL_SECS`], which is also the default.

// Rendezvous for typed codes (see `crate::Code`): side `a` is the inviter,
// a member of the space, side `b` the device that typed the code. Each
// message is written once and at most 4096 bytes; reads wait up to `wait`
// seconds (at most 25) and answer 204 if nothing came. A rendezvous lasts
// `crate::CODE_TTL_SECS`, at most 2 per space; 404 means it's gone.
//
// ```text
// POST   …/channels/{channel}/rendezvous             body: message a/0 → 201 RendezvousOpened
// PUT    …/channels/{channel}/rendezvous/{n}/a/{i}   write a/i
// GET    …/channels/{channel}/rendezvous/{n}/b/{i}   read b/i
// DELETE …/channels/{channel}/rendezvous/{n}         close it
// GET    /api/v1/rendezvous/{n}/a/{i}                read a/i (no token: the joiner has none);
//                                                    reading a/1 closes the rendezvous
// PUT    /api/v1/rendezvous/{n}/b/{i}                write b/i (no token)
// ```

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RendezvousOpened {
    /// The number the code starts with.
    pub nameplate: u16,
}

/// Body of every non-2xx JSON response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: String,
}
