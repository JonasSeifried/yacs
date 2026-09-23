//! Everything the webviews can ask the Rust side to do. Errors are plain
//! strings because they're shown to the user as-is.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::ipc::Response;
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_autostart::ManagerExt;
use yacs_client::Client;
use yacs_core::DEFAULT_PHRASE_WORDS;
use yacs_core::api::{ClipMeta, ServerConfig};

use crate::clips::{self, ClipView, Entry};
use crate::state::AppState;
use crate::{clipboard, hotkey, pairing, windows};

type CmdResult<T> = Result<T, String>;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    paired: bool,
    server_url: Option<String>,
    device_name: String,
    hotkey: String,
    hotkey_error: Option<String>,
    default_ttl_secs: u64,
    autostart: bool,
    /// `macos`, `windows`, `linux`: lets the UI show ⌘ vs Ctrl.
    os: &'static str,
}

#[tauri::command]
pub fn status(app: AppHandle, state: State<'_, AppState>) -> Status {
    let settings = state.settings().clone();
    Status {
        paired: state.client().is_some(),
        server_url: settings.server_url,
        device_name: settings.device_name,
        hotkey: settings.hotkey,
        hotkey_error: state.hotkey_error.lock().expect("lock poisoned").clone(),
        default_ttl_secs: settings.default_ttl_secs,
        autostart: app.autolaunch().is_enabled().unwrap_or(false),
        os: std::env::consts::OS,
    }
}

#[tauri::command]
pub fn generate_phrase() -> CmdResult<String> {
    yacs_core::generate_phrase(DEFAULT_PHRASE_WORDS).map_err(|e| e.to_string())
}

/// Verify the pairing against the relay, then store it. Nothing is saved if
/// any step fails.
#[tauri::command]
pub async fn pair(
    app: AppHandle,
    state: State<'_, AppState>,
    server_url: String,
    token: Option<String>,
    phrase: String,
) -> CmdResult<()> {
    let connected = pairing::connect(&server_url, token.as_deref(), phrase).await?;
    state
        .secrets
        .save(&connected.stored)
        .map_err(|e| e.to_string())?;
    {
        let mut settings = state.settings();
        settings.server_url = Some(connected.server_url);
        state
            .settings_file
            .save(&settings)
            .map_err(|e| e.to_string())?;
    }
    state.set_client(Some(connected.client));
    let _ = app.emit(windows::EVENT_STATUS_CHANGED, ());
    Ok(())
}

#[tauri::command]
pub fn unpair(app: AppHandle, state: State<'_, AppState>) -> CmdResult<()> {
    state.secrets.delete().map_err(|e| e.to_string())?;
    state.set_client(None);
    {
        let mut settings = state.settings();
        settings.server_url = None;
        state
            .settings_file
            .save(&settings)
            .map_err(|e| e.to_string())?;
    }
    let _ = app.emit(windows::EVENT_STATUS_CHANGED, ());
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preferences {
    device_name: String,
    hotkey: String,
    default_ttl_secs: u64,
    autostart: bool,
}

#[tauri::command]
pub fn save_preferences(
    app: AppHandle,
    state: State<'_, AppState>,
    preferences: Preferences,
) -> CmdResult<()> {
    let device_name = preferences.device_name.trim().to_owned();
    if device_name.is_empty() {
        return Err("device name can't be empty".into());
    }
    if preferences.default_ttl_secs == 0 {
        return Err("expiry must be greater than zero".into());
    }

    let old_hotkey = state.settings().hotkey.clone();
    hotkey::replace(&app, &old_hotkey, &preferences.hotkey)?;
    *state.hotkey_error.lock().expect("lock poisoned") = None;

    let autolaunch = app.autolaunch();
    if autolaunch.is_enabled().unwrap_or(false) != preferences.autostart {
        let toggled = if preferences.autostart {
            autolaunch.enable()
        } else {
            autolaunch.disable()
        };
        toggled.map_err(|e| format!("couldn't change launch at login: {e}"))?;
    }

    {
        let mut settings = state.settings();
        settings.device_name = device_name;
        settings.hotkey = preferences.hotkey;
        settings.default_ttl_secs = preferences.default_ttl_secs;
        state
            .settings_file
            .save(&settings)
            .map_err(|e| e.to_string())?;
    }
    let _ = app.emit(windows::EVENT_STATUS_CHANGED, ());
    Ok(())
}

fn client(state: &AppState) -> CmdResult<Arc<Client>> {
    state
        .client()
        .ok_or_else(|| "this device isn't paired yet".into())
}

#[tauri::command]
pub async fn server_config(state: State<'_, AppState>) -> CmdResult<ServerConfig> {
    client(&state)?.config().await.map_err(|e| e.to_string())
}

/// Newest first. Also forgets cached clips that expired or were deleted.
#[tauri::command]
pub async fn list_clips(state: State<'_, AppState>) -> CmdResult<Vec<ClipMeta>> {
    let listed = client(&state)?.list().await.map_err(|e| e.to_string())?;
    state
        .clips
        .lock()
        .expect("clip cache lock poisoned")
        .retain_listed(&listed);
    Ok(listed)
}

async fn load(state: &AppState, id: &str) -> CmdResult<Arc<Entry>> {
    clips::load(&*client(state)?, &state.clips, id)
        .await?
        .ok_or_else(|| "this clip expired or was deleted".into())
}

/// Decrypted, for the preview. `None` if it's gone from the relay.
#[tauri::command]
pub async fn get_clip(state: State<'_, AppState>, id: String) -> CmdResult<Option<ClipView>> {
    let entry = clips::load(&*client(&state)?, &state.clips, &id).await?;
    Ok(entry.as_deref().map(ClipView::from))
}

/// The clip's image as raw bytes, which reach the webview as an `ArrayBuffer`.
#[tauri::command]
pub async fn clip_image(state: State<'_, AppState>, id: String) -> CmdResult<Response> {
    let entry = load(&state, &id).await?;
    let image = entry.image().ok_or("this clip has no image")?;
    Ok(Response::new(image.data.clone()))
}

/// Put every format of the clip on the clipboard, then get out of the way so
/// it can be pasted into the app that was in front.
#[tauri::command]
pub async fn copy_clip(app: AppHandle, state: State<'_, AppState>, id: String) -> CmdResult<()> {
    let entry = load(&state, &id).await?;
    tauri::async_runtime::spawn_blocking(move || clipboard::write(&entry.clip.items))
        .await
        .map_err(|e| e.to_string())??;
    windows::hide_spotlight(&app);
    Ok(())
}

/// Encrypt and upload what's on the clipboard right now.
#[tauri::command]
pub async fn send_clipboard(state: State<'_, AppState>, ttl_secs: u64) -> CmdResult<ClipView> {
    let client = client(&state)?;
    let items = tauri::async_runtime::spawn_blocking(clipboard::read)
        .await
        .map_err(|e| e.to_string())??;
    let device_name = state.settings().device_name.clone();
    let ttl = Duration::from_secs(ttl_secs.max(1));
    let entry = clips::send(&client, &state.clips, device_name, items, ttl).await?;
    Ok(ClipView::from(&*entry))
}

/// Deletes the clip on the relay, for every device.
#[tauri::command]
pub async fn delete_clip(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    client(&state)?
        .delete(&id)
        .await
        .map_err(|e| e.to_string())?;
    state
        .clips
        .lock()
        .expect("clip cache lock poisoned")
        .remove(&id);
    Ok(())
}

/// Spotlight's expiry dropdown: the last choice becomes the default.
#[tauri::command]
pub fn set_default_ttl(app: AppHandle, state: State<'_, AppState>, ttl_secs: u64) -> CmdResult<()> {
    if ttl_secs == 0 {
        return Err("expiry must be greater than zero".into());
    }
    {
        let mut settings = state.settings();
        settings.default_ttl_secs = ttl_secs;
        state
            .settings_file
            .save(&settings)
            .map_err(|e| e.to_string())?;
    }
    // Only Settings needs to know; Spotlight made the change.
    let _ = app.emit_to(windows::SETTINGS, windows::EVENT_STATUS_CHANGED, ());
    Ok(())
}

#[tauri::command]
pub fn hide_spotlight(app: AppHandle) {
    windows::hide_spotlight(&app);
}

#[tauri::command]
pub fn open_settings(app: AppHandle) {
    windows::show_settings(&app);
}
