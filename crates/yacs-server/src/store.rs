//! Encrypted clips on disk.
//!
//! ```text
//! {root}/{hex channel id}/{ulid}.{expires_at_ms}.bin           a clip
//! {root}/{hex channel id}/{ulid}.{expires_at_ms}.{size}.d/     a clip with chunks: header.bin, 00000000, 00000001, …
//! {root}/.tmp/{ulid}.tmp                                       in-flight uploads
//! {root}/.tmp/{upload id}.up/                                  chunked uploads, until they're complete
//! ```
//!
//! Channel dirs use hex rather than base64url so two ids can't collide on a
//! case-insensitive filesystem (macOS, Windows). The expiry (and a chunked
//! clip's size) lives in the name so listing and reaping never open a file.
//! Uploads are written to `.tmp` first and renamed into place, so a clip is
//! never seen half-written.
//!
//! Chunked uploads are tracked in memory: a restart drops them (the sender
//! starts over), and their dirs with the rest of `.tmp`.

use std::collections::{BTreeSet, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use axum::body::Bytes;
use futures_util::{Stream, StreamExt};
use tokio::fs;
use tokio::io::{AsyncWriteExt, BufWriter};
use ulid::{Generator, Ulid};
use yacs_core::api::ClipMeta;
use yacs_core::{CHUNK_TAG_LEN, ChannelId, MAX_CHUNK_SIZE, MIN_CHUNK_SIZE};

const TMP_DIR: &str = ".tmp";
const UPLOAD_SUFFIX: &str = ".up";
const HEADER_FILE: &str = "header.bin";
/// Chunked uploads a channel may have open at once.
pub const MAX_OPEN_UPLOADS: usize = 4;
/// Uploads that got no chunk for this long are dropped.
pub const UPLOAD_IDLE_MS: u64 = 24 * 3600 * 1000;
/// Chunks are read and written in pieces this big, not in the network's
/// small frames: each file operation is a trip to tokio's blocking pool.
pub const IO_BUFFER: usize = 256 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum PutError {
    #[error("storage quota exhausted")]
    Full,
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    #[error("storage quota exhausted")]
    Full,
    #[error("too many uploads in progress")]
    TooMany,
    #[error("no such upload")]
    NotFound,
    #[error("{0}")]
    Invalid(&'static str),
    #[error("chunk is larger than announced")]
    TooLarge,
    #[error("the upload is missing chunks")]
    Incomplete,
    #[error(transparent)]
    Io(io::Error),
}

impl From<io::Error> for UploadError {
    /// The upload's dir is gone: it was aborted or dropped meanwhile.
    fn from(e: io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::NotFound => Self::NotFound,
            _ => Self::Io(e),
        }
    }
}

/// How an upload's sealed bytes are cut into chunks, as the sender announced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkLayout {
    /// All chunks together.
    length: u64,
    /// Every chunk but the last.
    chunk_size: u64,
}

impl ChunkLayout {
    /// Sizes are sealed sizes (plaintext plus tag). Only an empty stream has
    /// an empty chunk, and then it's the only one.
    pub fn new(length: u64, chunk_size: u64) -> Result<Self, &'static str> {
        let tag = CHUNK_TAG_LEN as u64;
        let sizes = u64::from(MIN_CHUNK_SIZE) + tag..=u64::from(MAX_CHUNK_SIZE) + tag;
        if !sizes.contains(&chunk_size) {
            return Err("chunk size out of range");
        }
        if length < tag {
            return Err("upload length is too short");
        }
        let layout = Self { length, chunk_size };
        if layout.count() > 1 && layout.len(layout.count() - 1) <= tag {
            return Err("upload length doesn't fit the chunk size");
        }
        Ok(layout)
    }

    pub fn count(&self) -> u64 {
        self.length.div_ceil(self.chunk_size)
    }

    /// Sealed bytes of chunk `index`.
    pub fn len(&self, index: u64) -> u64 {
        if index + 1 < self.count() {
            self.chunk_size
        } else {
            self.length - (self.count() - 1) * self.chunk_size
        }
    }
}

struct Upload {
    channel: ChannelId,
    layout: ChunkLayout,
    /// Counted against the quota from the start: header plus all chunks.
    reserved: u64,
    ttl_ms: u64,
    received: BTreeSet<u64>,
    touched_ms: u64,
}

pub struct Store {
    root: PathBuf,
    max_clips: usize,
    max_disk: u64,
    /// Stored clips plus what open uploads reserved.
    used: AtomicU64,
    /// Serializes everything that renames or deletes, so eviction and reaping
    /// see a consistent directory. Uploads write their bytes outside of it.
    lock: tokio::sync::Mutex<()>,
    ids: Mutex<Generator>,
    uploads: Mutex<HashMap<Ulid, Upload>>,
    /// Names chunk writes apart, so a repeated PUT doesn't clash with the first.
    parts: AtomicU64,
}

#[derive(Debug)]
struct Entry {
    id: Ulid,
    expires_at_ms: u64,
    size: u64,
    chunked: bool,
    path: PathBuf,
}

impl Entry {
    fn meta(&self) -> ClipMeta {
        ClipMeta {
            id: self.id.to_string(),
            created_at_ms: self.id.timestamp_ms(),
            expires_at_ms: self.expires_at_ms,
            size: self.size,
            chunked: self.chunked,
        }
    }
}

impl Store {
    /// Open (or create) a store, discarding uploads interrupted by a crash or
    /// restart, and counting existing clips against the disk quota.
    pub async fn open(
        root: impl Into<PathBuf>,
        max_clips: usize,
        max_disk: u64,
    ) -> io::Result<Self> {
        let root = root.into();
        let tmp = root.join(TMP_DIR);
        if fs::try_exists(&tmp).await? {
            fs::remove_dir_all(&tmp).await?;
        }
        fs::create_dir_all(&tmp).await?;

        let mut used = 0;
        for dir in channel_dirs(&root).await? {
            used += entries(&dir).await?.iter().map(|e| e.size).sum::<u64>();
        }

        Ok(Self {
            root,
            max_clips,
            max_disk,
            used: AtomicU64::new(used),
            lock: tokio::sync::Mutex::new(()),
            ids: Mutex::new(Generator::new()),
            uploads: Mutex::new(HashMap::new()),
            parts: AtomicU64::new(0),
        })
    }

    pub fn used_bytes(&self) -> u64 {
        self.used.load(Ordering::SeqCst)
    }

    fn channel_dir(&self, channel: &ChannelId) -> PathBuf {
        self.root.join(hex::encode(channel.as_bytes()))
    }

    fn upload_dir(&self, id: Ulid) -> PathBuf {
        self.root.join(TMP_DIR).join(format!("{id}{UPLOAD_SUFFIX}"))
    }

    fn next_id(&self, now_ms: u64) -> Ulid {
        let now = UNIX_EPOCH + Duration::from_millis(now_ms);
        let mut ids = self.ids.lock().expect("ulid generator lock poisoned");
        ids.generate_from_datetime(now)
            .unwrap_or_else(|overflow| overflow.commit_overflow_increment())
    }

    fn uploads(&self) -> std::sync::MutexGuard<'_, HashMap<Ulid, Upload>> {
        self.uploads.lock().expect("uploads lock poisoned")
    }

    fn reserve(&self, size: u64) -> bool {
        if self.used.fetch_add(size, Ordering::SeqCst) + size > self.max_disk {
            self.release(size);
            return false;
        }
        true
    }

    /// Saturating: a file this store didn't count (see `reap`) must not wrap
    /// the counter around to "disk full".
    fn release(&self, size: u64) {
        let _ = self
            .used
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |used| {
                Some(used.saturating_sub(size))
            });
    }

    /// Store a clip, then evict the channel's oldest clips beyond `max_clips`.
    pub async fn put(
        &self,
        channel: &ChannelId,
        body: &[u8],
        now_ms: u64,
        expires_at_ms: u64,
    ) -> Result<ClipMeta, PutError> {
        let size = body.len() as u64;
        if !self.reserve(size) {
            return Err(PutError::Full);
        }

        let id = self.next_id(now_ms);
        let tmp = self.root.join(TMP_DIR).join(format!("{id}.tmp"));
        if let Err(e) = fs::write(&tmp, body).await {
            self.release(size);
            let _ = fs::remove_file(&tmp).await;
            return Err(e.into());
        }

        let _guard = self.lock.lock().await;
        let name = format!("{id}.{expires_at_ms}.bin");
        if let Err(e) = self.place(channel, &tmp, &name).await {
            self.release(size);
            let _ = fs::remove_file(&tmp).await;
            return Err(e.into());
        }
        self.evict(channel, now_ms).await?;

        Ok(ClipMeta {
            id: id.to_string(),
            created_at_ms: id.timestamp_ms(),
            expires_at_ms,
            size,
            chunked: false,
        })
    }

    /// Moves a finished clip from `.tmp` into its channel. Call with `lock` held.
    async fn place(&self, channel: &ChannelId, from: &Path, name: &str) -> io::Result<()> {
        let dir = self.channel_dir(channel);
        fs::create_dir_all(&dir).await?;
        fs::rename(from, dir.join(name)).await
    }

    /// Expired clips don't hold a history slot: a short-TTL clip may expire
    /// before an older long-TTL one, which must then survive eviction. Call
    /// with `lock` held.
    async fn evict(&self, channel: &ChannelId, now_ms: u64) -> io::Result<()> {
        let (live, expired): (Vec<_>, Vec<_>) = entries(&self.channel_dir(channel))
            .await?
            .into_iter()
            .partition(|e| e.expires_at_ms > now_ms);
        for old in expired.iter().chain(live.iter().skip(self.max_clips)) {
            self.remove(old).await?;
        }
        Ok(())
    }

    /// Start a chunked upload. `header` is the clip's envelope; the whole
    /// upload counts against the quota from now on. The TTL starts once it's
    /// complete.
    pub async fn create_upload(
        &self,
        channel: &ChannelId,
        header: &[u8],
        layout: ChunkLayout,
        ttl_ms: u64,
        now_ms: u64,
    ) -> Result<Ulid, UploadError> {
        let _guard = self.lock.lock().await;
        let open = self
            .uploads()
            .values()
            .filter(|u| u.channel == *channel)
            .count();
        if open >= MAX_OPEN_UPLOADS {
            return Err(UploadError::TooMany);
        }
        let reserved = header.len() as u64 + layout.length;
        if !self.reserve(reserved) {
            return Err(UploadError::Full);
        }

        let id = self.next_id(now_ms);
        let dir = self.upload_dir(id);
        let created = async {
            fs::create_dir(&dir).await?;
            fs::write(dir.join(HEADER_FILE), header).await
        };
        if let Err(e) = created.await {
            self.release(reserved);
            let _ = fs::remove_dir_all(&dir).await;
            return Err(UploadError::Io(e));
        }
        self.uploads().insert(
            id,
            Upload {
                channel: *channel,
                layout,
                reserved,
                ttl_ms,
                received: BTreeSet::new(),
                touched_ms: now_ms,
            },
        );
        Ok(id)
    }

    /// Store chunk `index`, streamed from `body`. Repeating a chunk replaces it.
    pub async fn put_chunk(
        &self,
        channel: &ChannelId,
        id: Ulid,
        index: u64,
        mut body: impl Stream<Item = io::Result<Bytes>> + Unpin,
        now_ms: u64,
    ) -> Result<(), UploadError> {
        let expected = {
            let mut uploads = self.uploads();
            let upload = uploads
                .get_mut(&id)
                .filter(|u| u.channel == *channel)
                .ok_or(UploadError::NotFound)?;
            if index >= upload.layout.count() {
                return Err(UploadError::Invalid("no such chunk"));
            }
            upload.touched_ms = now_ms;
            upload.layout.len(index)
        };

        let dir = self.upload_dir(id);
        let n = self.parts.fetch_add(1, Ordering::Relaxed);
        let part = dir.join(format!("{index}.{n}.part"));
        let written = async {
            let mut file = BufWriter::with_capacity(IO_BUFFER, fs::File::create(&part).await?);
            let mut written = 0;
            while let Some(bytes) = body.next().await {
                let bytes = bytes
                    .map_err(|_| UploadError::Invalid("the chunk didn't arrive completely"))?;
                written += bytes.len() as u64;
                if written > expected {
                    return Err(UploadError::TooLarge);
                }
                file.write_all(&bytes).await?;
            }
            if written != expected {
                return Err(UploadError::Invalid("chunk is shorter than announced"));
            }
            // Completes the writes before the rename.
            file.flush().await?;
            drop(file);
            fs::rename(&part, dir.join(chunk_name(index))).await?;
            Ok(())
        };
        if let Err(e) = written.await {
            let _ = fs::remove_file(&part).await;
            return Err(e);
        }

        let mut uploads = self.uploads();
        let upload = uploads.get_mut(&id).ok_or(UploadError::NotFound)?;
        upload.received.insert(index);
        upload.touched_ms = now_ms;
        Ok(())
    }

    /// The chunks received so far, ascending. `None` if there's no such upload.
    pub fn upload_status(&self, channel: &ChannelId, id: Ulid) -> Option<Vec<u64>> {
        let uploads = self.uploads();
        let upload = uploads.get(&id).filter(|u| u.channel == *channel)?;
        Some(upload.received.iter().copied().collect())
    }

    /// Turn a finished upload into a clip, then evict like [`put`](Self::put).
    /// The clip gets a new id, so it sorts as the newest.
    pub async fn complete_upload(
        &self,
        channel: &ChannelId,
        id: Ulid,
        now_ms: u64,
    ) -> Result<ClipMeta, UploadError> {
        let _guard = self.lock.lock().await;
        let upload = {
            let mut uploads = self.uploads();
            let upload = uploads
                .get(&id)
                .filter(|u| u.channel == *channel)
                .ok_or(UploadError::NotFound)?;
            if upload.received.len() as u64 != upload.layout.count() {
                return Err(UploadError::Incomplete);
            }
            uploads.remove(&id).expect("just found")
        };

        let clip = self.next_id(now_ms);
        let expires_at_ms = now_ms + upload.ttl_ms;
        let size = upload.reserved;
        let dir = self.upload_dir(id);
        let name = format!("{clip}.{expires_at_ms}.{size}.d");
        if let Err(e) = self.place(channel, &dir, &name).await {
            self.release(size);
            let _ = fs::remove_dir_all(&dir).await;
            return Err(UploadError::Io(e));
        }
        self.evict(channel, now_ms).await?;

        Ok(ClipMeta {
            id: clip.to_string(),
            created_at_ms: clip.timestamp_ms(),
            expires_at_ms,
            size,
            chunked: true,
        })
    }

    /// Returns whether the upload existed.
    pub async fn abort_upload(&self, channel: &ChannelId, id: Ulid) -> io::Result<bool> {
        let _guard = self.lock.lock().await;
        let upload = {
            let mut uploads = self.uploads();
            if !uploads.get(&id).is_some_and(|u| u.channel == *channel) {
                return Ok(false);
            }
            uploads.remove(&id).expect("just found")
        };
        self.release(upload.reserved);
        remove_dir_all(&self.upload_dir(id)).await?;
        Ok(true)
    }

    /// Unexpired clips, newest first.
    pub async fn list(&self, channel: &ChannelId, now_ms: u64) -> io::Result<Vec<ClipMeta>> {
        Ok(live_entries(&self.channel_dir(channel), now_ms)
            .await?
            .iter()
            .map(Entry::meta)
            .collect())
    }

    /// The clip's envelope; for a chunked clip, the header.
    pub async fn get(
        &self,
        channel: &ChannelId,
        id: Ulid,
        now_ms: u64,
    ) -> io::Result<Option<(ClipMeta, Vec<u8>)>> {
        let entries = live_entries(&self.channel_dir(channel), now_ms).await?;
        read_entry(entries.iter().find(|e| e.id == id)).await
    }

    pub async fn latest(
        &self,
        channel: &ChannelId,
        now_ms: u64,
    ) -> io::Result<Option<(ClipMeta, Vec<u8>)>> {
        let entries = live_entries(&self.channel_dir(channel), now_ms).await?;
        read_entry(entries.first()).await
    }

    /// Chunk `index` of a chunked clip, opened, with its length.
    pub async fn chunk(
        &self,
        channel: &ChannelId,
        id: Ulid,
        index: u64,
        now_ms: u64,
    ) -> io::Result<Option<(ClipMeta, fs::File, u64)>> {
        let entries = live_entries(&self.channel_dir(channel), now_ms).await?;
        let Some(entry) = entries.iter().find(|e| e.id == id && e.chunked) else {
            return Ok(None);
        };
        match fs::File::open(entry.path.join(chunk_name(index))).await {
            Ok(file) => {
                let len = file.metadata().await?.len();
                Ok(Some((entry.meta(), file, len)))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Returns whether the clip existed.
    pub async fn delete(&self, channel: &ChannelId, id: Ulid) -> io::Result<bool> {
        let _guard = self.lock.lock().await;
        let entries = entries(&self.channel_dir(channel)).await?;
        match entries.iter().find(|e| e.id == id) {
            Some(entry) => self.remove(entry).await.map(|()| true),
            None => Ok(false),
        }
    }

    pub async fn clear(&self, channel: &ChannelId) -> io::Result<()> {
        let _guard = self.lock.lock().await;
        let dir = self.channel_dir(channel);
        for entry in entries(&dir).await? {
            self.remove(&entry).await?;
        }
        remove_dir_if_empty(&dir).await
    }

    /// Delete every expired clip, every emptied channel dir, and uploads idle
    /// for [`UPLOAD_IDLE_MS`]. Returns the number of clips removed.
    ///
    /// Also re-counts the disk usage from what's left, so the quota heals if
    /// files changed behind the store's back (deleted by hand, or a second
    /// server wrongly pointed at the same data dir).
    pub async fn reap(&self, now_ms: u64) -> io::Result<usize> {
        let _guard = self.lock.lock().await;
        let mut removed = 0;
        let mut kept = 0;
        for dir in channel_dirs(&self.root).await? {
            for entry in entries(&dir).await? {
                if entry.expires_at_ms <= now_ms {
                    self.remove(&entry).await?;
                    removed += 1;
                } else {
                    kept += entry.size;
                }
            }
            remove_dir_if_empty(&dir).await?;
        }

        let (idle, open): (Vec<_>, Vec<_>) = {
            let mut uploads = self.uploads();
            let idle: Vec<Ulid> = uploads
                .iter()
                .filter(|(_, u)| u.touched_ms + UPLOAD_IDLE_MS <= now_ms)
                .map(|(id, _)| *id)
                .collect();
            for id in &idle {
                uploads.remove(id);
            }
            kept += uploads.values().map(|u| u.reserved).sum::<u64>();
            (idle, uploads.keys().copied().collect())
        };
        for id in idle {
            remove_dir_all(&self.upload_dir(id)).await?;
        }
        // Dirs of uploads that failed to clean up after themselves.
        let mut tmp = fs::read_dir(self.root.join(TMP_DIR)).await?;
        while let Some(item) = tmp.next_entry().await? {
            let name = item.file_name();
            let upload = name
                .to_str()
                .and_then(|n| n.strip_suffix(UPLOAD_SUFFIX))
                .and_then(|id| id.parse::<Ulid>().ok());
            if upload.is_some_and(|id| !open.contains(&id)) {
                remove_dir_all(&item.path()).await?;
            }
        }

        self.used.store(kept, Ordering::SeqCst);
        Ok(removed)
    }

    async fn remove(&self, entry: &Entry) -> io::Result<()> {
        let removed = match entry.chunked {
            true => fs::remove_dir_all(&entry.path).await,
            false => fs::remove_file(&entry.path).await,
        };
        match removed {
            Ok(()) => {
                self.release(entry.size);
                Ok(())
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

fn chunk_name(index: u64) -> String {
    format!("{index:08}")
}

async fn remove_dir_all(dir: &Path) -> io::Result<()> {
    match fs::remove_dir_all(dir).await {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

async fn channel_dirs(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    let mut read = fs::read_dir(root).await?;
    while let Some(item) = read.next_entry().await? {
        let name = item.file_name();
        if item.file_type().await?.is_dir() && name != TMP_DIR {
            dirs.push(item.path());
        }
    }
    Ok(dirs)
}

/// All clips in a channel dir, expired or not, newest first. A missing dir is an empty channel.
async fn entries(dir: &Path) -> io::Result<Vec<Entry>> {
    let mut read = match fs::read_dir(dir).await {
        Ok(read) => read,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut entries = Vec::new();
    while let Some(item) = read.next_entry().await? {
        let Some(name) = item.file_name().to_str().and_then(parse_name) else {
            continue;
        };
        let size = match name.size {
            Some(size) => size,
            None => match item.metadata().await {
                Ok(m) => m.len(),
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            },
        };
        entries.push(Entry {
            id: name.id,
            expires_at_ms: name.expires_at_ms,
            size,
            chunked: name.size.is_some(),
            path: item.path(),
        });
    }
    entries.sort_unstable_by_key(|e| std::cmp::Reverse(e.id));
    Ok(entries)
}

async fn live_entries(dir: &Path, now_ms: u64) -> io::Result<Vec<Entry>> {
    let mut entries = entries(dir).await?;
    entries.retain(|e| e.expires_at_ms > now_ms);
    Ok(entries)
}

/// A clip deleted between listing and reading counts as not found.
async fn read_entry(entry: Option<&Entry>) -> io::Result<Option<(ClipMeta, Vec<u8>)>> {
    let Some(entry) = entry else { return Ok(None) };
    let path = match entry.chunked {
        true => entry.path.join(HEADER_FILE),
        false => entry.path.clone(),
    };
    match fs::read(&path).await {
        Ok(bytes) => Ok(Some((entry.meta(), bytes))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Name {
    id: Ulid,
    expires_at_ms: u64,
    /// Chunked clips (dirs) carry their size in the name.
    size: Option<u64>,
}

fn parse_name(name: &str) -> Option<Name> {
    if let Some(rest) = name.strip_suffix(".bin") {
        let (id, expires) = rest.split_once('.')?;
        return Some(Name {
            id: id.parse().ok()?,
            expires_at_ms: expires.parse().ok()?,
            size: None,
        });
    }
    let mut parts = name.strip_suffix(".d")?.split('.');
    let (id, expires, size) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    Some(Name {
        id: id.parse().ok()?,
        expires_at_ms: expires.parse().ok()?,
        size: Some(size.parse().ok()?),
    })
}

async fn remove_dir_if_empty(dir: &Path) -> io::Result<()> {
    let mut read = match fs::read_dir(dir).await {
        Ok(read) => read,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if read.next_entry().await?.is_none() {
        match fs::remove_dir(dir).await {
            Ok(()) => {}
            // Something landed in it meanwhile, or someone else removed it.
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::DirectoryNotEmpty
                ) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_clip_names() {
        let id = Ulid::from_parts(1_000, 42);
        let name = |expires_at_ms, size| {
            Some(Name {
                id,
                expires_at_ms,
                size,
            })
        };
        assert_eq!(parse_name(&format!("{id}.5000.bin")), name(5000, None));
        assert_eq!(parse_name(&format!("{id}.5000.77.d")), name(5000, Some(77)));
        assert_eq!(parse_name(&format!("{id}.tmp")), None);
        assert_eq!(parse_name(&format!("{id}.bin")), None);
        assert_eq!(parse_name(&format!("{id}.5000.d")), None);
        assert_eq!(parse_name(&format!("{id}.5000.7.7.d")), None);
        assert_eq!(parse_name(&format!("{id}.up")), None);
        assert_eq!(parse_name("notaulid.5000.bin"), None);
        assert_eq!(parse_name(".DS_Store"), None);
    }

    #[test]
    fn chunk_layouts() {
        let tag = CHUNK_TAG_LEN as u64;
        let size = u64::from(MIN_CHUNK_SIZE) + tag;
        let layout = ChunkLayout::new(2 * size + 17, size).unwrap();
        assert_eq!(layout.count(), 3);
        assert_eq!(
            (layout.len(0), layout.len(1), layout.len(2)),
            (size, size, 17)
        );
        assert_eq!(ChunkLayout::new(size, size).unwrap().count(), 1);
        // An empty stream: one chunk with just the tag.
        assert_eq!(ChunkLayout::new(tag, size).unwrap().len(0), tag);

        assert!(ChunkLayout::new(tag - 1, size).is_err());
        assert!(ChunkLayout::new(size + tag, size).is_err());
        assert!(ChunkLayout::new(size, size - 1).is_err());
        assert!(ChunkLayout::new(size, u64::from(MAX_CHUNK_SIZE) + tag + 1).is_err());
    }

    /// Two servers on one data dir: each one's count misses the other's files.
    #[tokio::test]
    async fn usage_never_wraps_and_reap_recounts_it() {
        let dir = tempfile::tempdir().unwrap();
        let channel = ChannelId::from_bytes([1; 32]);
        let late = Store::open(dir.path(), 50, 1000).await.unwrap();
        let early = Store::open(dir.path(), 50, 1000).await.unwrap();

        early.put(&channel, &[0; 100], 1_000, 2_000).await.unwrap();
        assert_eq!(late.used_bytes(), 0);
        // `late` removes a file it never counted.
        assert_eq!(late.reap(5_000).await.unwrap(), 1);
        assert_eq!(late.used_bytes(), 0);
        late.put(&channel, &[0; 30], 6_000, 60_000).await.unwrap();
        assert_eq!(late.used_bytes(), 30);

        // `early` still counts its reaped clip and misses `late`'s: 150, but 80 are on disk.
        early.put(&channel, &[0; 50], 7_000, 60_000).await.unwrap();
        assert_eq!(early.used_bytes(), 150);
        early.reap(8_000).await.unwrap();
        assert_eq!(early.used_bytes(), 80);
    }
}
