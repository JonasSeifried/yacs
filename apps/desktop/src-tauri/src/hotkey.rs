use tauri::AppHandle;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};

pub fn parse(hotkey: &str) -> Result<Shortcut, String> {
    hotkey
        .parse()
        .map_err(|e| format!("\"{hotkey}\" is not a valid shortcut: {e}"))
}

pub fn register(app: &AppHandle, hotkey: &str) -> Result<(), String> {
    app.global_shortcut()
        .register(parse(hotkey)?)
        .map_err(|e| format!("couldn't register {hotkey}, another app may be using it ({e})"))
}

/// Swap the active hotkey. If the new one can't be registered, the old one is
/// restored, so the user is never left without a way to open Spotlight.
pub fn replace(app: &AppHandle, old: &str, new: &str) -> Result<(), String> {
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
}
