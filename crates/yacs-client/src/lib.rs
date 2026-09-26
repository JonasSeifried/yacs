//! Talks to a YACS relay on behalf of one space (channel). Encrypts before
//! sending and decrypts after receiving, so callers only see plaintext clips.

mod chunks;
pub mod spaces;
mod sse;

pub use chunks::{LocalFiles, Progress, Sink, stream_of};

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{self, HeaderMap};
use reqwest::{RequestBuilder, Response, StatusCode, Url};
use yacs_core::api::{
    ChannelEvent, ClipMeta, ENVELOPE_CONTENT_TYPE, ErrorBody, HEADER_CHUNKED, HEADER_CLIP_ID,
    HEADER_CREATED_AT, HEADER_EXPIRES_AT, HEADER_SIZE, ServerConfig,
};
use yacs_core::{Envelope, Invite, InviteSecret, Pairing, Payload};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid server URL: {0}")]
    InvalidUrl(String),
    #[error("could not reach the server: {0}")]
    Http(#[from] reqwest::Error),
    #[error("the server rejected the access token")]
    Unauthorized,
    #[error("the clip is too large for this server")]
    TooLarge,
    #[error("the server's storage is full")]
    StorageFull,
    #[error("rate limited by the server, try again shortly")]
    RateLimited,
    #[error("too many uploads are running on this channel; try again when one is done")]
    TooManyUploads,
    #[error("server error {status}: {message}")]
    Server { status: u16, message: String },
    #[error("the server sent a malformed response")]
    BadResponse,
    #[error("the relay is too old for invites; update it (`yacs relay update` on its machine)")]
    NoInvites,
    #[error("this invite was already used or has expired; make a new one on the other device")]
    InviteGone,
    #[error("the live update connection went quiet")]
    Stalled,
    #[error("cancelled")]
    Cancelled,
    #[error(transparent)]
    Protocol(#[from] yacs_core::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// A live event stream sends a keep-alive every 20 s; this much silence means
/// the connection is dead (e.g. after the computer slept).
const EVENTS_IDLE: Duration = Duration::from_secs(60);

pub struct Client {
    http: reqwest::Client,
    /// Without the overall timeout, which would cut off the event stream.
    stream_http: reqwest::Client,
    events_url: Url,
    clips_url: Url,
    uploads_url: Url,
    invites_url: Url,
    config_url: Url,
    token: Option<String>,
    pairing: Pairing,
}

impl Client {
    /// `server` is the relay's base URL, e.g. `https://clip.example.com`.
    pub fn new(server: &str, token: Option<String>, pairing: Pairing) -> Result<Self> {
        let base = Url::parse(server).map_err(|e| Error::InvalidUrl(e.to_string()))?;
        if !matches!(base.scheme(), "http" | "https") {
            return Err(Error::InvalidUrl(
                "must start with http:// or https://".into(),
            ));
        }
        let api = |path: &str| {
            let base = base.as_str().trim_end_matches('/');
            Url::parse(&format!("{base}/api/v1/{path}"))
                .map_err(|e| Error::InvalidUrl(e.to_string()))
        };
        let builder = || {
            reqwest::Client::builder()
                .user_agent(concat!("yacs/", env!("CARGO_PKG_VERSION")))
                .connect_timeout(Duration::from_secs(10))
        };
        Ok(Self {
            http: builder().timeout(Duration::from_secs(120)).build()?,
            stream_http: builder().build()?,
            events_url: api(&format!("channels/{}/events", pairing.channel_id))?,
            clips_url: api(&format!("channels/{}/clips", pairing.channel_id))?,
            uploads_url: api(&format!("channels/{}/uploads", pairing.channel_id))?,
            invites_url: api(&format!("channels/{}/invites", pairing.channel_id))?,
            config_url: api("config")?,
            token: token.filter(|t| !t.is_empty()),
            pairing,
        })
    }

    pub fn pairing(&self) -> &Pairing {
        &self.pairing
    }

    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    pub async fn config(&self) -> Result<ServerConfig> {
        let res = self.send(self.http.get(self.config_url.clone())).await?;
        res.json().await.map_err(|_| Error::BadResponse)
    }

    /// Encrypt and upload. `ttl: None` uses the server default; the server clamps it to its max.
    pub async fn push(&self, payload: &Payload, ttl: Option<Duration>) -> Result<ClipMeta> {
        self.push_reporting(payload, ttl, None).await
    }

    /// Like [`push`](Self::push), telling `progress` how many bytes of the
    /// upload went out, and of how many.
    pub async fn push_reporting(
        &self,
        payload: &Payload,
        ttl: Option<Duration>,
        progress: Option<Arc<dyn Fn(u64, u64) + Send + Sync>>,
    ) -> Result<ClipMeta> {
        let body = Envelope::seal(&self.pairing, payload)?.to_bytes();
        let len = body.len();
        let body = match progress.clone() {
            None => reqwest::Body::from(body),
            Some(progress) => {
                // Handed out a piece at a time: each piece asked for means the
                // ones before it are on their way. All of it is only once the
                // relay answers.
                const PIECE: usize = 64 * 1024;
                let bytes = bytes::Bytes::from(body);
                let pieces = (0..len).step_by(PIECE).map(move |start| {
                    progress(start as u64, len as u64);
                    Ok::<_, std::io::Error>(bytes.slice(start..(start + PIECE).min(len)))
                });
                reqwest::Body::wrap_stream(futures_util::stream::iter(pieces))
            }
        };
        let mut req = self
            .http
            .post(self.clips_url.clone())
            .header(header::CONTENT_TYPE, ENVELOPE_CONTENT_TYPE)
            .header(header::CONTENT_LENGTH, len)
            .body(body);
        if let Some(ttl) = ttl {
            req = req.query(&[("ttl", ttl.as_secs().max(1))]);
        }
        let res = self.send(req).await?;
        let meta = res.json().await.map_err(|_| Error::BadResponse)?;
        if let Some(progress) = progress {
            progress(len as u64, len as u64);
        }
        Ok(meta)
    }

    /// Unexpired clips, newest first. Metadata only; nothing is decrypted.
    pub async fn list(&self) -> Result<Vec<ClipMeta>> {
        let res = self.send(self.http.get(self.clips_url.clone())).await?;
        res.json().await.map_err(|_| Error::BadResponse)
    }

    pub async fn latest(&self) -> Result<Option<(ClipMeta, Payload)>> {
        self.fetch(self.clip_url("latest")).await
    }

    pub async fn get(&self, id: &str) -> Result<Option<(ClipMeta, Payload)>> {
        self.fetch(self.clip_url(id)).await
    }

    /// Returns whether the clip existed.
    pub async fn delete(&self, id: &str) -> Result<bool> {
        match self.send(self.http.delete(self.clip_url(id))).await {
            Ok(_) => Ok(true),
            Err(Error::Server { status: 404, .. }) => Ok(false),
            Err(e) => Err(e),
        }
    }

    pub async fn clear(&self) -> Result<()> {
        self.send(self.http.delete(self.clips_url.clone())).await?;
        Ok(())
    }

    /// Subscribe to changes. Once this returns, nothing is missed until the
    /// stream ends, so list after calling it, not before. Relays before 0.2.0
    /// answer 404.
    pub async fn events(&self) -> Result<Events> {
        let req = self
            .stream_http
            .get(self.events_url.clone())
            .header(header::ACCEPT, "text/event-stream");
        Ok(Events {
            res: self.send(req).await?,
            parser: sse::Parser::default(),
            ready: VecDeque::new(),
        })
    }

    /// Parks a one-time invite to this space on the relay, for a day, and
    /// returns its secret for the link. It carries this client's token.
    pub async fn invite(&self, space_name: &str, inviter: &str) -> Result<InviteSecret> {
        let secret = InviteSecret::generate()?;
        let invite = Invite::new(space_name, inviter, self.token(), &self.pairing);
        let mut url = self.invites_url.clone();
        url.path_segments_mut()
            .expect("http(s) URLs have path segments")
            .push(&secret.slot().to_string());
        let req = self
            .http
            .put(url)
            .header(header::CONTENT_TYPE, ENVELOPE_CONTENT_TYPE)
            .body(secret.seal(&invite)?);
        match self.send(req).await {
            Ok(_) => Ok(secret),
            Err(Error::Server {
                status: 404 | 405, ..
            }) => Err(Error::NoInvites),
            Err(e) => Err(e),
        }
    }

    fn clip_url(&self, id: &str) -> Url {
        let mut url = self.clips_url.clone();
        url.path_segments_mut()
            .expect("http(s) URLs have path segments")
            .push(id);
        url
    }

    async fn fetch(&self, url: Url) -> Result<Option<(ClipMeta, Payload)>> {
        let res = match self.send(self.http.get(url)).await {
            Ok(res) => res,
            Err(Error::Server { status: 404, .. }) => return Ok(None),
            Err(e) => return Err(e),
        };
        let headers = res.headers().clone();
        let body = res.bytes().await?;
        let meta = meta_from_headers(&headers, body.len() as u64).ok_or(Error::BadResponse)?;
        let payload = Envelope::from_bytes(&body)?.open(&self.pairing)?;
        Ok(Some((meta, payload)))
    }

    async fn send(&self, req: RequestBuilder) -> Result<Response> {
        let req = match &self.token {
            Some(token) => req.bearer_auth(token),
            None => req,
        };
        checked(req.send().await?).await
    }
}

/// Takes the invite behind `secret` from `relay`, which hands it out once:
/// `None` if it was taken already, expired or never existed.
pub async fn take_invite(relay: &str, secret: &InviteSecret) -> Result<Option<Invite>> {
    let base = Url::parse(relay).map_err(|e| Error::InvalidUrl(e.to_string()))?;
    if !matches!(base.scheme(), "http" | "https") {
        return Err(Error::InvalidUrl(
            "must start with http:// or https://".into(),
        ));
    }
    let base = base.as_str().trim_end_matches('/');
    let url = Url::parse(&format!("{base}/api/v1/invites/{}", secret.slot()))
        .map_err(|e| Error::InvalidUrl(e.to_string()))?;
    let http = reqwest::Client::builder()
        .user_agent(concat!("yacs/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()?;
    let res = match checked(http.get(url).send().await?).await {
        Ok(res) => res,
        Err(Error::Server { status: 404, .. }) => return Ok(None),
        Err(e) => return Err(e),
    };
    Ok(Some(secret.open(&res.bytes().await?)?))
}

/// The response if it succeeded, the relay's error otherwise.
async fn checked(res: Response) -> Result<Response> {
    let status = res.status();
    if status.is_success() {
        return Ok(res);
    }
    Err(match status {
        StatusCode::UNAUTHORIZED => Error::Unauthorized,
        StatusCode::PAYLOAD_TOO_LARGE => Error::TooLarge,
        StatusCode::INSUFFICIENT_STORAGE => Error::StorageFull,
        StatusCode::TOO_MANY_REQUESTS => Error::RateLimited,
        _ => {
            let text = res.text().await.unwrap_or_default();
            let message = serde_json::from_str::<ErrorBody>(&text)
                .map(|b| b.error)
                .unwrap_or(text);
            Error::Server {
                status: status.as_u16(),
                message,
            }
        }
    })
}

/// See [`Client::events`].
pub struct Events {
    res: Response,
    parser: sse::Parser,
    ready: VecDeque<String>,
}

impl Events {
    /// The next change; `Ok(None)` once the relay ended the stream (it's
    /// shutting down, or this client fell behind). Reconnect after either.
    pub async fn next(&mut self) -> Result<Option<ChannelEvent>> {
        loop {
            if let Some(data) = self.ready.pop_front() {
                return serde_json::from_str(&data)
                    .map(Some)
                    .map_err(|_| Error::BadResponse);
            }
            let chunk = tokio::time::timeout(EVENTS_IDLE, self.res.chunk())
                .await
                .map_err(|_| Error::Stalled)??;
            match chunk {
                Some(bytes) => self.ready.extend(self.parser.feed(&bytes)),
                None => return Ok(None),
            }
        }
    }
}

/// `body_len` is the size for relays that don't say (before 0.3.0).
fn meta_from_headers(headers: &HeaderMap, body_len: u64) -> Option<ClipMeta> {
    let get = |name: &str| headers.get(name)?.to_str().ok();
    Some(ClipMeta {
        id: get(HEADER_CLIP_ID)?.to_owned(),
        created_at_ms: get(HEADER_CREATED_AT)?.parse().ok()?,
        expires_at_ms: get(HEADER_EXPIRES_AT)?.parse().ok()?,
        size: get(HEADER_SIZE)
            .and_then(|s| s.parse().ok())
            .unwrap_or(body_len),
        chunked: get(HEADER_CHUNKED) == Some("1"),
    })
}
