//! Encrypted clips on disk.
//!
//! ```text
//! {root}/{hex channel id}/{ulid}.{expires_at_ms}.bin
//! {root}/.tmp/                                      in-flight uploads
//! ```
//!
//! Channel dirs use hex rather than base64url so two ids can't collide on a
//! case-insensitive filesystem (macOS, Windows). The expiry lives in the file
//! name so listing and reaping never open a file. Uploads are written to
//! `.tmp` first and renamed into place, so a clip is never seen half-written.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use tokio::fs;
use ulid::{Generator, Ulid};
use yacs_core::ChannelId;
use yacs_core::api::ClipMeta;

const TMP_DIR: &str = ".tmp";

#[derive(Debug, thiserror::Error)]
pub enum PutError {
    #[error("storage quota exhausted")]
    Full,
    #[error(transparent)]
    Io(#[from] io::Error),
}

pub struct Store {
    root: PathBuf,
    max_clips: usize,
    max_disk: u64,
    used: AtomicU64,
    /// Serializes everything that renames or deletes, so eviction and reaping
    /// see a consistent directory. Uploads write their bytes outside of it.
    lock: tokio::sync::Mutex<()>,
    ids: Mutex<Generator>,
}

#[derive(Debug)]
struct Entry {
    id: Ulid,
    expires_at_ms: u64,
    size: u64,
    path: PathBuf,
}

impl Entry {
    fn meta(&self) -> ClipMeta {
        ClipMeta {
            id: self.id.to_string(),
            created_at_ms: self.id.timestamp_ms(),
            expires_at_ms: self.expires_at_ms,
            size: self.size,
        }
    }
}

impl Store {
    /// Open (or create) a store, discarding uploads interrupted by a crash and
    /// counting existing clips against the disk quota.
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
        })
    }

    pub fn used_bytes(&self) -> u64 {
        self.used.load(Ordering::SeqCst)
    }

    fn channel_dir(&self, channel: &ChannelId) -> PathBuf {
        self.root.join(hex::encode(channel.as_bytes()))
    }

    fn next_id(&self, now_ms: u64) -> Ulid {
        let now = UNIX_EPOCH + Duration::from_millis(now_ms);
        let mut ids = self.ids.lock().expect("ulid generator lock poisoned");
        ids.generate_from_datetime(now)
            .unwrap_or_else(|overflow| overflow.commit_overflow_increment())
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
        if self.used.fetch_add(size, Ordering::SeqCst) + size > self.max_disk {
            self.used.fetch_sub(size, Ordering::SeqCst);
            return Err(PutError::Full);
        }

        let id = self.next_id(now_ms);
        let tmp = self.root.join(TMP_DIR).join(format!("{id}.tmp"));
        if let Err(e) = fs::write(&tmp, body).await {
            self.used.fetch_sub(size, Ordering::SeqCst);
            let _ = fs::remove_file(&tmp).await;
            return Err(e.into());
        }

        let _guard = self.lock.lock().await;
        let dir = self.channel_dir(channel);
        let placed = async {
            fs::create_dir_all(&dir).await?;
            fs::rename(&tmp, dir.join(format!("{id}.{expires_at_ms}.bin"))).await
        };
        if let Err(e) = placed.await {
            self.used.fetch_sub(size, Ordering::SeqCst);
            let _ = fs::remove_file(&tmp).await;
            return Err(e.into());
        }

        // Expired clips don't hold a history slot: a short-TTL clip may expire
        // before an older long-TTL one, which must then survive eviction.
        let (live, expired): (Vec<_>, Vec<_>) = entries(&dir)
            .await?
            .into_iter()
            .partition(|e| e.expires_at_ms > now_ms);
        for old in expired.iter().chain(live.iter().skip(self.max_clips)) {
            self.remove(old).await?;
        }

        Ok(ClipMeta {
            id: id.to_string(),
            created_at_ms: id.timestamp_ms(),
            expires_at_ms,
            size,
        })
    }

    /// Unexpired clips, newest first.
    pub async fn list(&self, channel: &ChannelId, now_ms: u64) -> io::Result<Vec<ClipMeta>> {
        Ok(live_entries(&self.channel_dir(channel), now_ms)
            .await?
            .iter()
            .map(Entry::meta)
            .collect())
    }

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

    /// Delete every expired clip and every emptied channel dir. Returns the number of clips removed.
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
        self.used.store(kept, Ordering::SeqCst);
        Ok(removed)
    }

    async fn remove(&self, entry: &Entry) -> io::Result<()> {
        match fs::remove_file(&entry.path).await {
            Ok(()) => {
                // Saturating: a file this store didn't count (see `reap`)
                // must not wrap the counter around to "disk full".
                let _ = self
                    .used
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |used| {
                        Some(used.saturating_sub(entry.size))
                    });
                Ok(())
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
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
        let Some((id, expires_at_ms)) = item.file_name().to_str().and_then(parse_name) else {
            continue;
        };
        let size = match item.metadata().await {
            Ok(m) => m.len(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        entries.push(Entry {
            id,
            expires_at_ms,
            size,
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
    match fs::read(&entry.path).await {
        Ok(bytes) => Ok(Some((entry.meta(), bytes))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn parse_name(name: &str) -> Option<(Ulid, u64)> {
    let (id, expires) = name.strip_suffix(".bin")?.split_once('.')?;
    Some((id.parse().ok()?, expires.parse().ok()?))
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
    fn parses_only_clip_file_names() {
        let id = Ulid::from_parts(1_000, 42);
        assert_eq!(parse_name(&format!("{id}.5000.bin")), Some((id, 5000)));
        assert_eq!(parse_name(&format!("{id}.tmp")), None);
        assert_eq!(parse_name(&format!("{id}.bin")), None);
        assert_eq!(parse_name("notaulid.5000.bin"), None);
        assert_eq!(parse_name(".DS_Store"), None);
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
