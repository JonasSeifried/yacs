use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Wry};

use crate::{update, windows};

const TRAY_ID: &str = "main";

fn menu(app: &AppHandle, update: Option<&str>) -> tauri::Result<Menu<Wry>> {
    let open = MenuItem::with_id(app, "open", "Open YACS", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit YACS", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&open, &settings, &separator, &quit])?;
    if let Some(version) = update {
        let label = format!("Update to {version} and restart");
        menu.prepend(&PredefinedMenuItem::separator(app)?)?;
        menu.prepend(&MenuItem::with_id(
            app,
            "update",
            label,
            true,
            None::<&str>,
        )?)?;
    }
    Ok(menu)
}

/// Offer (or stop offering) an update in the tray menu.
pub fn set_update(app: &AppHandle, version: Option<&str>) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    match menu(app, version) {
        Ok(menu) => {
            let _ = tray.set_menu(Some(menu));
        }
        Err(e) => tracing::warn!(error = %e, "can't rebuild the tray menu"),
    }
}

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let menu = menu(app, None)?;

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
            "open" => windows::toggle_spotlight(app),
            "settings" => windows::show_settings(app),
            "quit" => app.exit(0),
            "update" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = update::install(&app).await {
                        tracing::warn!(error = %e, "update failed");
                        windows::show_settings(&app);
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
