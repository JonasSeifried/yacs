//! Talks to a YACS relay on behalf of one paired channel. Encrypts before
//! sending and decrypts after receiving, so callers only see plaintext clips.

use std::time::Duration;

use reqwest::header::{self, HeaderMap};
use reqwest::{RequestBuilder, Response, StatusCode, Url};
use yacs_core::api::{
    ClipMeta, ENVELOPE_CONTENT_TYPE, ErrorBody, HEADER_CLIP_ID, HEADER_CREATED_AT,
    HEADER_EXPIRES_AT, ServerConfig,
};
use yacs_core::{Envelope, Pairing, Payload};

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
    #[error("server error {status}: {message}")]
    Server { status: u16, message: String },
    #[error("the server sent a malformed response")]
    BadResponse,
    #[error(transparent)]
    Protocol(#[from] yacs_core::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

pub struct Client {
    http: reqwest::Client,
    clips_url: Url,
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
        let http = reqwest::Client::builder()
            .user_agent(concat!("yacs/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .build()?;
        Ok(Self {
            http,
            clips_url: api(&format!("channels/{}/clips", pairing.channel_id))?,
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
        let body = Envelope::seal(&self.pairing, payload)?.to_bytes();
        let mut req = self
            .http
            .post(self.clips_url.clone())
            .header(header::CONTENT_TYPE, ENVELOPE_CONTENT_TYPE)
            .body(body);
        if let Some(ttl) = ttl {
            req = req.query(&[("ttl", ttl.as_secs().max(1))]);
        }
        let res = self.send(req).await?;
        res.json().await.map_err(|_| Error::BadResponse)
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
        let res = req.send().await?;
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
}

fn meta_from_headers(headers: &HeaderMap, size: u64) -> Option<ClipMeta> {
    let get = |name: &str| headers.get(name)?.to_str().ok();
    Some(ClipMeta {
        id: get(HEADER_CLIP_ID)?.to_owned(),
        created_at_ms: get(HEADER_CREATED_AT)?.parse().ok()?,
        expires_at_ms: get(HEADER_EXPIRES_AT)?.parse().ok()?,
        size,
    })
}
