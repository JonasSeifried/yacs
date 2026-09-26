use std::io;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, MatchedPath, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use futures_util::TryStreamExt;
use serde::Deserialize;
use subtle::ConstantTimeEq;
use tokio_util::io::ReaderStream;
use tower_http::trace::{DefaultOnResponse, TraceLayer};
use tracing::Level;
use ulid::Ulid;
use yacs_core::api::{
    ChannelEvent, ChunkedConfig, ClipMeta, ENVELOPE_CONTENT_TYPE, ErrorBody, HEADER_CHUNKED,
    HEADER_CLIP_ID, HEADER_CREATED_AT, HEADER_EXPIRES_AT, HEADER_SIZE, RendezvousOpened,
    ServerConfig, UploadCreated, UploadStatus,
};
use yacs_core::{
    CODE_TTL_SECS, ChannelId, Envelope, InviteSlot, MAX_CHUNK_SIZE, MAX_INVITE_TTL_SECS,
    MAX_SEALED_INVITE,
};

use crate::clock::Clock;
use crate::config::Config;
use crate::events::Events;
use crate::invites::{InviteError, Invites};
use crate::rendezvous::{MAX_INDEX, MAX_MESSAGE, Rendezvous, RendezvousError, Side};
use crate::store::{ChunkLayout, IO_BUFFER, PutError, Store, UploadError};

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Store>,
    pub config: Arc<Config>,
    pub clock: Arc<dyn Clock>,
    pub events: Arc<Events>,
    pub invites: Arc<Invites>,
    pub rendezvous: Arc<Rendezvous>,
}

pub fn router(state: AppState) -> Router {
    let max_size = state.config.max_size.as_u64() as usize;
    let api = Router::new()
        .route("/config", get(server_config))
        .route(
            "/channels/{channel}/clips",
            get(list).post(create).delete(clear),
        )
        .route("/channels/{channel}/clips/latest", get(latest))
        .route("/channels/{channel}/events", get(events))
        .route(
            "/channels/{channel}/clips/{id}",
            get(get_clip).delete(delete_clip),
        )
        .route(
            "/channels/{channel}/clips/{id}/chunks/{index}",
            get(get_chunk),
        )
        .route(
            "/channels/{channel}/invites/{slot}",
            put(create_invite).delete(revoke_invite),
        )
        .route("/channels/{channel}/rendezvous", post(open_rendezvous))
        .route(
            "/channels/{channel}/rendezvous/{nameplate}",
            axum::routing::delete(close_rendezvous),
        )
        .route(
            "/channels/{channel}/rendezvous/{nameplate}/a/{index}",
            put(inviter_writes),
        )
        .route(
            "/channels/{channel}/rendezvous/{nameplate}/b/{index}",
            get(inviter_reads),
        )
        .route("/channels/{channel}/uploads", post(create_upload))
        .route(
            "/channels/{channel}/uploads/{id}",
            get(upload_status).delete(abort_upload),
        )
        .route(
            "/channels/{channel}/uploads/{id}/complete",
            post(complete_upload),
        )
        .layer(DefaultBodyLimit::max(max_size))
        // Streamed to disk and limited by the upload's layout instead.
        .route(
            "/channels/{channel}/uploads/{id}/chunks/{index}",
            put(put_chunk).layer(DefaultBodyLimit::disable()),
        )
        .route_layer(middleware::from_fn_with_state(state.clone(), require_token))
        // The invited device has no token yet.
        .route("/invites/{slot}", get(take_invite))
        .route("/rendezvous/{nameplate}/a/{index}", get(joiner_reads))
        .route("/rendezvous/{nameplate}/b/{index}", put(joiner_writes));

    Router::new()
        .nest("/api/v1", api)
        .route("/healthz", get(|| async { "ok" }))
        .merge(crate::web::routes())
        .with_state(state)
        .layer(
            // Log the route pattern, not the URI: the URI contains the channel id.
            TraceLayer::new_for_http()
                .make_span_with(|req: &Request| {
                    let route = req
                        .extensions()
                        .get::<MatchedPath>()
                        .map_or("<unmatched>", MatchedPath::as_str);
                    tracing::info_span!("request", method = %req.method(), route)
                })
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
}

#[derive(Debug)]
enum ApiError {
    BadRequest(&'static str),
    Unauthorized,
    NotFound,
    TooLarge,
    Conflict(&'static str),
    TooManyUploads,
    TooManyInvites,
    TooManyCodes,
    StorageFull,
    Internal(io::Error),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg),
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "missing or wrong access token"),
            Self::NotFound => (StatusCode::NOT_FOUND, "not found"),
            Self::TooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "chunk is larger than announced",
            ),
            Self::Conflict(msg) => (StatusCode::CONFLICT, msg),
            Self::TooManyUploads => (
                StatusCode::TOO_MANY_REQUESTS,
                "too many uploads in progress on this channel",
            ),
            Self::TooManyInvites => (
                StatusCode::TOO_MANY_REQUESTS,
                "too many open invites for this channel",
            ),
            Self::TooManyCodes => (
                StatusCode::TOO_MANY_REQUESTS,
                "too many codes open for this channel",
            ),
            Self::StorageFull => (StatusCode::INSUFFICIENT_STORAGE, "server storage is full"),
            Self::Internal(e) => {
                tracing::error!(error = %e, "storage error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error")
            }
        };
        let body = Json(ErrorBody {
            error: message.into(),
        });
        (status, body).into_response()
    }
}

impl From<io::Error> for ApiError {
    fn from(e: io::Error) -> Self {
        Self::Internal(e)
    }
}

impl From<PutError> for ApiError {
    fn from(e: PutError) -> Self {
        match e {
            PutError::Full => Self::StorageFull,
            PutError::Io(e) => Self::Internal(e),
        }
    }
}

impl From<RendezvousError> for ApiError {
    fn from(e: RendezvousError) -> Self {
        match e {
            RendezvousError::NotFound => Self::NotFound,
            RendezvousError::Taken => Self::Conflict("that message was written already"),
            RendezvousError::TooMany | RendezvousError::Full => Self::TooManyCodes,
        }
    }
}

impl From<UploadError> for ApiError {
    fn from(e: UploadError) -> Self {
        match e {
            UploadError::Full => Self::StorageFull,
            UploadError::TooMany => Self::TooManyUploads,
            UploadError::NotFound => Self::NotFound,
            UploadError::Invalid(msg) => Self::BadRequest(msg),
            UploadError::TooLarge => Self::TooLarge,
            UploadError::Incomplete => Self::Conflict("the upload is missing chunks"),
            UploadError::Io(e) => Self::Internal(e),
        }
    }
}

fn parse_channel(s: &str) -> Result<ChannelId, ApiError> {
    s.parse()
        .map_err(|_| ApiError::BadRequest("invalid channel id"))
}

fn parse_slot(s: &str) -> Result<InviteSlot, ApiError> {
    s.parse()
        .map_err(|_| ApiError::BadRequest("invalid invite slot"))
}

fn parse_id(s: &str) -> Result<Ulid, ApiError> {
    s.parse()
        .map_err(|_| ApiError::BadRequest("invalid clip id"))
}

async fn require_token(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    if let Some(expected) = &state.config.access_token {
        let given = req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or("");
        if !bool::from(given.as_bytes().ct_eq(expected.as_bytes())) {
            return Err(ApiError::Unauthorized);
        }
    }
    Ok(next.run(req).await)
}

async fn server_config(State(state): State<AppState>) -> Json<ServerConfig> {
    let c = &state.config;
    Json(ServerConfig {
        default_ttl_secs: c.default_ttl.as_secs(),
        max_ttl_secs: c.max_ttl.as_secs(),
        max_size_bytes: c.max_size.as_u64(),
        max_clips: c.max_clips_per_channel,
        version: Some(env!("CARGO_PKG_VERSION").into()),
        chunked: Some(ChunkedConfig {
            max_chunk_bytes: u64::from(MAX_CHUNK_SIZE),
        }),
    })
}

#[derive(Deserialize)]
struct CreateQuery {
    /// Seconds. Missing means the server default; longer than the max is clamped.
    ttl: Option<u64>,
}

async fn create(
    State(state): State<AppState>,
    Path(channel): Path<String>,
    Query(query): Query<CreateQuery>,
    body: Bytes,
) -> Result<(StatusCode, Json<ClipMeta>), ApiError> {
    let channel = parse_channel(&channel)?;
    check_envelope(&body)?;
    let ttl_ms = ttl_ms(&state.config, query.ttl)?;
    let now = state.clock.now_ms();
    let meta = state.store.put(&channel, &body, now, now + ttl_ms).await?;
    state
        .events
        .publish(&channel, ChannelEvent::Added { clip: meta.clone() });
    Ok((StatusCode::CREATED, Json(meta)))
}

/// Only the structure is checked; the server can't (and shouldn't) decrypt.
fn check_envelope(body: &[u8]) -> Result<(), ApiError> {
    Envelope::from_bytes(body).map_err(|_| ApiError::BadRequest("body is not a YACS envelope"))?;
    Ok(())
}

fn ttl_ms(config: &Config, ttl_secs: Option<u64>) -> Result<u64, ApiError> {
    Ok(match ttl_secs {
        None => config.default_ttl.as_millis() as u64,
        Some(0) => return Err(ApiError::BadRequest("ttl must be greater than zero")),
        Some(secs) => secs
            .saturating_mul(1000)
            .min(config.max_ttl.as_millis() as u64),
    })
}

async fn create_invite(
    State(state): State<AppState>,
    Path((channel, slot)): Path<(String, String)>,
    Query(query): Query<CreateQuery>,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let (channel, slot) = (parse_channel(&channel)?, parse_slot(&slot)?);
    if body.is_empty() || body.len() > MAX_SEALED_INVITE {
        return Err(ApiError::BadRequest("an invite is 1 to 4096 bytes"));
    }
    let ttl_ms = match query.ttl {
        Some(0) => return Err(ApiError::BadRequest("ttl must be greater than zero")),
        ttl => ttl.unwrap_or(MAX_INVITE_TTL_SECS).min(MAX_INVITE_TTL_SECS) * 1000,
    };
    let now = state.clock.now_ms();
    state
        .invites
        .put(channel, slot, body, now, now + ttl_ms)
        .map_err(|e| match e {
            InviteError::Exists => ApiError::Conflict("there's already an invite in that slot"),
            InviteError::TooMany => ApiError::TooManyInvites,
        })?;
    Ok(StatusCode::CREATED)
}

async fn revoke_invite(
    State(state): State<AppState>,
    Path((channel, slot)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let (channel, slot) = (parse_channel(&channel)?, parse_slot(&slot)?);
    match state.invites.revoke(&channel, &slot) {
        true => Ok(StatusCode::NO_CONTENT),
        false => Err(ApiError::NotFound),
    }
}

async fn take_invite(
    State(state): State<AppState>,
    Path(slot): Path<String>,
) -> Result<Response, ApiError> {
    let slot = parse_slot(&slot)?;
    let (channel, sealed) = state
        .invites
        .take(&slot, state.clock.now_ms())
        .ok_or(ApiError::NotFound)?;
    state.events.publish(
        &channel,
        ChannelEvent::InviteUsed {
            slot: slot.to_string(),
        },
    );
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static(ENVELOPE_CONTENT_TYPE),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        sealed,
    )
        .into_response())
}

#[derive(Deserialize)]
struct WaitQuery {
    /// Seconds to wait for the message; at most 25.
    wait: Option<u64>,
}

fn check_message(body: &[u8]) -> Result<(), ApiError> {
    if body.is_empty() || body.len() > MAX_MESSAGE {
        return Err(ApiError::BadRequest("a message is 1 to 4096 bytes"));
    }
    Ok(())
}

fn check_index(index: u8) -> Result<u8, ApiError> {
    match index <= MAX_INDEX {
        true => Ok(index),
        false => Err(ApiError::BadRequest("no such message")),
    }
}

async fn open_rendezvous(
    State(state): State<AppState>,
    Path(channel): Path<String>,
    body: Bytes,
) -> Result<(StatusCode, Json<RendezvousOpened>), ApiError> {
    let channel = parse_channel(&channel)?;
    check_message(&body)?;
    let now = state.clock.now_ms();
    let nameplate = state
        .rendezvous
        .open(channel, body, now, now + CODE_TTL_SECS * 1000)?;
    Ok((StatusCode::CREATED, Json(RendezvousOpened { nameplate })))
}

async fn close_rendezvous(
    State(state): State<AppState>,
    Path((channel, nameplate)): Path<(String, u16)>,
) -> Result<StatusCode, ApiError> {
    let channel = parse_channel(&channel)?;
    match state
        .rendezvous
        .close(&channel, nameplate, state.clock.now_ms())
    {
        true => Ok(StatusCode::NO_CONTENT),
        false => Err(ApiError::NotFound),
    }
}

async fn inviter_writes(
    State(state): State<AppState>,
    Path((channel, nameplate, index)): Path<(String, u16, u8)>,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let channel = parse_channel(&channel)?;
    check_message(&body)?;
    let now = state.clock.now_ms();
    state.rendezvous.put(
        nameplate,
        Side::A,
        check_index(index)?,
        body,
        Some(&channel),
        now,
    )?;
    Ok(StatusCode::NO_CONTENT)
}

async fn joiner_writes(
    State(state): State<AppState>,
    Path((nameplate, index)): Path<(u16, u8)>,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    check_message(&body)?;
    let now = state.clock.now_ms();
    state
        .rendezvous
        .put(nameplate, Side::B, check_index(index)?, body, None, now)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn inviter_reads(
    State(state): State<AppState>,
    Path((channel, nameplate, index)): Path<(String, u16, u8)>,
    Query(query): Query<WaitQuery>,
) -> Result<Response, ApiError> {
    let channel = parse_channel(&channel)?;
    let index = check_index(index)?;
    read_message(&state, nameplate, Side::B, index, Some(&channel), query).await
}

async fn joiner_reads(
    State(state): State<AppState>,
    Path((nameplate, index)): Path<(u16, u8)>,
    Query(query): Query<WaitQuery>,
) -> Result<Response, ApiError> {
    let index = check_index(index)?;
    read_message(&state, nameplate, Side::A, index, None, query).await
}

async fn read_message(
    state: &AppState,
    nameplate: u16,
    side: Side,
    index: u8,
    channel: Option<&ChannelId>,
    query: WaitQuery,
) -> Result<Response, ApiError> {
    let wait = std::time::Duration::from_secs(query.wait.unwrap_or(25));
    let clock = state.clock.clone();
    let message = state
        .rendezvous
        .read(nameplate, side, index, channel, || clock.now_ms(), wait)
        .await?;
    Ok(match message {
        Some(message) => (
            [
                (
                    header::CONTENT_TYPE,
                    HeaderValue::from_static(ENVELOPE_CONTENT_TYPE),
                ),
                (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            ],
            message,
        )
            .into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    })
}

#[derive(Deserialize)]
struct UploadQuery {
    ttl: Option<u64>,
    /// Sealed bytes of all chunks.
    length: u64,
    /// Sealed bytes of every chunk but the last.
    chunk_size: u64,
}

async fn create_upload(
    State(state): State<AppState>,
    Path(channel): Path<String>,
    Query(query): Query<UploadQuery>,
    body: Bytes,
) -> Result<(StatusCode, Json<UploadCreated>), ApiError> {
    let channel = parse_channel(&channel)?;
    check_envelope(&body)?;
    let ttl_ms = ttl_ms(&state.config, query.ttl)?;
    let layout = ChunkLayout::new(query.length, query.chunk_size).map_err(ApiError::BadRequest)?;
    let id = state
        .store
        .create_upload(&channel, &body, layout, ttl_ms, state.clock.now_ms())
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(UploadCreated { id: id.to_string() }),
    ))
}

async fn put_chunk(
    State(state): State<AppState>,
    Path((channel, id, index)): Path<(String, String, u64)>,
    body: Body,
) -> Result<StatusCode, ApiError> {
    let (channel, id) = (parse_channel(&channel)?, parse_id(&id)?);
    let body = body.into_data_stream().map_err(io::Error::other);
    state
        .store
        .put_chunk(&channel, id, index, body, state.clock.now_ms())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn upload_status(
    State(state): State<AppState>,
    Path((channel, id)): Path<(String, String)>,
) -> Result<Json<UploadStatus>, ApiError> {
    let (channel, id) = (parse_channel(&channel)?, parse_id(&id)?);
    let received = state
        .store
        .upload_status(&channel, id)
        .ok_or(ApiError::NotFound)?;
    Ok(Json(UploadStatus { received }))
}

async fn complete_upload(
    State(state): State<AppState>,
    Path((channel, id)): Path<(String, String)>,
) -> Result<(StatusCode, Json<ClipMeta>), ApiError> {
    let (channel, id) = (parse_channel(&channel)?, parse_id(&id)?);
    let meta = state
        .store
        .complete_upload(&channel, id, state.clock.now_ms())
        .await?;
    state
        .events
        .publish(&channel, ChannelEvent::Added { clip: meta.clone() });
    Ok((StatusCode::CREATED, Json(meta)))
}

async fn abort_upload(
    State(state): State<AppState>,
    Path((channel, id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let (channel, id) = (parse_channel(&channel)?, parse_id(&id)?);
    match state.store.abort_upload(&channel, id).await? {
        true => Ok(StatusCode::NO_CONTENT),
        false => Err(ApiError::NotFound),
    }
}

async fn get_chunk(
    State(state): State<AppState>,
    Path((channel, id, index)): Path<(String, String, u64)>,
) -> Result<Response, ApiError> {
    let (channel, id) = (parse_channel(&channel)?, parse_id(&id)?);
    let now = state.clock.now_ms();
    let (meta, file, len) = state
        .store
        .chunk(&channel, id, index, now)
        .await?
        .ok_or(ApiError::NotFound)?;
    let max_age = meta.expires_at_ms.saturating_sub(now) / 1000;
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static(ENVELOPE_CONTENT_TYPE),
            ),
            (header::CONTENT_LENGTH, HeaderValue::from(len)),
            (
                header::CACHE_CONTROL,
                HeaderValue::try_from(format!("private, max-age={max_age}, immutable"))
                    .expect("numbers are valid header values"),
            ),
        ],
        Body::from_stream(ReaderStream::with_capacity(file, IO_BUFFER)),
    )
        .into_response())
}

async fn list(
    State(state): State<AppState>,
    Path(channel): Path<String>,
) -> Result<Json<Vec<ClipMeta>>, ApiError> {
    let channel = parse_channel(&channel)?;
    Ok(Json(
        state.store.list(&channel, state.clock.now_ms()).await?,
    ))
}

async fn latest(
    State(state): State<AppState>,
    Path(channel): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let channel = parse_channel(&channel)?;
    let (meta, body) = state
        .store
        .latest(&channel, state.clock.now_ms())
        .await?
        .ok_or(ApiError::NotFound)?;

    let etag = etag(&meta);
    if headers
        .get(header::IF_NONE_MATCH)
        .is_some_and(|v| v.as_bytes() == etag.as_bytes() || v.as_bytes() == b"*")
    {
        return Ok((StatusCode::NOT_MODIFIED, [(header::ETAG, etag)]).into_response());
    }
    Ok(envelope_response(&meta, body, "private, no-cache"))
}

async fn get_clip(
    State(state): State<AppState>,
    Path((channel, id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let (channel, id) = (parse_channel(&channel)?, parse_id(&id)?);
    let now = state.clock.now_ms();
    let (meta, body) = state
        .store
        .get(&channel, id, now)
        .await?
        .ok_or(ApiError::NotFound)?;
    // A clip never changes, so it may be cached until it expires.
    let max_age = meta.expires_at_ms.saturating_sub(now) / 1000;
    let cache = format!("private, max-age={max_age}, immutable");
    Ok(envelope_response(&meta, body, &cache))
}

async fn delete_clip(
    State(state): State<AppState>,
    Path((channel, id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let (channel, id) = (parse_channel(&channel)?, parse_id(&id)?);
    if state.store.delete(&channel, id).await? {
        state
            .events
            .publish(&channel, ChannelEvent::Deleted { id: id.to_string() });
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound)
    }
}

async fn clear(
    State(state): State<AppState>,
    Path(channel): Path<String>,
) -> Result<StatusCode, ApiError> {
    let channel = parse_channel(&channel)?;
    state.store.clear(&channel).await?;
    state.events.publish(&channel, ChannelEvent::Cleared);
    Ok(StatusCode::NO_CONTENT)
}

async fn events(
    State(state): State<AppState>,
    Path(channel): Path<String>,
) -> Result<Response, ApiError> {
    let channel = parse_channel(&channel)?;
    // Tells nginx to pass each event on at once instead of buffering them.
    let no_buffering = (HeaderName::from_static("x-accel-buffering"), "no");
    Ok(([no_buffering], state.events.stream(&channel)).into_response())
}

fn etag(meta: &ClipMeta) -> String {
    format!("\"{}\"", meta.id)
}

fn envelope_response(meta: &ClipMeta, body: Vec<u8>, cache_control: &str) -> Response {
    let header =
        |v: String| HeaderValue::try_from(v).expect("ids and numbers are valid header values");
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static(ENVELOPE_CONTENT_TYPE),
            ),
            (header::ETAG, header(etag(meta))),
            (header::CACHE_CONTROL, header(cache_control.to_owned())),
        ],
        [
            (HEADER_CLIP_ID, header(meta.id.clone())),
            (HEADER_CREATED_AT, header(meta.created_at_ms.to_string())),
            (HEADER_EXPIRES_AT, header(meta.expires_at_ms.to_string())),
            (HEADER_SIZE, header(meta.size.to_string())),
            (HEADER_CHUNKED, header(u8::from(meta.chunked).to_string())),
        ],
        body,
    )
        .into_response()
}
