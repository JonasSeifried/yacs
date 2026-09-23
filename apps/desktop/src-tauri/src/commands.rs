//! Everything the webviews can ask the Rust side to do. Errors are plain
//! strings because they're shown to the user as-is.

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_autostart::ManagerExt;
use yacs_client::Client;
use yacs_core::DEFAULT_PHRASE_WORDS;
use yacs_core::api::{ClipMeta, ServerConfig};

use crate::state::AppState;
use crate::{hotkey, pairing, windows};

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

fn client(state: &AppState) -> CmdResult<std::sync::Arc<Client>> {
    state
        .client()
        .ok_or_else(|| "this device isn't paired yet".into())
}

#[tauri::command]
pub async fn server_config(state: State<'_, AppState>) -> CmdResult<ServerConfig> {
    client(&state)?.config().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn list_clips(state: State<'_, AppState>) -> CmdResult<Vec<ClipMeta>> {
    client(&state)?.list().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub fn hide_spotlight(app: AppHandle) {
    windows::hide_spotlight(&app);
}

#[tauri::command]
pub fn open_settings(app: AppHandle) {
    windows::show_settings(&app);
}
