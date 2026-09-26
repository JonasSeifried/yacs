//! Which spaces the relay knows, and on which plan.
//!
//! A space (channel) is registered by its first request: with the relay's
//! account key it gets the owner's account, and on a public relay anyone
//! may register one on the free plan. After that its members need no key.
//! Registrations live in `{data dir}/accounts.json`, since joiners depend on
//! them; the per-IP and per-day counters are in memory only.
//!
//! Free spaces unused for [`FORGET_FREE_AFTER_DAYS`] are forgotten: their
//! clips are long gone, and their next request just registers them again.

use std::collections::HashMap;
use std::io;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio::fs;
use yacs_core::ChannelId;
use yacs_core::api::{Plan, SpaceLimits};

use crate::config::Config;

const FILE: &str = "accounts.json";
pub const FORGET_FREE_AFTER_DAYS: u64 = 30;
const DAY_MS: u64 = 24 * 3600 * 1000;

/// Whose space it is. The owner holds the relay's account key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Account {
    Owner,
    Free,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Registration {
    account: Account,
    /// Days since 1970 (UTC) the space was last used.
    used_day: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    /// By hex channel id.
    #[serde(default)]
    spaces: HashMap<String, Registration>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// No key, or the wrong one.
    Unauthorized,
    /// This address created its share of spaces today.
    TooManySpaces,
}

/// A space's limits, resolved for one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub plan: Plan,
    pub default_ttl_ms: u64,
    pub max_ttl_ms: u64,
    /// Chunks included; `None` means only the disk quota.
    pub max_clip_bytes: Option<u64>,
    pub daily_transfer: Option<u64>,
    pub max_clips: usize,
}

impl Limits {
    pub fn unlimited(config: &Config) -> Self {
        Self {
            plan: Plan::Unlimited,
            default_ttl_ms: config.default_ttl.as_millis() as u64,
            max_ttl_ms: config.max_ttl.as_millis() as u64,
            max_clip_bytes: None,
            daily_transfer: None,
            max_clips: config.max_clips_per_channel,
        }
    }

    pub fn free(config: &Config) -> Self {
        let max_ttl = config.max_ttl.min(config.free_max_ttl);
        Self {
            plan: Plan::Free,
            default_ttl_ms: config.default_ttl.min(max_ttl).as_millis() as u64,
            max_ttl_ms: max_ttl.as_millis() as u64,
            max_clip_bytes: Some(config.free_max_size.as_u64()),
            daily_transfer: Some(config.free_daily_transfer.as_u64()),
            max_clips: config.max_clips_per_channel,
        }
    }

    pub fn of(account: Account, config: &Config) -> Self {
        match account {
            Account::Owner => Self::unlimited(config),
            Account::Free => Self::free(config),
        }
    }

    pub fn report(&self, transfer_used_bytes: u64) -> SpaceLimits {
        SpaceLimits {
            plan: self.plan,
            default_ttl_secs: self.default_ttl_ms / 1000,
            max_ttl_secs: self.max_ttl_ms / 1000,
            max_clip_bytes: self.max_clip_bytes,
            daily_transfer_bytes: self.daily_transfer,
            transfer_used_bytes: match self.daily_transfer {
                Some(_) => transfer_used_bytes,
                None => 0,
            },
            max_clips: self.max_clips,
        }
    }
}

/// A count that starts over each day.
#[derive(Clone, Copy, Default)]
struct Daily {
    day: u64,
    count: u64,
}

impl Daily {
    fn get(&self, today: u64) -> u64 {
        if self.day == today { self.count } else { 0 }
    }

    fn set(&mut self, today: u64, count: u64) {
        *self = Self { day: today, count };
    }
}

pub struct Accounts {
    path: PathBuf,
    spaces: Mutex<HashMap<ChannelId, Registration>>,
    /// Used days changed since the last save.
    dirty: AtomicBool,
    /// Serializes saves, so an older snapshot never replaces a newer one.
    saving: tokio::sync::Mutex<()>,
    new_spaces: Mutex<HashMap<IpAddr, Daily>>,
    transfer: Mutex<HashMap<ChannelId, Daily>>,
}

pub fn day(now_ms: u64) -> u64 {
    now_ms / DAY_MS
}

impl Accounts {
    pub async fn open(data_dir: &Path) -> io::Result<Self> {
        let path = data_dir.join(FILE);
        let file: File = match fs::read(&path).await {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} is damaged: {e}", path.display()),
                )
            })?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => File::default(),
            Err(e) => return Err(e),
        };
        let spaces = file
            .spaces
            .into_iter()
            .filter_map(|(hex, r)| {
                let bytes: [u8; 32] = hex::decode(hex).ok()?.try_into().ok()?;
                Some((ChannelId::from_bytes(bytes), r))
            })
            .collect();
        Ok(Self {
            path,
            spaces: Mutex::new(spaces),
            dirty: AtomicBool::new(false),
            saving: tokio::sync::Mutex::new(()),
            new_spaces: Mutex::new(HashMap::new()),
            transfer: Mutex::new(HashMap::new()),
        })
    }

    /// The account of `channel` for a request carrying `key`, from `ip`.
    /// Registers the space if it's new and may be. `Ok(None)`: a relay
    /// without accounts, where every space is unlimited.
    pub async fn authorize(
        &self,
        config: &Config,
        channel: &ChannelId,
        key: Option<&str>,
        ip: IpAddr,
        now_ms: u64,
    ) -> Result<Option<Account>, Refusal> {
        let today = day(now_ms);
        let owner = match (key, &config.access_token) {
            (Some(given), Some(expected)) => {
                match bool::from(given.as_bytes().ct_eq(expected.as_bytes())) {
                    true => true,
                    false => return Err(Refusal::Unauthorized),
                }
            }
            // Nothing to check a key against: an open relay, or a public one
            // without an owner. Ignore it, as relays before 0.5 did.
            (Some(_), None) | (None, _) => false,
        };
        if !config.public && config.access_token.is_none() {
            return Ok(None);
        }

        let registered = {
            let mut spaces = self.spaces();
            match spaces.get_mut(channel) {
                Some(r) if owner && r.account != Account::Owner => None,
                Some(r) => {
                    if r.used_day != today {
                        r.used_day = today;
                        self.dirty.store(true, Ordering::SeqCst);
                    }
                    Some(r.account)
                }
                None => None,
            }
        };
        if let Some(account) = registered {
            return Ok(Some(account));
        }

        let account = match (owner, config.public) {
            (true, _) => Account::Owner,
            (false, true) => {
                let mut counts = self.new_spaces.lock().expect("new spaces lock poisoned");
                let count = counts.entry(ip).or_default();
                let n = count.get(today);
                if n >= u64::from(config.new_spaces_per_ip) {
                    return Err(Refusal::TooManySpaces);
                }
                count.set(today, n + 1);
                Account::Free
            }
            (false, false) => return Err(Refusal::Unauthorized),
        };
        self.spaces().insert(
            *channel,
            Registration {
                account,
                used_day: today,
            },
        );
        if let Err(e) = self.save().await {
            // Still registered in memory; the next save writes it.
            self.dirty.store(true, Ordering::SeqCst);
            tracing::error!(error = %e, "can't save the registered spaces");
        }
        Ok(Some(account))
    }

    /// Counts `bytes` against the space's daily transfer, unless they'd go
    /// over `limit`.
    pub fn charge(&self, channel: &ChannelId, bytes: u64, limit: Option<u64>, now_ms: u64) -> bool {
        let Some(limit) = limit else { return true };
        let today = day(now_ms);
        let mut transfer = self.transfer.lock().expect("transfer lock poisoned");
        let used = transfer.entry(*channel).or_default();
        let total = used.get(today).saturating_add(bytes);
        if total > limit {
            return false;
        }
        used.set(today, total);
        true
    }

    /// Bytes the space moved today.
    pub fn transferred(&self, channel: &ChannelId, now_ms: u64) -> u64 {
        let transfer = self.transfer.lock().expect("transfer lock poisoned");
        transfer.get(channel).map_or(0, |d| d.get(day(now_ms)))
    }

    /// Drops yesterday's counters and long-unused free spaces, and saves
    /// what changed.
    pub async fn prune(&self, now_ms: u64) -> io::Result<()> {
        let today = day(now_ms);
        self.new_spaces
            .lock()
            .expect("new spaces lock poisoned")
            .retain(|_, d| d.day == today);
        self.transfer
            .lock()
            .expect("transfer lock poisoned")
            .retain(|_, d| d.day == today);
        {
            let mut spaces = self.spaces();
            let before = spaces.len();
            spaces.retain(|_, r| {
                r.account != Account::Free || r.used_day + FORGET_FREE_AFTER_DAYS > today
            });
            if spaces.len() != before {
                self.dirty.store(true, Ordering::SeqCst);
            }
        }
        if self.dirty.load(Ordering::SeqCst) {
            self.save().await?;
        }
        Ok(())
    }

    /// Writes all registrations, replacing the file in one step.
    pub async fn save(&self) -> io::Result<()> {
        let _saving = self.saving.lock().await;
        self.dirty.store(false, Ordering::SeqCst);
        let file = File {
            spaces: self
                .spaces()
                .iter()
                .map(|(c, r)| (hex::encode(c.as_bytes()), r.clone()))
                .collect(),
        };
        let json = serde_json::to_vec(&file).expect("registrations serialize");
        let tmp = self.path.with_extension("json.tmp");
        let written = async {
            fs::write(&tmp, json).await?;
            fs::rename(&tmp, &self.path).await
        };
        if let Err(e) = written.await {
            self.dirty.store(true, Ordering::SeqCst);
            return Err(e);
        }
        Ok(())
    }

    pub fn account(&self, channel: &ChannelId) -> Option<Account> {
        self.spaces().get(channel).map(|r| r.account)
    }

    fn spaces(&self) -> std::sync::MutexGuard<'_, HashMap<ChannelId, Registration>> {
        self.spaces.lock().expect("spaces lock poisoned")
    }
}
