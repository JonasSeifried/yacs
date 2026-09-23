//! The PWA (`ui/`, built with `pnpm --filter @yacs/ui build:web`), embedded
//! into release binaries so a deployment is a single file. Debug builds read
//! it from `ui/dist/web` on each request instead.
//!
//! It's served without the access token: the page holds no data, and it's
//! what asks the user for the token.

use axum::Router;
use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::post;

#[derive(rust_embed::Embed)]
// Relative to this crate.
#[folder = "../../ui/dist/web"]
#[allow_missing = true]
struct Assets;

/// Scripts only from this origin (plus WASM compilation), no plugins, never
/// framed. Styles allow inline `style` attributes, which React sets.
const CSP: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; \
    style-src 'self' 'unsafe-inline'; img-src 'self' blob: data:; connect-src 'self'; \
    object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

pub fn routes<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new()
        // Android's share sheet POSTs here; the service worker takes it. This
        // only runs when the service worker isn't installed (yet).
        .route("/share", post(|| async { Redirect::to("/") }))
        .fallback(serve)
}

async fn serve(uri: Uri) -> Response {
    let path = match uri.path().trim_start_matches('/') {
        "" => "index.html",
        path => path,
    };
    let Some(file) = Assets::get(path) else {
        if path == "index.html" {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "The YACS web app isn't built into this server. Run `pnpm --filter @yacs/ui build:web`, then build yacs-server again.",
            )
                .into_response();
        }
        return StatusCode::NOT_FOUND.into_response();
    };

    let cache = if path.starts_with("assets/") {
        // Vite puts a content hash in these file names.
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let mut res = file.data.into_response();
    let headers = res.headers_mut();
    let mime = HeaderValue::from_str(file.metadata.mimetype())
        .unwrap_or(HeaderValue::from_static("application/octet-stream"));
    headers.insert(header::CONTENT_TYPE, mime);
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    res
}
