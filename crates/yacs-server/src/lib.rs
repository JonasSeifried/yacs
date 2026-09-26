//! YACS relay: stores end-to-end encrypted clips it cannot read, per channel,
//! until their TTL runs out.

mod api;
pub mod clock;
mod config;
mod events;
mod invites;
pub mod store;
mod web;

use std::future::Future;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;

pub use api::{AppState, router};
pub use clock::{Clock, ManualClock, SystemClock};
pub use config::Config;
pub use events::Events;
pub use invites::Invites;
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
    let config = Arc::new(config);
    let events = Arc::new(Events::default());
    let invites = Arc::new(Invites::default());
    let app = router(AppState {
        store: store.clone(),
        config: config.clone(),
        clock: clock.clone(),
        events: events.clone(),
        invites: invites.clone(),
    });

    let reaper_events = events.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(REAP_INTERVAL);
        loop {
            interval.tick().await;
            reaper_events.prune();
            invites.prune(clock.now_ms());
            match store.reap(clock.now_ms()).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(removed = n, "reaped expired clips"),
                Err(e) => tracing::error!(error = %e, "reaper failed"),
            }
        }
    });

    tracing::info!(addr = %listener.local_addr()?, "listening");
    if config.access_token.is_none() {
        tracing::warn!(
            "no access token set: anyone who can reach this server can store clips on it (set YACS_ACCESS_TOKEN)"
        );
    }
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown.await;
            // Event streams never end on their own.
            events.close();
        })
        .await
}
