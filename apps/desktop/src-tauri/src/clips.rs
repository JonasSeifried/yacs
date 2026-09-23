//! Decrypted clips and what Spotlight gets to see of them.
//!
//! Clips never change once uploaded, so they're cached in memory by ID and
//! only fetched once. Entries go when the relay stops listing them (expired
//! or deleted), and the oldest go first once the cache is full.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use yacs_client::Client;
use yacs_core::api::ClipMeta;
use yacs_core::{Clip, ClipItem, Payload};

/// Enough for a few dozen screenshots.
pub const CACHE_BYTES: u64 = 256 * 1024 * 1024;
/// Text beyond this isn't needed for a preview.
const PREVIEW_TEXT_CHARS: usize = 20_000;
/// Rendering huge HTML in the preview is slow; plain text is shown instead.
const PREVIEW_HTML_BYTES: usize = 512 * 1024;

pub struct Entry {
    pub meta: ClipMeta,
    pub clip: Clip,
}

impl Entry {
    pub fn image(&self) -> Option<&yacs_core::Image> {
        self.clip.items.iter().find_map(|item| match item {
            ClipItem::Image(image) => Some(image),
            _ => None,
        })
    }
}

pub struct ClipCache {
    entries: HashMap<String, Arc<Entry>>,
    /// Insertion order, oldest first.
    order: VecDeque<String>,
    bytes: u64,
    limit: u64,
}

impl ClipCache {
    pub fn new(limit: u64) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            bytes: 0,
            limit,
        }
    }

    pub fn get(&self, id: &str) -> Option<Arc<Entry>> {
        self.entries.get(id).cloned()
    }

    /// Always keeps the new entry, even if it alone exceeds the limit.
    pub fn insert(&mut self, entry: Entry) -> Arc<Entry> {
        let id = entry.meta.id.clone();
        self.remove(&id);
        let entry = Arc::new(entry);
        self.bytes += entry.meta.size;
        self.entries.insert(id.clone(), entry.clone());
        self.order.push_back(id);
        while self.bytes > self.limit && self.order.len() > 1 {
            let oldest = self.order[0].clone();
            self.remove(&oldest);
        }
        entry
    }

    pub fn remove(&mut self, id: &str) {
        if let Some(entry) = self.entries.remove(id) {
            self.bytes -= entry.meta.size;
            self.order.retain(|o| o != id);
        }
    }

    /// Drop everything the relay no longer lists.
    pub fn retain_listed(&mut self, listed: &[ClipMeta]) {
        let listed: HashSet<&str> = listed.iter().map(|m| m.id.as_str()).collect();
        let gone: Vec<String> = self
            .order
            .iter()
            .filter(|id| !listed.contains(id.as_str()))
            .cloned()
            .collect();
        for id in gone {
            self.remove(&id);
        }
    }

    pub fn clear(&mut self) {
        *self = Self::new(self.limit);
    }
}

/// From the cache, or fetched and decrypted. `None` if the relay doesn't
/// have the clip (anymore).
pub async fn load(
    client: &Client,
    cache: &Mutex<ClipCache>,
    id: &str,
) -> Result<Option<Arc<Entry>>, String> {
    if let Some(entry) = lock(cache).get(id) {
        return Ok(Some(entry));
    }
    let Some((meta, payload)) = client.get(id).await.map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    let Payload::Clip(clip) = payload;
    Ok(Some(lock(cache).insert(Entry { meta, clip })))
}

/// Encrypt and upload the items; the sent clip goes straight into the cache.
pub async fn send(
    client: &Client,
    cache: &Mutex<ClipCache>,
    device_name: String,
    items: Vec<ClipItem>,
    ttl: Duration,
) -> Result<Arc<Entry>, String> {
    let created_at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    let payload = Payload::Clip(Clip {
        created_at_ms,
        device_name,
        items,
    });
    let meta = client
        .push(&payload, Some(ttl))
        .await
        .map_err(|e| e.to_string())?;
    let Payload::Clip(clip) = payload;
    Ok(lock(cache).insert(Entry { meta, clip }))
}

fn lock(cache: &Mutex<ClipCache>) -> std::sync::MutexGuard<'_, ClipCache> {
    cache.lock().expect("clip cache lock poisoned")
}

/// A decrypted clip as Spotlight shows it: capped text for the preview and
/// image facts. Image bytes are fetched separately, as binary.
#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ClipView {
    pub meta: ClipMeta,
    pub device_name: String,
    pub text: Option<String>,
    /// `text` was cut to `PREVIEW_TEXT_CHARS`.
    pub text_truncated: bool,
    /// `None` when the clip has no HTML or it's too large to preview.
    pub html: Option<String>,
    pub rtf: bool,
    pub image: Option<ImageView>,
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ImageView {
    pub mime: String,
    pub size: usize,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

impl From<&Entry> for ClipView {
    fn from(entry: &Entry) -> Self {
        let mut view = ClipView {
            meta: entry.meta.clone(),
            device_name: entry.clip.device_name.clone(),
            text: None,
            text_truncated: false,
            html: None,
            rtf: false,
            image: None,
        };
        for item in &entry.clip.items {
            match item {
                ClipItem::Text(text) if view.text.is_none() => {
                    let cut = text.char_indices().nth(PREVIEW_TEXT_CHARS);
                    view.text_truncated = cut.is_some();
                    view.text = Some(match cut {
                        Some((at, _)) => text[..at].to_owned(),
                        None => text.clone(),
                    });
                }
                ClipItem::Html(html) if view.html.is_none() && html.len() <= PREVIEW_HTML_BYTES => {
                    view.html = Some(html.clone());
                }
                ClipItem::Rtf(_) => view.rtf = true,
                ClipItem::Image(image) if view.image.is_none() => {
                    let size = image::ImageReader::new(std::io::Cursor::new(&image.data))
                        .with_guessed_format()
                        .ok()
                        .and_then(|reader| reader.into_dimensions().ok());
                    view.image = Some(ImageView {
                        mime: image.mime.clone(),
                        size: image.data.len(),
                        width: size.map(|s| s.0),
                        height: size.map(|s| s.1),
                    });
                }
                _ => {}
            }
        }
        view
    }
}

#[cfg(test)]
mod tests {
    use yacs_core::{Image, Pairing};

    use super::*;
    use crate::test_support::{PHRASE, relay};

    fn entry(id: &str, size: u64, items: Vec<ClipItem>) -> Entry {
        Entry {
            meta: ClipMeta {
                id: id.into(),
                created_at_ms: 1,
                expires_at_ms: 2,
                size,
            },
            clip: Clip {
                created_at_ms: 1,
                device_name: "MacBook".into(),
                items,
            },
        }
    }

    #[test]
    fn cache_evicts_oldest_first_but_keeps_the_newest() {
        let mut cache = ClipCache::new(100);
        cache.insert(entry("a", 40, vec![]));
        cache.insert(entry("b", 40, vec![]));
        cache.insert(entry("c", 40, vec![]));
        assert!(cache.get("a").is_none());
        assert!(cache.get("b").is_some() && cache.get("c").is_some());

        cache.insert(entry("huge", 500, vec![]));
        assert!(cache.get("huge").is_some());
        assert!(cache.get("b").is_none() && cache.get("c").is_none());
        assert_eq!(cache.bytes, 500);
    }

    #[test]
    fn cache_drops_what_the_relay_no_longer_lists() {
        let mut cache = ClipCache::new(1000);
        for id in ["a", "b", "c"] {
            cache.insert(entry(id, 10, vec![]));
        }
        cache.retain_listed(&[entry("b", 10, vec![]).meta]);
        assert!(cache.get("a").is_none() && cache.get("c").is_none());
        assert!(cache.get("b").is_some());
        assert_eq!(cache.bytes, 10);
        cache.clear();
        assert!(cache.get("b").is_none());
        assert_eq!(cache.bytes, 0);
    }

    #[test]
    fn view_caps_text_and_skips_huge_html() {
        let long = "é".repeat(PREVIEW_TEXT_CHARS + 5);
        let huge_html = "x".repeat(PREVIEW_HTML_BYTES + 1);
        let view = ClipView::from(&entry(
            "a",
            1,
            vec![
                ClipItem::Text(long),
                ClipItem::Html(huge_html),
                ClipItem::Rtf("{\\rtf1}".into()),
            ],
        ));
        assert_eq!(view.text.unwrap().chars().count(), PREVIEW_TEXT_CHARS);
        assert!(view.text_truncated);
        assert_eq!(view.html, None);
        assert!(view.rtf);
        assert_eq!(view.image, None);
        assert_eq!(view.device_name, "MacBook");
    }

    #[test]
    fn view_reads_image_dimensions() {
        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbImage::new(64, 48)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let data = png.into_inner();
        let len = data.len();
        let view = ClipView::from(&entry(
            "a",
            1,
            vec![ClipItem::Image(Image {
                mime: "image/png".into(),
                data,
            })],
        ));
        let image = view.image.unwrap();
        assert_eq!((image.width, image.height), (Some(64), Some(48)));
        assert_eq!(image.size, len);
        assert_eq!(view.text, None);
    }

    #[tokio::test]
    async fn sends_then_loads_from_cache_or_relay() {
        let (url, _data) = relay(&[]).await;
        let client = Client::new(&url, None, Pairing::from_phrase(PHRASE).unwrap()).unwrap();

        let cache = Mutex::new(ClipCache::new(CACHE_BYTES));
        let items = vec![ClipItem::Text("hi".into())];
        let sent = send(
            &client,
            &cache,
            "PC".into(),
            items.clone(),
            Duration::from_secs(60),
        )
        .await
        .unwrap();
        assert_eq!(sent.clip.items, items);
        assert!(sent.meta.expires_at_ms - sent.meta.created_at_ms <= 60_000);

        // A fresh cache, as on another device: fetched and decrypted.
        let other = Mutex::new(ClipCache::new(CACHE_BYTES));
        let loaded = load(&client, &other, &sent.meta.id).await.unwrap().unwrap();
        assert_eq!(loaded.clip.device_name, "PC");
        assert_eq!(loaded.meta, sent.meta);
        assert!(lock(&other).get(&sent.meta.id).is_some());

        client.delete(&sent.meta.id).await.unwrap();
        // Still cached here until the next listing drops it.
        assert!(
            load(&client, &cache, &sent.meta.id)
                .await
                .unwrap()
                .is_some()
        );
        let fresh = Mutex::new(ClipCache::new(CACHE_BYTES));
        assert!(
            load(&client, &fresh, &sent.meta.id)
                .await
                .unwrap()
                .is_none()
        );
    }
}
