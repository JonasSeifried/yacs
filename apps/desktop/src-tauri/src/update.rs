//! Self-updates from GitHub Releases. Updates are signed with the release key
//! (see `plugins.updater.pubkey` in tauri.conf.json); the updater refuses
//! anything else, and also refuses downgrades to an older signed release.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::{tray, windows};

/// Soon after launch, so a release that came out while YACS wasn't running
/// shows up as soon as the user looks.
const FIRST_CHECK: Duration = Duration::from_secs(5);
/// Checks are skipped until the last one is this old.
const RECHECK_AFTER: Duration = Duration::from_secs(60 * 60);
/// Left in the config directory by an install, so the restarted app shows
/// Settings instead of starting quietly in the tray.
const UPDATED_MARKER: &str = "just-updated";

#[derive(Default)]
pub struct Updates {
    available: Mutex<Option<Update>>,
    /// The version being downloaded and installed right now.
    installing: Mutex<Option<String>>,
    /// Why the last install failed, until the next attempt. The tray's
    /// install has nowhere else to show it.
    error: Mutex<Option<String>>,
    /// When the last check started, or finished for a manual one.
    last_check: Mutex<Option<SystemTime>>,
}

impl Updates {
    /// Version of the update that's ready to install, if any.
    pub fn available(&self) -> Option<String> {
        self.lock().as_ref().map(|u| u.version.clone())
    }

    pub fn installing(&self) -> Option<String> {
        self.installing
            .lock()
            .expect("update lock poisoned")
            .clone()
    }

    pub fn error(&self) -> Option<String> {
        self.error.lock().expect("update lock poisoned").clone()
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
            check_if_due(&app).await;
            tokio::time::sleep(RECHECK_AFTER).await;
        }
    });
}

/// Check when a window opens, unless a check ran recently. The timer alone
/// isn't enough: it stops while the computer sleeps.
pub fn check_in_background(app: &AppHandle) {
    if cfg!(debug_assertions) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move { check_if_due(&app).await });
}

async fn check_if_due(app: &AppHandle) {
    let updates = app.state::<Updates>();
    if updates.installing().is_some() {
        return;
    }
    {
        let mut last = updates.last_check.lock().expect("update lock poisoned");
        let recent = last.is_some_and(|t| t.elapsed().is_ok_and(|age| age < RECHECK_AFTER));
        if recent {
            return;
        }
        *last = Some(SystemTime::now());
    }
    if let Err(e) = check(app).await {
        tracing::info!(error = %e, "update check failed");
        // Offline, say: try again the next time a window opens.
        *updates.last_check.lock().expect("update lock poisoned") = None;
    }
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
        *updates.last_check.lock().expect("update lock poisoned") = Some(SystemTime::now());
        let mut available = updates.lock();
        let changed = available.as_ref().map(|u| &u.version) != version.as_ref();
        *available = update;
        changed
    };
    if changed {
        tray::refresh(app);
        let _ = app.emit(windows::EVENT_STATUS_CHANGED, ());
    }
    Ok(version)
}

/// Download, verify and install the update found by `check`, then restart.
/// The tray and Settings show that it's running, and why it failed.
pub async fn install(app: &AppHandle) -> Result<(), String> {
    let updates = app.state::<Updates>();
    let update = updates
        .lock()
        .take()
        .ok_or("no update to install; check for updates first")?;
    tracing::info!(version = %update.version, "installing update");
    *updates.error.lock().expect("update lock poisoned") = None;
    set_installing(app, Some(update.version.clone()));
    // Written first: on Windows the installer quits and relaunches YACS itself.
    let marker = updated_marker(app);
    if let Some(marker) = &marker {
        if let Err(e) = std::fs::write(marker, &update.version) {
            tracing::warn!(error = %e, "can't note the update for the restart");
        }
    }
    if let Err(e) = update.download_and_install(|_, _| {}, || {}).await {
        if let Some(marker) = &marker {
            let _ = std::fs::remove_file(marker);
        }
        let message = format!("couldn't install the update: {e}");
        *updates.lock() = Some(update);
        *updates.error.lock().expect("update lock poisoned") = Some(message.clone());
        set_installing(app, None);
        return Err(message);
    }
    app.restart();
}

/// Whether this launch is the restart after an update. Only answers true once.
pub fn just_updated(app: &AppHandle) -> bool {
    updated_marker(app).is_some_and(|marker| std::fs::remove_file(marker).is_ok())
}

fn updated_marker(app: &AppHandle) -> Option<PathBuf> {
    Some(app.path().app_config_dir().ok()?.join(UPDATED_MARKER))
}

fn set_installing(app: &AppHandle, version: Option<String>) {
    *app.state::<Updates>()
        .installing
        .lock()
        .expect("update lock poisoned") = version;
    tray::refresh(app);
    let _ = app.emit(windows::EVENT_STATUS_CHANGED, ());
}
