//! Bridge between the Slint UI and the Rust core.
//!
//! `wire()` attaches every callback and starts a polling timer that drains
//! [`SerialEvent`]s from the worker thread into Slint model properties.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use parking_lot::Mutex;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::audio::AudioBackend;
use crate::events::EventEngine;
use crate::model::{
    led_for_button, next_id, ActionKind, AudioKind, AudioStream, BrightnessMode, ButtonAction,
    ButtonCategory, CombineLogic, Comparison, HttpMethod, LedCondition, LedConditionKind, LedConfig,
    LedControl, LedMode, LedModeKind, Preset, Settings, SliderCategory, ThemeMode,
};
use crate::protocol::{Frame, NUM_BUTTONS, NUM_SLIDERS};
use crate::serial::{RetryKicker, SerialEvent};
use crate::{
    AppWindow, AssignPick, ButtonCell, CategorySummary, LedConditionUI, LineItem, PickerEntry,
    SliderCell, WizardResult,
};

pub struct Shared {
    pub settings: Settings,
    pub preset: Preset,
    pub engine: EventEngine,
    pub audio: Arc<dyn AudioBackend>,
    pub cmd_tx: crossbeam_channel::Sender<crate::events::Cmd>,
    pub kicker: RetryKicker,
    /// Live LED state published by the actuator (for the configure-LED popup).
    pub led_state: crate::led::LedStateHandle,
    pub pending_assign: Option<(i32, i32)>,
    pub pending_wizard: Option<(i32, u32)>,
    pub pending_retry_deadline: Option<Instant>,
    /// When editing an existing category entry, the index being replaced.
    pub editing_idx: Option<usize>,
    /// Working copy for the configure-LED popup: (led index, config being edited).
    pub editing_led: Option<(usize, LedConfig)>,
}

impl Shared {
    /// Push the full LED configuration set + brightness + experimental flag to
    /// the actuator's LED engine. Call after any change to LED config/settings.
    pub fn push_leds(&self) {
        let _ = self.cmd_tx.send(crate::events::Cmd::SetLedExperimental(self.settings.led_experimental));
        let _ = self
            .cmd_tx
            .send(crate::events::Cmd::SetLeds(Box::new(self.preset.leds.clone())));
    }
}

pub fn wire(
    ui: &AppWindow,
    shared: Arc<Mutex<Shared>>,
    rx: Receiver<SerialEvent>,
    build_stamp: &'static str,
) {
    ui.set_build_stamp(SharedString::from(build_stamp));

    // Push initial state from settings/preset → UI.
    {
        let s = shared.lock();
        push_settings_to_ui(ui, &s.settings, &s.preset.settings);
        push_preset_to_ui(ui, &s.preset);
        apply_appearance(ui, &s.preset.settings);
        s.push_leds();
    }

    // Empty initial slider/button cells.
    push_cells(ui, &[0; NUM_SLIDERS], &[0; NUM_BUTTONS], &shared.lock().preset);
    ui.set_connection_msg("Searching for Slidr device…".into());
    ui.set_retry_eta_secs(-1.0);
    ui.set_connected(false);

    // ─── callbacks ─────────────────────────────────────────────────────────
    {
        let shared = shared.clone();
        ui.on_retry_now(move || shared.lock().kicker.kick());
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_open_assign(move |kind, idx| {
            let s = shared.lock();
            let options: Vec<AssignPick> = if kind == 0 {
                s.preset.slider_categories.iter().map(|c| AssignPick {
                    id: c.id as i32, name: c.name.clone().into(),
                }).collect()
            } else {
                s.preset.button_categories.iter().map(|c| AssignPick {
                    id: c.id as i32, name: c.name.clone().into(),
                }).collect()
            };
            drop(s);
            if let Some(ui) = weak.upgrade() {
                ui.set_assign_options(ModelRc::new(VecModel::from(options)));
                ui.set_assign_target_kind(kind);
                ui.set_assign_target_index(idx);
                ui.set_assign_popup_open(true);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_apply_assign(move |chosen_id| {
            let Some(ui) = weak.upgrade() else { return };
            let target_kind = ui.get_assign_target_kind();
            let idx = ui.get_assign_target_index() as usize;
            let chosen = if chosen_id < 0 { None } else { Some(chosen_id as u32) };
            let mut s = shared.lock();
            if target_kind == 0 && idx < NUM_SLIDERS {
                s.preset.assignments.sliders[idx] = chosen;
            } else if target_kind == 1 && idx < NUM_BUTTONS {
                s.preset.assignments.buttons[idx] = chosen;
            }
            let _ = crate::storage::save_preset(&s.preset);
            push_preset_to_ui(&ui, &s.preset);
            let live = s.engine.state().clone();
            push_cells(&ui, &live.sliders, &live.buttons, &s.preset);
        });
    }

    // ─── category CRUD ─────────────────────────────────────────────────────
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_add_category(move |kind, name| {
            if name.is_empty() { return }
            let mut s = shared.lock();
            if kind == 0 {
                let id = next_id(&s.preset.slider_categories, |c| c.id);
                s.preset.slider_categories.push(SliderCategory { id, name: name.to_string(), streams: vec![], collapsed: false });
            } else {
                let id = next_id(&s.preset.button_categories, |c| c.id);
                s.preset.button_categories.push(ButtonCategory { id, name: name.to_string(), actions: vec![], collapsed: false });
            }
            let _ = crate::storage::save_preset(&s.preset);
            let Some(ui) = weak.upgrade() else { return };
            push_preset_to_ui(&ui, &s.preset);
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_create_preset(move |name| {
            if name.is_empty() { return }
            let new_preset = Preset {
                id: 1,
                name: name.to_string(),
                ..Default::default()
            };
            if crate::storage::save_preset(&new_preset).is_ok() {
                let mut s = shared.lock();
                s.preset = new_preset;
                s.settings.active_preset = name.to_string();
                let _ = crate::storage::save_settings(&s.settings);
                if let Some(ui) = weak.upgrade() {
                    push_preset_to_ui(&ui, &s.preset);
                    push_settings_to_ui(&ui, &s.settings, &s.preset.settings);
                    apply_appearance(&ui, &s.preset.settings);
                    s.push_leds();
                    refresh_home(&ui, &s);
                    toast(&ui, &format!("Created profile: {name}"));
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_rename_category(move |kind, id, name| {
            let mut s = shared.lock();
            if kind == 0 {
                if let Some(c) = s.preset.slider_categories.iter_mut().find(|c| c.id as i32 == id) {
                    c.name = name.to_string();
                }
            } else if let Some(c) = s.preset.button_categories.iter_mut().find(|c| c.id as i32 == id) {
                c.name = name.to_string();
            }
            let _ = crate::storage::save_preset(&s.preset);
            let Some(ui) = weak.upgrade() else { return };
            push_preset_to_ui(&ui, &s.preset);
            refresh_home(&ui, &s);  // update Home preview names without a board input
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_delete_category(move |kind, id| {
            let mut s = shared.lock();
            let id = id as u32;
            if kind == 0 {
                s.preset.slider_categories.retain(|c| c.id != id);
                for slot in s.preset.assignments.sliders.iter_mut() {
                    if *slot == Some(id) { *slot = None }
                }
            } else {
                s.preset.button_categories.retain(|c| c.id != id);
                for slot in s.preset.assignments.buttons.iter_mut() {
                    if *slot == Some(id) { *slot = None }
                }
            }
            let _ = crate::storage::save_preset(&s.preset);
            let Some(ui) = weak.upgrade() else { return };
            push_preset_to_ui(&ui, &s.preset);
            refresh_home(&ui, &s);
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_delete_line(move |kind, cat_id, idx| {
            let mut s = shared.lock();
            let cat_id = cat_id as u32;
            let idx = idx as usize;
            if kind == 0 {
                if let Some(c) = s.preset.slider_categories.iter_mut().find(|c| c.id == cat_id) {
                    if idx < c.streams.len() { c.streams.remove(idx); }
                }
            } else if let Some(c) = s.preset.button_categories.iter_mut().find(|c| c.id == cat_id) {
                if idx < c.actions.len() { c.actions.remove(idx); }
            }
            let _ = crate::storage::save_preset(&s.preset);
            let Some(ui) = weak.upgrade() else { return };
            push_preset_to_ui(&ui, &s.preset);
        });
    }

    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_toggle_category_collapse(move |kind, id| {
            let mut s = shared.lock();
            let id = id as u32;
            if kind == 0 {
                if let Some(c) = s.preset.slider_categories.iter_mut().find(|c| c.id == id) {
                    c.collapsed = !c.collapsed;
                }
            } else if let Some(c) = s.preset.button_categories.iter_mut().find(|c| c.id == id) {
                c.collapsed = !c.collapsed;
            }
            let _ = crate::storage::save_preset(&s.preset);
            let Some(ui) = weak.upgrade() else { return };
            push_preset_to_ui(&ui, &s.preset);
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        // Reorder category cards (cosmetic, but persisted via Vec order).
        ui.on_reorder_category(move |kind, from_id, to_index| {
            let mut s = shared.lock();
            let from_id = from_id as u32;
            let to = to_index.max(0) as usize;
            if kind == 0 {
                reorder_by_id(&mut s.preset.slider_categories, from_id, to, |c| c.id);
            } else {
                reorder_by_id(&mut s.preset.button_categories, from_id, to, |c| c.id);
            }
            let _ = crate::storage::save_preset(&s.preset);
            let Some(ui) = weak.upgrade() else { return };
            push_preset_to_ui(&ui, &s.preset);
            refresh_home(&ui, &s);
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        // Move an action/stream within or between categories (same target-kind).
        ui.on_move_line(move |kind, from_cat, from_idx, to_cat, to_idx| {
            let mut s = shared.lock();
            let (from_cat, to_cat) = (from_cat as u32, to_cat as u32);
            let (from_idx, to_idx) = (from_idx.max(0) as usize, to_idx.max(0) as usize);
            if kind == 0 {
                move_line(&mut s.preset.slider_categories, from_cat, from_idx, to_cat, to_idx,
                    |c| c.id, |c| &mut c.streams);
            } else {
                move_line(&mut s.preset.button_categories, from_cat, from_idx, to_cat, to_idx,
                    |c| c.id, |c| &mut c.actions);
            }
            let _ = crate::storage::save_preset(&s.preset);
            let Some(ui) = weak.upgrade() else { return };
            push_preset_to_ui(&ui, &s.preset);
        });
    }

    // ─── wizard ────────────────────────────────────────────────────────────
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_open_wizard(move |kind, cat_id| {
            let Some(ui) = weak.upgrade() else { return };
            {
                let mut s = shared.lock();
                s.pending_wizard = Some((kind, cat_id as u32));
                s.editing_idx = None;
            }
            ui.set_wizard_target_kind(kind);
            ui.set_wizard_category_id(cat_id);
            ui.set_wizard_step(0);
            ui.set_wizard_kind(0);
            ui.set_wizard_property("".into());
            ui.set_wizard_display("".into());
            populate_live_lists(&ui, shared.clone());
            ui.set_wizard_filter("".into());
            push_wizard_picker(&ui, shared.clone());
            ui.set_wizard_open(true);
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_edit_line(move |target_kind, cat_id, idx| {
            let Some(ui) = weak.upgrade() else { return };
            let idx = idx as usize;
            let cat_id = cat_id as u32;
            // Resolve existing entry → wizard kind/property/display.
            let (wkind, prop, disp) = {
                let s = shared.lock();
                if target_kind == 0 {
                    let Some(c) = s.preset.slider_categories.iter().find(|c| c.id == cat_id) else { return };
                    let Some(stream) = c.streams.get(idx) else { return };
                    if let Some(p) = &stream.process { (0, p.clone(), p.clone()) }
                    else if let Some(m) = &stream.mic_name { (1, m.clone(), m.clone()) }
                    else { (2, stream.device_name.clone().unwrap_or_default(),
                            stream.device_name.clone().unwrap_or_default()) }
                } else {
                    let Some(c) = s.preset.button_categories.iter().find(|c| c.id == cat_id) else { return };
                    let Some(a) = c.actions.get(idx) else { return };
                    let k = action_kind_index(a.kind);
                    let prop = a.property.clone().unwrap_or_default();
                    // For KeyPress, show the friendly name in the picker.
                    let shown = if a.kind == ActionKind::KeyPress {
                        prop.parse::<u32>().ok()
                            .and_then(crate::keys_library::label_for_vk)
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| a.display.clone().unwrap_or(prop.clone()))
                    } else {
                        prop.clone()
                    };
                    (k, shown, a.display.clone().unwrap_or_default())
                }
            };
            shared.lock().editing_idx = Some(idx);
            ui.set_wizard_target_kind(target_kind);
            ui.set_wizard_category_id(cat_id as i32);
            ui.set_wizard_kind(wkind);
            ui.set_wizard_step(1);  // jump straight to the target step
            ui.set_wizard_property(prop.into());
            ui.set_wizard_display(disp.into());
            populate_live_lists(&ui, shared.clone());
            ui.set_wizard_filter("".into());
            push_wizard_picker(&ui, shared.clone());
            ui.set_wizard_open(true);
        });
    }
    {
        let shared = shared.clone();
        let weak = ui.as_weak();
        ui.on_refresh_live_lists(move || {
            let Some(ui) = weak.upgrade() else { return };
            populate_live_lists(&ui, shared.clone());
            push_wizard_picker(&ui, shared.clone());
        });
    }
    {
        let shared = shared.clone();
        let weak = ui.as_weak();
        ui.on_wizard_state_changed(move || {
            let Some(ui) = weak.upgrade() else { return };
            push_wizard_picker(&ui, shared.clone());
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_browse_file(move || {
            let path = rfd::FileDialog::new().pick_file();
            if let Some(p) = path {
                if let Some(ui) = weak.upgrade() {
                    let s = p.display().to_string();
                    ui.set_wizard_property(s.clone().into());
                    let name = p.file_stem().map(|s| s.to_string_lossy().to_string())
                                .unwrap_or(s.clone());
                    ui.set_wizard_display(name.into());
                }
            }
        });
    }
    {
        let shared = shared.clone();
        let weak = ui.as_weak();
        // Multi-select picker (Cycle output): toggle a device in/out of the
        // newline-separated set held in the wizard property.
        ui.on_wizard_toggle_pick(move |name| {
            let Some(ui) = weak.upgrade() else { return };
            let name = name.to_string();
            let mut list: Vec<String> = ui
                .get_wizard_property()
                .to_string()
                .lines()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            if let Some(pos) = list.iter().position(|n| n == &name) {
                list.remove(pos);
            } else {
                list.push(name);
            }
            ui.set_wizard_property(list.join("\n").into());
            push_wizard_picker(&ui, shared.clone());
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_wizard_finish(move |result| {
            let mut s = shared.lock();
            let editing = s.editing_idx.take();
            let cat_id = result.category_id as u32;
            if result.target_kind == 0 {
                if let Some(c) = s.preset.slider_categories.iter_mut().find(|c| c.id == cat_id) {
                    let stream = match result.kind {
                        0 => AudioStream { process: Some(result.property.into()), ..Default::default() },
                        1 => AudioStream { mic_name: Some(result.property.into()), ..Default::default() },
                        _ => AudioStream { device_name: Some(result.property.into()), ..Default::default() },
                    };
                    match editing {
                        Some(i) if i < c.streams.len() => c.streams[i] = stream,
                        _ => c.streams.push(stream),
                    }
                }
            } else if let Some(c) = s.preset.button_categories.iter_mut().find(|c| c.id == cat_id) {
                let kind = button_kind_from_index(result.kind);
                // For KeyPress, picker stores the friendly name; resolve to VK code.
                let (prop, disp) = if kind == ActionKind::KeyPress {
                    let name = result.property.to_string();
                    let vk = crate::keys_library::KEYS.iter()
                        .find(|k| k.name.eq_ignore_ascii_case(&name))
                        .map(|k| k.vk)
                        .or_else(|| name.parse::<u32>().ok())
                        .unwrap_or(0);
                    (Some(vk.to_string()), Some(name))
                } else if kind == ActionKind::CycleOutput {
                    // Property is the newline-separated device list (empty = all).
                    // The human-readable label is derived at render time
                    // (`action_secondary`), so no display is stored here.
                    let list = result.property.to_string();
                    let has_any = list.lines().any(|s| !s.is_empty());
                    (has_any.then_some(list), None)
                } else {
                    let prop = if result.property.is_empty() { None } else { Some(result.property.to_string()) };
                    let disp = if result.display.is_empty() { None } else { Some(result.display.to_string()) };
                    (prop, disp)
                };
                let action = ButtonAction { kind, property: prop, display: disp };
                match editing {
                    Some(i) if i < c.actions.len() => c.actions[i] = action,
                    _ => c.actions.push(action),
                }
            }
            let _ = crate::storage::save_preset(&s.preset);
            let Some(ui) = weak.upgrade() else { return };
            push_preset_to_ui(&ui, &s.preset);
            toast(&ui, if editing.is_some() { "Updated." } else { "Added." });
        });
    }

    // ─── settings ──────────────────────────────────────────────────────────
    {
        let shared = shared.clone();
        let weak = ui.as_weak();
        ui.on_theme_changed(move |idx| {
            let mode = match idx {
                1 => ThemeMode::Light, 2 => ThemeMode::Dark, _ => ThemeMode::System,
            };
            {
                let mut s = shared.lock();
                s.preset.settings.theme = mode;
                let _ = crate::storage::save_preset(&s.preset);
            }
            if let Some(ui) = weak.upgrade() {
                apply_color_scheme(&ui, mode);
            }
        });
    }
    {
        let shared = shared.clone();
        let weak = ui.as_weak();
        ui.on_curve_preset_changed(move |idx| {
            let mut s = shared.lock();
            let preset = index_to_curve_preset(idx);
            s.preset.settings.curve_preset = preset;
            // Custom starts from a gentle default each time it's freshly selected.
            if matches!(preset, crate::curve::CurvePreset::Custom) {
                s.preset.settings.custom_curve = crate::curve::BezierPoints::CUSTOM_DEFAULT;
            }
            let _ = crate::storage::save_preset(&s.preset);
            if let Some(ui) = weak.upgrade() {
                push_curve_to_ui(&ui, &s.preset.settings);
            }
        });
    }
    {
        let shared = shared.clone();
        let weak = ui.as_weak();
        ui.on_curve_changed(move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut s = shared.lock();
            s.preset.settings.custom_curve = crate::curve::BezierPoints {
                x1: ui.get_curve_x1(), y1: ui.get_curve_y1(),
                x2: ui.get_curve_x2(), y2: ui.get_curve_y2(),
            };
            s.preset.settings.curve_preset = crate::curve::CurvePreset::Custom;
            let _ = crate::storage::save_preset(&s.preset);
        });
    }
    {
        ui.on_exit_app(|| slint::quit_event_loop().ok().unwrap_or(()));
    }
    {
        let shared = shared.clone();
        let weak = ui.as_weak();
        ui.on_rename_preset(move |old, new| {
            if old.is_empty() || new.is_empty() || old == new { return }
            // Read the old preset, write under new name, delete old.
            if let Ok(mut p) = crate::storage::load_preset(&old) {
                p.name = new.to_string();
                if crate::storage::save_preset(&p).is_ok() {
                    let _ = crate::storage::delete_preset(&old);
                    let mut s = shared.lock();
                    if s.settings.active_preset == old.as_str() {
                        s.settings.active_preset = new.to_string();
                        let _ = crate::storage::save_settings(&s.settings);
                    }
                    if s.preset.name == old.as_str() {
                        s.preset.name = new.to_string();
                    }
                    if let Some(ui) = weak.upgrade() {
                        push_preset_to_ui(&ui, &s.preset);
                        toast(&ui, &format!("Renamed to {new}"));
                    }
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_export_preset(move |name| {
            let src = crate::storage::presets_dir().join(name.to_string()).join("preset.json");
            let dest = rfd::FileDialog::new()
                .set_file_name(format!("slidr-preset-{name}.json"))
                .add_filter("Slidr preset", &["json"])
                .save_file();
            let Some(dest) = dest else { return };
            match std::fs::copy(&src, &dest) {
                Ok(_) => if let Some(ui) = weak.upgrade() {
                    toast(&ui, &format!("Exported to {}", dest.display()));
                },
                Err(e) => if let Some(ui) = weak.upgrade() {
                    toast(&ui, &format!("Export failed: {e}"));
                },
            }
        });
    }
    {
        let shared = shared.clone();
        let weak = ui.as_weak();
        ui.on_import_preset(move || {
            let Some(path) = rfd::FileDialog::new()
                .add_filter("Slidr preset", &["json"])
                .pick_file() else { return };
            let Ok(data) = std::fs::read_to_string(&path) else { return };
            let Ok(mut preset) = serde_json::from_str::<Preset>(&data) else {
                if let Some(ui) = weak.upgrade() { toast(&ui, "Import failed: invalid preset file"); }
                return;
            };
            // Ensure a unique, non-empty name (avoid clobbering an existing one).
            if preset.name.trim().is_empty() {
                preset.name = path.file_stem().map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "Imported".into());
            }
            let existing = crate::storage::list_presets();
            let base = preset.name.clone();
            let mut n = 1;
            while existing.iter().any(|e| e == &preset.name) {
                n += 1;
                preset.name = format!("{base} ({n})");
            }
            match crate::storage::save_preset(&preset) {
                Ok(()) => if let Some(ui) = weak.upgrade() {
                    push_preset_to_ui(&ui, &shared.lock().preset);
                    toast(&ui, &format!("Imported profile: {}", preset.name));
                },
                Err(e) => if let Some(ui) = weak.upgrade() {
                    toast(&ui, &format!("Import failed: {e}"));
                },
            }
        });
    }
    {
        let shared = shared.clone();
        let weak = ui.as_weak();
        ui.on_save_settings(move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut s = shared.lock();
            // System → global settings; appearance/sliders → active profile.
            pull_global_from_ui(&ui, &mut s.settings);
            pull_profile_from_ui(&ui, &mut s.preset.settings);
            let _ = crate::storage::save_settings(&s.settings);
            let _ = crate::storage::save_preset(&s.preset);
            let _ = crate::autostart::set_enabled(s.settings.start_with_os, s.settings.start_minimized);
            // Live-apply appearance (accent / theme) immediately.
            apply_appearance(&ui, &s.preset.settings);
            // LED brightness / experimental flag may have changed.
            s.push_leds();
        });
    }
    {
        let shared = shared.clone();
        let weak = ui.as_weak();
        ui.on_save_preset(move || {
            let s = shared.lock();
            let result = crate::storage::save_preset(&s.preset);
            if let Some(ui) = weak.upgrade() {
                match result {
                    Ok(()) => toast(&ui, "Preset saved."),
                    Err(e) => toast(&ui, &format!("Save failed: {e}")),
                }
                push_preset_to_ui(&ui, &s.preset);
            }
        });
    }
    {
        let shared = shared.clone();
        let weak = ui.as_weak();
        ui.on_load_preset(move |name| {
            let Ok(p) = crate::storage::load_preset(&name) else { return };
            let mut s = shared.lock();
            s.preset = p;
            s.settings.active_preset = s.preset.name.clone();
            let _ = crate::storage::save_settings(&s.settings);
            if let Some(ui) = weak.upgrade() {
                push_preset_to_ui(&ui, &s.preset);
                push_settings_to_ui(&ui, &s.settings, &s.preset.settings);
                apply_appearance(&ui, &s.preset.settings);
                s.push_leds();
                // Refresh Home immediately with the new assignments using the
                // last live readings — no board input required.
                refresh_home(&ui, &s);
                toast(&ui, &format!("Switched to profile: {name}"));
            }
        });
    }
    {
        let shared = shared.clone();
        let weak = ui.as_weak();
        ui.on_delete_preset(move |name| {
            let _ = crate::storage::delete_preset(&name);
            if let Some(ui) = weak.upgrade() {
                push_preset_to_ui(&ui, &shared.lock().preset);
                toast(&ui, &format!("Deleted profile: {name}"));
            }
        });
    }
    {
        let shared = shared.clone();
        let weak = ui.as_weak();
        ui.on_set_active_preset(move |name| {
            let mut s = shared.lock();
            s.settings.active_preset = name.to_string();
            let _ = crate::storage::save_settings(&s.settings);
            if let Some(ui) = weak.upgrade() {
                push_settings_to_ui(&ui, &s.settings, &s.preset.settings);
            }
        });
    }

    // ─── LED state persistence (board EEPROM) ──────────────────────────────
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_led_save_state(move || {
            shared.lock().cmd_tx.send(crate::events::Cmd::LedSaveState).ok();
            if let Some(ui) = weak.upgrade() {
                toast(&ui, if ui.get_connected() { "Saved LED state to the board." }
                           else { "Not connected — connect the board first." });
            }
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_led_clear_state(move || {
            shared.lock().cmd_tx.send(crate::events::Cmd::LedClearSaved).ok();
            if let Some(ui) = weak.upgrade() {
                toast(&ui, if ui.get_connected() { "Cleared saved LED state (all off)." }
                           else { "Not connected — connect the board first." });
            }
        });
    }

    // ─── configure LED ─────────────────────────────────────────────────────
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_open_led_config(move |btn| {
            let Some(led) = led_for_button(btn as usize) else { return };
            let cfg = {
                let mut s = shared.lock();
                let cfg = s.preset.leds[led].clone().unwrap_or_default();
                s.editing_led = Some((led, cfg.clone()));
                cfg
            };
            let Some(ui) = weak.upgrade() else { return };
            ui.set_led_index(led as i32);
            ui.set_led_control(if cfg.control == LedControl::Manual { 1 } else { 0 });
            ui.set_led_combine(if cfg.combine == CombineLogic::All { 1 } else { 0 });
            // Populate the device lists first so target indices resolve.
            populate_live_lists(&ui, shared.clone());
            push_mode_to_ui(&ui, &cfg.active, true);
            push_mode_to_ui(&ui, &cfg.inactive, false);
            push_led_conditions(&ui, &cfg.conditions);
            ui.set_led_live_active(false);
            ui.set_led_popup_open(true);
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_led_cancel(move || {
            shared.lock().editing_led = None;
            if let Some(ui) = weak.upgrade() { ui.set_led_popup_open(false); }
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_led_save(move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut s = shared.lock();
            let Some((led, mut cfg)) = s.editing_led.take() else { return };
            cfg.control = if ui.get_led_control() == 1 { LedControl::Manual } else { LedControl::Conditional };
            cfg.combine = if ui.get_led_combine() == 1 { CombineLogic::All } else { CombineLogic::Any };
            cfg.active = led_mode_from_ui(
                ui.get_led_active_kind(),
                ui.get_led_active_on().as_str(),
                ui.get_led_active_off().as_str(),
                ui.get_led_active_cycle().as_str(),
                ui.get_led_active_bmode(),
                ui.get_led_active_bright(),
                ui.get_led_active_baudio(),
                ui.get_led_active_btarget().as_str(),
            );
            cfg.inactive = led_mode_from_ui(
                ui.get_led_inactive_kind(),
                ui.get_led_inactive_on().as_str(),
                ui.get_led_inactive_off().as_str(),
                ui.get_led_inactive_cycle().as_str(),
                ui.get_led_inactive_bmode(),
                ui.get_led_inactive_bright(),
                ui.get_led_inactive_baudio(),
                ui.get_led_inactive_btarget().as_str(),
            );
            s.preset.leds[led] = Some(cfg);
            let _ = crate::storage::save_preset(&s.preset);
            s.push_leds();
            drop(s);
            ui.set_led_popup_open(false);
            toast(&ui, "LED saved.");
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_led_add_condition(move || {
            let conds = {
                let mut s = shared.lock();
                let Some((_, cfg)) = s.editing_led.as_mut() else { return };
                cfg.conditions.push(LedCondition::default());
                cfg.conditions.clone()
            };
            if let Some(ui) = weak.upgrade() { push_led_conditions(&ui, &conds); }
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_led_remove_condition(move |idx| {
            let conds = {
                let mut s = shared.lock();
                let Some((_, cfg)) = s.editing_led.as_mut() else { return };
                let i = idx as usize;
                if i < cfg.conditions.len() { cfg.conditions.remove(i); }
                cfg.conditions.clone()
            };
            if let Some(ui) = weak.upgrade() { push_led_conditions(&ui, &conds); }
        });
    }
    {
        let shared = shared.clone();
        let weak = ui.as_weak();
        ui.on_led_refresh(move || {
            let Some(ui) = weak.upgrade() else { return };
            populate_live_lists(&ui, shared.clone());
            // Re-resolve dropdown indices against the freshly scanned lists.
            let conds = shared.lock().editing_led.as_ref().map(|(_, c)| c.conditions.clone());
            if let Some(conds) = conds {
                push_led_conditions(&ui, &conds);
            }
            ui.set_led_active_btidx(list_index_of(&ui, ui.get_led_active_baudio(), ui.get_led_active_btarget().as_str()));
            ui.set_led_inactive_btidx(list_index_of(&ui, ui.get_led_inactive_baudio(), ui.get_led_inactive_btarget().as_str()));
        });
    }
    {
        let shared = shared.clone();
        ui.on_led_manual_toggle(move || {
            let mut s = shared.lock();
            let Some((led, cfg)) = s.editing_led.as_mut() else { return };
            cfg.manual_active = !cfg.manual_active;
            let led = *led;
            let _ = s.cmd_tx.send(crate::events::Cmd::LedManualToggle(led as u8));
            if let Some(saved) = s.preset.leds.get_mut(led).and_then(|c| c.as_mut()) {
                if saved.control == LedControl::Manual {
                    saved.manual_active = !saved.manual_active;
                    let _ = crate::storage::save_preset(&s.preset);
                }
            }
        });
    }
    // Per-field condition mutators. Structural edits (kind/audio) re-push the
    // model so dependent fields show/hide; scalar/text edits update in place.
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_led_cond_kind(move |idx, v| {
            let conds = mutate_cond_struct(&shared, idx, |c| c.kind = cond_kind_from(v));
            if let (Some(ui), Some(conds)) = (weak.upgrade(), conds) { push_led_conditions(&ui, &conds); }
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_led_cond_audio(move |idx, v| {
            // Changing the endpoint family invalidates the previously picked target.
            let conds = mutate_cond_struct(&shared, idx, |c| {
                c.audio_kind = audio_kind_from(v);
                c.target = None;
            });
            if let (Some(ui), Some(conds)) = (weak.upgrade(), conds) { push_led_conditions(&ui, &conds); }
        });
    }
    {
        let shared = shared.clone();
        ui.on_led_cond_target(move |idx, v| {
            mutate_cond(&shared, idx, |c| c.target = opt(v.as_str()));
        });
    }
    {
        let shared = shared.clone();
        ui.on_led_cond_cmp(move |idx, v| { mutate_cond(&shared, idx, |c| c.cmp = cmp_from(v)); });
    }
    {
        let shared = shared.clone();
        ui.on_led_cond_value(move |idx, v| { mutate_cond(&shared, idx, |c| c.value = v.clamp(0, 100) as u8); });
    }
    {
        let shared = shared.clone();
        ui.on_led_cond_method(move |idx, v| { mutate_cond(&shared, idx, |c| c.api.method = HttpMethod::from_index(v)); });
    }
    {
        let shared = shared.clone();
        ui.on_led_cond_url(move |idx, v| { mutate_cond(&shared, idx, |c| c.api.url = v.to_string()); });
    }
    {
        let shared = shared.clone();
        ui.on_led_cond_payload(move |idx, v| { mutate_cond(&shared, idx, |c| c.api.payload = opt(v.as_str())); });
    }
    {
        let shared = shared.clone();
        ui.on_led_cond_bearer(move |idx, v| { mutate_cond(&shared, idx, |c| c.api.bearer = opt(v.as_str())); });
    }
    {
        let shared = shared.clone();
        ui.on_led_cond_interval(move |idx, v| {
            if let Ok(n) = v.as_str().trim().parse::<u32>() {
                mutate_cond(&shared, idx, |c| c.api.interval_ms = n);
            }
        });
    }
    {
        let shared = shared.clone();
        ui.on_led_cond_expect(move |idx, v| { mutate_cond(&shared, idx, |c| c.api.expect_body = opt(v.as_str())); });
    }

    // ─── serial event pump ────────────────────────────────────────────────
    let weak = ui.as_weak();
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(16), move || {
        let Some(ui) = weak.upgrade() else { return };
        let mut s = shared.lock();
        let mut changed = false;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                SerialEvent::Connected(port) => {
                    s.engine.set_connected(true);
                    ui.set_connected(true);
                    ui.set_connection_msg(format!("Connected on {port}").into());
                    ui.set_retry_eta_secs(-1.0);
                    // Board reset on connect; re-push all desired LED state.
                    s.push_leds();
                    changed = true;
                }
                SerialEvent::Disconnected { reason, retrying_in } => {
                    s.engine.set_connected(false);
                    ui.set_connected(false);
                    ui.set_connection_msg(reason.into());
                    ui.set_retry_eta_secs(retrying_in.as_secs_f32());
                    s.pending_retry_deadline = Some(Instant::now() + retrying_in);
                    changed = true;
                }
                SerialEvent::Frame(frame) => {
                    // Cheap diff on the UI thread; side effects go to the actuator.
                    let preset = s.preset.clone();
                    let (dirty, cmds) = s.engine.ingest(&frame, &preset);
                    let mut led_toggled = false;
                    for cmd in cmds {
                        // Persist a manual-LED toggle into the preset so it
                        // survives a restart (the actuator owns the live state).
                        if let crate::events::Cmd::LedManualToggle(led) = &cmd {
                            if let Some(cfg) = s.preset.leds.get_mut(*led as usize).and_then(|c| c.as_mut()) {
                                cfg.manual_active = !cfg.manual_active;
                                led_toggled = true;
                            }
                        }
                        let _ = s.cmd_tx.send(cmd);
                    }
                    if led_toggled {
                        let _ = crate::storage::save_preset(&s.preset);
                    }
                    if dirty { changed = true; }
                }
            }
        }
        // Mirror live LED state into the configure popup (manual-mode readout).
        if ui.get_led_popup_open() {
            let led = ui.get_led_index() as usize;
            let active = s.led_state.lock().get(led).map(|l| l.active).unwrap_or(false);
            ui.set_led_live_active(active);
        }

        // Animate retry countdown even between events.
        if let Some(deadline) = s.pending_retry_deadline {
            let remaining = deadline.saturating_duration_since(Instant::now()).as_secs_f32();
            if remaining > 0.0 {
                ui.set_retry_eta_secs(remaining);
            }
        }
        if changed {
            let live = s.engine.state().clone();
            push_cells(&ui, &live.sliders, &live.buttons, &s.preset);
            ui.set_badge_text(match live.badge {
                crate::protocol::Badge::Supporter => "Supporter".into(),
                crate::protocol::Badge::Premium => "Premium".into(),
                _ => "".into(),
            });
        }
    });
    // Leak the timer so it survives until window close.
    std::mem::forget(timer);
}

// Helpers ----------------------------------------------------------------

/// Refresh the Home live cells from the last known board state — used after
/// preset/category edits so names update without waiting for a board input.
fn refresh_home(ui: &AppWindow, s: &Shared) {
    let live = s.engine.state().clone();
    push_cells(ui, &live.sliders, &live.buttons, &s.preset);
}

fn push_cells(ui: &AppWindow, sliders: &[i32; NUM_SLIDERS], buttons: &[u8; NUM_BUTTONS], preset: &Preset) {
    let pts = crate::curve::BezierPoints::for_preset(
        preset_curve(preset).0, preset_curve(preset).1);
    let _ = pts; // curve preview unused at the cell level
    let slider_cells: Vec<SliderCell> = sliders
        .iter().enumerate().map(|(i, &v)| {
            let pct = crate::curve::normalize_raw(v);
            let cat = preset.slider_category(i);
            SliderCell {
                value: v,
                percent: pct,
                category: cat.map(|c| c.name.clone()).unwrap_or_default().into(),
                assigned: cat.is_some(),
            }
        }).collect();
    let button_cells: Vec<ButtonCell> = buttons
        .iter().enumerate().map(|(i, &b)| {
            let cat = preset.button_category(i);
            ButtonCell {
                pressed: b != 0,
                category: cat.map(|c| c.name.clone()).unwrap_or_default().into(),
                assigned: cat.is_some(),
            }
        }).collect();
    ui.set_sliders(ModelRc::new(VecModel::from(slider_cells)));
    let (top, right): (Vec<_>, Vec<_>) = button_cells.into_iter().enumerate()
        .partition(|(i, _)| *i < 6);
    let top: Vec<_> = top.into_iter().map(|(_, b)| b).collect();
    let right: Vec<_> = right.into_iter().map(|(_, b)| b).collect();
    ui.set_top_buttons(ModelRc::new(VecModel::from(top)));
    ui.set_right_buttons(ModelRc::new(VecModel::from(right)));
}

fn preset_curve(_preset: &Preset) -> (crate::curve::CurvePreset, crate::curve::BezierPoints) {
    (crate::curve::CurvePreset::Linear, crate::curve::BezierPoints::LINEAR)
}

/// Move the category with `from_id` to insertion index `to_index` (0..=len).
fn reorder_by_id<C>(cats: &mut Vec<C>, from_id: u32, to_index: usize, id: impl Fn(&C) -> u32) {
    let Some(from) = cats.iter().position(|c| id(c) == from_id) else { return };
    let item = cats.remove(from);
    // `to_index` was computed against the original list; removing the item
    // shifts everything after it down by one.
    let to = if from < to_index { to_index - 1 } else { to_index };
    cats.insert(to.min(cats.len()), item);
}

/// Move element `from_idx` of category `from_cat` to insertion index `to_idx` of
/// category `to_cat` (may be the same category). `to_idx` is in the destination's
/// original ordering.
fn move_line<C, E>(
    cats: &mut [C],
    from_cat: u32,
    from_idx: usize,
    to_cat: u32,
    to_idx: usize,
    id: impl Fn(&C) -> u32,
    lines: impl Fn(&mut C) -> &mut Vec<E>,
) {
    let Some(from_pos) = cats.iter().position(|c| id(c) == from_cat) else { return };
    let Some(to_pos) = cats.iter().position(|c| id(c) == to_cat) else { return };
    let item = {
        let v = lines(&mut cats[from_pos]);
        if from_idx >= v.len() {
            return;
        }
        v.remove(from_idx)
    };
    // Within the same category, a removal before the target shifts it down.
    let to = if from_cat == to_cat && from_idx < to_idx { to_idx - 1 } else { to_idx };
    let v = lines(&mut cats[to_pos]);
    v.insert(to.min(v.len()), item);
}

fn push_preset_to_ui(ui: &AppWindow, preset: &Preset) {
    let sliders: Vec<CategorySummary> = preset
        .slider_categories
        .iter()
        .map(|c| CategorySummary {
            id: c.id as i32,
            name: c.name.clone().into(),
            count: c.streams.len() as i32,
            collapsed: c.collapsed,
            lines: ModelRc::new(VecModel::from(
                c.streams.iter().enumerate().map(|(i, s)| LineItem {
                    id: i as i32,
                    primary: s.label().into(),
                    secondary: stream_secondary(s).into(),
                    icon_kind: stream_icon_kind(s),
                }).collect::<Vec<_>>(),
            )),
        })
        .collect();
    let buttons: Vec<CategorySummary> = preset
        .button_categories
        .iter()
        .map(|c| CategorySummary {
            id: c.id as i32,
            name: c.name.clone().into(),
            count: c.actions.len() as i32,
            collapsed: c.collapsed,
            lines: ModelRc::new(VecModel::from(
                c.actions.iter().enumerate().map(|(i, a)| LineItem {
                    id: i as i32,
                    primary: SharedString::from(a.kind.label()),
                    secondary: action_secondary(a).into(),
                    icon_kind: action_icon_kind(a.kind),
                }).collect::<Vec<_>>(),
            )),
        })
        .collect();
    ui.set_slider_categories(ModelRc::new(VecModel::from(sliders)));
    ui.set_button_categories(ModelRc::new(VecModel::from(buttons)));
    ui.set_active_preset_name(preset.name.clone().into());
    ui.set_preset_names(ModelRc::new(VecModel::from(
        crate::storage::list_presets().into_iter().map(SharedString::from).collect::<Vec<_>>(),
    )));
}

fn stream_icon_kind(s: &AudioStream) -> i32 {
    if s.process.is_some() { 0 }
    else if s.mic_name.is_some() { 1 }
    else { 2 }
}

/// Secondary line for a button action. Cycle-output derives its label from the
/// device list (so it stays correct without a stored display); others fall back
/// to the display, then the raw property.
fn action_secondary(a: &crate::model::ButtonAction) -> String {
    if a.kind == ActionKind::CycleOutput {
        let names: Vec<&str> =
            a.property.as_deref().unwrap_or("").lines().filter(|s| !s.is_empty()).collect();
        return if names.is_empty() {
            "All output devices".into()
        } else {
            names.join(" → ")
        };
    }
    a.display.clone().or_else(|| a.property.clone()).unwrap_or_default()
}

fn action_icon_kind(k: ActionKind) -> i32 {
    use ActionKind::*;
    match k {
        MuteProcess => 0, MuteMic => 1, MuteMainAudio => 5,
        OpenProcess => 0, OpenWebsite => 3, KeyPress => 4, CycleOutput => 2,
    }
}

fn stream_secondary(s: &AudioStream) -> String {
    if s.process.is_some() {
        "process".into()
    } else if s.mic_name.is_some() {
        "microphone".into()
    } else if s.device_name.is_some() {
        "output device".into()
    } else {
        "default output".into()
    }
}

/// Push system (global) + appearance/slider (per-profile) settings into the UI.
fn push_settings_to_ui(ui: &AppWindow, global: &Settings, profile: &crate::model::ProfileSettings) {
    // System (global)
    ui.set_start_with_os(global.start_with_os);
    ui.set_start_minimized(global.start_minimized);
    ui.set_minimize_to_tray(global.minimize_to_tray);
    ui.set_led_experimental(global.led_experimental);
    ui.set_active_preset_name(global.active_preset.clone().into());
    // Appearance + sliders (profile)
    ui.set_theme_index(match profile.theme {
        ThemeMode::System => 0, ThemeMode::Light => 1, ThemeMode::Dark => 2,
    });
    ui.set_accent_preset(profile.accent_preset);
    ui.set_dead_zone(profile.slider_dead_zone);
    ui.set_unmute_on_change(profile.unmute_on_change);
    ui.set_curve_preset_index(curve_preset_to_index(profile.curve_preset));
    push_curve_to_ui(ui, profile);
}

/// Push the active curve's control points + editability into the UI.
fn push_curve_to_ui(ui: &AppWindow, profile: &crate::model::ProfileSettings) {
    let pts = crate::curve::BezierPoints::for_preset(profile.curve_preset, profile.custom_curve);
    ui.set_curve_x1(pts.x1);
    ui.set_curve_y1(pts.y1);
    ui.set_curve_x2(pts.x2);
    ui.set_curve_y2(pts.y2);
    ui.set_curve_editable(matches!(profile.curve_preset, crate::curve::CurvePreset::Custom));
}

fn pull_global_from_ui(ui: &AppWindow, g: &mut Settings) {
    g.start_with_os = ui.get_start_with_os();
    g.start_minimized = ui.get_start_minimized();
    g.minimize_to_tray = ui.get_minimize_to_tray();
    g.led_experimental = ui.get_led_experimental();
}

fn pull_profile_from_ui(ui: &AppWindow, p: &mut crate::model::ProfileSettings) {
    p.theme = match ui.get_theme_index() { 1 => ThemeMode::Light, 2 => ThemeMode::Dark, _ => ThemeMode::System };
    p.accent_preset = ui.get_accent_preset();
    p.slider_dead_zone = ui.get_dead_zone();
    p.unmute_on_change = ui.get_unmute_on_change();
    p.curve_preset = index_to_curve_preset(ui.get_curve_preset_index());
}

/// Apply a profile's appearance (theme + accent) to the live UI.
fn apply_appearance(ui: &AppWindow, profile: &crate::model::ProfileSettings) {
    apply_color_scheme(ui, profile.theme);
    ui.set_accent_preset(profile.accent_preset);
    ui.invoke_apply_accent(profile.accent_preset);
}

fn curve_preset_to_index(p: crate::curve::CurvePreset) -> i32 {
    use crate::curve::CurvePreset::*;
    match p { Linear => 0, Ease => 1, EaseIn => 2, EaseOut => 3, EaseInOut => 4, Custom => 5 }
}
fn index_to_curve_preset(i: i32) -> crate::curve::CurvePreset {
    use crate::curve::CurvePreset::*;
    match i { 1 => Ease, 2 => EaseIn, 3 => EaseOut, 4 => EaseInOut, 5 => Custom, _ => Linear }
}

fn action_kind_index(k: ActionKind) -> i32 {
    use ActionKind::*;
    match k {
        MuteProcess => 0, MuteMainAudio => 1, MuteMic => 2,
        OpenProcess => 3, OpenWebsite => 4, KeyPress => 5, CycleOutput => 6,
    }
}

fn button_kind_from_index(i: i32) -> ActionKind {
    use ActionKind::*;
    match i {
        0 => MuteProcess, 1 => MuteMainAudio, 2 => MuteMic,
        3 => OpenProcess, 4 => OpenWebsite, 6 => CycleOutput, _ => KeyPress,
    }
}

fn apply_color_scheme(ui: &AppWindow, mode: ThemeMode) {
    // Trigger the slint-side `apply-theme(idx)` which sets `Palette.color-scheme`.
    let idx = match mode {
        ThemeMode::System => 0,
        ThemeMode::Light => 1,
        ThemeMode::Dark => 2,
    };
    ui.invoke_apply_theme(idx);
}

fn push_wizard_picker(ui: &AppWindow, shared: Arc<Mutex<Shared>>) {
    let target_kind = ui.get_wizard_target_kind();
    let kind = ui.get_wizard_kind();
    let filter = ui.get_wizard_filter().to_string().to_lowercase();

    let s = shared.lock();
    let source: Vec<String> = match (target_kind, kind) {
        // Slider streams
        (0, 0) => merged_processes(s.audio.as_ref()),
        (0, 1) => s.audio.list_mics(),
        (0, 2) => s.audio.list_outputs(),
        // Button actions
        (1, 0) => merged_processes(s.audio.as_ref()),
        (1, 1) => s.audio.list_outputs(),
        (1, 2) => s.audio.list_mics(),
        // Simulate key — full library
        (1, 5) => crate::keys_library::KEYS.iter().map(|k| k.name.to_string()).collect(),
        // Cycle output device — multi-select list of outputs
        (1, 6) => s.audio.list_outputs(),
        _ => Vec::new(),
    };
    drop(s);

    // Multi-select kinds (currently Cycle output) keep their chosen set in the
    // wizard property as a newline-separated list; flag matching rows as selected.
    let multi_select = target_kind == 1 && kind == 6;
    let chosen: Vec<String> = if multi_select {
        ui.get_wizard_property().to_string().lines().map(str::to_string).collect()
    } else {
        Vec::new()
    };

    let filter_norm = filter.trim();
    let mut filtered: Vec<String> = if filter_norm.is_empty() {
        source
    } else {
        source.into_iter().filter(|n| n.to_lowercase().contains(filter_norm)).collect()
    };
    // For non-key lists, dedup + sort. Keep key library order.
    if !(target_kind == 1 && kind == 5) {
        filtered.sort();
        filtered.dedup();
    }
    let icon_kind: i32 = match (target_kind, kind) {
        (_, 1) if target_kind == 0 => 1,   // slider mic
        (0, 2) => 2,                        // slider output
        (1, 2) => 1,                        // button mic
        (1, 1) => 2,                        // button output (main audio)
        (1, 5) => 4,                        // key
        (1, 6) => 2,                        // cycle output
        _ => 0,                             // process
    };
    ui.set_wizard_picker_source(ModelRc::new(VecModel::from(
        filtered.into_iter()
            .map(|n| crate::PickerEntry {
                selected: chosen.iter().any(|c| c == &n),
                name: n.into(),
                icon_kind,
            })
            .collect::<Vec<_>>(),
    )));
}

/// Merge sessions-with-audio with all running processes, so the picker isn't
/// empty when no audio is playing yet.
fn merged_processes(audio: &dyn AudioBackend) -> Vec<String> {
    let mut out = audio.list_processes();
    out.extend(list_running_processes());
    out
}

fn populate_live_lists(ui: &AppWindow, shared: Arc<Mutex<Shared>>) {
    let s = shared.lock();
    let mut procs = s.audio.list_processes();
    let mut mics = s.audio.list_mics();
    let mut outs = s.audio.list_outputs();
    // Also surface system processes (running apps) on top of audio-active ones, dedup.
    procs.extend(list_running_processes());
    procs.sort();
    procs.dedup();
    mics.sort(); mics.dedup();
    outs.sort(); outs.dedup();
    ui.set_live_processes(ModelRc::new(VecModel::from(
        procs.into_iter().map(SharedString::from).collect::<Vec<_>>(),
    )));
    ui.set_live_mics(ModelRc::new(VecModel::from(
        mics.into_iter().map(SharedString::from).collect::<Vec<_>>(),
    )));
    ui.set_live_outputs(ModelRc::new(VecModel::from(
        outs.into_iter().map(SharedString::from).collect::<Vec<_>>(),
    )));
}

#[cfg(target_os = "linux")]
fn list_running_processes() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir("/proc") {
        for e in rd.flatten() {
            let n = e.file_name();
            let n = n.to_string_lossy();
            if !n.chars().all(|c| c.is_ascii_digit()) { continue }
            if let Ok(comm) = std::fs::read_to_string(format!("/proc/{n}/comm")) {
                let comm = comm.trim();
                if !comm.is_empty() { out.push(comm.to_string()); }
            }
        }
    }
    out
}

#[cfg(target_os = "windows")]
fn list_running_processes() -> Vec<String> {
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };
    use windows::Win32::Foundation::CloseHandle;
    let mut out = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        if Process32FirstW(snap, &mut entry).is_ok() {
            loop {
                let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
                let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
                if !name.is_empty() { out.push(name); }
                if Process32NextW(snap, &mut entry).is_err() { break }
            }
        }
        let _ = CloseHandle(snap);
    }
    out
}

// ── LED config <-> UI conversions ──────────────────────────────────────────

fn opt(s: &str) -> Option<String> {
    if s.is_empty() { None } else { Some(s.to_string()) }
}

fn led_mode_kind_to(k: LedModeKind) -> i32 {
    match k { LedModeKind::Off => 0, LedModeKind::On => 1, LedModeKind::Blink => 2, LedModeKind::Breathe => 3 }
}
fn audio_kind_to(k: AudioKind) -> i32 {
    match k { AudioKind::Process => 0, AudioKind::Mic => 1, AudioKind::Output => 2 }
}

/// Snapshot a UI string-model into a Vec.
fn ui_list(model: &ModelRc<SharedString>) -> Vec<String> {
    (0..model.row_count()).filter_map(|i| model.row_data(i)).map(|s| s.to_string()).collect()
}

/// Index of `target` within the live list for audio kind `audio` (-1 if absent).
fn list_index_of(ui: &AppWindow, audio: i32, target: &str) -> i32 {
    if target.is_empty() {
        return -1;
    }
    let list = match audio {
        1 => ui_list(&ui.get_live_mics()),
        2 => ui_list(&ui.get_live_outputs()),
        _ => ui_list(&ui.get_live_processes()),
    };
    list.iter().position(|x| x == target).map(|p| p as i32).unwrap_or(-1)
}

fn led_mode_from_ui(
    kind: i32, on: &str, off: &str, cycle: &str,
    bmode: i32, bright: i32, baudio: i32, btarget: &str,
) -> LedMode {
    let kind = match kind {
        1 => LedModeKind::On,
        2 => LedModeKind::Blink,
        3 => LedModeKind::Breathe,
        _ => LedModeKind::Off,
    };
    LedMode {
        kind,
        on_ms: on.trim().parse().unwrap_or(500).max(1),
        off_ms: off.trim().parse().unwrap_or(500).max(1),
        cycle_ms: cycle.trim().parse().unwrap_or(2000).max(1),
        brightness_mode: if bmode == 1 { BrightnessMode::Volume } else { BrightnessMode::Fixed },
        brightness: bright.clamp(0, 100) as u8,
        brightness_audio: audio_kind_from(baudio),
        brightness_target: opt(btarget),
    }
}

fn cond_kind_from(v: i32) -> LedConditionKind {
    match v { 1 => LedConditionKind::Volume, 2 => LedConditionKind::Api, _ => LedConditionKind::Muted }
}
fn audio_kind_from(v: i32) -> AudioKind {
    match v { 1 => AudioKind::Mic, 2 => AudioKind::Output, _ => AudioKind::Process }
}
fn cmp_from(v: i32) -> Comparison {
    match v { 1 => Comparison::Above, 2 => Comparison::Exact, _ => Comparison::Below }
}

fn cond_to_ui(c: &LedCondition, target_index: i32) -> LedConditionUI {
    LedConditionUI {
        kind: match c.kind { LedConditionKind::Muted => 0, LedConditionKind::Volume => 1, LedConditionKind::Api => 2 },
        audio_kind: audio_kind_to(c.audio_kind),
        target: c.target.clone().unwrap_or_default().into(),
        target_index,
        cmp: match c.cmp { Comparison::Below => 0, Comparison::Above => 1, Comparison::Exact => 2 },
        value: c.value as i32,
        method: c.api.method.to_index(),
        url: c.api.url.clone().into(),
        payload: c.api.payload.clone().unwrap_or_default().into(),
        bearer: c.api.bearer.clone().unwrap_or_default().into(),
        interval: c.api.interval_ms.to_string().into(),
        expect_body: c.api.expect_body.clone().unwrap_or_default().into(),
    }
}

/// Rebuild the conditions model, resolving each target's index within the live
/// lists already on the UI (so its dropdown shows the right selection).
fn push_led_conditions(ui: &AppWindow, conds: &[LedCondition]) {
    let rows: Vec<LedConditionUI> = conds
        .iter()
        .map(|c| {
            let tidx = list_index_of(ui, audio_kind_to(c.audio_kind), c.target.as_deref().unwrap_or(""));
            cond_to_ui(c, tidx)
        })
        .collect();
    ui.set_led_conditions(ModelRc::new(VecModel::from(rows)));
}

/// Push one LED appearance into the UI scalar props (active or inactive).
fn push_mode_to_ui(ui: &AppWindow, m: &LedMode, active: bool) {
    let kind = led_mode_kind_to(m.kind);
    let on: SharedString = m.on_ms.to_string().into();
    let off: SharedString = m.off_ms.to_string().into();
    let cycle: SharedString = m.cycle_ms.to_string().into();
    let bmode = if m.brightness_mode == BrightnessMode::Volume { 1 } else { 0 };
    let bright = m.brightness as i32;
    let baudio = audio_kind_to(m.brightness_audio);
    let btarget: SharedString = m.brightness_target.clone().unwrap_or_default().into();
    let btidx = list_index_of(ui, baudio, m.brightness_target.as_deref().unwrap_or(""));
    if active {
        ui.set_led_active_kind(kind);
        ui.set_led_active_on(on);
        ui.set_led_active_off(off);
        ui.set_led_active_cycle(cycle);
        ui.set_led_active_bmode(bmode);
        ui.set_led_active_bright(bright);
        ui.set_led_active_baudio(baudio);
        ui.set_led_active_btarget(btarget);
        ui.set_led_active_btidx(btidx);
    } else {
        ui.set_led_inactive_kind(kind);
        ui.set_led_inactive_on(on);
        ui.set_led_inactive_off(off);
        ui.set_led_inactive_cycle(cycle);
        ui.set_led_inactive_bmode(bmode);
        ui.set_led_inactive_bright(bright);
        ui.set_led_inactive_baudio(baudio);
        ui.set_led_inactive_btarget(btarget);
        ui.set_led_inactive_btidx(btidx);
    }
}

/// Apply `f` to condition `idx` of the working LED config (no UI re-push).
fn mutate_cond<F: FnOnce(&mut LedCondition)>(shared: &Arc<Mutex<Shared>>, idx: i32, f: F) {
    let mut s = shared.lock();
    if let Some((_, cfg)) = s.editing_led.as_mut() {
        if let Some(c) = cfg.conditions.get_mut(idx as usize) {
            f(c);
        }
    }
}

/// Like [`mutate_cond`] but returns the updated condition list so the caller can
/// re-push the model (for structural changes that show/hide dependent fields).
fn mutate_cond_struct<F: FnOnce(&mut LedCondition)>(
    shared: &Arc<Mutex<Shared>>,
    idx: i32,
    f: F,
) -> Option<Vec<LedCondition>> {
    let mut s = shared.lock();
    let (_, cfg) = s.editing_led.as_mut()?;
    if let Some(c) = cfg.conditions.get_mut(idx as usize) {
        f(c);
    }
    Some(cfg.conditions.clone())
}

fn toast(ui: &AppWindow, msg: &str) {
    ui.set_toast_text(msg.into());
    let weak = ui.as_weak();
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::SingleShot, Duration::from_millis(1800), move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_toast_text("".into());
        }
    });
    std::mem::forget(timer);
}
