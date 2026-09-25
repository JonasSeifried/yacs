use std::path::Path;

use serde::Serialize;
use tauri::AppHandle;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};

use crate::TOGGLE_ARG;

/// Wayland doesn't let apps grab global shortcuts, so the user binds
/// `yacs-desktop --toggle` in their desktop's settings instead. Settings
/// shows how, in place of the shortcut recorder.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualShortcut {
    /// What the shortcut should run: this AppImage or the installed binary,
    /// with `--toggle`.
    command: String,
    appimage: bool,
    /// `gnome`, `kde`, `hyprland`, `sway` or `other`, from `XDG_CURRENT_DESKTOP`.
    desktop: &'static str,
}

/// `Some` in a Wayland session.
pub fn manual() -> Option<ManualShortcut> {
    let wayland = std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t == "wayland")
        || std::env::var_os("WAYLAND_DISPLAY").is_some();
    if !cfg!(target_os = "linux") || !wayland {
        return None;
    }
    // Set by the AppImage runtime; `current_exe` would be inside its mount.
    let appimage = std::env::var_os("APPIMAGE");
    let exe = match &appimage {
        Some(path) => path.into(),
        None => std::env::current_exe().ok()?,
    };
    Some(ManualShortcut {
        command: format!("{} {TOGGLE_ARG}", program(&exe)),
        appimage: appimage.is_some(),
        desktop: desktop(&std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default()),
    })
}

/// Just the name if it's on the usual PATH (the .deb and .rpm install to
/// /usr/bin), else the full path, quoted if needed.
fn program(exe: &Path) -> String {
    let on_path = matches!(
        exe.parent().and_then(Path::to_str),
        Some("/usr/bin" | "/usr/local/bin" | "/bin")
    );
    match exe.file_name().and_then(|n| n.to_str()) {
        Some(name) if on_path => name.to_owned(),
        _ => shell_quote(&exe.to_string_lossy()),
    }
}

fn shell_quote(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+".contains(c))
    {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// `XDG_CURRENT_DESKTOP` is a colon-separated list, e.g. `ubuntu:GNOME`.
fn desktop(current: &str) -> &'static str {
    let names: Vec<String> = current.split(':').map(str::to_ascii_lowercase).collect();
    let has = |name: &str| names.iter().any(|n| n == name);
    if has("gnome") {
        "gnome"
    } else if has("kde") {
        "kde"
    } else if has("hyprland") {
        "hyprland"
    } else if has("sway") {
        "sway"
    } else {
        "other"
    }
}

pub fn parse(hotkey: &str) -> Result<Shortcut, String> {
    hotkey
        .parse()
        .map_err(|e| format!("\"{hotkey}\" is not a valid shortcut: {e}"))
}

/// Does nothing on Wayland, where it wouldn't work (see `manual`).
pub fn register(app: &AppHandle, hotkey: &str) -> Result<(), String> {
    if manual().is_some() {
        return Ok(());
    }
    app.global_shortcut()
        .register(parse(hotkey)?)
        .map_err(|e| format!("couldn't register {hotkey}, another app may be using it ({e})"))
}

/// Off while Settings records a new shortcut: otherwise pressing the current
/// one opens Spotlight instead of reaching the recorder.
pub fn pause(app: &AppHandle, hotkey: &str) {
    if manual().is_some() {
        return;
    }
    if let Ok(shortcut) = parse(hotkey) {
        let _ = app.global_shortcut().unregister(shortcut);
    }
}

/// Undoes `pause`; does nothing if the hotkey is registered already.
pub fn resume(app: &AppHandle, hotkey: &str) -> Result<(), String> {
    if manual().is_some() || parse(hotkey).is_ok_and(|s| app.global_shortcut().is_registered(s)) {
        return Ok(());
    }
    register(app, hotkey)
}

/// Swap the active hotkey. If the new one can't be registered, the old one is
/// restored, so the user is never left without a way to open Spotlight.
pub fn replace(app: &AppHandle, old: &str, new: &str) -> Result<(), String> {
    if manual().is_some() {
        return Ok(());
    }
    let shortcuts = app.global_shortcut();
    let new_shortcut = parse(new)?;
    if old == new && shortcuts.is_registered(new_shortcut) {
        return Ok(());
    }
    if let Ok(old) = parse(old) {
        let _ = shortcuts.unregister(old);
    }
    register(app, new).inspect_err(|_| {
        let _ = register(app, old);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::DEFAULT_HOTKEY;

    #[test]
    fn parses_default_and_rejects_garbage() {
        assert!(parse(DEFAULT_HOTKEY).is_ok());
        assert!(parse("Alt+Shift+KeyV").is_ok());
        assert!(parse("Shift+").is_err());
        assert!(parse("Hyper+Banana").is_err());
    }

    #[test]
    fn names_the_program_to_bind() {
        assert_eq!(program(Path::new("/usr/bin/yacs-desktop")), "yacs-desktop");
        assert_eq!(
            program(Path::new("/home/me/Apps/YACS_0.2.4_amd64.AppImage")),
            "/home/me/Apps/YACS_0.2.4_amd64.AppImage"
        );
        assert_eq!(
            program(Path::new("/home/me/My Apps/it's.AppImage")),
            r"'/home/me/My Apps/it'\''s.AppImage'"
        );
    }

    #[test]
    fn reads_the_desktop() {
        assert_eq!(desktop("ubuntu:GNOME"), "gnome");
        assert_eq!(desktop("KDE"), "kde");
        assert_eq!(desktop("Hyprland"), "hyprland");
        assert_eq!(desktop("sway"), "sway");
        assert_eq!(desktop("X-Cinnamon"), "other");
        assert_eq!(desktop(""), "other");
    }
}
