use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Wry};

use std::time::Duration;

use crate::commands::{self, CmdResult, Done};
use crate::{update, windows};

const TRAY_ID: &str = "main";

fn menu(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let send = MenuItem::with_id(app, "send", "Send clipboard", true, None::<&str>)?;
    let copy = MenuItem::with_id(app, "copy-newest", "Copy newest clip", true, None::<&str>)?;
    let open = MenuItem::with_id(app, "open", "Open YACS", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit YACS", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &send,
            &copy,
            &PredefinedMenuItem::separator(app)?,
            &open,
            &settings,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;
    let updates = app.state::<update::Updates>();
    let item = match (updates.installing(), updates.available()) {
        (Some(version), _) => Some((format!("Installing {version}…"), false)),
        (None, Some(version)) => Some((format!("Update to {version} and restart"), true)),
        (None, None) => None,
    };
    if let Some((label, enabled)) = item {
        menu.prepend(&PredefinedMenuItem::separator(app)?)?;
        menu.prepend(&MenuItem::with_id(
            app,
            "update",
            label,
            enabled,
            None::<&str>,
        )?)?;
    }
    Ok(menu)
}

/// Offer an update in the tray menu (or show it installing, or neither).
pub fn refresh(app: &AppHandle) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    match menu(app) {
        Ok(menu) => {
            let _ = tray.set_menu(Some(menu));
        }
        Err(e) => tracing::warn!(error = %e, "can't rebuild the tray menu"),
    }
}

/// How long the menu bar says what a tray action did.
const DONE_SHOWN: Duration = Duration::from_secs(2);

/// Runs a tray action. It says what it did next to the icon (macOS and
/// Linux show a title there), and a failure opens Spotlight with the reason.
fn run<F, Fut>(app: &AppHandle, action: F)
where
    F: FnOnce(AppHandle) -> Fut + Send + 'static,
    Fut: Future<Output = CmdResult<Done>> + Send + 'static,
{
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        match action(app.clone()).await {
            Ok(done) => {
                let Some(tray) = app.tray_by_id(TRAY_ID) else {
                    return;
                };
                let _ = tray.set_title(Some(match done {
                    Done::Sent => "Sent",
                    Done::Copied => "Copied",
                    Done::Transferring => "Transferring…",
                }));
                tokio::time::sleep(DONE_SHOWN).await;
                let _ = tray.set_title(None::<&str>);
            }
            Err(e) => windows::show_error(&app, e.to_string()),
        }
    });
}

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let menu = menu(app)?;

    // macOS menu bar icons are monochrome templates that adapt to light/dark;
    // the Windows tray shows the colored app icon.
    #[cfg(target_os = "macos")]
    let icon = Image::from_bytes(include_bytes!("../icons/tray-template.png"))?;
    #[cfg(not(target_os = "macos"))]
    let icon = Image::from_bytes(include_bytes!("../icons/32x32.png"))?;

    let tray = TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .icon_as_template(cfg!(target_os = "macos"))
        .tooltip("YACS")
        .menu(&menu)
        // macOS convention: click opens the menu. Windows: left click opens
        // the app, right click opens the menu.
        .show_menu_on_left_click(cfg!(target_os = "macos"))
        .on_menu_event(|app, event| match event.id().as_ref() {
            "send" => run(
                app,
                |app| async move { commands::send_from_tray(&app).await },
            ),
            "copy-newest" => run(app, |app| async move {
                commands::copy_newest_from_tray(&app).await
            }),
            "open" => windows::toggle_spotlight(app),
            "settings" => windows::show_settings(app),
            "quit" => app.exit(0),
            "update" => {
                // Settings shows the install running, and why it failed
                // (`Status::update_error`).
                windows::show_settings(app);
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = update::install(&app).await {
                        tracing::warn!(error = %e, "update failed");
                    }
                });
            }
            _ => {}
        });

    #[cfg(not(target_os = "macos"))]
    let tray = tray.on_tray_icon_event(|tray, event| {
        use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            windows::toggle_spotlight(tray.app_handle());
        }
    });

    tray.build(app)?;
    Ok(())
}
