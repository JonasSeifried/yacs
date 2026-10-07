//! Listens to the relay's event stream while Spotlight or Settings is open,
//! so Spotlight shows new clips as they come and Settings hears that an
//! invite was used. Nothing is kept open while the app sits in the tray, and
//! clips are only downloaded when Spotlight shows them.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Notify;
use yacs_client::{Client, Error};
use yacs_core::api::ChannelEvent;

use crate::state::AppState;
use crate::windows;

const RETRY_MIN: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(60);
/// Relays before 0.2.0 have no event stream; look again now and then, in
/// case the relay was updated (and whenever Spotlight opens, see `wake`).
const OLD_RELAY_RETRY: Duration = Duration::from_secs(10 * 60);

#[derive(Default)]
pub struct Live {
    task: Mutex<Option<JoinHandle<()>>>,
    wake: Arc<Notify>,
}

/// The space in use changed: listen to the new one, if a window is open.
pub fn restart(app: &AppHandle) {
    connect(app, true);
}

/// A window opened: listen, or retry now if the listener is waiting to
/// reconnect (the relay was down or too old a moment ago).
pub fn resume(app: &AppHandle) {
    connect(app, false);
    app.state::<Live>().wake.notify_waiters();
}

/// A window closed: stop listening once none is open.
pub fn pause_if_hidden(app: &AppHandle) {
    connect(app, false);
}

fn connect(app: &AppHandle, replace: bool) {
    let open = windows::any_open(app);
    let client = app.state::<AppState>().client().filter(|_| open);
    let live = app.state::<Live>();
    let mut task = live.task.lock().expect("live lock poisoned");
    if (replace || client.is_none())
        && let Some(old) = task.take()
    {
        old.abort();
    }
    if let (Some(client), None) = (client, task.as_ref()) {
        let wake = live.wake.clone();
        *task = Some(tauri::async_runtime::spawn(listen(
            app.clone(),
            client,
            wake,
        )));
    }
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
                        Ok(Some(event)) => handle(&app, event),
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

fn handle(app: &AppHandle, event: ChannelEvent) {
    let state = app.state::<AppState>();
    match event {
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
        ChannelEvent::InviteUsed { slot } => {
            let _ = app.emit_to(windows::SETTINGS, windows::EVENT_INVITE_USED, slot);
            return;
        }
        ChannelEvent::Added { .. } | ChannelEvent::Other => {}
    }
    changed(app);
}

/// Spotlight re-lists when it's shown anyway, so only an open one is told.
fn changed(app: &AppHandle) {
    if windows::spotlight_open(app) {
        let _ = app.emit_to(windows::SPOTLIGHT, windows::EVENT_CLIPS_CHANGED, ());
    }
}
