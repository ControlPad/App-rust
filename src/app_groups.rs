//! Curated program categories ("Browsers", "Games", …) for process actions.
//!
//! A slider or button can target a *group* instead of one named executable, so a
//! newly installed browser or a freshly bought game is picked up without editing
//! the config. Membership comes from three places, cheapest first:
//!
//! 1. the bundled list in `assets/app_groups.json` (executable-name stems),
//! 2. path heuristics from the same file — anything under a game library folder
//!    is a game, whatever it happens to be called. This is what keeps the
//!    "Games" group useful without an impossible list of every game ever made,
//! 3. runtime discovery from the OS (Windows: browsers registered under
//!    `StartMenuInternet`), refreshed periodically.
//!
//! Users can extend or add groups with an `app_groups.json` in the Slidr config
//! directory; it is merged on top of the bundled file at startup.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

/// Sentinel prefix marking a stored target as a group id rather than a process
/// name. Written into `AudioStream::process` / `ButtonAction::property` by the
/// wizard, parsed back by [`parse_ref`].
const GROUP_PREFIX: &str = "@group:";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppGroup {
    pub id: String,
    pub name: String,
    /// Picker icon kind (matches the UI's `KindIcon`).
    #[serde(default)]
    pub icon: i32,
    /// Executable-name stems: a program matches when its name equals one of
    /// these or starts with it (".exe" stripped, case-insensitive).
    #[serde(default, rename = "match")]
    pub patterns: Vec<String>,
    /// Full-path fragments; a program matches when its image path contains one.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Name stems that veto a match (launchers, crash handlers, helpers).
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct GroupFile {
    #[serde(default)]
    groups: Vec<AppGroup>,
}

const BUNDLED: &str = include_str!("../assets/app_groups.json");

static GROUPS: OnceLock<Vec<AppGroup>> = OnceLock::new();

/// All known groups: the bundled list merged with the user's overrides.
pub fn groups() -> &'static [AppGroup] {
    GROUPS.get_or_init(|| {
        let mut list: Vec<AppGroup> = match serde_json::from_str::<GroupFile>(BUNDLED) {
            Ok(f) => f.groups,
            Err(e) => {
                // The bundled file ships inside the binary, so this is a build
                // mistake rather than a user problem — but never panic over it.
                log::error!("bundled app_groups.json is invalid ({e}); groups disabled");
                Vec::new()
            }
        };
        for extra in user_groups() {
            match list.iter_mut().find(|g| g.id == extra.id) {
                // Same id: extend the bundled group rather than replace it, so
                // adding one game doesn't throw away the curated list.
                Some(g) => {
                    g.patterns.extend(extra.patterns);
                    g.paths.extend(extra.paths);
                    g.exclude.extend(extra.exclude);
                    if !extra.name.is_empty() {
                        g.name = extra.name;
                    }
                }
                None => list.push(extra),
            }
        }
        list
    })
}

fn user_groups() -> Vec<AppGroup> {
    let path = crate::storage::config_root().join("app_groups.json");
    let Ok(text) = std::fs::read_to_string(&path) else { return Vec::new() };
    match serde_json::from_str::<GroupFile>(&text) {
        Ok(f) => {
            log::info!("app groups: merged {} user entries from {path:?}", f.groups.len());
            f.groups
        }
        Err(e) => {
            log::warn!("app groups: {path:?} is invalid ({e}); ignoring it");
            Vec::new()
        }
    }
}

pub fn group(id: &str) -> Option<&'static AppGroup> {
    groups().iter().find(|g| g.id == id)
}

/// Display name for a group id (falls back to the raw id).
pub fn display_name(id: &str) -> String {
    group(id).map(|g| g.name.clone()).unwrap_or_else(|| id.to_string())
}

/// Encode a group id as a stored target string.
pub fn make_ref(id: &str) -> String {
    format!("{GROUP_PREFIX}{id}")
}

/// Decode a stored target: `Some(group_id)` for a group, `None` for a plain
/// process name.
pub fn parse_ref(value: &str) -> Option<&str> {
    value.strip_prefix(GROUP_PREFIX)
}

/// Human-readable label for a stored target (group name, or the value itself).
pub fn label_for(value: &str) -> String {
    match parse_ref(value) {
        Some(id) => display_name(id),
        None => value.to_string(),
    }
}

/// Executable name without directories or a trailing ".exe", lowercased.
pub fn stem(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let base = base.strip_suffix(".exe").unwrap_or(base);
    base.to_lowercase()
}

/// Does `exe_name` (optionally with its full `exe_path`) belong to `group_id`?
pub fn matches(group_id: &str, exe_name: &str, exe_path: Option<&str>) -> bool {
    let Some(g) = group(group_id) else { return false };
    let name = stem(exe_name);
    if name.is_empty() {
        return false;
    }
    // A veto beats every rule below: launchers and crash handlers live in the
    // same folders as the programs they launch.
    if g.exclude.iter().any(|e| name.contains(&e.to_lowercase())) {
        return false;
    }
    // Name stems. Equality-or-prefix rather than `contains`, which would let a
    // three-letter stem claim half the process table.
    if g.patterns.iter().any(|m| {
        let m = stem(m);
        !m.is_empty() && (name == m || name.starts_with(&m))
    }) {
        return true;
    }
    // Path heuristics (game libraries and the like).
    if let Some(path) = exe_path {
        let path = path.to_lowercase();
        if g.paths.iter().any(|p| path.contains(&p.to_lowercase())) {
            return true;
        }
    }
    // OS-discovered members (browsers registered with Windows).
    discovered(group_id).iter().any(|d| &name == d)
}

// ── runtime discovery ───────────────────────────────────────────────────────

/// How long an OS-discovered member list is reused before re-querying.
const DISCOVERY_TTL: Duration = Duration::from_secs(300);

static DISCOVERED: OnceLock<Mutex<Option<(Instant, Vec<String>)>>> = OnceLock::new();

/// Executable stems the OS itself reports as belonging to `group_id`. Only
/// "browsers" has a registry to read today; every other group returns empty.
fn discovered(group_id: &str) -> Vec<String> {
    if group_id != "browsers" {
        return Vec::new();
    }
    let cell = DISCOVERED.get_or_init(|| Mutex::new(None));
    let mut slot = cell.lock();
    if let Some((at, list)) = slot.as_ref() {
        if at.elapsed() < DISCOVERY_TTL {
            return list.clone();
        }
    }
    let list = discover_browsers();
    log::debug!("app groups: discovered {} registered browser(s)", list.len());
    *slot = Some((Instant::now(), list.clone()));
    list
}

/// Browsers registered with Windows under `Clients\StartMenuInternet` — the same
/// list the "Default apps" page shows, so anything the user installed properly
/// is covered without us knowing its name in advance.
#[cfg(target_os = "windows")]
fn discover_browsers() -> Vec<String> {
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::System::Registry::{
        RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE,
        KEY_READ,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    let mut out: Vec<String> = Vec::new();
    let subkey = wide("SOFTWARE\\Clients\\StartMenuInternet");
    for root in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
        unsafe {
            let mut key = HKEY::default();
            if RegOpenKeyExW(root, PCWSTR(subkey.as_ptr()), 0, KEY_READ, &mut key).is_err() {
                continue;
            }
            for index in 0..64u32 {
                let mut buf = [0u16; 256];
                let mut len = buf.len() as u32;
                let r = RegEnumKeyExW(
                    key,
                    index,
                    PWSTR(buf.as_mut_ptr()),
                    &mut len,
                    None,
                    PWSTR::null(),
                    None,
                    None,
                );
                if r.is_err() {
                    break; // ERROR_NO_MORE_ITEMS, or nothing readable
                }
                // Subkey names are the browser's client name, which is usually
                // the executable ("firefox.exe", "Google Chrome").
                let s = stem(&String::from_utf16_lossy(&buf[..len as usize]));
                if !s.is_empty() && !out.contains(&s) {
                    out.push(s);
                }
            }
            let _ = RegCloseKey(key);
        }
    }
    out
}

#[cfg(not(target_os = "windows"))]
fn discover_browsers() -> Vec<String> {
    // No equivalent registry on Linux; the analogue would be .desktop files
    // registered for x-scheme-handler/http, if this is ever wanted there.
    Vec::new()
}

// ── running programs ────────────────────────────────────────────────────────

/// A running program: executable name plus its full image path when readable.
#[derive(Debug, Clone)]
pub struct RunningApp {
    pub name: String,
    pub path: Option<String>,
}

/// Running programs that belong to `group_id`.
pub fn running_in_group(group_id: &str) -> Vec<RunningApp> {
    running_apps()
        .into_iter()
        .filter(|a| matches(group_id, &a.name, a.path.as_deref()))
        .collect()
}

#[cfg(target_os = "windows")]
pub fn running_apps() -> Vec<RunningApp> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    let mut out = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snap, &mut entry).is_ok() {
            loop {
                let len = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
                if !name.is_empty() {
                    out.push(RunningApp { path: process_path(entry.th32ProcessID), name });
                }
                if Process32NextW(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    out
}

/// Full image path of a process, or `None` when it can't be opened (elevated or
/// protected processes). Path heuristics simply don't apply to those.
#[cfg(target_os = "windows")]
pub fn process_path(pid: u32) -> Option<String> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok =
            QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len)
                .is_ok();
        let _ = CloseHandle(h);
        if !ok || len == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

#[cfg(not(target_os = "windows"))]
pub fn running_apps() -> Vec<RunningApp> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir("/proc") else { return out };
    for e in rd.flatten() {
        let pid = e.file_name();
        let pid = pid.to_string_lossy();
        if !pid.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let path = std::fs::read_link(format!("/proc/{pid}/exe"))
            .ok()
            .map(|p| p.display().to_string());
        // `comm` is truncated to 15 chars, so prefer the resolved exe name.
        let name = path
            .as_deref()
            .and_then(|p| p.rsplit('/').next().map(str::to_string))
            .or_else(|| {
                std::fs::read_to_string(format!("/proc/{pid}/comm"))
                    .ok()
                    .map(|c| c.trim().to_string())
            })
            .unwrap_or_default();
        if !name.is_empty() {
            out.push(RunningApp { name, path });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_list_parses() {
        assert!(!groups().is_empty(), "bundled app_groups.json should load");
        assert!(group("browsers").is_some());
        assert!(group("games").is_some());
    }

    #[test]
    fn matches_by_name_stem() {
        assert!(matches("browsers", "firefox.exe", None));
        assert!(matches("browsers", "chrome", None));
        assert!(matches("music", "Spotify.exe", None));
        assert!(!matches("music", "notepad.exe", None));
    }

    #[test]
    fn exclusions_beat_name_and_path() {
        // Steam itself lives in the game library but is a launcher, not a game.
        assert!(!matches("games", "steam.exe", Some("C:\\Steam\\steamapps\\common\\x\\steam.exe")));
        assert!(!matches("browsers", "chrome_crashpad_handler.exe", None));
    }

    #[test]
    fn matches_unknown_game_by_library_path() {
        // The whole point: a game nobody listed, recognised by where it lives.
        assert!(matches(
            "games",
            "SomeBrandNewGame.exe",
            Some("D:\\SteamLibrary\\steamapps\\common\\Some Game\\SomeBrandNewGame.exe")
        ));
        assert!(!matches(
            "games",
            "SomeBrandNewGame.exe",
            Some("C:\\tools\\SomeBrandNewGame.exe")
        ));
    }

    #[test]
    fn group_refs_round_trip() {
        let r = make_ref("games");
        assert_eq!(parse_ref(&r), Some("games"));
        assert_eq!(parse_ref("spotify"), None);
        assert_eq!(label_for(&r), "Games");
        assert_eq!(label_for("spotify"), "spotify");
    }
}
