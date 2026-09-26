//! YACS relay: stores end-to-end encrypted clips it cannot read, per channel,
//! until their TTL runs out.

mod accounts;
mod api;
mod clients;
pub mod clock;
mod config;
mod events;
mod invites;
mod rendezvous;
pub mod store;
mod web;

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;

pub use accounts::Accounts;
pub use api::{AppState, router};
pub use clients::RateLimiter;
pub use clock::{Clock, ManualClock, SystemClock};
pub use config::Config;
pub use events::Events;
pub use invites::Invites;
pub use rendezvous::Rendezvous;
pub use store::Store;

const REAP_INTERVAL: Duration = Duration::from_secs(60);

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
    let limiter = Arc::new(RateLimiter::new(config.requests_per_minute));
    let config = Arc::new(config);
    let events = Arc::new(Events::default());
    let invites = Arc::new(Invites::default());
    let rendezvous = Arc::new(Rendezvous::default());
    let app = router(AppState {
        store: store.clone(),
        config: config.clone(),
        clock: clock.clone(),
        events: events.clone(),
        invites: invites.clone(),
        rendezvous: rendezvous.clone(),
        accounts: accounts.clone(),
        limiter: limiter.clone(),
    });

    let reaper_events = events.clone();
    let reaper_accounts = accounts.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(REAP_INTERVAL);
        loop {
            interval.tick().await;
            reaper_events.prune();
            invites.prune(clock.now_ms());
            rendezvous.prune(clock.now_ms());
            limiter.prune(clock.now_ms());
            if let Err(e) = reaper_accounts.prune(clock.now_ms()).await {
                tracing::error!(error = %e, "can't save the registered spaces");
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
    } else if config.access_token.is_none() {
        tracing::warn!(
            "no account key set: anyone who can reach this server can store clips on it (set YACS_ACCESS_TOKEN)"
        );
    }
    // The peer's address tells a reverse proxy from a client (see `clients`).
    let app = app.into_make_service_with_connect_info::<SocketAddr>();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown.await;
            // Event streams never end on their own.
            events.close();
        })
        .await?;
    accounts.save().await
}
