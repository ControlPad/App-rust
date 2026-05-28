//! Cross-platform autostart.
//!
//! * Linux: XDG `~/.config/autostart/slidr.desktop`
//! * Windows: `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\Slidr`

use std::path::PathBuf;

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
    let exe = exe.display();
    let exec_line = if start_minimized {
        format!("{exe} --hidden")
    } else {
        format!("{exe}")
    };
    let desktop = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Slidr\n\
         Exec={exec_line}\n\
         X-GNOME-Autostart-enabled=true\n\
         Terminal=false\n"
    );
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, desktop)?;
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn is_enabled() -> bool {
    autostart_file().exists()
}

#[cfg(target_os = "linux")]
fn autostart_file() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
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
    use windows::Win32::Foundation::HANDLE as _HANDLE;

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
            if start_minimized {
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

#[allow(dead_code)]
fn _path_unused() -> Option<PathBuf> {
    None
}
