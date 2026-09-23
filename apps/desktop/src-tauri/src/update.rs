//! Self-updates from GitHub Releases. Updates are signed with the release key
//! (see `plugins.updater.pubkey` in tauri.conf.json); the updater refuses
//! anything else, and also refuses downgrades to an older signed release.

use std::sync::Mutex;
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::{tray, windows};

const FIRST_CHECK: Duration = Duration::from_secs(30);
const CHECK_EVERY: Duration = Duration::from_secs(12 * 60 * 60);

#[derive(Default)]
pub struct Updates {
    available: Mutex<Option<Update>>,
}

impl Updates {
    /// Version of the update that's ready to install, if any.
    pub fn available(&self) -> Option<String> {
        self.lock().as_ref().map(|u| u.version.clone())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Update>> {
        self.available.lock().expect("update lock poisoned")
    }
}

/// Check in the background now and then. Release builds only: dev builds
/// would offer to "update" to the last release.
pub fn spawn_checks(app: &AppHandle) {
    if cfg!(debug_assertions) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK).await;
        loop {
            if let Err(e) = check(&app).await {
                tracing::info!(error = %e, "update check failed");
            }
            tokio::time::sleep(CHECK_EVERY).await;
        }
    });
}

/// Returns the new version, if there is one.
pub async fn check(app: &AppHandle) -> Result<Option<String>, String> {
    let update = app
        .updater()
        .map_err(|e| e.to_string())?
        .check()
        .await
        .map_err(|e| format!("couldn't check for updates: {e}"))?;
    let version = update.as_ref().map(|u| u.version.clone());
    let changed = {
        let updates = app.state::<Updates>();
        let mut available = updates.lock();
        let changed = available.as_ref().map(|u| &u.version) != version.as_ref();
        *available = update;
        changed
    };
    if changed {
        tray::set_update(app, version.as_deref());
        let _ = app.emit(windows::EVENT_STATUS_CHANGED, ());
    }
    Ok(version)
}

/// Download, verify and install the update found by `check`, then restart.
pub async fn install(app: &AppHandle) -> Result<(), String> {
    let update = app
        .state::<Updates>()
        .lock()
        .take()
        .ok_or("no update to install; check for updates first")?;
    tracing::info!(version = %update.version, "installing update");
    if let Err(e) = update.download_and_install(|_, _| {}, || {}).await {
        let message = format!("couldn't install the update: {e}");
        *app.state::<Updates>().lock() = Some(update);
        return Err(message);
    }
    app.restart();
}
