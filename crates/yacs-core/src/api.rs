//! JSON types of the relay's REST API, shared by server and clients.
//!
//! Clip bodies are not JSON: they travel as raw [`Envelope`](crate::Envelope)
//! bytes with content type [`ENVELOPE_CONTENT_TYPE`].

use serde::{Deserialize, Serialize};

pub const ENVELOPE_CONTENT_TYPE: &str = "application/octet-stream";

/// Response headers that carry a clip's metadata alongside its envelope body.
pub const HEADER_CLIP_ID: &str = "x-yacs-clip-id";
pub const HEADER_CREATED_AT: &str = "x-yacs-created-at";
pub const HEADER_EXPIRES_AT: &str = "x-yacs-expires-at";

/// What the server knows about a stored clip. Everything else is encrypted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipMeta {
    /// Server-assigned ULID. Sorts by creation time.
    pub id: String,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
    /// Envelope size in bytes.
    pub size: u64,
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
    /// A type from a newer relay. Treat it as "the history changed".
    #[serde(other)]
    Other,
}

/// Body of every non-2xx JSON response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: String,
}
