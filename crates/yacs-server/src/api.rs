use std::io;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::{
    DefaultBodyLimit, Extension, MatchedPath, Path, Query, RawPathParams, Request, State,
};
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
    AccountsConfig, ChannelEvent, ChunkedConfig, ClipMeta, ENVELOPE_CONTENT_TYPE, ErrorBody,
    HEADER_CHUNKED, HEADER_CLIP_ID, HEADER_CREATED_AT, HEADER_EXPIRES_AT, HEADER_SIZE, Plan,
    RendezvousOpened, ServerConfig, SpaceLimits, UploadCreated, UploadStatus,
};
use yacs_core::{
    CODE_TTL_SECS, ChannelId, Envelope, InviteSlot, MAX_CHUNK_SIZE, MAX_INVITE_TTL_SECS,
    MAX_SEALED_INVITE,
};

use crate::accounts::{Accounts, Limits, Refusal};
use crate::clients::{Client, Clients, Connection, Crowded};
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
    pub accounts: Arc<Accounts>,
    pub clients: Arc<Clients>,
}

/// Longest a free upload may take to complete, at least: its plan's longest TTL.
const MIN_UPLOAD_DEADLINE_MS: u64 = 3600 * 1000;

pub fn router(state: AppState) -> Router {
    // Clips and upload headers read their bodies themselves, up to their
    // space's plan (see `read_body`); the rest are small.
    let invite_limit = DefaultBodyLimit::max(MAX_SEALED_INVITE);
    let message_limit = DefaultBodyLimit::max(MAX_MESSAGE);
    let api = Router::new()
        .route("/channels/{channel}/limits", get(limits))
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
            put(create_invite).delete(revoke_invite).layer(invite_limit),
        )
        .route(
            "/channels/{channel}/rendezvous",
            post(open_rendezvous).layer(message_limit),
        )
        .route(
            "/channels/{channel}/rendezvous/{nameplate}",
            axum::routing::delete(close_rendezvous),
        )
        .route(
            "/channels/{channel}/rendezvous/{nameplate}/a/{index}",
            put(inviter_writes).layer(message_limit),
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
        // Streamed to disk and limited by the upload's layout instead.
        .route(
            "/channels/{channel}/uploads/{id}/chunks/{index}",
            put(put_chunk).layer(DefaultBodyLimit::disable()),
        )
        .route_layer(middleware::from_fn_with_state(state.clone(), authorize))
        .route("/config", get(server_config))
        .route("/address", get(address))
        // The invited device isn't in the space yet.
        .route("/invites/{slot}", get(take_invite))
        .route("/rendezvous/{nameplate}/a/{index}", get(joiner_reads))
        .route(
            "/rendezvous/{nameplate}/b/{index}",
            put(joiner_writes).layer(message_limit),
        )
        .layer(middleware::from_fn_with_state(state.clone(), identify))
        .layer(middleware::map_response(api_headers));

    Router::new()
        .nest("/api/v1", api)
        .route("/healthz", get(|| async { "ok" }))
        .route("/privacy", get(privacy))
        .route("/imprint", get(imprint))
        .merge(crate::web::routes())
        .with_state(state)
        .layer(middleware::map_response(hsts))
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

/// HTTPS only, for a year: the web app holds the space keys, so it mustn't be
/// served over plain HTTP even once. Browsers ignore this over HTTP, so a
/// relay without HTTPS (say, on a LAN) keeps working.
async fn hsts(mut res: Response) -> Response {
    res.headers_mut().insert(
        header::STRICT_TRANSPORT_SECURITY,
        HeaderValue::from_static("max-age=31536000"),
    );
    res
}

/// API responses carry bytes anyone in a space can upload, from the web
/// app's origin: never render or run them.
async fn api_headers(mut res: Response) -> Response {
    let headers = res.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'"),
    );
    res
}

async fn privacy(State(state): State<AppState>) -> Response {
    legal_page(state.config.privacy_url.as_deref())
}

async fn imprint(State(state): State<AppState>) -> Response {
    legal_page(state.config.imprint_url.as_deref())
}

fn legal_page(url: Option<&str>) -> Response {
    match url {
        Some(url) => axum::response::Redirect::to(url).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(Debug)]
enum ApiError {
    BadRequest(&'static str),
    Unauthorized,
    NotFound,
    TooLarge,
    /// Over the space's plan.
    OverPlan(String),
    TransferUsed,
    TooManySpaces,
    RateLimited,
    Conflict(&'static str),
    TooManyUploads,
    TooManyInvites,
    TooManyCodes,
    TooManyLookups,
    TooManyConnections,
    TooManyListeners,
    TooManyBuffered,
    UploadsUsed,
    BodyTooLarge,
    StorageFull,
    Internal(io::Error),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg),
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "this relay needs its account key to create a space",
            ),
            Self::NotFound => (StatusCode::NOT_FOUND, "not found"),
            Self::TooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "chunk is larger than announced",
            ),
            Self::OverPlan(msg) => {
                return (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    Json(ErrorBody { error: msg }),
                )
                    .into_response();
            }
            Self::TransferUsed => (
                StatusCode::TOO_MANY_REQUESTS,
                "this space used up today's transfer; it starts over at midnight UTC",
            ),
            Self::TooManySpaces => (
                StatusCode::TOO_MANY_REQUESTS,
                "too many new spaces from your address today; try again tomorrow",
            ),
            Self::RateLimited => (
                StatusCode::TOO_MANY_REQUESTS,
                "too many requests; slow down",
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
            Self::TooManyLookups => (
                StatusCode::TOO_MANY_REQUESTS,
                "too many codes tried from your address; try again in a few minutes",
            ),
            Self::TooManyConnections => (
                StatusCode::TOO_MANY_REQUESTS,
                "too many open connections from your address",
            ),
            Self::TooManyListeners => (
                StatusCode::TOO_MANY_REQUESTS,
                "too many devices are listening to this space",
            ),
            Self::TooManyBuffered => (
                StatusCode::TOO_MANY_REQUESTS,
                "too many uploads at once from your address",
            ),
            Self::UploadsUsed => (
                StatusCode::TOO_MANY_REQUESTS,
                "your address uploaded its share for today; it starts over at midnight UTC",
            ),
            Self::BodyTooLarge => (StatusCode::PAYLOAD_TOO_LARGE, "the request is too large"),
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
            UploadError::Busy => Self::Conflict("that chunk is being written already"),
            UploadError::TransferUsed => Self::TransferUsed,
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

/// Resolves the space's account, registering the space if it's new, and
/// hands its [`Limits`] to the handler.
async fn authorize(
    State(state): State<AppState>,
    params: RawPathParams,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let channel = params
        .iter()
        .find(|(name, _)| *name == "channel")
        .map(|(_, value)| parse_channel(value))
        .expect("authorize only guards routes under a channel")?;
    let key = bearer(req.headers());
    let config = &state.config;
    let client = client_of(&req);
    let account = state
        .accounts
        .authorize(config, &channel, key, client, state.clock.now_ms())
        .await
        .map_err(|refusal| match refusal {
            Refusal::Unauthorized => ApiError::Unauthorized,
            Refusal::TooManySpaces => ApiError::TooManySpaces,
        })?;
    let limits = match account {
        Some(account) => Limits::of(account, config),
        None => Limits::unlimited(config),
    };
    req.extensions_mut().insert(limits);
    Ok(next.run(req).await)
}

/// Who's asking, as a [`Client`] extension for the handlers; and per-address
/// request rates, on a public relay only.
async fn identify(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let client = Client::of(&req);
    state.clients.check_forwarding(&req, &client);
    req.extensions_mut().insert(client);
    let clients = &state.clients;
    if !clients.allow(&clients.requests, &client, state.clock.now_ms()) {
        // Only here: other 429s (quotas) don't pass by waiting a moment.
        let secs = (60 / state.config.requests_per_minute).max(1);
        return Ok((
            [(header::RETRY_AFTER, HeaderValue::from(secs))],
            ApiError::RateLimited,
        )
            .into_response());
    }
    Ok(next.run(req).await)
}

fn client_of(req: &Request) -> Client {
    req.extensions()
        .get::<Client>()
        .copied()
        .unwrap_or_else(|| Client::of(req))
}

/// The address this relay counts the caller's limits under: for checking that
/// a reverse proxy passes on the client's address. Only ever the caller's own.
async fn address(Extension(client): Extension<Client>) -> Response {
    (
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        format!("{}\n", client.display()),
    )
        .into_response()
}

/// An upload counted against the uploader's address (for a free space, on a
/// public relay) and the space's daily transfer. Given back when dropped,
/// unless [`keep`](Self::keep) says the upload was stored.
struct Charged<'a> {
    state: &'a AppState,
    channel: ChannelId,
    client: Client,
    bytes: u64,
    now_ms: u64,
    address: bool,
    space: bool,
}

impl<'a> Charged<'a> {
    fn take(
        state: &'a AppState,
        channel: &ChannelId,
        client: &Client,
        limits: &Limits,
        bytes: u64,
    ) -> Result<Self, ApiError> {
        let mut charged = Self {
            state,
            channel: *channel,
            client: *client,
            bytes,
            now_ms: state.clock.now_ms(),
            address: false,
            space: false,
        };
        if limits.plan == Plan::Free && state.clients.enabled() {
            let limit = state.config.free_daily_upload_per_ip.as_u64();
            if !state
                .accounts
                .charge_upload(client, bytes, limit, charged.now_ms)
            {
                return Err(ApiError::UploadsUsed);
            }
            charged.address = true;
        }
        charge(state, channel, limits, bytes)?;
        charged.space = limits.daily_transfer.is_some();
        Ok(charged)
    }

    fn keep(mut self) {
        (self.address, self.space) = (false, false);
    }
}

impl Drop for Charged<'_> {
    fn drop(&mut self) {
        let accounts = &self.state.accounts;
        if self.address {
            accounts.refund_upload(&self.client, self.bytes, self.now_ms);
        }
        if self.space {
            accounts.refund(&self.channel, self.bytes, self.now_ms);
        }
    }
}

/// A place among the uploads `client` has in memory at once, on a public relay.
fn buffer(state: &AppState, client: Client) -> Result<Option<Connection>, ApiError> {
    state
        .clients
        .buffer(client)
        .map_err(|Crowded| ApiError::TooManyBuffered)
}

/// The body, up to what the space's plan allows for a clip.
async fn read_body(state: &AppState, limits: &Limits, body: Body) -> Result<Bytes, ApiError> {
    let max = state.config.max_size.as_u64();
    let limit = limits.max_clip_bytes.map_or(max, |plan| plan.min(max));
    match axum::body::to_bytes(body, limit as usize).await {
        Ok(body) => Ok(body),
        // Over the plan's size, if it has one: say how big a clip may be.
        Err(_) => check_clip_size(limits, u64::MAX).and(Err(ApiError::BodyTooLarge)),
    }
}

/// Counts `bytes` against the space's daily transfer.
fn charge(
    state: &AppState,
    channel: &ChannelId,
    limits: &Limits,
    bytes: u64,
) -> Result<(), ApiError> {
    match state
        .accounts
        .charge(channel, bytes, limits.daily_transfer, state.clock.now_ms())
    {
        true => Ok(()),
        false => Err(ApiError::TransferUsed),
    }
}

/// Refuses clips bigger than the space's plan allows.
fn check_clip_size(limits: &Limits, bytes: u64) -> Result<(), ApiError> {
    match limits.max_clip_bytes {
        Some(max) if bytes > max => Err(ApiError::OverPlan(format!(
            "too large for this space: at most {}",
            bytesize::ByteSize::b(max).display().si()
        ))),
        _ => Ok(()),
    }
}

async fn limits(
    State(state): State<AppState>,
    Path(channel): Path<String>,
    Extension(limits): Extension<Limits>,
) -> Result<Json<SpaceLimits>, ApiError> {
    let channel = parse_channel(&channel)?;
    let used = state.accounts.transferred(&channel, state.clock.now_ms());
    Ok(Json(limits.report(used)))
}

/// Open to all: devices joining a space have no key. A wrong key is still
/// refused, so apps can check one here.
async fn server_config(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<ServerConfig>, ApiError> {
    let c = &state.config;
    if let (Some(given), Some(expected)) = (bearer(&headers), &c.access_token)
        && !bool::from(given.as_bytes().ct_eq(expected.as_bytes()))
    {
        return Err(ApiError::Unauthorized);
    }
    Ok(Json(ServerConfig {
        default_ttl_secs: c.default_ttl.as_secs(),
        max_ttl_secs: c.max_ttl.as_secs(),
        max_size_bytes: c.max_size.as_u64(),
        max_clips: c.max_clips_per_channel,
        version: Some(env!("CARGO_PKG_VERSION").into()),
        chunked: Some(ChunkedConfig {
            max_chunk_bytes: u64::from(MAX_CHUNK_SIZE),
        }),
        accounts: Some(AccountsConfig { public: c.public }),
        legal: c.privacy_url.is_some() && c.imprint_url.is_some(),
    }))
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|v| !v.is_empty())
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
    Extension(limits): Extension<Limits>,
    Extension(client): Extension<Client>,
    body: Body,
) -> Result<(StatusCode, Json<ClipMeta>), ApiError> {
    let channel = parse_channel(&channel)?;
    let _buffer = buffer(&state, client)?;
    let body = read_body(&state, &limits, body).await?;
    check_envelope(&body)?;
    let ttl_ms = ttl_ms(&limits, query.ttl)?;
    check_clip_size(&limits, body.len() as u64)?;
    let charged = Charged::take(&state, &channel, &client, &limits, body.len() as u64)?;
    let now = state.clock.now_ms();
    let meta = state
        .store
        .put(&channel, &body, now, now + ttl_ms, limits.disk_share)
        .await?;
    charged.keep();
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

fn ttl_ms(limits: &Limits, ttl_secs: Option<u64>) -> Result<u64, ApiError> {
    Ok(match ttl_secs {
        None => limits.default_ttl_ms,
        Some(0) => return Err(ApiError::BadRequest("ttl must be greater than zero")),
        Some(secs) => secs.saturating_mul(1000).min(limits.max_ttl_ms),
    })
}

async fn create_invite(
    State(state): State<AppState>,
    Path((channel, slot)): Path<(String, String)>,
    Query(query): Query<CreateQuery>,
    Extension(client): Extension<Client>,
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
        .put(channel, client, slot, body, now, now + ttl_ms)
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
    Extension(client): Extension<Client>,
    body: Bytes,
) -> Result<(StatusCode, Json<RendezvousOpened>), ApiError> {
    let channel = parse_channel(&channel)?;
    check_message(&body)?;
    let now = state.clock.now_ms();
    let clients = &state.clients;
    // Else one space could open and close codes until none are left.
    if !clients.allow(&clients.codes, &client, now) {
        return Err(ApiError::TooManyCodes);
    }
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
    Extension(client): Extension<Client>,
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
        client,
        now,
    )?;
    Ok(StatusCode::NO_CONTENT)
}

/// Every answer counts against the address's lookups: a stranger could
/// otherwise spoil (and guess) every open code.
async fn joiner_writes(
    State(state): State<AppState>,
    Path((nameplate, index)): Path<(u16, u8)>,
    Extension(client): Extension<Client>,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    check_message(&body)?;
    let index = check_index(index)?;
    let now = state.clock.now_ms();
    let clients = &state.clients;
    if !clients.allow(&clients.lookups, &client, now) {
        return Err(ApiError::TooManyLookups);
    }
    state
        .rendezvous
        .put(nameplate, Side::B, index, body, None, client, now)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn inviter_reads(
    State(state): State<AppState>,
    Path((channel, nameplate, index)): Path<(String, u16, u8)>,
    Query(query): Query<WaitQuery>,
    Extension(client): Extension<Client>,
) -> Result<Response, ApiError> {
    let channel = parse_channel(&channel)?;
    let index = check_index(index)?;
    read_message(
        &state,
        nameplate,
        Side::B,
        index,
        Some(&channel),
        client,
        query,
    )
    .await
}

/// Codes that aren't there count against the address's lookups, so nobody
/// can look for the open ones.
async fn joiner_reads(
    State(state): State<AppState>,
    Path((nameplate, index)): Path<(u16, u8)>,
    Query(query): Query<WaitQuery>,
    Extension(client): Extension<Client>,
) -> Result<Response, ApiError> {
    let index = check_index(index)?;
    let clients = &state.clients;
    if !clients.ready(&clients.lookups, &client, state.clock.now_ms()) {
        return Err(ApiError::TooManyLookups);
    }
    let read = read_message(&state, nameplate, Side::A, index, None, client, query).await;
    if matches!(read, Err(ApiError::NotFound)) {
        clients.spend(&clients.lookups, &client, state.clock.now_ms());
    }
    read
}

async fn read_message(
    state: &AppState,
    nameplate: u16,
    side: Side,
    index: u8,
    channel: Option<&ChannelId>,
    client: Client,
    query: WaitQuery,
) -> Result<Response, ApiError> {
    let wait = std::time::Duration::from_secs(query.wait.unwrap_or(25));
    // Only a read that waits holds a connection open.
    let _connection = match wait.is_zero() {
        true => None,
        false => state
            .clients
            .connect(client)
            .map_err(|Crowded| ApiError::TooManyConnections)?,
    };
    let clock = state.clock.clone();
    let message = state
        .rendezvous
        .read(
            nameplate,
            side,
            index,
            channel,
            client,
            || clock.now_ms(),
            wait,
        )
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
    Extension(limits): Extension<Limits>,
    Extension(client): Extension<Client>,
    body: Body,
) -> Result<(StatusCode, Json<UploadCreated>), ApiError> {
    let channel = parse_channel(&channel)?;
    let _buffer = buffer(&state, client)?;
    let body = read_body(&state, &limits, body).await?;
    check_envelope(&body)?;
    let ttl_ms = ttl_ms(&limits, query.ttl)?;
    let layout = ChunkLayout::new(query.length, query.chunk_size).map_err(ApiError::BadRequest)?;
    let size = (body.len() as u64).saturating_add(query.length);
    check_clip_size(&limits, size)?;
    let charged = Charged::take(&state, &channel, &client, &limits, size)?;
    let now = state.clock.now_ms();
    // A free upload can't hold its share of the disk for longer than its clip could.
    let deadline =
        (limits.plan == Plan::Free).then(|| now + limits.max_ttl_ms.max(MIN_UPLOAD_DEADLINE_MS));
    let id = state
        .store
        .create_upload(
            &channel,
            &body,
            layout,
            ttl_ms,
            now,
            deadline,
            limits.disk_share,
        )
        .await?;
    charged.keep();
    Ok((
        StatusCode::CREATED,
        Json(UploadCreated { id: id.to_string() }),
    ))
}

async fn put_chunk(
    State(state): State<AppState>,
    Path((channel, id, index)): Path<(String, String, u64)>,
    Extension(limits): Extension<Limits>,
    body: Body,
) -> Result<StatusCode, ApiError> {
    let (channel, id) = (parse_channel(&channel)?, parse_id(&id)?);
    let body = body.into_data_stream().map_err(io::Error::other);
    // A chunk sent again (say, after a lost connection) counts once more.
    let again = |len| charge(&state, &channel, &limits, len).is_ok();
    state
        .store
        .put_chunk(&channel, id, index, body, state.clock.now_ms(), again)
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
    Extension(limits): Extension<Limits>,
) -> Result<Response, ApiError> {
    let (channel, id) = (parse_channel(&channel)?, parse_id(&id)?);
    let now = state.clock.now_ms();
    let (meta, file, len) = state
        .store
        .chunk(&channel, id, index, now)
        .await?
        .ok_or(ApiError::NotFound)?;
    charge(&state, &channel, &limits, len)?;
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
    Extension(limits): Extension<Limits>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let channel = parse_channel(&channel)?;
    let (meta, file, len) = state
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
    charge(&state, &channel, &limits, len)?;
    Ok(envelope_response(&meta, file, len, "private, no-cache"))
}

async fn get_clip(
    State(state): State<AppState>,
    Path((channel, id)): Path<(String, String)>,
    Extension(limits): Extension<Limits>,
) -> Result<Response, ApiError> {
    let (channel, id) = (parse_channel(&channel)?, parse_id(&id)?);
    let now = state.clock.now_ms();
    let (meta, file, len) = state
        .store
        .get(&channel, id, now)
        .await?
        .ok_or(ApiError::NotFound)?;
    charge(&state, &channel, &limits, len)?;
    // A clip never changes, so it may be cached until it expires.
    let max_age = meta.expires_at_ms.saturating_sub(now) / 1000;
    let cache = format!("private, max-age={max_age}, immutable");
    Ok(envelope_response(&meta, file, len, &cache))
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
    Extension(client): Extension<Client>,
) -> Result<Response, ApiError> {
    let channel = parse_channel(&channel)?;
    let connection = state
        .clients
        .connect(client)
        .map_err(|Crowded| ApiError::TooManyConnections)?;
    let stream = state
        .events
        .stream(&channel, connection)
        .ok_or(ApiError::TooManyListeners)?;
    // Tells nginx to pass each event on at once instead of buffering them.
    let no_buffering = (HeaderName::from_static("x-accel-buffering"), "no");
    Ok(([no_buffering], stream).into_response())
}

fn etag(meta: &ClipMeta) -> String {
    format!("\"{}\"", meta.id)
}

/// Streamed from the file, like chunks: a clip may be megabytes.
fn envelope_response(
    meta: &ClipMeta,
    file: tokio::fs::File,
    len: u64,
    cache_control: &str,
) -> Response {
    let header =
        |v: String| HeaderValue::try_from(v).expect("ids and numbers are valid header values");
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static(ENVELOPE_CONTENT_TYPE),
            ),
            (header::CONTENT_LENGTH, HeaderValue::from(len)),
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
        Body::from_stream(ReaderStream::with_capacity(file, IO_BUFFER)),
    )
        .into_response()
}
