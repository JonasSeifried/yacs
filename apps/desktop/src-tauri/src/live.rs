//! Listens to the relay's event stream while in a space, so an open Spotlight
//! updates by itself and a new clip is already decrypted when it's opened.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Notify;
use yacs_client::{Client, Error};
use yacs_core::api::ChannelEvent;

use crate::clips;
use crate::state::AppState;
use crate::windows;

const RETRY_MIN: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(60);
/// Relays before 0.2.0 have no event stream; look again now and then, in
/// case the relay was updated (and whenever Spotlight opens, see `wake`).
const OLD_RELAY_RETRY: Duration = Duration::from_secs(10 * 60);
/// New clips up to this size are fetched right away.
const PREFETCH_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Default)]
pub struct Live {
    task: Mutex<Option<JoinHandle<()>>>,
    wake: Arc<Notify>,
}

/// Listen for the space in use, replacing any earlier listener. Call it
/// whenever that changes.
pub fn restart(app: &AppHandle) {
    let live = app.state::<Live>();
    let mut task = live.task.lock().expect("live lock poisoned");
    if let Some(old) = task.take() {
        old.abort();
    }
    if let Some(client) = app.state::<AppState>().client() {
        let wake = live.wake.clone();
        *task = Some(tauri::async_runtime::spawn(listen(
            app.clone(),
            client,
            wake,
        )));
    }
}

/// Retry now if the listener is waiting to reconnect, e.g. because the relay
/// was down or too old a moment ago. Called when Spotlight opens.
pub fn wake(app: &AppHandle) {
    app.state::<Live>().wake.notify_waiters();
}

/// Sleep for `duration`, or until `wake`.
async fn pause(wake: &Notify, duration: Duration) {
    let _ = tokio::time::timeout(duration, wake.notified()).await;
}

async fn listen(app: AppHandle, client: Arc<Client>, wake: Arc<Notify>) {
    let mut retry = RETRY_MIN;
    loop {
        match client.events().await {
            Ok(mut events) => {
                let connected = Instant::now();
                // Anything could have changed while we weren't listening.
                changed(&app);
                loop {
                    match events.next().await {
                        Ok(Some(event)) => handle(&app, &client, event).await,
                        Ok(None) => break,
                        Err(e) => {
                            tracing::info!(error = %e, "live updates interrupted");
                            break;
                        }
                    }
                }
                // A stream that stays up is healthy; one that keeps dropping
                // right away backs off like a failed connect.
                if connected.elapsed() > RETRY_MAX {
                    retry = RETRY_MIN;
                }
            }
            Err(Error::Server { status: 404, .. }) => {
                tracing::info!("relay has no live updates (older than 0.2.0)");
                pause(&wake, OLD_RELAY_RETRY).await;
                retry = RETRY_MIN;
                continue;
            }
            Err(e) => tracing::info!(error = %e, "can't connect for live updates"),
        }
        pause(&wake, retry).await;
        retry = (retry * 2).min(RETRY_MAX);
    }
}

async fn handle(app: &AppHandle, client: &Client, event: ChannelEvent) {
    let state = app.state::<AppState>();
    match event {
        // A chunked clip is only its small header here.
        ChannelEvent::Added { clip } if clip.size <= PREFETCH_BYTES || clip.chunked => {
            // Before telling Spotlight, so it finds the clip in the cache.
            if let Err(e) = clips::load(client, &state.clips, &clip.id).await {
                tracing::debug!(error = %e, "prefetch failed");
            }
        }
        ChannelEvent::Deleted { id } => state
            .clips
            .lock()
            .expect("clip cache lock poisoned")
            .remove(&id),
        ChannelEvent::Cleared => state
            .clips
            .lock()
            .expect("clip cache lock poisoned")
            .clear(),
        ChannelEvent::Added { .. } | ChannelEvent::Other => {}
    }
    changed(app);
}

/// Spotlight re-lists when it's shown anyway, so only an open one is told.
fn changed(app: &AppHandle) {
    let open = app
        .get_webview_window(windows::SPOTLIGHT)
        .is_some_and(|w| w.is_visible().unwrap_or(false));
    if open {
        let _ = app.emit_to(windows::SPOTLIGHT, windows::EVENT_CLIPS_CHANGED, ());
    }
}
