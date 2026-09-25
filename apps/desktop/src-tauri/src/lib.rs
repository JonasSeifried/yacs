//! YACS desktop: a tray app with a global hotkey that opens a Spotlight-style
//! panel for sending and receiving clips.

mod cli;
mod clipboard;
mod clips;
mod commands;
mod hotkey;
mod live;
mod pairing;
mod secrets;
mod settings;
mod state;
#[cfg(test)]
mod test_support;
mod transfers;
mod tray;
mod update;
mod windows;

use tauri::{Manager, RunEvent};
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_global_shortcut::ShortcutState;
use tracing_subscriber::EnvFilter;

use crate::state::AppState;

const TOGGLE_ARG: &str = "--toggle";

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("yacs_desktop_lib=info")),
        )
        .init();

    tauri::Builder::default()
        // Must come first. A second launch is forwarded here and exits:
        // `yacs-desktop --toggle` toggles Spotlight (for desktops where global
        // hotkeys are blocked, e.g. Wayland: bind the command there instead),
        // any other launch brings up Settings.
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if args.iter().any(|a| a == TOGGLE_ARG) {
                windows::toggle_spotlight(app);
            } else {
                windows::show_settings(app);
            }
        }))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state == ShortcutState::Pressed {
                        windows::toggle_spotlight(app);
                    }
                })
                .build(),
        )
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(|app| {
            // Menu bar app: no Dock icon, no Cmd+Tab entry.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let state = AppState::load(&app.path().app_config_dir()?);
            let hotkey = state.settings().hotkey.clone();
            let paired = state.client().is_some();
            app.manage(state);
            app.manage(update::Updates::default());
            app.manage(live::Live::default());
            app.manage(transfers::Transfers::default());
            live::restart(app.handle());

            windows::create(app.handle())?;
            tray::create(app.handle())?;
            if let Err(e) = hotkey::register(app.handle(), &hotkey) {
                tracing::warn!(error = %e, "hotkey not registered");
                *app.state::<AppState>()
                    .hotkey_error
                    .lock()
                    .expect("lock poisoned") = Some(e);
            }
            if !paired {
                windows::show_settings(app.handle());
            }
            update::spawn_checks(app.handle());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::status,
            commands::generate_phrase,
            commands::pair,
            commands::unpair,
            commands::save_preferences,
            commands::server_config,
            commands::list_clips,
            commands::get_clip,
            commands::clip_image,
            commands::copy_clip,
            commands::send_clipboard,
            commands::transfer_status,
            commands::cancel_transfer,
            commands::delete_clip,
            commands::set_default_ttl,
            commands::phone_pairing,
            commands::install_cli,
            commands::uninstall_cli,
            commands::check_update,
            commands::install_update,
            commands::hide_spotlight,
            commands::open_settings,
        ])
        .build(tauri::generate_context!())
        .expect("failed to build YACS")
        .run(|_app, event| {
            // Hiding the last window must not quit a tray app; only "Quit" does.
            if let RunEvent::ExitRequested { api, code, .. } = event {
                if code.is_none() {
                    api.prevent_exit();
                }
            }
        });
}
