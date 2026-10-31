//! Settings → Presets: the user's own EQ curves and effect chains — rename,
//! export to a file, import from one, delete. Built-in presets aren't listed;
//! they ship with Slidr and can't be changed.

use std::sync::Arc;

use parking_lot::Mutex;
use slint::{ComponentHandle, ModelRc, VecModel};

use crate::glue::{toast, Shared};
use crate::model::{PresetFile, Settings};
use crate::{AppWindow, UserPresetRow};

pub fn push(ui: &AppWindow, s: &Settings) {
    let count = |n: usize, one: &str, many: &str| if n == 1 { format!("1 {one}") } else { format!("{n} {many}") };
    let eq = s.eq_presets.iter().enumerate().map(|(i, p)| UserPresetRow {
        eq: true,
        index: i as i32,
        name: p.name.as_str().into(),
        detail: format!("EQ · {}", count(p.bands.len(), "band", "bands")).into(),
    });
    let fx = s.fx_presets.iter().enumerate().map(|(i, p)| UserPresetRow {
        eq: false,
        index: i as i32,
        name: p.name.as_str().into(),
        detail: format!("Effects · {}", p.chain.iter().map(|s| s.fx.name()).collect::<Vec<_>>().join(" → ")).into(),
    });
    ui.set_user_presets(ModelRc::new(VecModel::from(eq.chain(fx).collect::<Vec<_>>())));
}

/// The preset a row stands for, as it would be written to a file.
fn file_of(s: &Settings, eq: bool, i: i32) -> Option<PresetFile> {
    let i = usize::try_from(i).ok()?;
    if eq {
        s.eq_presets.get(i).cloned().map(PresetFile::Eq)
    } else {
        s.fx_presets.get(i).cloned().map(PresetFile::Effects)
    }
}

/// A name not taken yet among `taken`: "Name", else "Name (2)", "Name (3)", …
fn free_name(name: &str, taken: &[&str]) -> String {
    if !taken.contains(&name) {
        return name.to_string();
    }
    (2..).map(|n| format!("{name} ({n})")).find(|n| !taken.contains(&n.as_str())).unwrap_or_default()
}

fn save(ui: &AppWindow, s: &Shared) {
    if let Err(e) = crate::storage::save_settings(&s.settings) {
        log::warn!("could not save settings: {e:#}");
    }
    push(ui, &s.settings);
}

pub fn wire(ui: &AppWindow, shared: Arc<Mutex<Shared>>) {
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_user_preset_rename(move |eq, i, name| {
            let Some(ui) = weak.upgrade() else { return };
            let name = name.trim();
            let mut s = shared.lock();
            let i = i as usize;
            let taken: Vec<String> = if eq {
                s.settings.eq_presets.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, p)| p.name.clone()).collect()
            } else {
                s.settings.fx_presets.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, p)| p.name.clone()).collect()
            };
            if name.is_empty() || taken.iter().any(|t| t == name) {
                if !name.is_empty() {
                    toast(&ui, &format!("There already is a preset called \"{name}\""));
                }
                push(&ui, &s.settings);
                return;
            }
            let slot = if eq {
                s.settings.eq_presets.get_mut(i).map(|p| &mut p.name)
            } else {
                s.settings.fx_presets.get_mut(i).map(|p| &mut p.name)
            };
            if let Some(n) = slot {
                *n = name.to_string();
            }
            save(&ui, &s);
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_user_preset_delete(move |eq, i| {
            let Some(ui) = weak.upgrade() else { return };
            let mut s = shared.lock();
            let i = i as usize;
            if eq && i < s.settings.eq_presets.len() {
                s.settings.eq_presets.remove(i);
            } else if !eq && i < s.settings.fx_presets.len() {
                s.settings.fx_presets.remove(i);
            }
            save(&ui, &s);
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_user_preset_export(move |eq, i| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(file) = file_of(&shared.lock().settings, eq, i) else { return };
            let name = match &file {
                PresetFile::Eq(p) => p.name.clone(),
                PresetFile::Effects(p) => p.name.clone(),
            };
            let Some(dest) = rfd::FileDialog::new()
                .set_file_name(format!("slidr-{}-{name}.json", if eq { "eq" } else { "effects" }))
                .add_filter("Slidr preset", &["json"])
                .save_file()
            else {
                return;
            };
            let written = serde_json::to_string_pretty(&file)
                .map_err(std::io::Error::other)
                .and_then(|json| std::fs::write(&dest, json));
            match written {
                Ok(()) => toast(&ui, &format!("Exported to {}", dest.display())),
                Err(e) => toast(&ui, &format!("Export failed: {e}")),
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_user_preset_import(move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(path) = rfd::FileDialog::new().add_filter("Slidr preset", &["json"]).pick_file() else { return };
            let parsed = std::fs::read_to_string(&path).ok().and_then(|d| serde_json::from_str::<PresetFile>(&d).ok());
            let Some(file) = parsed else {
                toast(&ui, "Import failed: not an EQ or effects preset");
                return;
            };
            let mut s = shared.lock();
            // An import never overwrites one of yours: a taken name gets a number.
            let name = match file {
                PresetFile::Eq(mut p) => {
                    let presets = &mut s.settings.eq_presets;
                    p.name = free_name(&p.name, &presets.iter().map(|p| p.name.as_str()).collect::<Vec<_>>());
                    p.id = presets.iter().map(|p| p.id).max().unwrap_or(1000) + 1;
                    presets.push(p.clone());
                    p.name
                }
                PresetFile::Effects(mut p) => {
                    let presets = &mut s.settings.fx_presets;
                    p.name = free_name(&p.name, &presets.iter().map(|p| p.name.as_str()).collect::<Vec<_>>());
                    presets.push(p.clone());
                    p.name
                }
            };
            save(&ui, &s);
            toast(&ui, &format!("Imported preset \"{name}\""));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_get_a_free_name() {
        assert_eq!(free_name("Warm", &[]), "Warm");
        assert_eq!(free_name("Warm", &["Warm", "Warm (2)"]), "Warm (3)");
    }
}
