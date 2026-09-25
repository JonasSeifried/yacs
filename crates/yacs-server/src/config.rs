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

    /// If set, clients must send `Authorization: Bearer <token>`.
    #[arg(long, env = "YACS_ACCESS_TOKEN", hide_env_values = true)]
    pub access_token: Option<String>,
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
