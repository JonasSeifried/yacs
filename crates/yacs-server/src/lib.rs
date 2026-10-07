//! YACS relay: stores end-to-end encrypted clips it cannot read, per channel,
//! until their TTL runs out.

mod accounts;
mod api;
pub mod clients;
pub mod clock;
mod config;
mod events;
mod invites;
mod rendezvous;
mod stats;
pub mod store;
mod web;

use std::future::{Future, IntoFuture};
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::Notify;

pub use accounts::Accounts;
pub use api::{AppState, router};
pub use clients::{Client, Clients, RateLimiter};
pub use clock::{Clock, ManualClock, SystemClock};
pub use config::Config;
pub use events::Events;
pub use invites::Invites;
pub use rendezvous::Rendezvous;
pub use stats::Stats;
pub use store::Store;

const REAP_INTERVAL: Duration = Duration::from_secs(60);
/// How long requests still running may finish after the stop signal: under
/// the 10 s Docker waits before it kills the relay. A code exchange waits up
/// to 25 s for its message and a big upload takes minutes, so they're cut off.
const SHUTDOWN_DEADLINE: Duration = if cfg!(test) {
    Duration::from_millis(300)
} else {
    Duration::from_secs(8)
};

/// Serve until `shutdown` resolves. Also runs the reaper.
pub async fn run(
    listener: TcpListener,
    config: Config,
    clock: Arc<dyn Clock>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> io::Result<()> {
    let store = Arc::new(
        Store::open(
            &config.data_dir,
            config.max_clips_per_channel,
            config.max_disk.as_u64(),
        )
        .await?,
    );
    let accounts = Arc::new(Accounts::open(&config.data_dir).await?);
    let stats = Arc::new(Stats::open(&config.data_dir, clock.now_ms()).await?);
    let clients = Arc::new(Clients::new(config.public, config.requests_per_minute));
    let invites = Arc::new(Invites::new(config.public));
    let config = Arc::new(config);
    let events = Arc::new(Events::default());
    let rendezvous = Arc::new(Rendezvous::default());
    let app = router(AppState {
        store: store.clone(),
        config: config.clone(),
        clock: clock.clone(),
        events: events.clone(),
        invites: invites.clone(),
        rendezvous: rendezvous.clone(),
        accounts: accounts.clone(),
        clients: clients.clone(),
        stats: stats.clone(),
    });

    let reaper_events = events.clone();
    let reaper_accounts = accounts.clone();
    let reaper_stats = stats.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(REAP_INTERVAL);
        loop {
            interval.tick().await;
            reaper_events.prune();
            invites.prune(clock.now_ms());
            rendezvous.prune(clock.now_ms());
            clients.prune(clock.now_ms());
            if let Err(e) = reaper_accounts.prune(clock.now_ms()).await {
                tracing::error!(error = %e, "can't save the registered spaces");
            }
            if let Err(e) = reaper_stats.save().await {
                tracing::warn!(error = %e, "can't save the stats");
            }
            match store.reap(clock.now_ms()).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(removed = n, "reaped expired clips"),
                Err(e) => tracing::error!(error = %e, "reaper failed"),
            }
        }
    });

    tracing::info!(addr = %listener.local_addr()?, "listening");
    if config.public {
        tracing::info!(
            "public relay: anyone can create spaces on the free plan; limits count per client address from X-Forwarded-For"
        );
        if config.privacy_url.is_none() || config.imprint_url.is_none() {
            tracing::warn!(
                "public relay without YACS_PRIVACY_URL and YACS_IMPRINT_URL: its users see no privacy policy or imprint"
            );
        }
    } else if config.access_token.is_none() {
        tracing::warn!(
            "no account key set: anyone who can reach this server can store clips on it (set YACS_ACCESS_TOKEN)"
        );
    }
    // The peer's address tells a reverse proxy from a client (see `clients`).
    let app = app.into_make_service_with_connect_info::<SocketAddr>();
    let stop = Arc::new(Notify::new());
    let serve = axum::serve(listener, app)
        .with_graceful_shutdown({
            let stop = stop.clone();
            async move { stop.notified().await }
        })
        .into_future();
    tokio::pin!(serve);
    tokio::select! {
        served = &mut serve => return served.and(accounts.save().await),
        () = shutdown => {}
    }
    // Free spaces registered since the last reap are only in memory, so
    // they're saved before anything else can take time.
    if let Err(e) = accounts.save().await {
        tracing::error!(error = %e, "can't save the registered spaces");
    }
    // Event streams never end on their own.
    events.close();
    stop.notify_one();
    match tokio::time::timeout(SHUTDOWN_DEADLINE, serve).await {
        Ok(served) => served?,
        Err(_) => tracing::warn!("stopping with requests still running"),
    }
    if let Err(e) = stats.save().await {
        tracing::warn!(error = %e, "can't save the stats");
    }
    accounts.save().await
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use clap::Parser;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    use super::*;

    async fn send(addr: SocketAddr, request: String) -> TcpStream {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        stream
    }

    /// A code exchange waits up to 25 s, longer than Docker gives the relay
    /// to stop; the free space it registered is saved all the same.
    #[tokio::test]
    async fn stops_in_time_and_saves_first() {
        let data = tempfile::TempDir::new().unwrap();
        let dir = data.path().to_str().unwrap();
        let config = Config::try_parse_from(["yacs-server", "--data-dir", dir, "--public"])
            .unwrap()
            .validate()
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let relay = tokio::spawn(run(listener, config, Arc::new(SystemClock), async {
            stopped.await.ok();
        }));

        let channel = yacs_core::ChannelId::from_bytes([0xab; 32]).to_string();
        let mut opened = send(
            addr,
            format!(
                "POST /api/v1/channels/{channel}/rendezvous HTTP/1.1\r\nHost: relay\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello"
            ),
        )
        .await;
        let mut answer = String::new();
        opened.read_to_string(&mut answer).await.unwrap();
        assert!(answer.starts_with("HTTP/1.1 201"), "{answer}");
        let nameplate: String = answer
            .split("\"nameplate\":")
            .nth(1)
            .unwrap()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        let mut waiting = send(
            addr,
            format!(
                "GET /api/v1/channels/{channel}/rendezvous/{nameplate}/b/0?wait=25 HTTP/1.1\r\nHost: relay\r\n\r\n"
            ),
        )
        .await;
        let mut first = [0u8; 1];
        let early = tokio::time::timeout(Duration::from_millis(200), waiting.read(&mut first));
        assert!(early.await.is_err(), "the read should still be waiting");

        let asked = Instant::now();
        stop.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), relay)
            .await
            .expect("the relay stops within its deadline")
            .unwrap()
            .unwrap();
        assert!(asked.elapsed() >= SHUTDOWN_DEADLINE);
        let saved = std::fs::read_to_string(data.path().join("accounts.json")).unwrap();
        // The file names spaces by their hex id.
        assert!(saved.contains(&"ab".repeat(32)), "{saved}");
    }
}
