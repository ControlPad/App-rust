//! On-disk persistence: settings + presets.
//!
//! Layout (under [`config_root`]):
//! ```text
//! settings.json                     -- global Settings
//! presets/
//!   <PresetName>/
//!     preset.json                   -- Preset (categories + assignments)
//! ```

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::Context;

use crate::model::{Preset, Settings};

pub fn config_root() -> PathBuf {
    if let Ok(p) = std::env::var("SLIDR_CONFIG_DIR") {
        return PathBuf::from(p);
    }
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("slidr")
}

pub fn settings_path() -> PathBuf {
    config_root().join("settings.json")
}

pub fn presets_dir() -> PathBuf {
    config_root().join("presets")
}

pub fn load_settings() -> Settings {
    let p = settings_path();
    match fs::read_to_string(&p) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
            log::warn!("settings parse failed ({e}); using defaults");
            Settings::default()
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Settings::default(),
        Err(e) => {
            log::warn!("settings read failed ({e}); using defaults");
            Settings::default()
        }
    }
}

pub fn save_settings(s: &Settings) -> anyhow::Result<()> {
    let path = settings_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("mkdir {parent:?}"))?;
    }
    let json = serde_json::to_string_pretty(s)?;
    write_atomic(&path, json.as_bytes())
}

pub fn list_presets() -> Vec<String> {
    let dir = presets_dir();
    let Ok(rd) = fs::read_dir(&dir) else { return Vec::new() };
    let mut names: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
        .collect();
    names.sort();
    names
}

pub fn load_preset(name: &str) -> anyhow::Result<Preset> {
    let path = presets_dir().join(name).join("preset.json");
    let data = fs::read_to_string(&path).with_context(|| format!("read {path:?}"))?;
    let preset: Preset = serde_json::from_str(&data)?;
    Ok(preset)
}

pub fn save_preset(preset: &Preset) -> anyhow::Result<()> {
    if preset.name.trim().is_empty() {
        anyhow::bail!("preset has no name");
    }
    let dir = presets_dir().join(&preset.name);
    fs::create_dir_all(&dir)?;
    let json = serde_json::to_string_pretty(preset)?;
    write_atomic(&dir.join("preset.json"), json.as_bytes())
}

pub fn delete_preset(name: &str) -> anyhow::Result<()> {
    let dir = presets_dir().join(name);
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
    Ok(())
}

fn write_atomic(path: &Path, data: &[u8]) -> anyhow::Result<()> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path)?;
    Ok(())
}
