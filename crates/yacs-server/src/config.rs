use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use bytesize::ByteSize;
use clap::Parser;

/// Every setting can be given as a flag or as the `YACS_*` environment variable.
#[derive(Debug, Clone, Parser)]
#[command(name = "yacs-server", version, about = "Self-hosted YACS relay")]
pub struct Config {
    /// Address to listen on.
    #[arg(long, env = "YACS_BIND", default_value = "0.0.0.0:8080")]
    pub bind: SocketAddr,

    /// Where encrypted clips are stored.
    #[arg(long, env = "YACS_DATA_DIR", default_value = "./data")]
    pub data_dir: PathBuf,

    /// TTL for clips whose sender didn't ask for one.
    #[arg(long, env = "YACS_DEFAULT_TTL", default_value = "15m", value_parser = humantime::parse_duration)]
    pub default_ttl: Duration,

    /// Longest TTL a sender may ask for; longer requests are clamped.
    #[arg(long, env = "YACS_MAX_TTL", default_value = "24h", value_parser = humantime::parse_duration)]
    pub max_ttl: Duration,

    /// Largest clip sent in one piece (encrypted size). Big files go as
    /// chunks, which only the disk quota limits.
    #[arg(long, env = "YACS_MAX_SIZE", default_value = "20MB")]
    pub max_size: ByteSize,

    /// History length per channel; the oldest clip is evicted beyond this.
    #[arg(long, env = "YACS_MAX_CLIPS_PER_CHANNEL", default_value_t = 50)]
    pub max_clips_per_channel: usize,

    /// Total disk space for all channels, uploads in progress included. New
    /// clips are refused when it's full.
    #[arg(long, env = "YACS_MAX_DISK", default_value = "25GB")]
    pub max_disk: ByteSize,

    /// The relay owner's account key. Creating a space needs it (unless the
    /// relay is public); joining one doesn't. Spaces created with it get only
    /// the limits above.
    #[arg(long, env = "YACS_ACCESS_TOKEN", hide_env_values = true)]
    pub access_token: Option<String>,

    /// Let anyone create spaces, on the free plan below. Run it behind a
    /// reverse proxy that sets `X-Forwarded-For` to the client's address.
    #[arg(long, env = "YACS_PUBLIC", value_parser = clap::builder::BoolishValueParser::new())]
    pub public: bool,

    /// Free plan: largest clip, chunks included.
    #[arg(long, env = "YACS_FREE_MAX_SIZE", default_value = "10MB")]
    pub free_max_size: ByteSize,

    /// Free plan: longest TTL.
    #[arg(long, env = "YACS_FREE_MAX_TTL", default_value = "1h", value_parser = humantime::parse_duration)]
    pub free_max_ttl: Duration,

    /// Free plan: bytes a space may upload and download per day.
    #[arg(long, env = "YACS_FREE_DAILY_TRANSFER", default_value = "500MB")]
    pub free_daily_transfer: ByteSize,

    /// Public relay: new spaces one IP address may create per day.
    #[arg(long, env = "YACS_NEW_SPACES_PER_IP", default_value_t = 10)]
    pub new_spaces_per_ip: u32,

    /// Public relay: API requests per minute from one IP address, in bursts
    /// of up to a fifth of it.
    #[arg(long, env = "YACS_REQUESTS_PER_MINUTE", default_value_t = 600)]
    pub requests_per_minute: u32,
}

impl Config {
    /// Reject settings that parse but make no sense together.
    pub fn validate(mut self) -> Result<Self, String> {
        if self.default_ttl.is_zero() || self.max_ttl.is_zero() {
            return Err("TTLs must be greater than zero".into());
        }
        if self.default_ttl > self.max_ttl {
            return Err(format!(
                "default TTL ({}) is longer than max TTL ({})",
                humantime::format_duration(self.default_ttl),
                humantime::format_duration(self.max_ttl)
            ));
        }
        if self.max_clips_per_channel == 0 {
            return Err("max clips per channel must be at least 1".into());
        }
        if self.max_size > self.max_disk {
            return Err("max size is larger than max disk".into());
        }
        if self
            .access_token
            .as_deref()
            .is_some_and(|t| t.trim().is_empty())
        {
            self.access_token = None;
        }
        if self.public {
            if self.free_max_ttl.is_zero() {
                return Err("the free plan's TTL must be greater than zero".into());
            }
            if self.requests_per_minute == 0 {
                return Err("requests per minute must be at least 1".into());
            }
        }
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Config, String> {
        let argv = std::iter::once("yacs-server").chain(args.iter().copied());
        Config::try_parse_from(argv).unwrap().validate()
    }

    #[test]
    fn defaults_match_the_plan() {
        let c = parse(&[]).unwrap();
        assert_eq!(c.default_ttl, Duration::from_secs(15 * 60));
        assert_eq!(c.max_ttl, Duration::from_secs(24 * 3600));
        assert_eq!(c.max_size, ByteSize::mb(20));
        assert_eq!(c.max_clips_per_channel, 50);
        assert_eq!(c.max_disk, ByteSize::gb(25));
        assert_eq!(c.access_token, None);
        assert!(!c.public);
        assert_eq!(c.free_max_size, ByteSize::mb(10));
        assert_eq!(c.free_max_ttl, Duration::from_secs(3600));
        assert_eq!(c.free_daily_transfer, ByteSize::mb(500));
    }

    #[test]
    fn rejects_default_above_max() {
        assert!(parse(&["--default-ttl", "2h", "--max-ttl", "1h"]).is_err());
    }

    #[test]
    fn blank_token_means_no_token() {
        assert_eq!(parse(&["--access-token", "  "]).unwrap().access_token, None);
    }
}
