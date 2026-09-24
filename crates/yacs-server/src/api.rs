use std::io;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, MatchedPath, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use subtle::ConstantTimeEq;
use tower_http::trace::{DefaultOnResponse, TraceLayer};
use tracing::Level;
use ulid::Ulid;
use yacs_core::api::{
    ChannelEvent, ClipMeta, ENVELOPE_CONTENT_TYPE, ErrorBody, HEADER_CLIP_ID, HEADER_CREATED_AT,
    HEADER_EXPIRES_AT, ServerConfig,
};
use yacs_core::{ChannelId, Envelope};

use crate::clock::Clock;
use crate::config::Config;
use crate::events::Events;
use crate::store::{PutError, Store};

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Store>,
    pub config: Arc<Config>,
    pub clock: Arc<dyn Clock>,
    pub events: Arc<Events>,
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
        .layer(DefaultBodyLimit::max(max_size))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_token));

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
    StorageFull,
    Internal(io::Error),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg),
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "missing or wrong access token"),
            Self::NotFound => (StatusCode::NOT_FOUND, "not found"),
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

fn parse_channel(s: &str) -> Result<ChannelId, ApiError> {
    s.parse()
        .map_err(|_| ApiError::BadRequest("invalid channel id"))
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
    // Only the structure is checked; the server can't (and shouldn't) decrypt.
    Envelope::from_bytes(&body).map_err(|_| ApiError::BadRequest("body is not a YACS envelope"))?;

    let config = &state.config;
    let ttl_ms = match query.ttl {
        None => config.default_ttl.as_millis() as u64,
        Some(0) => return Err(ApiError::BadRequest("ttl must be greater than zero")),
        Some(secs) => secs
            .saturating_mul(1000)
            .min(config.max_ttl.as_millis() as u64),
    };
    let now = state.clock.now_ms();
    let meta = state.store.put(&channel, &body, now, now + ttl_ms).await?;
    state
        .events
        .publish(&channel, ChannelEvent::Added { clip: meta.clone() });
    Ok((StatusCode::CREATED, Json(meta)))
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
        ],
        body,
    )
        .into_response()
}
