//! Big files as chunks: uploaded a few at a time from disk and downloaded in
//! order, so no more than a few chunks are ever in memory.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use futures_util::{StreamExt, TryStreamExt, stream};
use reqwest::StatusCode;
use yacs_core::api::{ClipMeta, ENVELOPE_CONTENT_TYPE, UploadCreated};
use yacs_core::{CHUNK_TAG_LEN, Clip, ClipItem, Envelope, Payload, Stream, StreamCipher};

use crate::{Client, Error, Result};

/// Chunks on the wire at once, each way.
const IN_FLIGHT: usize = 3;
/// Attempts per chunk before the transfer fails. With the backoff below
/// that's about a minute of trying.
const ATTEMPTS: u32 = 6;
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(20);
/// One chunk over a slow connection (4 MiB at 100 kB/s is 40 s).
const CHUNK_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Called with the plaintext bytes done and the total, after every chunk.
pub type Progress<'a> = &'a (dyn Fn(u64, u64) + Send + Sync);

/// The files of a [`Stream`], on disk, in stream order.
pub struct LocalFiles(Vec<(PathBuf, u64)>);

impl LocalFiles {
    /// Each path with the size the stream announces for it.
    pub fn new(files: Vec<(PathBuf, u64)>) -> Self {
        Self(files)
    }

    /// `len` bytes of the concatenated files, from `offset`, with room for
    /// the tag. Blocking.
    fn read(&self, mut offset: u64, len: usize) -> io::Result<Vec<u8>> {
        let mut out = Vec::with_capacity(len + CHUNK_TAG_LEN);
        out.resize(len, 0);
        let mut filled = 0;
        for (path, size) in &self.0 {
            if filled == len {
                break;
            }
            if offset >= *size {
                offset -= size;
                continue;
            }
            let take = (len - filled).min((size - offset) as usize);
            let mut file = File::open(path)?;
            file.seek(SeekFrom::Start(offset))?;
            file.read_exact(&mut out[filled..filled + take])
                .map_err(|e| match e.kind() {
                    io::ErrorKind::UnexpectedEof => io::Error::other(format!(
                        "{} got shorter while it was being sent",
                        path.display()
                    )),
                    _ => e,
                })?;
            filled += take;
            offset = 0;
        }
        if filled < len {
            return Err(io::Error::other("the files are shorter than announced"));
        }
        Ok(out)
    }
}

/// Where downloaded files go. Gets every file of the stream in order:
/// `start_file`, then its bytes (none for an empty file).
pub trait Sink {
    fn start_file(&mut self, index: usize) -> io::Result<()>;
    fn write(&mut self, data: &[u8]) -> io::Result<()>;
}

/// The clip's [`Stream`], if its files travel as chunks.
pub fn stream_of(clip: &Clip) -> Option<&Stream> {
    clip.items.iter().find_map(|item| match item {
        ClipItem::Stream(stream) => Some(stream),
        _ => None,
    })
}

impl Client {
    /// Encrypt and upload a clip whose [`Stream`] item's files are read from
    /// `files`. Only relays with [`ServerConfig::chunked`] take this.
    /// Resolving `cancel` stops it with [`Error::Cancelled`].
    ///
    /// [`ServerConfig::chunked`]: yacs_core::api::ServerConfig::chunked
    pub async fn push_stream(
        &self,
        clip: &Clip,
        files: LocalFiles,
        ttl: Option<Duration>,
        progress: Progress<'_>,
        cancel: impl Future<Output = ()>,
    ) -> Result<ClipMeta> {
        let stream = stream_of(clip).ok_or(yacs_core::Error::Malformed)?;
        let cipher = Arc::new(stream.cipher(&self.pairing)?);
        let header = Envelope::seal(&self.pairing, &Payload::Clip(clip.clone()))?.to_bytes();
        let chunk_size = u64::from(stream.chunk_size) + CHUNK_TAG_LEN as u64;
        let mut query = vec![("length", stream.sealed_len()), ("chunk_size", chunk_size)];
        if let Some(ttl) = ttl {
            query.push(("ttl", ttl.as_secs().max(1)));
        }
        let req = self
            .http
            .post(self.uploads_url.clone())
            .query(&query)
            .header(reqwest::header::CONTENT_TYPE, ENVELOPE_CONTENT_TYPE)
            .body(header);
        let res = self.send(req).await?;
        let id = res
            .json::<UploadCreated>()
            .await
            .map_err(|_| Error::BadResponse)?
            .id;

        let result = tokio::select! {
            result = self.upload_chunks(&id, cipher, Arc::new(files), progress) => result,
            () = cancel => Err(Error::Cancelled),
        };
        let result = match result {
            // Retried like the chunks, or one bad gateway loses all of them.
            // Safe: the relay makes the clip once, and a retry after that
            // finds no upload.
            Ok(()) => {
                let url = self.upload_url(&id, &["complete"]);
                retry(|| async {
                    let res = self.send(self.http.post(url.clone())).await?;
                    res.json().await.map_err(|_| Error::BadResponse)
                })
                .await
            }
            Err(e) => Err(e),
        };
        if result.is_err() {
            // Frees the relay's quota now rather than in a day.
            let _ = self.send(self.http.delete(self.upload_url(&id, &[]))).await;
        }
        result
    }

    async fn upload_chunks(
        &self,
        id: &str,
        cipher: Arc<StreamCipher>,
        files: Arc<LocalFiles>,
        progress: Progress<'_>,
    ) -> Result<()> {
        let stream = cipher.stream();
        let total = stream.total();
        let done = AtomicU64::new(0);
        progress(0, total);
        stream::iter(0..stream.chunk_count())
            .map(|index| {
                let (cipher, files) = (cipher.clone(), files.clone());
                let (done, url) = (&done, self.upload_url(id, &["chunks", &index.to_string()]));
                async move {
                    let offset = index * u64::from(cipher.stream().chunk_size);
                    let len = cipher.stream().chunk_len(index);
                    // Sealed once and kept for retries: sealing the chunk
                    // again could reuse its nonce for changed data.
                    let sealed = tokio::task::spawn_blocking(move || {
                        let plaintext = files.read(offset, len)?;
                        cipher.seal(index, plaintext).map_err(Error::from)
                    })
                    .await
                    .map_err(|e| Error::Io(io::Error::other(e)))??;
                    let body = Bytes::from(sealed);
                    retry(|| {
                        let req = self
                            .stream_http
                            .put(url.clone())
                            .timeout(CHUNK_TIMEOUT)
                            .header(reqwest::header::CONTENT_TYPE, ENVELOPE_CONTENT_TYPE)
                            .body(body.clone());
                        async move { self.send(req).await.map(drop) }
                    })
                    .await?;
                    let now = done.fetch_add(len as u64, Ordering::SeqCst) + len as u64;
                    progress(now, total);
                    Ok::<_, Error>(())
                }
            })
            .buffer_unordered(IN_FLIGHT)
            .try_collect::<()>()
            .await
    }

    /// Download and decrypt a chunked clip's files into `sink`, in order.
    pub async fn fetch_stream(
        &self,
        id: &str,
        stream: &Stream,
        sink: &mut (dyn Sink + Send),
        progress: Progress<'_>,
    ) -> Result<()> {
        let cipher = Arc::new(stream.cipher(&self.pairing)?);
        let total = stream.total();
        let mut chunks = stream::iter(0..stream.chunk_count())
            .map(|index| {
                let cipher = cipher.clone();
                let url = self.clip_chunk_url(id, index);
                let len = stream.sealed_chunk_len(index);
                async move {
                    let sealed = retry(|| {
                        let req = self.stream_http.get(url.clone()).timeout(CHUNK_TIMEOUT);
                        async move {
                            let mut res = self.send(req).await?;
                            // Exactly the chunk's size: growing buffers bloat the heap.
                            let mut sealed = Vec::with_capacity(len);
                            while let Some(bytes) = res.chunk().await? {
                                if sealed.len() + bytes.len() > len {
                                    return Err(Error::BadResponse);
                                }
                                sealed.extend_from_slice(&bytes);
                            }
                            Ok(sealed)
                        }
                    })
                    .await?;
                    tokio::task::spawn_blocking(move || cipher.open(index, sealed))
                        .await
                        .map_err(|e| Error::Io(io::Error::other(e)))?
                        .map_err(Error::from)
                }
            })
            .buffered(IN_FLIGHT);

        let files = &stream.files;
        let mut file = 0;
        let mut left = files.first().map_or(0, |f| f.size);
        let mut done = 0;
        if !files.is_empty() {
            sink.start_file(0)?;
        }
        progress(0, total);
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk?;
            let mut data = &chunk[..];
            while !data.is_empty() {
                while left == 0 {
                    file += 1;
                    sink.start_file(file)?;
                    left = files[file].size;
                }
                let n = data.len().min(left as usize);
                sink.write(&data[..n])?;
                (data, left) = (&data[n..], left - n as u64);
            }
            done += chunk.len() as u64;
            progress(done, total);
        }
        // Empty files at the end.
        while file + 1 < files.len() {
            file += 1;
            sink.start_file(file)?;
        }
        Ok(())
    }

    fn upload_url(&self, id: &str, rest: &[&str]) -> reqwest::Url {
        let mut url = self.uploads_url.clone();
        url.path_segments_mut()
            .expect("http(s) URLs have path segments")
            .push(id)
            .extend(rest);
        url
    }

    fn clip_chunk_url(&self, id: &str, index: u64) -> reqwest::Url {
        let mut url = self.clip_url(id);
        url.path_segments_mut()
            .expect("http(s) URLs have path segments")
            .push("chunks")
            .push(&index.to_string());
        url
    }
}

/// Retries network trouble and server errors with backoff; anything the
/// relay refused on purpose fails right away.
pub(crate) async fn retry<T, F: Future<Output = Result<T>>>(
    mut attempt: impl FnMut() -> F,
) -> Result<T> {
    let mut delay = BACKOFF_MIN;
    for _ in 1..ATTEMPTS {
        match attempt().await {
            Err(e) if transient(&e) => {
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(BACKOFF_MAX);
            }
            result => return result,
        }
    }
    attempt().await
}

pub(crate) fn transient(e: &Error) -> bool {
    match e {
        Error::Http(_) | Error::RateLimited => true,
        Error::Server { status, .. } => {
            *status >= 500 || *status == StatusCode::REQUEST_TIMEOUT.as_u16()
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_across_file_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let file = |name: &str, data: &[u8]| {
            let path = dir.path().join(name);
            std::fs::write(&path, data).unwrap();
            (path, data.len() as u64)
        };
        let files = LocalFiles::new(vec![
            file("a", b"hello"),
            file("empty", b""),
            file("b", b" world"),
        ]);
        assert_eq!(files.read(0, 11).unwrap(), b"hello world");
        assert_eq!(files.read(3, 5).unwrap(), b"lo wo");
        assert_eq!(files.read(5, 6).unwrap(), b" world");
        assert!(files.read(8, 5).is_err());

        let shrunk = LocalFiles::new(vec![(dir.path().join("a"), 9)]);
        let err = shrunk.read(0, 9).unwrap_err().to_string();
        assert!(err.contains("got shorter"), "{err}");
    }

    #[derive(Default)]
    struct Calls {
        completes: AtomicU64,
        deletes: AtomicU64,
    }

    /// Takes an upload, but answers its `complete` with `status` the first
    /// `fails` times, as a proxy in front of a relay might.
    async fn flaky_relay(status: StatusCode, fails: u64) -> (String, Arc<Calls>) {
        use axum::http::Method;
        use axum::response::IntoResponse;

        let calls = Arc::new(Calls::default());
        let counted = calls.clone();
        let app = axum::Router::new().fallback(move |method: Method, uri: axum::http::Uri| {
            let calls = counted.clone();
            async move {
                let path = uri.path();
                match method {
                    Method::POST if path.ends_with("/uploads") => (
                        StatusCode::CREATED,
                        axum::Json(UploadCreated { id: "u".into() }),
                    )
                        .into_response(),
                    Method::PUT => StatusCode::NO_CONTENT.into_response(),
                    Method::POST if path.ends_with("/complete") => {
                        if calls.completes.fetch_add(1, Ordering::SeqCst) < fails {
                            return status.into_response();
                        }
                        let meta = ClipMeta {
                            id: "c".into(),
                            created_at_ms: 1,
                            expires_at_ms: 2,
                            size: 3,
                            chunked: true,
                        };
                        (StatusCode::CREATED, axum::Json(meta)).into_response()
                    }
                    Method::DELETE => {
                        calls.deletes.fetch_add(1, Ordering::SeqCst);
                        StatusCode::NO_CONTENT.into_response()
                    }
                    _ => StatusCode::NOT_FOUND.into_response(),
                }
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, calls)
    }

    async fn push(relay: &str) -> Result<ClipMeta> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        std::fs::write(&path, b"hello").unwrap();
        let file = yacs_core::StreamFile {
            name: "f".into(),
            mime: "application/octet-stream".into(),
            size: 5,
        };
        let clip = Clip {
            created_at_ms: 1,
            device_name: "test".into(),
            items: vec![ClipItem::Stream(Stream::new(vec![file]).unwrap())],
        };
        let pairing = yacs_core::Pairing {
            channel_id: yacs_core::ChannelId::from_bytes([1; 32]),
            key: yacs_core::ChannelKey::from_bytes([2; 32]),
        };
        let client = Client::new(relay, None, pairing).unwrap();
        let files = LocalFiles::new(vec![(path, 5)]);
        let pending = std::future::pending();
        client
            .push_stream(&clip, files, None, &|_, _| {}, pending)
            .await
    }

    /// Losing every chunk of a big file to one bad gateway would be a waste.
    #[tokio::test]
    async fn completing_an_upload_is_retried() {
        let (relay, calls) = flaky_relay(StatusCode::BAD_GATEWAY, 1).await;
        assert_eq!(push(&relay).await.unwrap().id, "c");
        assert_eq!(calls.completes.load(Ordering::SeqCst), 2);
        assert_eq!(calls.deletes.load(Ordering::SeqCst), 0);
    }

    /// A refusal isn't retried, and the upload is dropped to free its quota.
    #[tokio::test]
    async fn a_refused_upload_is_dropped() {
        let (relay, calls) = flaky_relay(StatusCode::CONFLICT, u64::MAX).await;
        assert!(push(&relay).await.is_err());
        assert_eq!(calls.completes.load(Ordering::SeqCst), 1);
        assert_eq!(calls.deletes.load(Ordering::SeqCst), 1);
    }
}
