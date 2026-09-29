//! The two windows: the frameless Spotlight panel and a regular Settings window.
//! Both are created hidden at startup and only ever shown/hidden afterwards,
//! so the hotkey opens Spotlight instantly.

use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

pub const SPOTLIGHT: &str = "spotlight";
pub const SETTINGS: &str = "settings";

/// Sent to the Spotlight webview every time it's shown, so it refreshes.
pub const EVENT_SPOTLIGHT_SHOWN: &str = "spotlight-shown";
/// Sent to the Settings webview every time it's shown, so it refreshes.
pub const EVENT_SETTINGS_SHOWN: &str = "settings-shown";
/// Sent to all webviews when pairing or preferences change.
pub const EVENT_STATUS_CHANGED: &str = "status-changed";
/// Sent to an open Spotlight when the relay reports a change to the history.
pub const EVENT_CLIPS_CHANGED: &str = "clips-changed";
/// To Settings, with the slot: one of the space's invites was taken.
pub const EVENT_INVITE_USED: &str = "invite-used";

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
    #[cfg(windows)]
    set_frame(&spotlight, spotlight.theme().unwrap_or(tauri::Theme::Light));

    let handle = app.clone();
    #[cfg(windows)]
    let window = spotlight.clone();
    spotlight.on_window_event(move |event| match event {
        WindowEvent::Focused(false) => hide_spotlight(&handle),
        #[cfg(windows)]
        WindowEvent::ThemeChanged(theme) => set_frame(&window, *theme),
        _ => {}
    });

    let settings =
        WebviewWindowBuilder::new(app, SETTINGS, WebviewUrl::App("settings.html".into()))
            .title("YACS Settings")
            .inner_size(520.0, 700.0)
            .min_inner_size(440.0, 520.0)
            .visible(false)
            .center()
            .build()?;
    #[cfg(windows)]
    set_frame(&settings, settings.theme().unwrap_or(tauri::Theme::Light));

    let handle = app.clone();
    #[cfg(windows)]
    let window = settings.clone();
    settings.on_window_event(move |event| match event {
        // Closing only hides: YACS keeps running in the tray.
        WindowEvent::CloseRequested { api, .. } => {
            api.prevent_close();
            hide_settings(&handle);
        }
        #[cfg(windows)]
        WindowEvent::ThemeChanged(theme) => set_frame(&window, *theme),
        _ => {}
    });
    Ok(())
}

/// With "Show accent colour on title bars and window borders" on, Windows 11
/// draws window borders, and Settings' title bar, in the accent colour. They're
/// drawn in the theme instead: the border in `--border` (so Spotlight skips its
/// own), the title bar in `--bg` and `--text`, like the page under it.
#[cfg(windows)]
fn set_frame(window: &tauri::WebviewWindow, theme: tauri::Theme) {
    use windows_sys::Win32::Graphics::Dwm::{
        DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DwmSetWindowAttribute,
    };
    // COLORREF is 0x00BBGGRR.
    let (border, caption, text): (u32, u32, u32) = match theme {
        // #35323f, #17161c, #eceaf2
        tauri::Theme::Dark => (0x003f_3235, 0x001c_1617, 0x00f2_eaec),
        // #e3e1ea, #f6f6f8, #1c1b22
        _ => (0x00ea_e1e3, 0x00f8_f6f6, 0x0022_1b1c),
    };
    let Ok(hwnd) = window.hwnd() else { return };
    // Each fails harmlessly before Windows 11, which has none of them, and
    // the caption ones do nothing on the undecorated Spotlight.
    for (attribute, color) in [
        (DWMWA_BORDER_COLOR, border),
        (DWMWA_CAPTION_COLOR, caption),
        (DWMWA_TEXT_COLOR, text),
    ] {
        unsafe {
            DwmSetWindowAttribute(
                hwnd.0,
                attribute as _,
                (&raw const color).cast(),
                size_of::<u32>() as u32,
            );
        }
    }
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
        // One YACS window at a time: Spotlight replaces Settings, as Settings
        // replaces Spotlight. Hidden after Spotlight has focus, so focus never
        // goes back to the previous app in between.
        if let Some(settings) = app.get_webview_window(SETTINGS) {
            let _ = settings.hide();
        }
        let _ = app.emit_to(SPOTLIGHT, EVENT_SPOTLIGHT_SHOWN, ());
        crate::live::wake(app);
        crate::update::check_in_background(app);
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
        let _ = app.emit_to(SETTINGS, EVENT_SETTINGS_SHOWN, ());
        crate::update::check_in_background(app);
    }
    hide_spotlight(app);
}

pub fn hide_settings(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(SETTINGS) {
        let _ = w.hide();
    }
    // In case it closed while recording a shortcut.
    crate::commands::resume_hotkey(app.clone(), app.state());
    return_focus(app);
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
