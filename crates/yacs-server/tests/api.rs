use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use clap::Parser;
use http_body_util::BodyExt;
use tempfile::TempDir;
use tower::ServiceExt;
use yacs_core::api::RendezvousOpened;
use yacs_core::api::{
    AccountsConfig, ChannelEvent, ChunkedConfig, ClipMeta, HEADER_CHUNKED, HEADER_CLIP_ID,
    HEADER_SIZE, Plan, ServerConfig, SpaceLimits, UploadCreated, UploadStatus,
};
use yacs_core::{
    CHUNK_TAG_LEN, ChannelId, ChannelKey, Clip, ClipItem, Code, CodeInviter, CodeJoiner, Envelope,
    Invite, InviteSecret, MAX_CHUNK_SIZE, MAX_SEALED_INVITE, MIN_CHUNK_SIZE, Pairing, Payload,
};
use yacs_server::{
    Accounts, AppState, Config, Events, Invites, ManualClock, RateLimiter, Rendezvous, Store,
    router,
};

const START_MS: u64 = 1_758_600_000_000;
const MINUTE: Duration = Duration::from_secs(60);

struct TestApp {
    router: Router,
    clock: Arc<ManualClock>,
    store: Arc<Store>,
    events: Arc<Events>,
    accounts: Arc<Accounts>,
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
    app_in(TempDir::new().unwrap(), args).await
}

/// A relay on `dir`, as if restarted there.
async fn app_in(dir: TempDir, args: &[&str]) -> TestApp {
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
    let accounts = Arc::new(Accounts::open(&config.data_dir).await.unwrap());
    let limiter = Arc::new(RateLimiter::new(config.requests_per_minute));
    let router = router(AppState {
        store: store.clone(),
        config: Arc::new(config),
        clock: clock.clone(),
        events: events.clone(),
        invites: Arc::new(Invites::default()),
        rendezvous: Arc::new(Rendezvous::default()),
        accounts: accounts.clone(),
        limiter,
    });
    TestApp {
        router,
        clock,
        store,
        events,
        accounts,
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

    /// With `key` as the bearer token.
    async fn get_as(&self, uri: &str, key: &str) -> Res {
        let auth = format!("Bearer {key}");
        self.call(Method::GET, uri, &[("authorization", &auth)], vec![])
            .await
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

    fn clock_now(&self) -> u64 {
        yacs_server::Clock::now_ms(&*self.clock)
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
            accounts: Some(AccountsConfig { public: false }),
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
    // Joining devices have no key, but a wrong one is refused.
    assert_eq!(app.get(uri).await.status, StatusCode::OK);
    assert_eq!(
        app.get_as(uri, "nope").await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(app.get_as(uri, "s3cret").await.status, StatusCode::OK);
    assert_eq!(
        app.get(&clips(&channel(1))).await.status,
        StatusCode::UNAUTHORIZED
    );
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

    // Spaces still want the key.
    assert_eq!(
        app.get(&clips(&channel(1))).await.status,
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
    // The key registered the space, so its members need none from now on.
    assert_eq!(app.delete(&uri).await.status, StatusCode::NO_CONTENT);
    assert_eq!(
        app.get(&taken_uri(&secret)).await.status,
        StatusCode::NOT_FOUND
    );
}

fn rendezvous(channel: &str) -> String {
    format!("/api/v1/channels/{channel}/rendezvous")
}

#[tokio::test]
async fn a_code_exchange_through_the_relay() {
    let app = app(&["--access-token", "s3cret"]).await;
    let ch = channel(1);
    let auth = [("authorization", "Bearer s3cret")];
    let invite = Invite::new(
        "Home",
        "MacBook",
        Some("s3cret"),
        &Pairing::from_root(&[1; 32]),
    );

    // The inviter opens a rendezvous with its first message.
    let (inviter, message) = CodeInviter::start().unwrap();
    assert_eq!(
        app.post(&rendezvous(&ch), message.clone()).await.status,
        StatusCode::UNAUTHORIZED
    );
    let opened = app
        .call(Method::POST, &rendezvous(&ch), &auth, message)
        .await;
    assert_eq!(opened.status, StatusCode::CREATED);
    let nameplate = opened.json::<RendezvousOpened>().nameplate;
    assert_eq!(nameplate, 1);
    // What the inviter shows, typed on the other device.
    let code: Code = inviter.code(nameplate).to_string().parse().unwrap();

    // The joiner, without a token.
    let a0 = app
        .get(&format!("/api/v1/rendezvous/{nameplate}/a/0"))
        .await;
    assert_eq!(a0.status, StatusCode::OK);
    let (answer, joiner_key) = CodeJoiner::start(&code)
        .answer(&a0.body, "Anna's iPhone")
        .unwrap();
    let b0 = format!("/api/v1/rendezvous/{nameplate}/b/0");
    assert_eq!(
        app.put(&b0, answer.clone()).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(app.put(&b0, answer).await.status, StatusCode::CONFLICT);

    // Only the inviting space reads the answer and writes the invite.
    let other_b0 = format!("{}/{nameplate}/b/0?wait=0", rendezvous(&channel(9)));
    let other = app.call(Method::GET, &other_b0, &auth, vec![]).await;
    assert_eq!(other.status, StatusCode::NOT_FOUND);
    let member_b0 = format!("{}/{nameplate}/b/0?wait=0", rendezvous(&ch));
    let got = app.call(Method::GET, &member_b0, &auth, vec![]).await;
    assert_eq!(got.status, StatusCode::OK);
    let (device, inviter_key) = inviter.finish(nameplate, &got.body).unwrap();
    assert_eq!(device, "Anna's iPhone");
    let a1 = format!("{}/{nameplate}/a/1", rendezvous(&ch));
    let sealed = inviter_key.seal_invite(&invite).unwrap();
    let put = app.call(Method::PUT, &a1, &auth, sealed).await;
    assert_eq!(put.status, StatusCode::NO_CONTENT);

    let taken = app
        .get(&format!("/api/v1/rendezvous/{nameplate}/a/1?wait=0"))
        .await;
    assert_eq!(taken.status, StatusCode::OK);
    assert_eq!(joiner_key.open_invite(&taken.body).unwrap(), invite);
    // That was the end of it.
    let gone = app
        .get(&format!("/api/v1/rendezvous/{nameplate}/a/0"))
        .await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn codes_wait_expire_and_close() {
    let app = app(&[]).await;
    let ch = channel(1);
    let opened = app.post(&rendezvous(&ch), vec![1]).await;
    let nameplate = opened.json::<RendezvousOpened>().nameplate;
    let b0 = format!("{}/{nameplate}/b/0?wait=0", rendezvous(&ch));
    assert_eq!(app.get(&b0).await.status, StatusCode::NO_CONTENT);
    let other = format!("{}/{nameplate}/b/0?wait=0", rendezvous(&channel(2)));
    assert_eq!(app.get(&other).await.status, StatusCode::NOT_FOUND);

    app.clock.advance(Duration::from_secs(10 * 60));
    assert_eq!(app.get(&b0).await.status, StatusCode::NOT_FOUND);

    let opened = app.post(&rendezvous(&ch), vec![1]).await;
    let nameplate = opened.json::<RendezvousOpened>().nameplate;
    let close = format!("{}/{nameplate}", rendezvous(&ch));
    assert_eq!(
        app.delete(&format!("{}/{nameplate}", rendezvous(&channel(2))))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(app.delete(&close).await.status, StatusCode::NO_CONTENT);
    let a0 = format!("/api/v1/rendezvous/{nameplate}/a/0");
    assert_eq!(app.get(&a0).await.status, StatusCode::NOT_FOUND);

    for (uri, body) in [
        (rendezvous(&ch), vec![]),
        (rendezvous(&ch), vec![1; 4097]),
        (format!("/api/v1/rendezvous/{nameplate}/b/9"), vec![1]),
    ] {
        let res = app
            .call(
                if uri.ends_with("/9") {
                    Method::PUT
                } else {
                    Method::POST
                },
                &uri,
                &[],
                body,
            )
            .await;
        assert_eq!(res.status, StatusCode::BAD_REQUEST, "{uri}");
    }
    app.post(&rendezvous(&ch), vec![1]).await;
    app.post(&rendezvous(&ch), vec![1]).await;
    let third = app.post(&rendezvous(&ch), vec![1]).await;
    assert_eq!(third.status, StatusCode::TOO_MANY_REQUESTS);
}

fn limits_uri(channel: &str) -> String {
    format!("/api/v1/channels/{channel}/limits")
}

/// From `ip`, as a reverse proxy would say.
fn from(ip: &str) -> [(&'static str, &str); 1] {
    [("x-forwarded-for", ip)]
}

#[tokio::test]
async fn the_key_registers_a_space_and_members_need_none() {
    let app = app(&["--access-token", "s3cret"]).await;
    let (ch, other) = (channel(1), channel(2));
    assert_eq!(app.get(&clips(&ch)).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        app.get_as(&clips(&ch), "nope").await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.get_as(&clips(&ch), "s3cret").await.status,
        StatusCode::OK
    );

    // Joined devices have no key.
    app.create(&ch, None).await;
    assert_eq!(app.list(&ch).await.len(), 1);
    assert_eq!(
        app.get_as(&clips(&ch), "nope").await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.get(&clips(&other)).await.status,
        StatusCode::UNAUTHORIZED
    );
    let limits: SpaceLimits = app.get(&limits_uri(&ch)).await.json();
    assert_eq!(limits.plan, Plan::Unlimited);
    assert_eq!(limits.max_clip_bytes, None);

    // Registrations outlive a restart.
    let app = app_in(app.dir, &["--access-token", "s3cret"]).await;
    assert_eq!(app.get(&clips(&ch)).await.status, StatusCode::OK);
    assert_eq!(
        app.get(&clips(&other)).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn a_relay_without_a_key_takes_every_space_unlimited() {
    let app = app(&[]).await;
    let limits: SpaceLimits = app.get(&limits_uri(&channel(1))).await.json();
    assert_eq!(
        limits,
        SpaceLimits {
            plan: Plan::Unlimited,
            default_ttl_secs: 15 * 60,
            max_ttl_secs: 24 * 3600,
            max_clip_bytes: None,
            daily_transfer_bytes: None,
            transfer_used_bytes: 0,
            max_clips: 50,
        }
    );
    // A key is ignored, as before.
    assert_eq!(
        app.get_as(&clips(&channel(1)), "anything").await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_public_relay_puts_new_spaces_on_the_free_plan() {
    let app = app(&["--public", "--free-max-size", "2KiB"]).await;
    let config: ServerConfig = app.get("/api/v1/config").await.json();
    assert_eq!(config.accounts, Some(AccountsConfig { public: true }));

    let ch = channel(1);
    let limits: SpaceLimits = app.get(&limits_uri(&ch)).await.json();
    assert_eq!(
        limits,
        SpaceLimits {
            plan: Plan::Free,
            default_ttl_secs: 15 * 60,
            max_ttl_secs: 3600,
            max_clip_bytes: Some(2048),
            daily_transfer_bytes: Some(500_000_000),
            transfer_used_bytes: 0,
            max_clips: 50,
        }
    );

    // TTLs are clamped to the plan's.
    let meta = app.create(&ch, Some("86400")).await;
    assert_eq!(meta.expires_at_ms - meta.created_at_ms, 3600 * 1000);

    // So are sizes, of single clips and of chunked ones.
    let big = app.post(&clips(&ch), envelope(&"x".repeat(3000))).await;
    assert_eq!(big.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert!(
        String::from_utf8_lossy(&big.body).contains("at most 2.0 kB"),
        "{}",
        String::from_utf8_lossy(&big.body)
    );
    let upload = app
        .post(&upload_uri(&ch, 2 * CHUNK, CHUNK), envelope("header"))
        .await;
    assert_eq!(upload.status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn free_spaces_have_a_daily_transfer() {
    let app = app(&["--public", "--free-daily-transfer", "1KB"]).await;
    let ch = channel(1);
    let meta = app.create(&ch, None).await;
    let clip = format!("{}/{}", clips(&ch), meta.id);
    // Uploads and downloads count, until the next one doesn't fit.
    let mut used = meta.size;
    while used + meta.size <= 1000 {
        assert_eq!(app.get(&clip).await.status, StatusCode::OK);
        used += meta.size;
    }
    let limits: SpaceLimits = app.get(&limits_uri(&ch)).await.json();
    assert_eq!(limits.transfer_used_bytes, used);
    assert_eq!(app.get(&clip).await.status, StatusCode::TOO_MANY_REQUESTS);
    let upload = app.post(&clips(&ch), envelope("hi")).await;
    assert_eq!(upload.status, StatusCode::TOO_MANY_REQUESTS);
    // Other spaces have their own.
    app.create(&channel(2), None).await;

    app.clock.advance(Duration::from_secs(24 * 3600));
    let fresh = app.create(&ch, None).await;
    let limits: SpaceLimits = app.get(&limits_uri(&ch)).await.json();
    assert_eq!(limits.transfer_used_bytes, fresh.size);
}

#[tokio::test]
async fn one_address_creates_few_spaces_a_day() {
    let app = app(&["--public", "--new-spaces-per-ip", "2"]).await;
    let get = |ch: String, ip: &'static str| {
        let app = &app;
        async move {
            app.call(Method::GET, &clips(&ch), &from(ip), vec![])
                .await
                .status
        }
    };
    assert_eq!(get(channel(1), "203.0.113.1").await, StatusCode::OK);
    assert_eq!(get(channel(2), "203.0.113.1").await, StatusCode::OK);
    assert_eq!(
        get(channel(3), "203.0.113.1").await,
        StatusCode::TOO_MANY_REQUESTS
    );
    // Spaces it has keep working, and others may still join them.
    assert_eq!(get(channel(1), "203.0.113.1").await, StatusCode::OK);
    assert_eq!(get(channel(2), "198.51.100.1").await, StatusCode::OK);
    assert_eq!(get(channel(3), "198.51.100.1").await, StatusCode::OK);

    app.clock.advance(Duration::from_secs(24 * 3600));
    assert_eq!(get(channel(4), "203.0.113.1").await, StatusCode::OK);
}

#[tokio::test]
async fn a_public_relay_limits_request_rates() {
    // Bursts of one.
    let app = app(&["--public", "--requests-per-minute", "5"]).await;
    let status = |ip: &'static str| {
        let app = &app;
        async move {
            app.call(Method::GET, "/api/v1/config", &from(ip), vec![])
                .await
                .status
        }
    };
    assert_eq!(status("203.0.113.1").await, StatusCode::OK);
    let limited = app
        .call(Method::GET, "/api/v1/config", &from("203.0.113.1"), vec![])
        .await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(limited.headers[header::RETRY_AFTER], "12");
    assert_eq!(status("203.0.113.2").await, StatusCode::OK);
    app.clock.advance(Duration::from_secs(12));
    assert_eq!(status("203.0.113.1").await, StatusCode::OK);
    // Only the API.
    assert_eq!(app.get("/healthz").await.status, StatusCode::OK);
    assert_eq!(app.get("/healthz").await.status, StatusCode::OK);
}

#[tokio::test]
async fn the_owners_key_lifts_the_free_plan_on_a_public_relay() {
    let app = app(&["--public", "--access-token", "s3cret"]).await;
    let ch = channel(1);
    let limits: SpaceLimits = app.get(&limits_uri(&ch)).await.json();
    assert_eq!(limits.plan, Plan::Free);
    let limits: SpaceLimits = app.get_as(&limits_uri(&ch), "s3cret").await.json();
    assert_eq!(limits.plan, Plan::Unlimited);
    // For every member, from now on.
    let limits: SpaceLimits = app.get(&limits_uri(&ch)).await.json();
    assert_eq!(limits.plan, Plan::Unlimited);
    let meta = app.create(&ch, Some("86400")).await;
    assert_eq!(meta.expires_at_ms - meta.created_at_ms, 86400 * 1000);
}

#[tokio::test]
async fn unused_free_spaces_are_forgotten() {
    let app = app(&["--public", "--access-token", "s3cret"]).await;
    let (free, owned) = (channel(1), channel(2));
    app.get(&clips(&free)).await;
    app.get_as(&clips(&owned), "s3cret").await;
    let (free_id, owned_id): (ChannelId, ChannelId) =
        (free.parse().unwrap(), owned.parse().unwrap());

    app.clock.advance(Duration::from_secs(29 * 24 * 3600));
    app.accounts.prune(app.clock_now()).await.unwrap();
    assert!(app.accounts.account(&free_id).is_some());

    app.clock.advance(Duration::from_secs(24 * 3600));
    app.accounts.prune(app.clock_now()).await.unwrap();
    assert!(app.accounts.account(&free_id).is_none());
    assert!(app.accounts.account(&owned_id).is_some());
    let app = app_in(app.dir, &["--public", "--access-token", "s3cret"]).await;
    assert!(app.accounts.account(&free_id).is_none());
    assert!(app.accounts.account(&owned_id).is_some());
}
