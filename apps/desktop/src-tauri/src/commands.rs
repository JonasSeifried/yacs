//! Everything the webviews can ask the Rust side to do. Errors are plain
//! strings because they're shown to the user as-is.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::ipc::{Channel, Response};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_autostart::ManagerExt;
use yacs_client::spaces::{DEFAULT_SPACE_NAME, InviteLink, Space};
use yacs_client::{Client, LocalFiles, stream_of};
use yacs_core::api::{ClipMeta, ServerConfig};
use yacs_core::{Clip, ClipItem, Pairing, Stream, StreamFile};

use crate::clipboard::{Copied, LocalFile};
use crate::clips::{self, ClipView, Entry};
use crate::state::{AppState, client_for};
use crate::transfers::{self, Direction, FolderSink, Transfer, Transfers};
use crate::update::{self, Updates};
use crate::{cli, clipboard, hotkey, live, pairing, windows};

type CmdResult<T> = Result<T, String>;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// The space in use; `None` until this computer starts or joins one.
    space: Option<SpaceStatus>,
    device_name: String,
    hotkey: String,
    hotkey_error: Option<String>,
    /// Wayland: bind the command in the desktop instead of recording a hotkey.
    manual_shortcut: Option<hotkey::ManualShortcut>,
    default_ttl_secs: u64,
    autostart: bool,
    /// `macos`, `windows`, `linux`: lets the UI show ⌘ vs Ctrl.
    os: &'static str,
    version: String,
    /// A newer release that's ready to install.
    update: Option<String>,
    /// The version being installed right now (from the tray, say).
    update_installing: Option<String>,
    /// Why the last install failed.
    update_error: Option<String>,
    cli: cli::CliStatus,
}

#[derive(Serialize)]
pub struct SpaceStatus {
    name: String,
    relay: String,
}

#[tauri::command]
pub fn status(app: AppHandle, state: State<'_, AppState>) -> Status {
    let settings = state.settings().clone();
    let space = state
        .client()
        .and(state.spaces().current().map(|s| SpaceStatus {
            name: s.name.clone(),
            relay: s.relay.clone(),
        }));
    Status {
        space,
        device_name: settings.device_name,
        hotkey: settings.hotkey,
        hotkey_error: state.hotkey_error.lock().expect("lock poisoned").clone(),
        manual_shortcut: hotkey::manual(),
        default_ttl_secs: settings.default_ttl_secs,
        autostart: app.autolaunch().is_enabled().unwrap_or(false),
        os: std::env::consts::OS,
        version: app.package_info().version.to_string(),
        update: app.state::<Updates>().available(),
        update_installing: app.state::<Updates>().installing(),
        update_error: app.state::<Updates>().error(),
        cli: cli::status(),
    }
}

/// Start a new space with a random key on `server_url`, once the relay
/// accepts it. Nothing is saved if that fails.
#[tauri::command]
pub async fn create_space(
    app: AppHandle,
    state: State<'_, AppState>,
    server_url: String,
    token: Option<String>,
    name: Option<String>,
) -> CmdResult<()> {
    let pairing = Pairing::generate().map_err(|e| e.to_string())?;
    let connected = pairing::connect(&server_url, token.as_deref(), pairing.clone()).await?;
    let name = name.as_deref().unwrap_or(DEFAULT_SPACE_NAME);
    let space = Space::new(name, &connected.relay, &pairing);
    use_space(&app, &state, space, connected)
}

/// Join the space in an invite link from another device, once its relay
/// accepts it. Nothing is saved if that fails.
#[tauri::command]
pub async fn join_space(app: AppHandle, state: State<'_, AppState>, link: String) -> CmdResult<()> {
    let link = InviteLink::parse(&link).map_err(|e| {
        format!("{e}. Copy one on a computer in the space from Settings → Invite a device…")
    })?;
    let connected =
        pairing::connect(&link.relay, link.token.as_deref(), link.pairing.clone()).await?;
    let name = link.name.as_deref().unwrap_or(DEFAULT_SPACE_NAME);
    let space = Space::new(name, &connected.relay, &link.pairing);
    use_space(&app, &state, space, connected)
}

fn use_space(
    app: &AppHandle,
    state: &AppState,
    space: Space,
    connected: pairing::Connected,
) -> CmdResult<()> {
    app.state::<Transfers>().cancel();
    {
        let mut spaces = state.spaces();
        let mut next = spaces.clone();
        next.set_current(space, connected.token);
        state.spaces_file.save(&next).map_err(|e| e.to_string())?;
        *spaces = next;
    }
    state.set_client(Some(connected.client));
    live::restart(app);
    let _ = app.emit(windows::EVENT_STATUS_CHANGED, ());
    Ok(())
}

/// This computer's own name for the space in use. Returns the name as saved.
#[tauri::command]
pub fn rename_space(app: AppHandle, state: State<'_, AppState>, name: String) -> CmdResult<String> {
    let name = {
        let mut spaces = state.spaces();
        let mut next = spaces.clone();
        let name = next
            .rename_current(&name)
            .ok_or("the name can't be empty")?;
        state.spaces_file.save(&next).map_err(|e| e.to_string())?;
        *spaces = next;
        name
    };
    let _ = app.emit(windows::EVENT_STATUS_CHANGED, ());
    Ok(name)
}

/// Forget the space in use on this computer. Its clips stay on the relay for
/// its other devices.
#[tauri::command]
pub fn leave_space(app: AppHandle, state: State<'_, AppState>) -> CmdResult<()> {
    let client = {
        let mut spaces = state.spaces();
        let mut next = spaces.clone();
        next.leave_current();
        state.spaces_file.save(&next).map_err(|e| e.to_string())?;
        *spaces = next;
        client_for(&spaces)
    };
    app.state::<Transfers>().cancel();
    state.set_client(client);
    live::restart(&app);
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
        .ok_or_else(|| "this computer isn't in a space yet".into())
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
    Ok(Response::new(image.to_vec()))
}

/// Put every format of the clip on the clipboard. Files are saved to
/// Downloads first and go on the clipboard as files. Returns `false` when
/// they're big and download in the background first (see `transfers`); they
/// go on the clipboard once they're in.
#[tauri::command]
pub async fn copy_clip(app: AppHandle, state: State<'_, AppState>, id: String) -> CmdResult<bool> {
    let entry = load(&state, &id).await?;
    let downloads = match entry.has_files() {
        true => Some(
            app.path()
                .download_dir()
                .map_err(|e| format!("can't find the Downloads folder: {e}"))?,
        ),
        false => None,
    };
    if let (Some(stream), Some(dir)) = (stream_of(&entry.clip), &downloads) {
        if let Some(paths) = downloaded(&state, &id, stream) {
            tauri::async_runtime::spawn_blocking(move || clipboard::write_files(&paths))
                .await
                .map_err(|e| e.to_string())??;
            return Ok(true);
        }
        download(&app, client(&state)?, id, stream.clone(), dir.clone())?;
        return Ok(false);
    }
    tauri::async_runtime::spawn_blocking(move || match downloads {
        Some(dir) => clipboard::write_files(&clipboard::save_files(&entry.clip.items, &dir)?),
        None => clipboard::write(&entry.clip.items),
    })
    .await
    .map_err(|e| e.to_string())??;
    Ok(true)
}

/// Where the clip's big files were downloaded before, if they're all still there.
fn downloaded(state: &AppState, id: &str, stream: &Stream) -> Option<Vec<PathBuf>> {
    let paths = state
        .downloaded
        .lock()
        .expect("lock poisoned")
        .get(id)?
        .clone();
    let intact = paths.len() == stream.files.len()
        && paths
            .iter()
            .zip(&stream.files)
            .all(|(path, file)| std::fs::metadata(path).is_ok_and(|m| m.len() == file.size));
    intact.then_some(paths)
}

fn download(
    app: &AppHandle,
    client: Arc<Client>,
    id: String,
    stream: Stream,
    dir: PathBuf,
) -> CmdResult<Transfer> {
    let label = transfers::label(stream.files.iter().map(|f| f.name.as_str()));
    let them = if stream.files.len() == 1 {
        "it"
    } else {
        "them"
    };
    let handle = app.clone();
    transfers::start(
        app,
        Direction::Download,
        label.clone(),
        stream.total(),
        move |reporter, cancel| async move {
            let mut sink = FolderSink::new(&dir, &stream.files)?;
            let progress = |done, total| reporter.report(done, total);
            let fetched = tokio::select! {
                fetched = client.fetch_stream(&id, &stream, &mut sink, &progress) => {
                    fetched.map_err(|e| format!("Downloading {label} failed: {e}"))
                }
                () = cancel.notified() => Err("Download cancelled.".into()),
            };
            if let Err(e) = fetched {
                sink.discard();
                return Err(e);
            }
            let paths = sink.finish()?;
            let state = handle.state::<AppState>();
            state
                .downloaded
                .lock()
                .expect("lock poisoned")
                .insert(id, paths.clone());
            tauri::async_runtime::spawn_blocking(move || clipboard::write_files(&paths))
                .await
                .map_err(|e| e.to_string())??;
            Ok(format!("Saved {label} to Downloads and copied {them}."))
        },
    )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Sent {
    /// Sent already.
    clip: Option<ClipView>,
    /// Big files: uploading in the background (see `transfers`).
    upload: Option<Transfer>,
}

/// Encrypt and upload what's on the clipboard right now.
#[tauri::command]
pub async fn send_clipboard(
    app: AppHandle,
    state: State<'_, AppState>,
    ttl_secs: u64,
    on_progress: Channel<SendProgress>,
) -> CmdResult<Sent> {
    let client = client(&state)?;
    let config = client.config().await.map_err(|e| e.to_string())?;
    let copied = tauri::async_runtime::spawn_blocking(clipboard::read)
        .await
        .map_err(|e| e.to_string())??;
    let device_name = state.settings().device_name.clone();
    let ttl = Duration::from_secs(ttl_secs.max(1));
    let items = match copied {
        Copied::Items(items) => items,
        Copied::Files(files) => {
            let total: u64 = files.iter().map(|f| f.size).sum();
            let limit = config.inline_file_limit();
            if total > limit {
                let Some(chunked) = &config.chunked else {
                    return Err(clipboard::files_too_big(&files, limit));
                };
                let mut stream = Stream::new(stream_files(&files)).map_err(|e| e.to_string())?;
                stream.chunk_size = chunked.chunk_size();
                let clip = Clip {
                    created_at_ms: clips::now_ms(),
                    device_name,
                    items: vec![ClipItem::Stream(stream)],
                };
                let upload = upload(&app, client, clip, files, ttl)?;
                return Ok(Sent {
                    clip: None,
                    upload: Some(upload),
                });
            }
            tauri::async_runtime::spawn_blocking(move || clipboard::read_files(files))
                .await
                .map_err(|e| e.to_string())??
        }
    };
    let progress = reporter(on_progress);
    let entry = clips::send(
        &client,
        &state.clips,
        device_name,
        items,
        ttl,
        Some(progress),
    )
    .await?;
    Ok(Sent {
        clip: Some(ClipView::from(&*entry)),
        upload: None,
    })
}

#[derive(Clone, Serialize)]
pub struct SendProgress {
    done: u64,
    total: u64,
}

/// Progress to Spotlight, ten times a second at most (and when it's done).
fn reporter(channel: Channel<SendProgress>) -> Arc<dyn Fn(u64, u64) + Send + Sync> {
    let last = std::sync::Mutex::new(None::<std::time::Instant>);
    Arc::new(move |done, total| {
        let mut last = last.lock().expect("lock poisoned");
        if done < total && last.is_some_and(|t| t.elapsed() < Duration::from_millis(100)) {
            return;
        }
        *last = Some(std::time::Instant::now());
        let _ = channel.send(SendProgress { done, total });
    })
}

fn stream_files(files: &[LocalFile]) -> Vec<StreamFile> {
    files
        .iter()
        .map(|f| StreamFile {
            name: f.name.clone(),
            mime: f.mime.clone(),
            size: f.size,
        })
        .collect()
}

fn upload(
    app: &AppHandle,
    client: Arc<Client>,
    clip: Clip,
    files: Vec<LocalFile>,
    ttl: Duration,
) -> CmdResult<Transfer> {
    let label = transfers::label(files.iter().map(|f| f.name.as_str()));
    let total = files.iter().map(|f| f.size).sum();
    let sources = LocalFiles::new(files.into_iter().map(|f| (f.path, f.size)).collect());
    let handle = app.clone();
    transfers::start(
        app,
        Direction::Upload,
        label.clone(),
        total,
        move |reporter, cancel| async move {
            let progress = |done, total| reporter.report(done, total);
            let sent = client
                .push_stream(&clip, sources, Some(ttl), &progress, cancel.notified())
                .await;
            let meta = sent.map_err(|e| match e {
                yacs_client::Error::Cancelled => "Upload cancelled.".to_owned(),
                e => format!("Sending {label} failed: {e}"),
            })?;
            let state = handle.state::<AppState>();
            state
                .clips
                .lock()
                .expect("clip cache lock poisoned")
                .insert(Entry { meta, clip });
            Ok(format!("Sent {label}."))
        },
    )
}

/// The big upload or download in progress, if any.
#[tauri::command]
pub fn transfer_status(app: AppHandle) -> Option<Transfer> {
    app.state::<Transfers>().current()
}

#[tauri::command]
pub fn cancel_transfer(app: AppHandle) {
    app.state::<Transfers>().cancel();
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Invite {
    url: String,
    /// `data:image/svg+xml` URL of the QR code.
    qr: String,
    warning: Option<String>,
}

/// For "Invite a device" (QR code and link). Only shown on request: it's the key.
#[tauri::command]
pub fn invite(state: State<'_, AppState>) -> CmdResult<Invite> {
    let link = invite_link(&state)?;
    let svg = qrcode::QrCode::new(&link.url)
        .map_err(|e| e.to_string())?
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(240, 240)
        .quiet_zone(true)
        .build();
    let qr = format!(
        "data:image/svg+xml,{}",
        url::form_urlencoded::byte_serialize(svg.as_bytes())
            .collect::<String>()
            .replace('+', "%20")
    );
    Ok(Invite {
        url: link.url,
        qr,
        warning: link.warning,
    })
}

fn invite_link(state: &AppState) -> CmdResult<pairing::Invite> {
    client(state)?;
    let spaces = state.spaces();
    let space = spaces
        .current()
        .ok_or("this computer isn't in a space yet")?;
    pairing::invite(space, spaces.token(&space.relay))
}

/// Puts `yacs` on the PATH and, if this computer is in a space, adds the
/// command to it. May wait for macOS's admin password prompt.
#[tauri::command]
pub async fn install_cli(app: AppHandle, state: State<'_, AppState>) -> CmdResult<()> {
    let link = invite_link(&state).ok().map(|link| link.url);
    let result = tauri::async_runtime::spawn_blocking(move || cli::install(link.as_deref()))
        .await
        .map_err(|e| e.to_string())?;
    let _ = app.emit(windows::EVENT_STATUS_CHANGED, ());
    result
}

#[tauri::command]
pub async fn uninstall_cli(app: AppHandle) -> CmdResult<()> {
    let result = tauri::async_runtime::spawn_blocking(cli::uninstall)
        .await
        .map_err(|e| e.to_string())?;
    let _ = app.emit(windows::EVENT_STATUS_CHANGED, ());
    result
}

/// Returns the new version, if there is one.
#[tauri::command]
pub async fn check_update(app: AppHandle) -> CmdResult<Option<String>> {
    update::check(&app).await
}

/// Restarts the app on success, so it only ever returns an error.
#[tauri::command]
pub async fn install_update(app: AppHandle) -> CmdResult<()> {
    update::install(&app).await
}

#[tauri::command]
pub fn hide_spotlight(app: AppHandle) {
    windows::hide_spotlight(&app);
}

#[tauri::command]
pub fn open_settings(app: AppHandle) {
    windows::show_settings(&app);
}

#[tauri::command]
pub fn hide_settings(app: AppHandle) {
    windows::hide_settings(&app);
}

/// While Settings records a shortcut (see `hotkey::pause`).
#[tauri::command]
pub fn pause_hotkey(app: AppHandle, state: State<'_, AppState>) {
    hotkey::pause(&app, &state.settings().hotkey);
}

#[tauri::command]
pub fn resume_hotkey(app: AppHandle, state: State<'_, AppState>) {
    let hotkey = state.settings().hotkey.clone();
    if let Err(e) = hotkey::resume(&app, &hotkey) {
        tracing::warn!(error = %e, "hotkey not registered again");
        *state.hotkey_error.lock().expect("lock poisoned") = Some(e);
        let _ = app.emit(windows::EVENT_STATUS_CHANGED, ());
    }
}
