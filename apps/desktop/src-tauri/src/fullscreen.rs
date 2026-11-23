//! Is a full-screen app in front, like a game (exclusive or borderless) or a
//! video? Then the hotkey leaves it alone instead of opening Spotlight over it.
//!
//! Asked when the hotkey is pressed, so it's about the app in front then:
//! YACS's own windows are never full screen. Windows only, where people game;
//! elsewhere the hotkey always works.

/// The front window covers a whole monitor. False when it can't tell, and
/// off Windows.
pub fn app_in_front() -> bool {
    imp::app_in_front()
}

/// Does `window` (left, top, right, bottom) cover `screen`? A pixel or two
/// short still counts: some games round their size.
#[cfg_attr(not(windows), allow(dead_code))]
fn covers(window: (f64, f64, f64, f64), screen: (f64, f64, f64, f64)) -> bool {
    const SLACK: f64 = 2.0;
    let (wl, wt, wr, wb) = window;
    let (sl, st, sr, sb) = screen;
    sr > sl
        && sb > st
        && wl <= sl + SLACK
        && wt <= st + SLACK
        && wr >= sr - SLACK
        && wb >= sb - SLACK
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::{HWND, RECT};
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GWL_STYLE, GetClassNameW, GetForegroundWindow, GetShellWindow, GetWindowLongW,
        GetWindowRect, IsZoomed, WS_CAPTION,
    };

    /// The foreground window against its monitor. Exclusive full screen and
    /// borderless windowed both cover the monitor. The desktop does too, so
    /// it's left out, and so are maximized windows with a title bar: with the
    /// taskbar set to hide, a maximized app covers the monitor but is no game.
    /// Some games maximize their borderless window; those have no title bar.
    pub fn app_in_front() -> bool {
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.is_null() || hwnd == GetShellWindow() || is_desktop(hwnd) {
                return false;
            }
            let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
            if IsZoomed(hwnd) != 0 && style & WS_CAPTION == WS_CAPTION {
                return false;
            }
            let mut window: RECT = std::mem::zeroed();
            if GetWindowRect(hwnd, &mut window) == 0 {
                return false;
            }
            let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            let mut info: MONITORINFO = std::mem::zeroed();
            info.cbSize = size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(monitor, &mut info) == 0 {
                return false;
            }
            super::covers(edges(&window), edges(&info.rcMonitor))
        }
    }

    fn edges(r: &RECT) -> (f64, f64, f64, f64) {
        (r.left.into(), r.top.into(), r.right.into(), r.bottom.into())
    }

    /// The wallpaper windows that are in front when you click the desktop.
    unsafe fn is_desktop(hwnd: HWND) -> bool {
        let mut name = [0u16; 32];
        let len = unsafe { GetClassNameW(hwnd, name.as_mut_ptr(), name.len() as i32) };
        let name = String::from_utf16_lossy(&name[..len.max(0) as usize]);
        matches!(name.as_str(), "Progman" | "WorkerW")
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn app_in_front() -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covering_the_screen() {
        let screen = (0.0, 0.0, 1920.0, 1080.0);
        assert!(covers(screen, screen));
        assert!(covers((-8.0, -8.0, 1928.0, 1088.0), screen));
        assert!(covers((1.0, 0.0, 1919.0, 1080.0), screen));
        // Maximized under a taskbar, or a zoomed Mac window under the menu bar.
        assert!(!covers((0.0, 0.0, 1920.0, 1040.0), screen));
        assert!(!covers((0.0, 25.0, 1920.0, 1080.0), screen));
        // Full screen on the other display.
        assert!(!covers((1920.0, 0.0, 3840.0, 1080.0), screen));
        assert!(covers(
            (1920.0, 0.0, 3840.0, 1080.0),
            (1920.0, 0.0, 3840.0, 1080.0)
        ));
    }
}
