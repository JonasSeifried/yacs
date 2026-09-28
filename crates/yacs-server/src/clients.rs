//! Who's asking, for the public relay's per-address limits. Addresses are
//! only kept in memory, as counters, and never logged.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{ConnectInfo, Request};

/// An IPv6 /48 gets this many times an address's share of each limit: one
/// customer (or one tunnel broker's user) usually holds a whole /48, and
/// could otherwise count as 65,536 addresses.
pub const SITE_SHARE: u32 = 8;

/// Where a request comes from. IPv6 addresses count per /64, what one line
/// usually gets, and again per /48 (see [`SITE_SHARE`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Client {
    line: IpAddr,
    site: Option<Ipv6Addr>,
}

/// What a limit counts a client under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Key {
    Line(IpAddr),
    Site(Ipv6Addr),
}

impl Client {
    /// The client of `req`. Behind a reverse proxy (a peer on loopback or a
    /// private network, e.g. Docker's) that's the last `X-Forwarded-For`
    /// entry, the one the proxy added; otherwise the peer itself, whose
    /// header could be made up.
    pub fn of(req: &Request) -> Self {
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
        Self::from_ip(ip)
    }

    pub const fn from_v4(octets: [u8; 4]) -> Self {
        Self {
            line: IpAddr::V4(Ipv4Addr::new(octets[0], octets[1], octets[2], octets[3])),
            site: None,
        }
    }

    pub fn from_ip(ip: IpAddr) -> Self {
        match ip.to_canonical() {
            IpAddr::V4(v4) => Self {
                line: IpAddr::V4(v4),
                site: None,
            },
            IpAddr::V6(v6) => Self {
                line: IpAddr::V6(prefix(v6, 64)),
                site: Some(prefix(v6, 48)),
            },
        }
    }

    /// The address as the limits count it, e.g. `203.0.113.9` or `2001:db8:1:2::/64`.
    pub fn display(&self) -> String {
        match self.line {
            IpAddr::V4(v4) => v4.to_string(),
            IpAddr::V6(v6) => format!("{v6}/64"),
        }
    }

    /// Whether this is a proxy's address rather than a client's: the proxy
    /// didn't say whom it forwards for.
    pub fn is_proxy(&self) -> bool {
        is_proxy(self.line)
    }

    /// Each key with its share of a limit.
    fn keys(&self) -> impl Iterator<Item = (Key, u32)> + use<> {
        let site = self.site.map(|site| (Key::Site(site), SITE_SHARE));
        std::iter::once((Key::Line(self.line), 1)).chain(site)
    }
}

fn prefix(v6: Ipv6Addr, bits: u32) -> Ipv6Addr {
    Ipv6Addr::from(u128::from(v6) & !(u128::MAX >> bits))
}

fn is_proxy(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local() || v6.is_unicast_link_local(),
    }
}

/// Whether `client` has fewer than `limit` of `others` (its share of it, per key).
pub fn below(client: &Client, others: impl Iterator<Item = Client>, limit: usize) -> bool {
    let mut counts: HashMap<Key, usize> = HashMap::new();
    let mine: Vec<(Key, u32)> = client.keys().collect();
    for other in others {
        for (key, _) in other.keys() {
            if mine.iter().any(|(k, _)| *k == key) {
                *counts.entry(key).or_default() += 1;
            }
        }
    }
    mine.iter().all(|(key, share)| {
        counts.get(key).copied().unwrap_or(0) < limit.saturating_mul(*share as usize)
    })
}

/// A token bucket per client: `count` tokens per `per_ms`, in bursts of up
/// to `burst`.
pub struct RateLimiter {
    per_ms: f64,
    burst: f64,
    buckets: Mutex<HashMap<Key, Bucket>>,
}

struct Bucket {
    tokens: f64,
    at_ms: u64,
}

impl RateLimiter {
    /// `per_minute` requests a minute, in bursts of up to a fifth of that.
    pub fn new(per_minute: u32) -> Self {
        Self::per(per_minute, 60_000, (per_minute / 5).max(1))
    }

    pub fn per(count: u32, per_ms: u64, burst: u32) -> Self {
        Self {
            per_ms: f64::from(count) / per_ms as f64,
            burst: f64::from(burst.max(1)),
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Takes a token, if `client` has one left under every key.
    pub fn allow(&self, client: &Client, now_ms: u64) -> bool {
        self.ready(client, now_ms) && {
            self.spend(client, now_ms);
            true
        }
    }

    /// Whether `client` has a token left, without taking it.
    pub fn ready(&self, client: &Client, now_ms: u64) -> bool {
        let buckets = self.lock();
        client.keys().all(|(key, share)| {
            buckets
                .get(&key)
                .is_none_or(|b| self.refilled(b, share, now_ms) >= 1.0)
        })
    }

    /// Takes a token even if none is left, e.g. for a miss found only afterwards.
    pub fn spend(&self, client: &Client, now_ms: u64) {
        let mut buckets = self.lock();
        for (key, share) in client.keys() {
            let full = self.burst * f64::from(share);
            let bucket = buckets.entry(key).or_insert(Bucket {
                tokens: full,
                at_ms: now_ms,
            });
            bucket.tokens = (self.refilled(bucket, share, now_ms) - 1.0).max(0.0);
            bucket.at_ms = now_ms;
        }
    }

    /// Forgets clients whose bucket is full again: they'd start full anyway.
    pub fn prune(&self, now_ms: u64) {
        let mut buckets = self.lock();
        buckets.retain(|key, b| {
            let share = match key {
                Key::Line(_) => 1,
                Key::Site(_) => SITE_SHARE,
            };
            self.refilled(b, share, now_ms) < self.burst * f64::from(share)
        });
    }

    fn refilled(&self, bucket: &Bucket, share: u32, now_ms: u64) -> f64 {
        let share = f64::from(share);
        let elapsed = now_ms.saturating_sub(bucket.at_ms) as f64;
        (bucket.tokens + elapsed * self.per_ms * share).min(self.burst * share)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Key, Bucket>> {
        self.buckets.lock().expect("rate limiter lock poisoned")
    }
}

/// An amount per client that starts over each day (UTC).
#[derive(Default)]
pub struct DailyQuota {
    used: Mutex<HashMap<Key, (u64, u64)>>,
}

impl DailyQuota {
    /// Counts `amount` against `client`'s `limit` for `today` (its share of
    /// it, per key), unless that would go over.
    pub fn take(&self, client: &Client, amount: u64, limit: u64, today: u64) -> bool {
        let mut used = self.used.lock().expect("daily quota lock poisoned");
        let used_today = |used: &HashMap<Key, (u64, u64)>, key| match used.get(&key) {
            Some((day, n)) if *day == today => *n,
            _ => 0,
        };
        let fits = client.keys().all(|(key, share)| {
            used_today(&used, key).saturating_add(amount) <= limit.saturating_mul(u64::from(share))
        });
        if fits {
            for (key, _) in client.keys() {
                let n = used_today(&used, key) + amount;
                used.insert(key, (today, n));
            }
        }
        fits
    }

    /// Gives back `amount` taken earlier `today`, for something that didn't happen.
    pub fn give_back(&self, client: &Client, amount: u64, today: u64) {
        let mut used = self.used.lock().expect("daily quota lock poisoned");
        for (key, _) in client.keys() {
            if let Some((day, n)) = used.get_mut(&key) {
                if *day == today {
                    *n = n.saturating_sub(amount);
                }
            }
        }
    }

    /// Forgets the days before `today`.
    pub fn prune(&self, today: u64) {
        let mut used = self.used.lock().expect("daily quota lock poisoned");
        used.retain(|_, (day, _)| *day == today);
    }
}

/// Connections held open per client: event streams and waiting reads.
pub struct Connections {
    limit: usize,
    open: Mutex<HashMap<Key, usize>>,
}

/// One open connection; closing it (dropping this) frees its place.
pub struct Connection {
    connections: Arc<Connections>,
    client: Client,
}

impl Connections {
    pub fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            limit,
            open: Mutex::new(HashMap::new()),
        })
    }

    pub fn open(self: &Arc<Self>, client: Client) -> Option<Connection> {
        let mut open = self.lock();
        let full = client.keys().any(|(key, share)| {
            open.get(&key).copied().unwrap_or(0) >= self.limit.saturating_mul(share as usize)
        });
        if full {
            return None;
        }
        for (key, _) in client.keys() {
            *open.entry(key).or_default() += 1;
        }
        Some(Connection {
            connections: self.clone(),
            client,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Key, usize>> {
        self.open.lock().expect("connections lock poisoned")
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let mut open = self.connections.lock();
        for (key, _) in self.client.keys() {
            if let Some(n) = open.get_mut(&key) {
                *n -= 1;
                if *n == 0 {
                    open.remove(&key);
                }
            }
        }
    }
}

/// Codes an address may open per [`CODE_WINDOW_MS`]: each Invite window opens
/// one, and a new one after a wrong guess.
pub const CODES_PER_WINDOW: u32 = 20;
/// Missed lookups and answers an address may send per [`CODE_WINDOW_MS`] on
/// the side typing a code. Someone typing codes needs a few; finding other
/// people's open codes to spoil or guess takes thousands.
pub const LOOKUPS_PER_WINDOW: u32 = 30;
const CODE_WINDOW_MS: u64 = 10 * 60 * 1000;
/// Event streams and waiting reads one address may hold open.
pub const MAX_CONNECTIONS: usize = 32;
/// Uploads one address may have in memory at once: each holds up to a clip's
/// size until it's stored.
pub const MAX_BUFFERED: usize = 4;

/// The client holds all the places it may already.
#[derive(Debug)]
pub struct Crowded;

/// Every per-client limit of a public relay. On other relays they're all off:
/// their proxy may not say who the client is, so everyone would share them.
pub struct Clients {
    enabled: bool,
    pub requests: RateLimiter,
    pub codes: RateLimiter,
    pub lookups: RateLimiter,
    pub connections: Arc<Connections>,
    pub buffers: Arc<Connections>,
    warned_unforwarded: AtomicBool,
    warned_cloudflare: AtomicBool,
}

impl Clients {
    pub fn new(public: bool, requests_per_minute: u32) -> Self {
        Self {
            enabled: public,
            requests: RateLimiter::new(requests_per_minute),
            codes: RateLimiter::per(CODES_PER_WINDOW, CODE_WINDOW_MS, CODES_PER_WINDOW),
            lookups: RateLimiter::per(LOOKUPS_PER_WINDOW, CODE_WINDOW_MS, LOOKUPS_PER_WINDOW),
            connections: Connections::new(MAX_CONNECTIONS),
            buffers: Connections::new(MAX_BUFFERED),
            warned_unforwarded: AtomicBool::new(false),
            warned_cloudflare: AtomicBool::new(false),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Takes a token from `limiter`; always allowed when limits are off.
    pub fn allow(&self, limiter: &RateLimiter, client: &Client, now_ms: u64) -> bool {
        !self.enabled || limiter.allow(client, now_ms)
    }

    pub fn ready(&self, limiter: &RateLimiter, client: &Client, now_ms: u64) -> bool {
        !self.enabled || limiter.ready(client, now_ms)
    }

    pub fn spend(&self, limiter: &RateLimiter, client: &Client, now_ms: u64) {
        if self.enabled {
            limiter.spend(client, now_ms);
        }
    }

    /// A place for a long-lived response. `Err` when the client holds too many.
    pub fn connect(&self, client: Client) -> Result<Option<Connection>, Crowded> {
        self.hold(&self.connections, client)
    }

    /// A place for an upload read into memory. `Err` when the client has too many.
    pub fn buffer(&self, client: Client) -> Result<Option<Connection>, Crowded> {
        self.hold(&self.buffers, client)
    }

    fn hold(
        &self,
        places: &Arc<Connections>,
        client: Client,
    ) -> Result<Option<Connection>, Crowded> {
        match self.enabled {
            true => places.open(client).map(Some).ok_or(Crowded),
            false => Ok(None),
        }
    }

    pub fn prune(&self, now_ms: u64) {
        self.requests.prune(now_ms);
        self.codes.prune(now_ms);
        self.lookups.prune(now_ms);
    }

    /// Warns (once each) when the reverse proxy in front of a public relay
    /// doesn't pass on the client's address, so every client would share
    /// one set of limits, or passes on one that disagrees with Cloudflare's.
    pub fn check_forwarding(&self, req: &Request, client: &Client) {
        if !self.enabled {
            return;
        }
        if client.is_proxy() && !self.warned_unforwarded.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                "a request's client address is a local one: the reverse proxy doesn't set X-Forwarded-For to the client's address, so all clients share one set of limits (see deploy/nginx.conf)"
            );
        }
        let cloudflare = req
            .headers()
            .get("cf-connecting-ip")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<IpAddr>().ok());
        if let Some(cf) = cloudflare {
            if Client::from_ip(cf).line != client.line
                && !self.warned_cloudflare.swap(true, Ordering::Relaxed)
            {
                tracing::warn!(
                    "a request's CF-Connecting-IP differs from its client address: either the proxy takes Cloudflare's address instead of the client's (set up nginx's real_ip for Cloudflare), or someone reached this server without going through Cloudflare"
                );
            }
        }
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

    fn client(s: &str) -> Client {
        Client::from_ip(s.parse().unwrap())
    }

    #[test]
    fn trusts_the_header_only_from_a_proxy() {
        let proxied = request(Some("127.0.0.1:5000"), Some("6.6.6.6, 203.0.113.9"));
        assert_eq!(Client::of(&proxied), client("203.0.113.9"));
        let docker = request(Some("172.18.0.2:5000"), Some("203.0.113.9"));
        assert_eq!(Client::of(&docker), client("203.0.113.9"));
        let direct = request(Some("198.51.100.7:5000"), Some("203.0.113.9"));
        assert_eq!(Client::of(&direct), client("198.51.100.7"));
        let no_header = request(Some("127.0.0.1:5000"), None);
        assert_eq!(Client::of(&no_header), client("127.0.0.1"));
        assert!(Client::of(&no_header).is_proxy());
        let mapped = request(Some("[::ffff:198.51.100.7]:5000"), None);
        assert_eq!(Client::of(&mapped), client("198.51.100.7"));
    }

    #[test]
    fn ipv6_counts_per_64_and_per_48() {
        let a = Client::of(&request(Some("[2001:db8:1:2:aaaa::1]:5000"), None));
        let b = Client::of(&request(Some("[::1]:5000"), Some("2001:db8:1:2:bbbb::9")));
        assert_eq!(a, b);
        assert_eq!(a.display(), "2001:db8:1:2::/64");
        let neighbor = client("2001:db8:1:3::1");
        assert_ne!(a, neighbor);
        assert_eq!(a.site, neighbor.site);
    }

    #[test]
    fn bursts_then_refills() {
        let limiter = RateLimiter::new(60); // one a second, bursts of 12
        let a = client("203.0.113.1");
        assert!((0..12).all(|_| limiter.allow(&a, 0)));
        assert!(!limiter.allow(&a, 0));
        assert!(limiter.allow(&client("203.0.113.2"), 0));
        assert!(limiter.allow(&a, 1000));
        assert!(!limiter.allow(&a, 1000));

        // The second address is full again by now, so it's forgotten.
        limiter.prune(1000);
        assert_eq!(limiter.lock().len(), 1);
        limiter.prune(60_000);
        assert!(limiter.lock().is_empty());
    }

    #[test]
    fn a_48_shares_a_bigger_bucket() {
        let limiter = RateLimiter::per(1, 60_000, 1);
        // Every /64 of one /48 gets its own token, until the /48's run out.
        let lines: Vec<Client> = (0..=SITE_SHARE)
            .map(|n| client(&format!("2001:db8:1:{n:x}::1")))
            .collect();
        for line in &lines[..SITE_SHARE as usize] {
            assert!(limiter.allow(line, 0));
            assert!(!limiter.allow(line, 0));
        }
        assert!(!limiter.allow(&lines[SITE_SHARE as usize], 0));
        assert!(limiter.allow(&client("2001:db8:2::1"), 0));
    }

    #[test]
    fn spending_past_empty_stays_empty() {
        let limiter = RateLimiter::per(1, 60_000, 2);
        let a = client("203.0.113.1");
        limiter.spend(&a, 0);
        limiter.spend(&a, 0);
        limiter.spend(&a, 0);
        assert!(!limiter.ready(&a, 0));
        assert!(limiter.ready(&a, 60_000));
    }

    #[test]
    fn daily_quotas_start_over() {
        let quota = DailyQuota::default();
        let a = client("203.0.113.1");
        assert!(quota.take(&a, 6, 10, 0));
        assert!(!quota.take(&a, 5, 10, 0));
        assert!(quota.take(&a, 4, 10, 0));
        assert!(quota.take(&a, 10, 10, 1));
        quota.prune(1);
        assert!(!quota.take(&a, 1, 10, 1));
        // Given back only on the day it was taken.
        quota.give_back(&a, 3, 0);
        assert!(!quota.take(&a, 1, 10, 1));
        quota.give_back(&a, 3, 1);
        assert!(quota.take(&a, 3, 10, 1));
        assert!(!quota.take(&a, 1, 10, 1));
    }

    #[test]
    fn connections_are_limited_while_open() {
        let connections = Connections::new(2);
        let a = client("203.0.113.1");
        let first = connections.open(a).unwrap();
        let _second = connections.open(a).unwrap();
        assert!(connections.open(a).is_none());
        assert!(connections.open(client("203.0.113.2")).is_some());
        drop(first);
        assert!(connections.open(a).is_some());
        drop(_second);
    }

    #[test]
    fn counts_below_a_limit_per_key() {
        let a = client("2001:db8:1:1::1");
        let same_line = std::iter::repeat_n(a, 3);
        assert!(!below(&a, same_line.clone(), 3));
        assert!(below(&a, same_line, 4));
        let neighbors = (0..16).map(|n| client(&format!("2001:db8:1:{:x}::1", n + 2)));
        assert!(!below(&a, neighbors, 2));
    }
}
