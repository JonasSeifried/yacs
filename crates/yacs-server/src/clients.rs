//! Who's asking, for the public relay's per-address limits. Addresses are
//! only kept in memory, as counters, and never logged.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::Mutex;

use axum::extract::{ConnectInfo, Request};

/// The address limits apply to. Behind a reverse proxy (a peer on loopback
/// or a private network, e.g. Docker's) that's the last `X-Forwarded-For`
/// entry, the one the proxy added; otherwise the peer itself, whose header
/// could be made up. IPv6 addresses count per /64, what one line usually gets.
pub fn client_ip(req: &Request) -> IpAddr {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip());
    let forwarded = || {
        let header = req.headers().get("x-forwarded-for")?.to_str().ok()?;
        header.rsplit(',').next()?.trim().parse::<IpAddr>().ok()
    };
    let ip = match peer {
        Some(peer) if !is_proxy(peer) => peer,
        // No peer only in tests, which don't connect.
        _ => forwarded().or(peer).unwrap_or(IpAddr::from([0, 0, 0, 0])),
    };
    per_line(ip)
}

fn is_proxy(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local() || v6.is_unicast_link_local(),
    }
}

fn per_line(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V4(v4) => IpAddr::V4(v4),
        IpAddr::V6(v6) => {
            let bits = u128::from(v6) & !((1u128 << 64) - 1);
            IpAddr::V6(Ipv6Addr::from(bits))
        }
    }
}

/// A token bucket per address: `per_minute` requests a minute, in bursts of
/// up to a fifth of that.
pub struct RateLimiter {
    per_ms: f64,
    burst: f64,
    buckets: Mutex<HashMap<IpAddr, Bucket>>,
}

struct Bucket {
    tokens: f64,
    at_ms: u64,
}

impl RateLimiter {
    pub fn new(per_minute: u32) -> Self {
        Self {
            per_ms: f64::from(per_minute) / 60_000.0,
            burst: (f64::from(per_minute) / 5.0).max(1.0),
            buckets: Mutex::new(HashMap::new()),
        }
    }

    pub fn allow(&self, ip: IpAddr, now_ms: u64) -> bool {
        let mut buckets = self.buckets.lock().expect("rate limiter lock poisoned");
        let bucket = buckets.entry(ip).or_insert(Bucket {
            tokens: self.burst,
            at_ms: now_ms,
        });
        bucket.tokens = self.refilled(bucket, now_ms);
        bucket.at_ms = now_ms;
        if bucket.tokens < 1.0 {
            return false;
        }
        bucket.tokens -= 1.0;
        true
    }

    /// Forgets addresses whose bucket is full again: they'd start full anyway.
    pub fn prune(&self, now_ms: u64) {
        let mut buckets = self.buckets.lock().expect("rate limiter lock poisoned");
        buckets.retain(|_, b| self.refilled(b, now_ms) < self.burst);
    }

    fn refilled(&self, bucket: &Bucket, now_ms: u64) -> f64 {
        let elapsed = now_ms.saturating_sub(bucket.at_ms) as f64;
        (bucket.tokens + elapsed * self.per_ms).min(self.burst)
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;

    use super::*;

    fn request(peer: Option<&str>, forwarded: Option<&str>) -> Request {
        let mut req = Request::new(Body::empty());
        if let Some(peer) = peer {
            req.extensions_mut()
                .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
        }
        if let Some(forwarded) = forwarded {
            req.headers_mut()
                .insert("x-forwarded-for", forwarded.parse().unwrap());
        }
        req
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn trusts_the_header_only_from_a_proxy() {
        let proxied = request(Some("127.0.0.1:5000"), Some("6.6.6.6, 203.0.113.9"));
        assert_eq!(client_ip(&proxied), ip("203.0.113.9"));
        let docker = request(Some("172.18.0.2:5000"), Some("203.0.113.9"));
        assert_eq!(client_ip(&docker), ip("203.0.113.9"));
        let direct = request(Some("198.51.100.7:5000"), Some("203.0.113.9"));
        assert_eq!(client_ip(&direct), ip("198.51.100.7"));
        let no_header = request(Some("127.0.0.1:5000"), None);
        assert_eq!(client_ip(&no_header), ip("127.0.0.1"));
        let mapped = request(Some("[::ffff:198.51.100.7]:5000"), None);
        assert_eq!(client_ip(&mapped), ip("198.51.100.7"));
    }

    #[test]
    fn ipv6_counts_per_64() {
        let a = request(Some("[2001:db8:1:2:aaaa::1]:5000"), None);
        let b = request(Some("[::1]:5000"), Some("2001:db8:1:2:bbbb::9"));
        assert_eq!(client_ip(&a), ip("2001:db8:1:2::"));
        assert_eq!(client_ip(&a), client_ip(&b));
    }

    #[test]
    fn bursts_then_refills() {
        let limiter = RateLimiter::new(60); // one a second, bursts of 12
        let a = ip("203.0.113.1");
        assert!((0..12).all(|_| limiter.allow(a, 0)));
        assert!(!limiter.allow(a, 0));
        assert!(limiter.allow(ip("203.0.113.2"), 0));
        assert!(limiter.allow(a, 1000));
        assert!(!limiter.allow(a, 1000));

        // The second address is full again by now, so it's forgotten.
        limiter.prune(1000);
        assert_eq!(limiter.buckets.lock().unwrap().len(), 1);
        limiter.prune(60_000);
        assert!(limiter.buckets.lock().unwrap().is_empty());
    }
}
