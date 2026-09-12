//! Cross-platform autostart.
//!
//! * Linux: XDG `~/.config/autostart/slidr.desktop`
//! * Windows: `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\Slidr`

/// Whether this build has a system tray to minimise into. Only Windows does
/// (`src/tray.rs`), so elsewhere `--hidden` is never written into the autostart
/// entry: it would start a process with no window and no way to open one.
const HAS_TRAY: bool = cfg!(target_os = "windows");

#[cfg(target_os = "linux")]
pub fn set_enabled(enable: bool, start_minimized: bool) -> anyhow::Result<()> {
    use std::fs;
    let path = autostart_file();
    if !enable {
        if path.exists() {
            fs::remove_file(&path)?;
        }
        return Ok(());
    }
    let exe = std::env::current_exe()?;
    let mut exec_line = quote_exec_arg(&exe.to_string_lossy());
    if start_minimized && HAS_TRAY {
        exec_line.push_str(" --hidden");
    }
    let desktop = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Slidr\n\
         Comment=Arduino-based audio mixer and macro pad\n\
         Icon=slidr\n\
         Exec={exec_line}\n\
         Categories=AudioVideo;Audio;Utility;\n\
         X-GNOME-Autostart-enabled=true\n\
         Terminal=false\n"
    );
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, desktop)?;
    Ok(())
}

/// Quote a path for a desktop-entry `Exec=` value.
///
/// Unquoted `Exec` splits on whitespace, so an install under e.g.
/// `~/My Apps/slidr` would never launch. The desktop-entry spec wants the
/// argument in double quotes with `"`, `` ` ``, `$` and `\` backslash-escaped.
#[cfg(target_os = "linux")]
fn quote_exec_arg(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

#[cfg(target_os = "linux")]
pub fn is_enabled() -> bool {
    autostart_file().exists()
}

/// Rewrite the autostart entry if it exists but points at a different binary.
///
/// The entry stores an absolute path, so moving, renaming or reinstalling the
/// executable leaves a file that silently fails at login. Called once at
/// startup; a no-op when autostart is off or already current.
#[cfg(target_os = "linux")]
pub fn resync(start_minimized: bool) {
    let path = autostart_file();
    if !path.exists() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let current = quote_exec_arg(&exe.to_string_lossy());
    let stale = match std::fs::read_to_string(&path) {
        Ok(text) => !text
            .lines()
            .any(|l| l.strip_prefix("Exec=").is_some_and(|v| v.starts_with(&current))),
        Err(e) => {
            log::warn!("autostart: cannot read {}: {e}", path.display());
            return;
        }
    };
    if stale {
        log::info!("autostart entry points elsewhere; rewriting for {}", exe.display());
        if let Err(e) = set_enabled(true, start_minimized) {
            log::warn!("autostart: rewrite failed: {e}");
        }
    }
}

#[cfg(target_os = "linux")]
fn autostart_file() -> std::path::PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("autostart")
        .join("slidr.desktop")
}

#[cfg(target_os = "windows")]
pub fn set_enabled(enable: bool, start_minimized: bool) -> anyhow::Result<()> {
    use windows::core::HSTRING;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegSetValueExW, HKEY,
        HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
    };

    let subkey = HSTRING::from("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
    let name = HSTRING::from("Slidr");
    let mut hkey: HKEY = HKEY::default();
    unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &subkey,
            0,
            windows::core::PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut hkey,
            None,
        )
        .ok()?;
        if enable {
            let exe = std::env::current_exe()?;
            let mut cmd = format!("\"{}\"", exe.display());
            if start_minimized && HAS_TRAY {
                cmd.push_str(" --hidden");
            }
            let wide: Vec<u16> = cmd.encode_utf16().chain(std::iter::once(0)).collect();
            let bytes = std::slice::from_raw_parts(
                wide.as_ptr() as *const u8,
                wide.len() * 2,
            );
            RegSetValueExW(hkey, &name, 0, REG_SZ, Some(bytes)).ok()?;
        } else {
            let _ = RegDeleteValueW(hkey, &name);
        }
        RegCloseKey(hkey).ok()?;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
pub fn is_enabled() -> bool {
    // Best-effort: skipped (the settings file is the source of truth).
    false
}

/// The registry value is rewritten on every settings save, so there is nothing
/// to repair on Windows.
#[cfg(target_os = "windows")]
pub fn resync(_start_minimized: bool) {}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::quote_exec_arg;

    #[test]
    fn quotes_paths_with_spaces() {
        assert_eq!(quote_exec_arg("/home/u/My Apps/slidr"), "\"/home/u/My Apps/slidr\"");
    }

    #[test]
    fn escapes_reserved_characters() {
        assert_eq!(quote_exec_arg("/opt/a$b"), "\"/opt/a\\$b\"");
        assert_eq!(quote_exec_arg("/opt/a\"b"), "\"/opt/a\\\"b\"");
        assert_eq!(quote_exec_arg("/opt/a\\b"), "\"/opt/a\\\\b\"");
    }
}
