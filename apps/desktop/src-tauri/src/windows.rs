//! The two windows: the frameless Spotlight panel and a regular Settings window.
//! Both are created hidden at startup and only ever shown/hidden afterwards,
//! so the hotkey opens Spotlight instantly.

use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

pub const SPOTLIGHT: &str = "spotlight";
pub const SETTINGS: &str = "settings";

/// Sent to the Spotlight webview every time it's shown, so it refreshes.
pub const EVENT_SPOTLIGHT_SHOWN: &str = "spotlight-shown";
/// Sent to all webviews when pairing or preferences change.
pub const EVENT_STATUS_CHANGED: &str = "status-changed";
/// Sent to an open Spotlight when the relay reports a change to the history.
pub const EVENT_CLIPS_CHANGED: &str = "clips-changed";

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let spotlight =
        WebviewWindowBuilder::new(app, SPOTLIGHT, WebviewUrl::App("spotlight.html".into()))
            .title("YACS")
            .inner_size(680.0, 440.0)
            .resizable(false)
            .decorations(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .visible_on_all_workspaces(true)
            .shadow(true)
            .visible(false)
            .center();
    // macOS draws the rounded panel itself on a transparent window; Windows 11
    // rounds undecorated windows with a shadow on its own.
    #[cfg(target_os = "macos")]
    let spotlight = spotlight.transparent(true);
    let spotlight = spotlight.build()?;

    let handle = app.clone();
    spotlight.on_window_event(move |event| {
        if let WindowEvent::Focused(false) = event {
            hide_spotlight(&handle);
        }
    });

    let settings =
        WebviewWindowBuilder::new(app, SETTINGS, WebviewUrl::App("settings.html".into()))
            .title("YACS Settings")
            .inner_size(520.0, 700.0)
            .min_inner_size(440.0, 520.0)
            .visible(false)
            .center()
            .build()?;

    let handle = app.clone();
    settings.on_window_event(move |event| {
        // Closing only hides: YACS keeps running in the tray.
        if let WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            if let Some(w) = handle.get_webview_window(SETTINGS) {
                let _ = w.hide();
            }
            return_focus(&handle);
        }
    });
    Ok(())
}

pub fn toggle_spotlight(app: &AppHandle) {
    let Some(w) = app.get_webview_window(SPOTLIGHT) else {
        return;
    };
    if w.is_visible().unwrap_or(false) {
        hide_spotlight(app);
    } else {
        unhide_app(app);
        let _ = w.center();
        let _ = w.show();
        let _ = w.set_focus();
        let _ = app.emit_to(SPOTLIGHT, EVENT_SPOTLIGHT_SHOWN, ());
    }
}

pub fn hide_spotlight(app: &AppHandle) {
    let Some(w) = app.get_webview_window(SPOTLIGHT) else {
        return;
    };
    if w.is_visible().unwrap_or(false) {
        let _ = w.hide();
        return_focus(app);
    }
}

pub fn show_settings(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(SETTINGS) {
        // Show Settings before hiding Spotlight, so focus isn't handed back
        // to the previous app in between.
        unhide_app(app);
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
    hide_spotlight(app);
}

/// Undo `return_focus`: windows of a hidden macOS app stay invisible even
/// after `show()`, so the app has to be unhidden first.
fn unhide_app(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    let _ = app.show();
    #[cfg(not(target_os = "macos"))]
    let _ = app;
}

/// On macOS, hiding a window of an accessory (menu bar) app leaves no app
/// focused. Hiding the app itself hands focus back to whatever was in front.
fn return_focus(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    {
        let visible = |label| {
            app.get_webview_window(label)
                .and_then(|w| w.is_visible().ok())
                .unwrap_or(false)
        };
        if !visible(SPOTLIGHT) && !visible(SETTINGS) {
            let _ = app.hide();
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = app;
}
