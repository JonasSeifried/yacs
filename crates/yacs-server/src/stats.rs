//! Usage totals for the relay's owner (`GET /api/v1/stats`): per minute for
//! the last hour, per hour for two days and per day for 90. Only totals,
//! never per space or address.
//!
//! Hours and days live in `{data dir}/stats.json`, saved with each reap, so a
//! restart loses at most a minute of them. A damaged file starts them over:
//! they aren't worth refusing to start for.

use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::fs;
use yacs_core::api::{Usage, UsageAt};

const FILE: &str = "stats.json";
const MINUTE_MS: u64 = 60 * 1000;
const HOUR_MS: u64 = 60 * MINUTE_MS;
const DAY_MS: u64 = 24 * HOUR_MS;
const MINUTES: u64 = 60;
pub const HOURS: u64 = 48;
pub const DAYS: u64 = 90;

/// Buckets `width` ms long, oldest first, only those something happened in.
#[derive(Default, Serialize, Deserialize)]
struct Series {
    buckets: VecDeque<Bucket>,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
struct Bucket {
    start_ms: u64,
    #[serde(flatten)]
    usage: Usage,
}

impl Series {
    fn record(&mut self, width: u64, keep: u64, now_ms: u64, f: &dyn Fn(&mut Usage)) {
        let start_ms = now_ms - now_ms % width;
        match self.buckets.back_mut() {
            // A clock set back counts in the newest bucket.
            Some(newest) if newest.start_ms >= start_ms => f(&mut newest.usage),
            _ => {
                let mut usage = Usage::default();
                f(&mut usage);
                self.buckets.push_back(Bucket { start_ms, usage });
            }
        }
        let oldest = start_ms.saturating_sub(width * (keep - 1));
        while self.buckets.front().is_some_and(|b| b.start_ms < oldest) {
            self.buckets.pop_front();
        }
    }

    /// Everything in the buckets that start at `from_ms` or later.
    fn since(&self, from_ms: u64) -> Usage {
        let mut sum = Usage::default();
        for b in self.buckets.iter().filter(|b| b.start_ms >= from_ms) {
            sum += b.usage;
        }
        sum
    }

    /// Every bucket from `from_ms` to the one holding `now_ms`, quiet ones as 0.
    fn filled(&self, width: u64, from_ms: u64, now_ms: u64) -> Vec<UsageAt> {
        let (from, to) = (from_ms - from_ms % width, now_ms - now_ms % width);
        (0..=to.saturating_sub(from) / width)
            .map(|i| {
                let start_ms = from + i * width;
                let usage = self
                    .buckets
                    .iter()
                    .find(|b| b.start_ms == start_ms)
                    .map_or_else(Usage::default, |b| b.usage);
                UsageAt {
                    start: humantime::format_rfc3339_seconds(
                        UNIX_EPOCH + Duration::from_millis(start_ms),
                    )
                    .to_string(),
                    start_ms,
                    usage,
                }
            })
            .collect()
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Kept {
    /// Only for the last hour: not saved.
    #[serde(skip)]
    minutes: Series,
    hours: Series,
    days: Series,
    /// When the first thing was recorded: days start from there.
    #[serde(default)]
    since_ms: Option<u64>,
}

/// What [`Stats::report`] gives.
pub struct Recent {
    pub last_hour: Usage,
    pub today: Usage,
    pub hours: Vec<UsageAt>,
    pub days: Vec<UsageAt>,
}

pub struct Stats {
    path: PathBuf,
    started_ms: u64,
    series: Mutex<Kept>,
    /// Recorded since the last save.
    dirty: AtomicBool,
    /// Serializes saves, so an older snapshot never replaces a newer one.
    saving: tokio::sync::Mutex<()>,
}

impl Stats {
    pub async fn open(data_dir: &Path, now_ms: u64) -> io::Result<Self> {
        let path = data_dir.join(FILE);
        let series = match fs::read(&path).await {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "{} is damaged; the stats start over", path.display());
                Kept::default()
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Kept::default(),
            Err(e) => return Err(e),
        };
        Ok(Self {
            path,
            started_ms: now_ms,
            series: Mutex::new(series),
            dirty: AtomicBool::new(false),
            saving: tokio::sync::Mutex::new(()),
        })
    }

    /// Adds to the minute, hour and day of `now_ms`.
    pub fn record(&self, now_ms: u64, f: impl Fn(&mut Usage)) {
        let mut s = self.lock();
        s.since_ms.get_or_insert(now_ms);
        s.minutes.record(MINUTE_MS, MINUTES, now_ms, &f);
        s.hours.record(HOUR_MS, HOURS, now_ms, &f);
        s.days.record(DAY_MS, DAYS, now_ms, &f);
        self.dirty.store(true, Ordering::Relaxed);
    }

    pub fn report(&self, now_ms: u64) -> Recent {
        let s = self.lock();
        let minute = now_ms - now_ms % MINUTE_MS;
        let today = now_ms - now_ms % DAY_MS;
        let since = s.since_ms.unwrap_or(now_ms);
        Recent {
            last_hour: s
                .minutes
                .since(minute.saturating_sub((MINUTES - 1) * MINUTE_MS)),
            today: s.days.since(today),
            hours: s.hours.filled(
                HOUR_MS,
                now_ms.saturating_sub((HOURS - 1) * HOUR_MS),
                now_ms,
            ),
            days: s.days.filled(
                DAY_MS,
                since
                    .max(now_ms.saturating_sub((DAYS - 1) * DAY_MS))
                    .min(today),
                now_ms,
            ),
        }
    }

    pub fn uptime_secs(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.started_ms) / 1000
    }

    /// Writes the hours and days if anything was recorded since the last save.
    pub async fn save(&self) -> io::Result<()> {
        let _saving = self.saving.lock().await;
        if !self.dirty.swap(false, Ordering::Relaxed) {
            return Ok(());
        }
        let json = serde_json::to_vec(&*self.lock()).expect("stats serialize");
        let tmp = self.path.with_extension("json.tmp");
        let written = async {
            fs::write(&tmp, &json).await?;
            fs::rename(&tmp, &self.path).await
        };
        if let Err(e) = written.await {
            self.dirty.store(true, Ordering::Relaxed);
            return Err(e);
        }
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Kept> {
        self.series.lock().expect("stats lock poisoned")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const START_MS: u64 = 1_758_600_000_000;

    async fn stats(dir: &Path, now_ms: u64) -> Stats {
        Stats::open(dir, now_ms).await.unwrap()
    }

    #[tokio::test]
    async fn sums_the_last_hour_and_today() {
        let dir = tempfile::TempDir::new().unwrap();
        let s = stats(dir.path(), START_MS).await;
        let midnight = START_MS - START_MS % DAY_MS;
        s.record(midnight + 1000, |u| u.requests += 1);
        s.record(midnight + 2 * HOUR_MS, |u| u.requests += 10);
        s.record(midnight + 3 * HOUR_MS - MINUTE_MS, |u| u.bytes_in += 5);
        let now = midnight + 3 * HOUR_MS;
        let r = s.report(now);
        assert_eq!(r.today.requests, 11);
        assert_eq!(r.last_hour.requests, 0);
        assert_eq!(r.last_hour.bytes_in, 5);
        assert_eq!(r.hours.len() as u64, HOURS);
        assert_eq!(r.hours.last().unwrap().start_ms, now);
        assert_eq!(r.hours[HOURS as usize - 2].usage.bytes_in, 5);
        assert_eq!(r.hours[HOURS as usize - 3].usage.requests, 0);
        // Only from the first day anything happened.
        assert_eq!(r.days.len(), 1);
        assert_eq!(r.days[0].start_ms, midnight);
        assert!(
            r.days[0].start.ends_with("T00:00:00Z"),
            "{}",
            r.days[0].start
        );
    }

    #[tokio::test]
    async fn fills_quiet_days_and_forgets_old_ones() {
        let dir = tempfile::TempDir::new().unwrap();
        let s = stats(dir.path(), START_MS).await;
        s.record(START_MS, |u| u.clips += 1);
        let later = START_MS + 3 * DAY_MS;
        s.record(later, |u| u.clips += 2);
        let r = s.report(later);
        let clips: Vec<u64> = r.days.iter().map(|d| d.usage.clips).collect();
        assert_eq!(clips, [1, 0, 0, 2]);
        assert_eq!(r.today.clips, 2);

        let much_later = START_MS + (DAYS + 10) * DAY_MS;
        s.record(much_later, |u| u.clips += 3);
        let r = s.report(much_later);
        assert_eq!(r.days.len() as u64, DAYS);
        assert_eq!(r.days.iter().map(|d| d.usage.clips).sum::<u64>(), 3);
    }

    #[tokio::test]
    async fn keeps_hours_and_days_across_restarts() {
        let dir = tempfile::TempDir::new().unwrap();
        let s = stats(dir.path(), START_MS).await;
        s.record(START_MS, |u| u.new_spaces += 4);
        s.save().await.unwrap();
        let s = stats(dir.path(), START_MS + MINUTE_MS).await;
        let r = s.report(START_MS + MINUTE_MS);
        assert_eq!(r.today.new_spaces, 4);
        assert_eq!(r.hours.last().unwrap().usage.new_spaces, 4);
        // The minutes aren't saved.
        assert_eq!(r.last_hour.new_spaces, 0);
        assert_eq!(s.uptime_secs(START_MS + 2 * MINUTE_MS), 60);
    }

    #[tokio::test]
    async fn starts_over_from_a_damaged_file() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join(FILE), b"{\"hours\": [").unwrap();
        let s = stats(dir.path(), START_MS).await;
        assert_eq!(s.report(START_MS).today, Usage::default());
    }
}
