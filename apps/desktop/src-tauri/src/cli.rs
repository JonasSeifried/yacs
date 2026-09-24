//! Settings → Command line: puts the `yacs` command that ships inside the app
//! on the PATH, paired like this computer. Release builds bundle it next to
//! the app's own binary (`bundle.externalBin`, set in release.yml), so the
//! app's updates keep it current. Nothing happens unless the user asks.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliStatus {
    /// Release builds have the command; dev builds may not.
    available: bool,
    installed: bool,
    /// Where it goes: `/usr/local/bin/yacs`, or the folder added to PATH.
    location: Option<String>,
}

fn bundled() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let cli = exe.with_file_name(if cfg!(windows) { "yacs.exe" } else { "yacs" });
    cli.is_file().then_some(cli)
}

pub fn status() -> CliStatus {
    match bundled() {
        Some(cli) => CliStatus {
            available: true,
            installed: os::is_installed(&cli),
            location: Some(os::location(&cli)),
        },
        None => CliStatus {
            available: false,
            installed: false,
            location: None,
        },
    }
}

/// Installs, then pairs the command with `pairing_link` (see
/// `pairing::phone_link`), if this computer is paired.
pub fn install(pairing_link: Option<&str>) -> Result<(), String> {
    let cli = bundled().ok_or("this build of YACS doesn't include the yacs command")?;
    os::install(&cli)?;
    if let Some(link) = pairing_link {
        pair(&cli, link).map_err(|e| format!("Installed yacs, but couldn't pair it: {e}"))?;
    }
    Ok(())
}

/// Leaves the command's own pairing (`yacs unpair` forgets that).
pub fn uninstall() -> Result<(), String> {
    match bundled() {
        Some(cli) => os::uninstall(&cli),
        None => Ok(()),
    }
}

/// `yacs pair` reads the link from stdin when that isn't a terminal.
fn pair(cli: &Path, link: &str) -> Result<(), String> {
    let mut cmd = Command::new(cli);
    cmd.arg("pair")
        .env_remove("YACS_SERVER")
        .env_remove("YACS_TOKEN")
        .env_remove("YACS_PHRASE")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    hide_console(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(format!("{link}\n").as_bytes())
        .map_err(|e| e.to_string())?;
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr.trim().trim_start_matches("error: ").to_owned());
    }
    Ok(())
}

/// A console program started from a GUI app gets a console window otherwise.
fn hide_console(_cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        _cmd.creation_flags(CREATE_NO_WINDOW);
    }
}

/// A symlink in /usr/local/bin, which is on the default PATH. Creating it
/// usually needs an admin password, which macOS asks for.
#[cfg(target_os = "macos")]
mod os {
    use std::path::Path;
    use std::process::Command;

    const LINK: &str = "/usr/local/bin/yacs";

    pub fn location(_cli: &Path) -> String {
        LINK.into()
    }

    pub fn is_installed(cli: &Path) -> bool {
        std::fs::read_link(LINK).is_ok_and(|target| target == cli)
    }

    pub fn install(cli: &Path) -> Result<(), String> {
        let target = cli
            .to_str()
            .ok_or("YACS is in a folder with an unusual name")?;
        // Opened from the disk image or Downloads: that path goes away.
        if target.starts_with("/Volumes/") || target.contains("/AppTranslocation/") {
            return Err(
                "Move YACS to your Applications folder and open it from there first.".into(),
            );
        }
        shell(&format!(
            "mkdir -p /usr/local/bin && ln -sfn {} {LINK}",
            quote(target)
        ))
    }

    pub fn uninstall(cli: &Path) -> Result<(), String> {
        if !is_installed(cli) {
            return Ok(()); // not ours
        }
        shell(&format!("rm -f {LINK}"))
    }

    /// As the user if that's allowed, else as admin (macOS asks for the password).
    fn shell(script: &str) -> Result<(), String> {
        let direct = Command::new("/bin/sh")
            .args(["-c", script])
            .stderr(std::process::Stdio::null())
            .status();
        if direct.is_ok_and(|s| s.success()) {
            return Ok(());
        }
        let applescript = format!(
            "do shell script \"{}\" with administrator privileges",
            script.replace('\\', "\\\\").replace('"', "\\\"")
        );
        let output = Command::new("/usr/bin/osascript")
            .args(["-e", &applescript])
            .output()
            .map_err(|e| e.to_string())?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("-128") {
            return Err("Cancelled.".into());
        }
        Err(format!("couldn't change {LINK}: {}", stderr.trim()))
    }

    fn quote(s: &str) -> String {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// The app's folder (which has `yacs.exe`) on the user's PATH. No admin
/// needed; terminals opened afterwards see it.
#[cfg(windows)]
mod os {
    use std::path::Path;

    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, RegType};

    pub fn location(cli: &Path) -> String {
        dir(cli)
    }

    pub fn is_installed(cli: &Path) -> bool {
        read_path().is_ok_and(|(entries, _)| entries.iter().any(|e| same(e, &dir(cli))))
    }

    pub fn install(cli: &Path) -> Result<(), String> {
        let dir = dir(cli);
        let (mut entries, vtype) = read_path().map_err(|e| e.to_string())?;
        if entries.iter().any(|e| same(e, &dir)) {
            return Ok(());
        }
        entries.push(dir);
        write_path(&entries, vtype)
    }

    pub fn uninstall(cli: &Path) -> Result<(), String> {
        let dir = dir(cli);
        let (entries, vtype) = read_path().map_err(|e| e.to_string())?;
        let kept: Vec<String> = entries.iter().filter(|e| !same(e, &dir)).cloned().collect();
        if kept.len() == entries.len() {
            return Ok(());
        }
        write_path(&kept, vtype)
    }

    fn dir(cli: &Path) -> String {
        cli.parent().unwrap_or(cli).display().to_string()
    }

    fn same(entry: &str, dir: &str) -> bool {
        entry
            .trim_end_matches('\\')
            .eq_ignore_ascii_case(dir.trim_end_matches('\\'))
    }

    fn environment() -> std::io::Result<RegKey> {
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags("Environment", KEY_READ | KEY_WRITE)
    }

    /// Raw, so `%USERPROFILE%`-style entries stay unexpanded.
    fn read_path() -> std::io::Result<(Vec<String>, RegType)> {
        let value = match environment()?.get_raw_value("Path") {
            Ok(value) => value,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok((Vec::new(), RegType::REG_EXPAND_SZ));
            }
            Err(e) => return Err(e),
        };
        let wide: Vec<u16> = value
            .bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&c| c != 0)
            .collect();
        let entries = String::from_utf16_lossy(&wide)
            .split(';')
            .filter(|e| !e.is_empty())
            .map(str::to_owned)
            .collect();
        let vtype = match value.vtype {
            RegType::REG_SZ => RegType::REG_SZ,
            _ => RegType::REG_EXPAND_SZ,
        };
        Ok((entries, vtype))
    }

    fn write_path(entries: &[String], vtype: RegType) -> Result<(), String> {
        let bytes = entries
            .join(";")
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        environment()
            .and_then(|key| key.set_raw_value("Path", &winreg::RegValue { bytes, vtype }))
            .map_err(|e| format!("couldn't change your PATH: {e}"))?;
        broadcast();
        Ok(())
    }

    /// Tells Explorer, so terminals started from it get the new PATH.
    fn broadcast() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
        };
        let name: Vec<u16> = "Environment".encode_utf16().chain([0]).collect();
        let mut result = 0;
        // SAFETY: `name` is a NUL-terminated UTF-16 string that outlives the call.
        unsafe {
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                0,
                name.as_ptr() as isize,
                SMTO_ABORTIFHUNG,
                5000,
                &mut result,
            );
        }
    }
}

/// No desktop app bundle for other systems yet.
#[cfg(not(any(target_os = "macos", windows)))]
mod os {
    use std::path::Path;

    pub fn location(cli: &Path) -> String {
        cli.display().to_string()
    }

    pub fn is_installed(_cli: &Path) -> bool {
        false
    }

    pub fn install(_cli: &Path) -> Result<(), String> {
        Err("not supported on this system yet".into())
    }

    pub fn uninstall(_cli: &Path) -> Result<(), String> {
        Ok(())
    }
}
