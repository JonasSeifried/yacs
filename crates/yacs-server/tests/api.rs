use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use clap::Parser;
use http_body_util::BodyExt;
use tempfile::TempDir;
use tower::ServiceExt;
use yacs_core::api::{
    ChannelEvent, ChunkedConfig, ClipMeta, HEADER_CHUNKED, HEADER_CLIP_ID, HEADER_SIZE,
    ServerConfig, UploadCreated, UploadStatus,
};
use yacs_core::{
    CHUNK_TAG_LEN, ChannelId, ChannelKey, Clip, ClipItem, Envelope, Invite, InviteSecret,
    MAX_CHUNK_SIZE, MAX_SEALED_INVITE, MIN_CHUNK_SIZE, Pairing, Payload,
};
use yacs_server::{AppState, Config, Events, Invites, ManualClock, Store, router};

const START_MS: u64 = 1_758_600_000_000;
const MINUTE: Duration = Duration::from_secs(60);

struct TestApp {
    router: Router,
    clock: Arc<ManualClock>,
    store: Arc<Store>,
    events: Arc<Events>,
    dir: TempDir,
}

struct Res {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Vec<u8>,
}

impl Res {
    fn json<T: serde::de::DeserializeOwned>(&self) -> T {
        serde_json::from_slice(&self.body).unwrap()
    }
}

async fn app(args: &[&str]) -> TestApp {
    let dir = TempDir::new().unwrap();
    let data_dir = dir.path().to_str().unwrap().to_owned();
    let argv = ["yacs-server", "--data-dir", &data_dir];
    let config = Config::try_parse_from(argv.iter().chain(args))
        .unwrap()
        .validate()
        .unwrap();
    let store = Arc::new(
        Store::open(
            &config.data_dir,
            config.max_clips_per_channel,
            config.max_disk.as_u64(),
        )
        .await
        .unwrap(),
    );
    let clock = Arc::new(ManualClock::new(START_MS));
    let events = Arc::new(Events::default());
    let router = router(AppState {
        store: store.clone(),
        config: Arc::new(config),
        clock: clock.clone(),
        events: events.clone(),
        invites: Arc::new(Invites::default()),
    });
    TestApp {
        router,
        clock,
        store,
        events,
        dir,
    }
}

impl TestApp {
    async fn call(
        &self,
        method: Method,
        uri: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
    ) -> Res {
        let mut req = Request::builder().method(method).uri(uri);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let res = self
            .router
            .clone()
            .oneshot(req.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let body = res.into_body().collect().await.unwrap().to_bytes().to_vec();
        Res {
            status,
            headers,
            body,
        }
    }

    async fn get(&self, uri: &str) -> Res {
        self.call(Method::GET, uri, &[], vec![]).await
    }

    async fn post(&self, uri: &str, body: Vec<u8>) -> Res {
        self.call(Method::POST, uri, &[], body).await
    }

    async fn delete(&self, uri: &str) -> Res {
        self.call(Method::DELETE, uri, &[], vec![]).await
    }

    async fn put(&self, uri: &str, body: Vec<u8>) -> Res {
        self.call(Method::PUT, uri, &[], body).await
    }

    /// Starts an upload of `length` sealed bytes in chunks of [`CHUNK`].
    async fn start_upload(&self, channel: &str, length: u64) -> String {
        let res = self
            .post(&upload_uri(channel, length, CHUNK), envelope("header"))
            .await;
        assert_eq!(
            res.status,
            StatusCode::CREATED,
            "{}",
            String::from_utf8_lossy(&res.body)
        );
        res.json::<UploadCreated>().id
    }

    /// Uploads every chunk of `data` and completes it.
    async fn upload(&self, channel: &str, data: &[u8]) -> ClipMeta {
        let id = self.start_upload(channel, data.len() as u64).await;
        for (i, chunk) in data.chunks(CHUNK as usize).enumerate() {
            let res = self.put(&chunk_uri(channel, &id, i), chunk.to_vec()).await;
            assert_eq!(res.status, StatusCode::NO_CONTENT);
        }
        let res = self
            .post(&format!("{}/complete", uploads(channel, &id)), vec![])
            .await;
        assert_eq!(res.status, StatusCode::CREATED);
        res.json()
    }

    async fn create(&self, channel: &str, ttl: Option<&str>) -> ClipMeta {
        let uri = match ttl {
            Some(ttl) => format!("{}?ttl={ttl}", clips(channel)),
            None => clips(channel),
        };
        let res = self.post(&uri, envelope("hi")).await;
        assert_eq!(
            res.status,
            StatusCode::CREATED,
            "{}",
            String::from_utf8_lossy(&res.body)
        );
        res.json()
    }

    async fn list(&self, channel: &str) -> Vec<ClipMeta> {
        let res = self.get(&clips(channel)).await;
        assert_eq!(res.status, StatusCode::OK);
        res.json()
    }

    /// Opens a channel's event stream; the relay is subscribed once this returns.
    async fn listen(&self, channel: &str, headers: &[(&str, &str)]) -> Listener {
        let mut req = Request::get(format!("/api/v1/channels/{channel}/events"));
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let res = self
            .router
            .clone()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        Listener {
            status: res.status(),
            headers: res.headers().clone(),
            body: res.into_body(),
            buf: String::new(),
        }
    }

    fn files(&self) -> usize {
        walk(self.dir.path())
    }
}

struct Listener {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Body,
    buf: String,
}

impl Listener {
    /// The next event, skipping keep-alive comments. `None` once the stream ended.
    async fn next(&mut self) -> Option<ChannelEvent> {
        loop {
            if let Some(end) = self.buf.find("\n\n") {
                let message: String = self.buf.drain(..end + 2).collect();
                let data: String = message
                    .lines()
                    .filter_map(|l| l.strip_prefix("data: "))
                    .collect();
                if data.is_empty() {
                    continue;
                }
                return Some(serde_json::from_str(&data).unwrap());
            }
            let frame = tokio::time::timeout(Duration::from_secs(5), self.body.frame())
                .await
                .expect("no event within 5 s")?
                .unwrap();
            if let Ok(data) = frame.into_data() {
                self.buf.push_str(std::str::from_utf8(&data).unwrap());
            }
        }
    }
}

fn walk(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .map(|p| if p.is_dir() { walk(&p) } else { 1 })
        .sum()
}

fn channel(n: u8) -> String {
    ChannelId::from_bytes([n; 32]).to_string()
}

fn clips(channel: &str) -> String {
    format!("/api/v1/channels/{channel}/clips")
}

/// Sealed bytes per chunk, the smallest the relay takes.
const CHUNK: u64 = MIN_CHUNK_SIZE as u64 + CHUNK_TAG_LEN as u64;

fn upload_uri(channel: &str, length: u64, chunk_size: u64) -> String {
    format!("/api/v1/channels/{channel}/uploads?length={length}&chunk_size={chunk_size}")
}

fn uploads(channel: &str, id: &str) -> String {
    format!("/api/v1/channels/{channel}/uploads/{id}")
}

fn chunk_uri(channel: &str, id: &str, index: usize) -> String {
    format!("{}/chunks/{index}", uploads(channel, id))
}

/// The relay never decrypts chunks, so any bytes do.
fn sealed(len: u64) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

fn envelope(text: &str) -> Vec<u8> {
    let pairing = Pairing {
        channel_id: ChannelId::from_bytes([1; 32]),
        key: ChannelKey::from_bytes([2; 32]),
    };
    let payload = Payload::Clip(Clip {
        created_at_ms: 0,
        device_name: "test".into(),
        items: vec![ClipItem::Text(text.into())],
    });
    Envelope::seal(&pairing, &payload).unwrap().to_bytes()
}

#[tokio::test]
async fn reports_config() {
    let app = app(&["--max-ttl", "7d"]).await;
    let config: ServerConfig = app.get("/api/v1/config").await.json();
    assert_eq!(
        config,
        ServerConfig {
            default_ttl_secs: 15 * 60,
            max_ttl_secs: 7 * 24 * 3600,
            max_size_bytes: 20_000_000,
            max_clips: 50,
            version: Some(env!("CARGO_PKG_VERSION").into()),
            chunked: Some(ChunkedConfig {
                max_chunk_bytes: u64::from(MAX_CHUNK_SIZE),
            }),
        }
    );
}

#[tokio::test]
async fn ttl_defaults_and_clamps() {
    let app = app(&[]).await;
    let ch = channel(1);

    let default = app.create(&ch, None).await;
    assert_eq!(default.created_at_ms, START_MS);
    assert_eq!(default.expires_at_ms, START_MS + 15 * 60_000);

    let hour = app.create(&ch, Some("3600")).await;
    assert_eq!(hour.expires_at_ms, START_MS + 3_600_000);

    let clamped = app.create(&ch, Some("99999999")).await;
    assert_eq!(clamped.expires_at_ms, START_MS + 24 * 3_600_000);

    let zero = app
        .post(&format!("{}?ttl=0", clips(&ch)), envelope("x"))
        .await;
    assert_eq!(zero.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn lists_newest_first_and_hides_expired() {
    let app = app(&[]).await;
    let ch = channel(1);
    let long = app.create(&ch, Some("3600")).await;
    app.clock.advance(Duration::from_millis(1));
    let short = app.create(&ch, Some("60")).await;
    app.clock.advance(Duration::from_millis(1));
    let newest = app.create(&ch, Some("600")).await;

    assert_eq!(
        app.list(&ch).await,
        vec![newest.clone(), short, long.clone()]
    );

    app.clock.advance(2 * MINUTE);
    assert_eq!(app.list(&ch).await, vec![newest, long]);
}

#[tokio::test]
async fn latest_supports_etag() {
    let app = app(&[]).await;
    let ch = channel(1);
    let latest_uri = format!("{}/latest", clips(&ch));
    assert_eq!(app.get(&latest_uri).await.status, StatusCode::NOT_FOUND);

    app.create(&ch, None).await;
    app.clock.advance(Duration::from_millis(1));
    let body = envelope("second");
    let second: ClipMeta = app.post(&clips(&ch), body.clone()).await.json();

    let res = app.get(&latest_uri).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.body, body);
    assert_eq!(res.headers[HEADER_CLIP_ID], second.id.as_str());
    let etag = res.headers[header::ETAG].to_str().unwrap().to_owned();

    let cached = app
        .call(
            Method::GET,
            &latest_uri,
            &[("if-none-match", &etag)],
            vec![],
        )
        .await;
    assert_eq!(cached.status, StatusCode::NOT_MODIFIED);
    assert!(cached.body.is_empty());

    app.clock.advance(Duration::from_millis(1));
    app.create(&ch, None).await;
    let changed = app
        .call(
            Method::GET,
            &latest_uri,
            &[("if-none-match", &etag)],
            vec![],
        )
        .await;
    assert_eq!(changed.status, StatusCode::OK);
}

#[tokio::test]
async fn get_and_delete_by_id() {
    let app = app(&[]).await;
    let ch = channel(1);
    let body = envelope("hello");
    let meta: ClipMeta = app.post(&clips(&ch), body.clone()).await.json();
    let uri = format!("{}/{}", clips(&ch), meta.id);

    let res = app.get(&uri).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.body, body);
    assert_eq!(
        res.headers[header::CACHE_CONTROL],
        "private, max-age=900, immutable"
    );

    assert_eq!(app.delete(&uri).await.status, StatusCode::NO_CONTENT);
    assert_eq!(app.get(&uri).await.status, StatusCode::NOT_FOUND);
    assert_eq!(app.delete(&uri).await.status, StatusCode::NOT_FOUND);
    assert_eq!(app.store.used_bytes(), 0);
}

#[tokio::test]
async fn expired_clip_is_not_served() {
    let app = app(&[]).await;
    let ch = channel(1);
    let meta = app.create(&ch, Some("60")).await;
    app.clock.advance(MINUTE);
    let uri = format!("{}/{}", clips(&ch), meta.id);
    assert_eq!(app.get(&uri).await.status, StatusCode::NOT_FOUND);
    assert_eq!(
        app.get(&format!("{}/latest", clips(&ch))).await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn clear_removes_history() {
    let app = app(&[]).await;
    let ch = channel(1);
    app.create(&ch, None).await;
    app.create(&ch, None).await;
    assert_eq!(app.delete(&clips(&ch)).await.status, StatusCode::NO_CONTENT);
    assert!(app.list(&ch).await.is_empty());
    assert_eq!(app.files(), 0);
}

#[tokio::test]
async fn evicts_oldest_beyond_history_limit() {
    let app = app(&["--max-clips-per-channel", "3"]).await;
    let ch = channel(1);
    let mut ids = Vec::new();
    for _ in 0..5 {
        ids.push(app.create(&ch, None).await.id);
        app.clock.advance(Duration::from_millis(1));
    }
    let listed: Vec<_> = app.list(&ch).await.into_iter().map(|m| m.id).collect();
    assert_eq!(listed, vec![ids[4].clone(), ids[3].clone(), ids[2].clone()]);
    assert_eq!(app.files(), 3);
}

#[tokio::test]
async fn expired_clips_dont_hold_history_slots() {
    let app = app(&["--max-clips-per-channel", "2"]).await;
    let ch = channel(1);
    let long = app.create(&ch, Some("3600")).await;
    app.clock.advance(Duration::from_millis(1));
    app.create(&ch, Some("60")).await;
    app.clock.advance(2 * MINUTE);
    let newest = app.create(&ch, None).await;
    assert_eq!(app.list(&ch).await, vec![newest, long]);
}

#[tokio::test]
async fn reaper_deletes_expired_files() {
    let app = app(&[]).await;
    app.create(&channel(1), Some("60")).await;
    app.create(&channel(2), Some("3600")).await;
    assert_eq!(app.files(), 2);

    app.clock.advance(2 * MINUTE);
    assert_eq!(app.store.reap(START_MS + 2 * 60_000).await.unwrap(), 1);
    assert_eq!(app.files(), 1);
    // The emptied channel dir is gone too; only channel 2 and the tmp dir remain.
    assert_eq!(std::fs::read_dir(app.dir.path()).unwrap().count(), 2);
}

#[tokio::test]
async fn channels_are_isolated() {
    let app = app(&[]).await;
    app.create(&channel(1), None).await;
    assert!(app.list(&channel(2)).await.is_empty());
}

#[tokio::test]
async fn rejects_bad_input() {
    let app = app(&["--max-size", "1KB"]).await;
    let ch = channel(1);

    assert_eq!(
        app.get(&clips("not-a-channel")).await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        app.get(&format!("{}/nope", clips(&ch))).await.status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        app.post(&clips(&ch), b"plaintext".to_vec()).await.status,
        StatusCode::BAD_REQUEST
    );

    let too_big = app.post(&clips(&ch), envelope(&"x".repeat(2000))).await;
    assert_eq!(too_big.status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn refuses_uploads_when_disk_quota_is_full() {
    // Each "hi" envelope is 54 bytes, so exactly two fit.
    let app = app(&["--max-size", "100B", "--max-disk", "120B"]).await;
    let ch = channel(1);
    app.create(&ch, None).await;
    app.create(&ch, None).await;
    let full = app.post(&clips(&ch), envelope("hi")).await;
    assert_eq!(full.status, StatusCode::INSUFFICIENT_STORAGE);

    app.delete(&clips(&ch)).await;
    app.create(&ch, None).await;
}

#[tokio::test]
async fn access_token_guards_the_api_only() {
    let app = app(&["--access-token", "s3cret"]).await;
    let uri = "/api/v1/config";
    assert_eq!(app.get(uri).await.status, StatusCode::UNAUTHORIZED);
    let wrong = app
        .call(
            Method::GET,
            uri,
            &[("authorization", "Bearer nope")],
            vec![],
        )
        .await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    let right = app
        .call(
            Method::GET,
            uri,
            &[("authorization", "Bearer s3cret")],
            vec![],
        )
        .await;
    assert_eq!(right.status, StatusCode::OK);
    assert_eq!(app.get("/healthz").await.status, StatusCode::OK);
}

#[tokio::test]
async fn survives_restart() {
    let app = app(&[]).await;
    let ch = channel(1);
    let meta = app.create(&ch, None).await;
    // A crashed upload left behind in the tmp dir.
    std::fs::write(app.dir.path().join(".tmp/crashed.tmp"), b"junk").unwrap();

    let reopened = Store::open(app.dir.path(), 50, 1 << 30).await.unwrap();
    assert_eq!(reopened.used_bytes(), meta.size);
    let id: ChannelId = ch.parse().unwrap();
    assert_eq!(reopened.list(&id, START_MS).await.unwrap(), vec![meta]);
    assert_eq!(
        std::fs::read_dir(app.dir.path().join(".tmp"))
            .unwrap()
            .count(),
        0
    );
}

#[tokio::test]
async fn serves_the_web_app_without_a_token() {
    let app = app(&["--access-token", "s3cret"]).await;
    let res = app.get("/").await;
    let built = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../ui/dist/web/index.html")
        .exists();
    // CI's Rust job doesn't build the UI; then the page explains what's missing.
    if !built {
        assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(String::from_utf8_lossy(&res.body).contains("build:web"));
    } else {
        assert_eq!(res.status, StatusCode::OK);
        assert_eq!(res.headers[header::CONTENT_TYPE], "text/html");
        assert_eq!(res.headers[header::CACHE_CONTROL], "no-cache");
        let csp = res.headers[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap();
        assert!(
            csp.contains("script-src 'self' 'wasm-unsafe-eval'"),
            "{csp}"
        );
        assert!(csp.contains("frame-ancestors 'none'"), "{csp}");
        assert_eq!(res.headers[header::REFERRER_POLICY], "no-referrer");
    }

    assert_eq!(
        app.get("/assets/missing.js").await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        app.get("/../Cargo.toml").await.status,
        StatusCode::NOT_FOUND
    );
    let share = app.post("/share", vec![]).await;
    assert_eq!(share.status, StatusCode::SEE_OTHER);
    assert_eq!(share.headers[header::LOCATION], "/");

    // The API still wants the token.
    assert_eq!(
        app.get("/api/v1/config").await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn streams_changes_to_listeners_of_the_channel() {
    let app = app(&[]).await;
    let (ch, other) = (channel(1), channel(2));
    let mut listener = app.listen(&ch, &[]).await;
    assert_eq!(listener.status, StatusCode::OK);
    assert_eq!(listener.headers[header::CONTENT_TYPE], "text/event-stream");
    assert_eq!(listener.headers["x-accel-buffering"], "no");

    app.create(&other, None).await;
    let clip = app.create(&ch, None).await;
    assert_eq!(
        listener.next().await,
        Some(ChannelEvent::Added { clip: clip.clone() })
    );

    app.delete(&format!("{}/{}", clips(&ch), clip.id)).await;
    assert_eq!(
        listener.next().await,
        Some(ChannelEvent::Deleted { id: clip.id })
    );

    app.delete(&clips(&ch)).await;
    assert_eq!(listener.next().await, Some(ChannelEvent::Cleared));
}

#[tokio::test]
async fn event_streams_need_the_token_and_end_on_shutdown() {
    let app = app(&["--access-token", "s3cret"]).await;
    let ch = channel(1);
    assert_eq!(app.listen(&ch, &[]).await.status, StatusCode::UNAUTHORIZED);

    let mut listener = app.listen(&ch, &[("authorization", "Bearer s3cret")]).await;
    assert_eq!(listener.status, StatusCode::OK);
    app.events.close();
    assert_eq!(listener.next().await, None);
}

#[tokio::test]
async fn chunked_upload_round_trip() {
    let app = app(&[]).await;
    let ch = channel(1);
    let data = sealed(2 * CHUNK + 100);
    let parts: Vec<&[u8]> = data.chunks(CHUNK as usize).collect();
    let mut listener = app.listen(&ch, &[]).await;

    let id = app.start_upload(&ch, data.len() as u64).await;
    // Any order, and repeating a chunk is fine.
    for i in [2, 0, 0] {
        let res = app.put(&chunk_uri(&ch, &id, i), parts[i].to_vec()).await;
        assert_eq!(res.status, StatusCode::NO_CONTENT);
    }
    let status: UploadStatus = app.get(&uploads(&ch, &id)).await.json();
    assert_eq!(status.received, [0, 2]);
    let complete = format!("{}/complete", uploads(&ch, &id));
    assert_eq!(
        app.post(&complete, vec![]).await.status,
        StatusCode::CONFLICT
    );
    // Not listed until it's complete.
    assert!(app.list(&ch).await.is_empty());

    app.put(&chunk_uri(&ch, &id, 1), parts[1].to_vec()).await;
    app.clock.advance(MINUTE);
    let res = app.post(&complete, vec![]).await;
    assert_eq!(res.status, StatusCode::CREATED);
    let meta: ClipMeta = res.json();
    let header = envelope("header");
    assert!(meta.chunked);
    assert_eq!(meta.size, header.len() as u64 + data.len() as u64);
    assert_eq!(meta.created_at_ms, START_MS + 60_000);
    assert_eq!(meta.expires_at_ms, START_MS + 60_000 + 15 * 60_000);
    assert_eq!(app.list(&ch).await, std::slice::from_ref(&meta));
    assert_eq!(
        listener.next().await,
        Some(ChannelEvent::Added { clip: meta.clone() })
    );
    assert_eq!(app.store.used_bytes(), meta.size);
    // The upload is gone.
    assert_eq!(
        app.get(&uploads(&ch, &id)).await.status,
        StatusCode::NOT_FOUND
    );

    let clip = format!("{}/{}", clips(&ch), meta.id);
    let res = app.get(&clip).await;
    assert_eq!(res.body.len(), header.len());
    assert_eq!(res.headers[HEADER_SIZE], meta.size.to_string().as_str());
    assert_eq!(res.headers[HEADER_CHUNKED], "1");
    for (i, part) in parts.iter().enumerate() {
        let res = app.get(&format!("{clip}/chunks/{i}")).await;
        assert_eq!(res.status, StatusCode::OK);
        assert_eq!(res.body, *part);
        assert_eq!(
            res.headers[header::CACHE_CONTROL],
            "private, max-age=900, immutable"
        );
    }
    assert_eq!(
        app.get(&format!("{clip}/chunks/3")).await.status,
        StatusCode::NOT_FOUND
    );
    // A clip without chunks has none to give.
    let plain = app.create(&ch, None).await;
    let res = app.get(&format!("{}/{}", clips(&ch), plain.id)).await;
    assert_eq!(res.headers[HEADER_CHUNKED], "0");
    let res = app
        .get(&format!("{}/{}/chunks/0", clips(&ch), plain.id))
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);

    assert_eq!(app.delete(&clip).await.status, StatusCode::NO_CONTENT);
    assert_eq!(app.store.used_bytes(), plain.size);
    assert_eq!(app.files(), 1);
}

#[tokio::test]
async fn rejects_bad_uploads_and_chunks() {
    let app = app(&[]).await;
    let ch = channel(1);
    for (length, chunk_size) in [
        (CHUNK, CHUNK - 1),
        (CHUNK, u64::from(MAX_CHUNK_SIZE) + 17),
        (15, CHUNK),
        // The last chunk would be just a tag.
        (CHUNK + 16, CHUNK),
    ] {
        let res = app
            .post(&upload_uri(&ch, length, chunk_size), envelope("h"))
            .await;
        assert_eq!(res.status, StatusCode::BAD_REQUEST, "{length} {chunk_size}");
    }
    let res = app
        .post(&upload_uri(&ch, CHUNK, CHUNK), b"junk".to_vec())
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);

    let id = app.start_upload(&ch, CHUNK + 20).await;
    let put = |i: usize, len: u64| {
        let (app, uri) = (&app, chunk_uri(&ch, &id, i));
        async move { app.put(&uri, sealed(len)).await }
    };
    assert_eq!(put(2, 20).await.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        put(0, CHUNK + 1).await.status,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(put(0, CHUNK - 1).await.status, StatusCode::BAD_REQUEST);
    assert_eq!(put(1, 21).await.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(put(1, 20).await.status, StatusCode::NO_CONTENT);
    // Nothing half-written is left behind.
    assert_eq!(app.files(), 2);

    let other = channel(2);
    let res = app.put(&chunk_uri(&other, &id, 0), sealed(CHUNK)).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(
        app.get(&uploads(&other, &id)).await.status,
        StatusCode::NOT_FOUND
    );
    let res = app
        .post(&format!("{}/complete", uploads(&other, &id)), vec![])
        .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(
        app.delete(&uploads(&other, &id)).await.status,
        StatusCode::NOT_FOUND
    );
    let unknown = ulid_like();
    let res = app.put(&chunk_uri(&ch, &unknown, 0), sealed(CHUNK)).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);

    assert_eq!(
        app.delete(&uploads(&ch, &id)).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(put(0, CHUNK).await.status, StatusCode::NOT_FOUND);
    assert_eq!(app.store.used_bytes(), 0);
    assert_eq!(app.files(), 0);
}

fn ulid_like() -> String {
    "01K5ZZZZZZZZZZZZZZZZZZZZZZ".into()
}

#[tokio::test]
async fn uploads_reserve_the_quota_up_front() {
    // One upload of four chunks fits, and nothing else.
    let length = 4 * CHUNK;
    let disk = format!("{}B", length + 100);
    let app = app(&["--max-size", "100B", "--max-disk", &disk]).await;
    let ch = channel(1);
    let id = app.start_upload(&ch, length).await;
    let header = envelope("header").len() as u64;
    assert_eq!(app.store.used_bytes(), length + header);

    let res = app
        .post(&upload_uri(&ch, length, CHUNK), envelope("h"))
        .await;
    assert_eq!(res.status, StatusCode::INSUFFICIENT_STORAGE);
    let res = app.post(&clips(&ch), envelope("hi")).await;
    assert_eq!(res.status, StatusCode::INSUFFICIENT_STORAGE);

    app.delete(&uploads(&ch, &id)).await;
    assert_eq!(app.store.used_bytes(), 0);
    app.create(&ch, None).await;
}

#[tokio::test]
async fn limits_open_uploads_per_channel() {
    let app = app(&[]).await;
    let ch = channel(1);
    let mut ids = Vec::new();
    for _ in 0..yacs_server::store::MAX_OPEN_UPLOADS {
        ids.push(app.start_upload(&ch, CHUNK).await);
    }
    let res = app
        .post(&upload_uri(&ch, CHUNK, CHUNK), envelope("h"))
        .await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS);
    // Other channels have their own.
    app.start_upload(&channel(2), CHUNK).await;
    // A finished upload makes room.
    app.put(&chunk_uri(&ch, &ids[0], 0), sealed(CHUNK)).await;
    app.post(&format!("{}/complete", uploads(&ch, &ids[0])), vec![])
        .await;
    app.start_upload(&ch, CHUNK).await;
}

#[tokio::test]
async fn reaper_drops_idle_uploads_and_expired_chunked_clips() {
    let app = app(&[]).await;
    let ch = channel(1);
    let clip = app.upload(&ch, &sealed(CHUNK + 50)).await;
    let idle = app.start_upload(&ch, 2 * CHUNK).await;
    app.put(&chunk_uri(&ch, &idle, 0), sealed(CHUNK)).await;
    let busy = app.start_upload(&ch, 2 * CHUNK).await;
    let files = app.files();

    let hour = 60 * MINUTE;
    app.clock.advance(23 * hour);
    app.put(&chunk_uri(&ch, &busy, 0), sealed(CHUNK)).await;
    app.clock.advance(hour);
    let now = START_MS + 24 * 3_600_000;
    // The clip expired after 15 minutes; `idle` got nothing for a day.
    assert_eq!(app.store.reap(now).await.unwrap(), 1);
    assert_eq!(
        app.get(&uploads(&ch, &idle)).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(app.get(&uploads(&ch, &busy)).await.status, StatusCode::OK);
    // Left: `busy`'s header and first chunk.
    assert_eq!(app.files(), files - (clip_files(&clip) + 2) + 1);
    let header = envelope("header").len() as u64;
    assert_eq!(app.store.used_bytes(), header + 2 * CHUNK);
}

/// Header plus chunks.
fn clip_files(meta: &ClipMeta) -> usize {
    1 + (meta.size - envelope("header").len() as u64).div_ceil(CHUNK) as usize
}

#[tokio::test]
async fn eviction_removes_chunked_clips_whole() {
    let app = app(&["--max-clips-per-channel", "1"]).await;
    let ch = channel(1);
    app.upload(&ch, &sealed(3 * CHUNK)).await;
    assert_eq!(app.files(), 4);
    app.clock.advance(Duration::from_millis(1));
    let newest = app.create(&ch, None).await;
    assert_eq!(app.list(&ch).await, std::slice::from_ref(&newest));
    assert_eq!(app.files(), 1);
    assert_eq!(app.store.used_bytes(), newest.size);
}

#[tokio::test]
async fn restart_keeps_chunked_clips_and_drops_uploads() {
    let app = app(&[]).await;
    let ch = channel(1);
    let meta = app.upload(&ch, &sealed(CHUNK + 1000)).await;
    app.start_upload(&ch, CHUNK).await;

    let reopened = Store::open(app.dir.path(), 50, 1 << 30).await.unwrap();
    assert_eq!(reopened.used_bytes(), meta.size);
    let id: ChannelId = ch.parse().unwrap();
    assert_eq!(reopened.list(&id, START_MS).await.unwrap(), vec![meta]);
    assert_eq!(app.files(), 3);
}

fn invite_uri(channel: &str, secret: &InviteSecret) -> String {
    format!("/api/v1/channels/{channel}/invites/{}", secret.slot())
}

fn taken_uri(secret: &InviteSecret) -> String {
    format!("/api/v1/invites/{}", secret.slot())
}

fn sealed_invite(secret: &InviteSecret) -> Vec<u8> {
    let pairing = Pairing::from_root(&[1; 32]);
    secret
        .seal(&Invite::new("Home", "MacBook", None, &pairing))
        .unwrap()
}

#[tokio::test]
async fn an_invite_is_taken_once_and_its_space_hears_of_it() {
    let app = app(&[]).await;
    let ch = channel(1);
    let secret = InviteSecret::generate().unwrap();
    let mut listener = app.listen(&ch, &[]).await;
    let sealed = sealed_invite(&secret);
    let put = app.put(&invite_uri(&ch, &secret), sealed.clone()).await;
    assert_eq!(put.status, StatusCode::CREATED);
    let again = app.put(&invite_uri(&ch, &secret), sealed.clone()).await;
    assert_eq!(again.status, StatusCode::CONFLICT);

    let taken = app.get(&taken_uri(&secret)).await;
    assert_eq!(taken.status, StatusCode::OK);
    assert_eq!(taken.headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(taken.body, sealed);
    assert_eq!(
        secret.open(&taken.body).unwrap().pairing(),
        Pairing::from_root(&[1; 32])
    );
    assert_eq!(
        listener.next().await,
        Some(ChannelEvent::InviteUsed {
            slot: secret.slot().to_string()
        })
    );
    assert_eq!(
        app.get(&taken_uri(&secret)).await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn invites_expire_and_can_be_revoked() {
    let app = app(&[]).await;
    let ch = channel(1);
    let (a, b) = (
        InviteSecret::generate().unwrap(),
        InviteSecret::generate().unwrap(),
    );
    let uri = format!("{}?ttl=172800", invite_uri(&ch, &a));
    assert_eq!(
        app.put(&uri, sealed_invite(&a)).await.status,
        StatusCode::CREATED
    );
    // Clamped to a day.
    app.clock.advance(Duration::from_secs(24 * 60 * 60));
    assert_eq!(app.get(&taken_uri(&a)).await.status, StatusCode::NOT_FOUND);

    app.put(&invite_uri(&ch, &b), sealed_invite(&b)).await;
    let other_space = invite_uri(&channel(2), &b);
    assert_eq!(app.delete(&other_space).await.status, StatusCode::NOT_FOUND);
    assert_eq!(
        app.delete(&invite_uri(&ch, &b)).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(app.get(&taken_uri(&b)).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn rejects_bad_invites() {
    let app = app(&[]).await;
    let ch = channel(1);
    let secret = InviteSecret::generate().unwrap();
    let uri = invite_uri(&ch, &secret);
    for (uri, body) in [
        (uri.clone(), vec![]),
        (uri.clone(), vec![1; MAX_SEALED_INVITE + 1]),
        (format!("{uri}?ttl=0"), vec![1]),
        (format!("/api/v1/channels/{ch}/invites/nope"), vec![1]),
    ] {
        assert_eq!(
            app.put(&uri, body).await.status,
            StatusCode::BAD_REQUEST,
            "{uri}"
        );
    }
    assert_eq!(
        app.get("/api/v1/invites/nope").await.status,
        StatusCode::BAD_REQUEST
    );
    for _ in 0..20 {
        let secret = InviteSecret::generate().unwrap();
        app.put(&invite_uri(&ch, &secret), vec![1]).await;
    }
    let res = app.put(&uri, vec![1]).await;
    assert_eq!(res.status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn inviting_needs_the_token_taking_an_invite_doesnt() {
    let app = app(&["--access-token", "s3cret"]).await;
    let ch = channel(1);
    let secret = InviteSecret::generate().unwrap();
    let uri = invite_uri(&ch, &secret);
    assert_eq!(
        app.put(&uri, sealed_invite(&secret)).await.status,
        StatusCode::UNAUTHORIZED
    );
    let auth = [("authorization", "Bearer s3cret")];
    let put = app
        .call(Method::PUT, &uri, &auth, sealed_invite(&secret))
        .await;
    assert_eq!(put.status, StatusCode::CREATED);
    assert_eq!(app.delete(&uri).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(app.get(&taken_uri(&secret)).await.status, StatusCode::OK);
}
