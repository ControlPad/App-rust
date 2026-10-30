//! Mixer page ↔ profile/settings. The page edits the active profile's
//! `MixerConfig` and the global `MixerDevices`; every edit is saved and handed
//! to the mixer right away.

use std::sync::Arc;

use parking_lot::Mutex;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::glue::Shared;
use crate::model::{MixChannel, MixerConfig};
use crate::{AppWindow, MixChannelUI, PickerEntry};

/// Device names behind the page's selectors, as last shown — an index from
/// the UI is resolved against exactly the list it was picked from.
#[derive(Default)]
struct Lists {
    /// Headphones candidates.
    outputs: Vec<String>,
    /// Real microphones (virtual cables left out: recording one would loop).
    mics: Vec<String>,
    /// Silent-output candidates (never the Slidr Mic's own cable).
    sinks: Vec<String>,
    /// Slidr Mic choices: (name Discord shows, output Slidr plays into).
    cables: Vec<(String, String)>,
}

static LISTS: Mutex<Lists> =
    Mutex::new(Lists { outputs: Vec::new(), mics: Vec::new(), sinks: Vec::new(), cables: Vec::new() });

/// Where VB-CABLE comes from; opened by the hint's download button.
const CABLE_DOWNLOAD: &str = "https://vb-audio.com/Cable/";

/// Channel whose programs the picker popup is editing.
static PICKING: Mutex<Option<u32>> = Mutex::new(None);

use crate::mixer::adapter;

fn is_virtual(adapter: &str) -> bool {
    let a = adapter.to_lowercase();
    ["virtual", "cable", "vb-audio", "voicemeeter"].iter().any(|k| a.contains(k))
}

/// Virtual cables as (recording end, playback end): what Discord lists as a
/// microphone, and the output whose audio comes out there. Prefers the plain
/// input over multi-channel variants ("CABLE In 16ch").
fn cables(mics: &[String], outputs: &[String]) -> Vec<(String, String)> {
    mics.iter()
        .filter_map(|mic| {
            let a = adapter(mic).filter(|a| is_virtual(a))?;
            let mut ends: Vec<&String> = outputs.iter().filter(|o| adapter(o) == Some(a)).collect();
            ends.sort_by_key(|o| o.to_lowercase().contains("16ch"));
            Some((mic.clone(), ends.first()?.to_string()))
        })
        .collect()
}

/// Fill the Mixer page from the profile and settings (and refresh device lists).
pub fn push(ui: &AppWindow, s: &mut Shared) {
    if s.preset.mixer.eq_presets.is_empty() {
        s.preset.mixer.eq_presets = MixerConfig::default_eq_presets();
    }
    let mut outputs = s.audio.list_outputs();
    let all_mics = s.audio.list_mics();
    let d = &s.settings.mixer_devices;
    // A configured device that is unplugged right now stays selectable, so
    // opening the page doesn't silently drop the choice.
    for name in [&d.monitor, &d.sink, &d.mic_out].into_iter().flatten() {
        if !outputs.contains(name) {
            outputs.push(name.clone());
        }
    }
    let mut cables = cables(&all_mics, &outputs);
    if let Some(out) = &d.mic_out {
        if !cables.iter().any(|(_, o)| o == out) {
            cables.push((out.clone(), out.clone()));
        }
    }
    let mut mics: Vec<String> = all_mics.into_iter().filter(|m| !cables.iter().any(|(c, _)| c == m)).collect();
    if let Some(name) = &d.mic_input {
        if !mics.contains(name) {
            mics.push(name.clone());
        }
    }
    let cable_adapter = d.mic_out.as_deref().and_then(adapter);
    let sinks: Vec<String> = outputs
        .iter()
        .filter(|o| cable_adapter.is_none() || adapter(o) != cable_adapter || Some(o.as_str()) == d.sink.as_deref())
        .cloned()
        .collect();

    let index = |list: &[String], v: &Option<String>| -> i32 {
        v.as_ref().and_then(|v| list.iter().position(|n| n == v)).map_or(0, |i| i as i32 + 1)
    };
    ui.set_mixer_monitor_names(names("System default", outputs.iter().map(String::as_str)));
    ui.set_mixer_mic_names(names("System default", mics.iter().map(String::as_str)));
    ui.set_mixer_spare_names(names("Not used", sinks.iter().map(String::as_str)));
    ui.set_mixer_cable_names(names("Not used", cables.iter().map(|(c, _)| c.as_str())));
    ui.set_mixer_monitor_idx(index(&outputs, &d.monitor));
    ui.set_mixer_mic_idx(index(&mics, &d.mic_input));
    ui.set_mixer_sink_idx(index(&sinks, &d.sink));
    ui.set_mixer_cable_idx(
        d.mic_out.as_ref().and_then(|o| cables.iter().position(|(_, c)| c == o)).map_or(0, |i| i as i32 + 1),
    );

    let m = &s.preset.mixer;
    ui.set_mixer_enabled(m.enabled);
    ui.set_mixer_eq_names(ModelRc::new(VecModel::from(
        std::iter::once(SharedString::from("Off"))
            .chain(m.eq_presets.iter().map(|p| SharedString::from(p.name.as_str())))
            .collect::<Vec<_>>(),
    )));
    ui.set_mixer_voice_level(m.voice.level * 100.0);
    ui.set_mixer_voice_muted(m.voice.muted);
    ui.set_mixer_voice_monitor(m.voice.to_monitor);
    ui.set_mixer_voice_eq(eq_index(m, m.voice.eq, m.voice.eq_enabled));
    push_channels(ui, m);

    let no_cable = cables.is_empty();
    ui.set_mixer_hint_download(no_cable);
    ui.set_mixer_hint(
        if no_cable {
            "No virtual cable found. Slidr needs one for the Slidr Mic: install VB-CABLE (free), then pick \"CABLE Output\" here and in Discord."
        } else if m.enabled && d.mic_out.is_none() {
            "Choose the Slidr Mic below so Discord can hear your voice through Slidr."
        } else {
            ""
        }
        .into(),
    );
    *LISTS.lock() = Lists { outputs, mics, sinks, cables };
}

/// Selector model: a leading "none" entry, then the names.
fn names<'a>(first: &'a str, list: impl Iterator<Item = &'a str>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(std::iter::once(first).chain(list).map(SharedString::from).collect::<Vec<_>>()))
}

fn push_channels(ui: &AppWindow, m: &MixerConfig) {
    let rows: Vec<MixChannelUI> = m
        .channels
        .iter()
        .map(|c| MixChannelUI {
            id: c.id as i32,
            name: c.name.as_str().into(),
            programs: c.programs.iter().map(|p| crate::app_groups::label_for(p)).collect::<Vec<_>>().join(", ").into(),
            level: c.level * 100.0,
            muted: c.muted,
            eq: eq_index(m, c.eq, c.eq_enabled),
            reroute: c.reroute,
            send_on: c.mic_send.is_some(),
            send: c.mic_send.unwrap_or(1.0) * 100.0,
        })
        .collect();
    ui.set_mixer_channels(ModelRc::new(VecModel::from(rows)));
}

/// Selector index for an EQ choice: 0 = off, else 1 + the preset's position.
fn eq_index(m: &MixerConfig, eq: Option<u32>, enabled: bool) -> i32 {
    eq.filter(|_| enabled)
        .and_then(|id| m.eq_presets.iter().position(|p| p.id == id))
        .map_or(0, |i| i as i32 + 1)
}

/// Inverse of [`eq_index`]: (preset id, enabled).
fn eq_choice(m: &MixerConfig, idx: i32) -> (Option<u32>, bool) {
    match usize::try_from(idx - 1).ok().and_then(|i| m.eq_presets.get(i)) {
        Some(p) => (Some(p.id), true),
        None => (None, false),
    }
}

/// Picker rows for `channel`: program categories, then programs playing audio
/// right now, then everything else that runs — plus whatever the channel
/// already holds, so a program that isn't running can still be removed.
fn push_picker(ui: &AppWindow, s: &Shared, channel: u32) {
    let Some(c) = s.preset.mixer.channels.iter().find(|c| c.id == channel) else { return };
    let filter = ui.get_mixer_picker_filter().to_lowercase();
    let filter = filter.trim();
    let chosen = |v: &str| c.programs.iter().any(|p| p.eq_ignore_ascii_case(v));
    let mut seen: Vec<String> = Vec::new();
    let mut rows: Vec<PickerEntry> = Vec::new();
    let mut add = |name: String, value: String, icon: i32, group: bool| {
        if seen.iter().any(|v| v.eq_ignore_ascii_case(&value)) {
            return;
        }
        if !filter.is_empty() && !name.to_lowercase().contains(filter) {
            return;
        }
        rows.push(PickerEntry {
            selected: chosen(&value),
            name: name.into(),
            value: value.as_str().into(),
            icon_kind: icon,
            is_group: group,
        });
        seen.push(value);
    };
    for g in crate::app_groups::groups() {
        add(g.name.clone(), crate::app_groups::make_ref(&g.id), g.icon, true);
    }
    for p in &c.programs {
        add(crate::app_groups::label_for(p), p.clone(), 0, crate::app_groups::parse_ref(p).is_some());
    }
    let mut playing = s.audio.list_processes();
    playing.sort_by_key(|p| p.to_lowercase());
    for p in playing {
        add(p.clone(), p, 5, false);
    }
    let mut running = crate::glue::merged_processes(s.audio.as_ref());
    running.sort_by_key(|p| p.to_lowercase());
    for p in running {
        add(p.clone(), p, 0, false);
    }
    // Selected rows first, so the current choice is visible without scrolling.
    rows.sort_by_key(|r| !r.selected);
    ui.set_mixer_picker_options(ModelRc::new(VecModel::from(rows)));
}

fn commit(s: &mut Shared) {
    s.save_preset();
    if let Err(e) = crate::storage::save_settings(&s.settings) {
        log::warn!("could not save settings: {e:#}");
    }
    s.push_mixer();
}

pub fn wire(ui: &AppWindow, shared: Arc<parking_lot::Mutex<Shared>>) {
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_page_changed(move |page| {
            if page == 4 {
                if let Some(ui) = weak.upgrade() {
                    push(&ui, &mut shared.lock());
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_changed(move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut s = shared.lock();
            let lists = LISTS.lock();
            let pick = |list: &[String], idx: i32| -> Option<String> {
                usize::try_from(idx - 1).ok().and_then(|i| list.get(i)).cloned()
            };
            let d = &mut s.settings.mixer_devices;
            d.monitor = pick(&lists.outputs, ui.get_mixer_monitor_idx());
            d.mic_input = pick(&lists.mics, ui.get_mixer_mic_idx());
            d.sink = pick(&lists.sinks, ui.get_mixer_sink_idx());
            d.mic_out = usize::try_from(ui.get_mixer_cable_idx() - 1)
                .ok()
                .and_then(|i| lists.cables.get(i))
                .map(|(_, out)| out.clone());
            // The silent output must never be the Slidr Mic's own cable, or
            // the rerouted programs would end up in Discord.
            if d.sink.as_deref().and_then(adapter).is_some_and(|a| Some(a) == d.mic_out.as_deref().and_then(adapter)) {
                d.sink = None;
            }
            drop(lists);
            let was_enabled = s.preset.mixer.enabled;
            let m = &mut s.preset.mixer;
            m.enabled = ui.get_mixer_enabled();
            m.voice.level = (ui.get_mixer_voice_level() / 100.0).clamp(0.0, 1.0);
            m.voice.muted = ui.get_mixer_voice_muted();
            m.voice.to_monitor = ui.get_mixer_voice_monitor();
            let (eq, on) = eq_choice(m, ui.get_mixer_voice_eq());
            if eq.is_some() {
                m.voice.eq = eq;
            }
            m.voice.eq_enabled = on;
            commit(&mut s);
            // Lists depend on each other (the silent output excludes the
            // Slidr Mic's cable) and the hint on the switch.
            let _ = was_enabled;
            push(&ui, &mut s);
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_channel_set(move |id, field, num, text| {
            let mut s = shared.lock();
            let presets = s.preset.mixer.eq_presets.clone();
            let Some(c) = s.preset.mixer.channels.iter_mut().find(|c| c.id as i32 == id) else { return };
            let mut repush = false;
            match field.as_str() {
                "name" => c.name = text.to_string(),
                "level" => c.level = (num / 100.0).clamp(0.0, 1.0),
                "muted" => c.muted = num > 0.5,
                "reroute" => c.reroute = num > 0.5,
                "eq" => match usize::try_from(num as i32 - 1).ok().and_then(|i| presets.get(i)) {
                    Some(p) => {
                        c.eq = Some(p.id);
                        c.eq_enabled = true;
                    }
                    None => c.eq_enabled = false,
                },
                "send-on" => {
                    c.mic_send = (num > 0.5).then_some(1.0);
                    // Shows or hides the send fader.
                    repush = true;
                }
                "send" => c.mic_send = Some((num / 100.0).clamp(0.0, 1.0)),
                other => log::warn!("mixer page: unknown channel field {other:?}"),
            }
            commit(&mut s);
            if repush {
                if let Some(ui) = weak.upgrade() {
                    push_channels(&ui, &s.preset.mixer);
                }
            }
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_add_channel(move || {
            let mut s = shared.lock();
            let m = &mut s.preset.mixer;
            let id = m.channels.iter().map(|c| c.id).max().unwrap_or(0) + 1;
            m.channels.push(MixChannel {
                id,
                name: format!("Channel {id}"),
                programs: Vec::new(),
                level: 1.0,
                muted: false,
                eq: None,
                eq_enabled: false,
                reroute: true,
                mic_send: None,
            });
            commit(&mut s);
            if let Some(ui) = weak.upgrade() {
                push_channels(&ui, &s.preset.mixer);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_choose_programs(move |id| {
            let Some(ui) = weak.upgrade() else { return };
            let s = shared.lock();
            let Some(c) = s.preset.mixer.channels.iter().find(|c| c.id as i32 == id) else { return };
            *PICKING.lock() = Some(c.id);
            ui.set_mixer_picker_title(format!("Programs for {}", c.name).into());
            ui.set_mixer_picker_filter("".into());
            push_picker(&ui, &s, c.id);
            ui.set_mixer_picker_open(true);
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_toggle_program(move |value| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(id) = *PICKING.lock() else { return };
            let mut s = shared.lock();
            let Some(c) = s.preset.mixer.channels.iter_mut().find(|c| c.id == id) else { return };
            match c.programs.iter().position(|p| p.eq_ignore_ascii_case(&value)) {
                Some(i) => {
                    c.programs.remove(i);
                }
                None => c.programs.push(value.to_string()),
            }
            commit(&mut s);
            push_picker(&ui, &s, id);
            push_channels(&ui, &s.preset.mixer);
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_picker_filter_changed(move |_| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(id) = *PICKING.lock() else { return };
            push_picker(&ui, &shared.lock(), id);
        });
    }
    ui.on_mixer_picker_closed(|| *PICKING.lock() = None);
    ui.on_mixer_open_download(|| {
        if let Err(e) = open::that(CABLE_DOWNLOAD) {
            log::warn!("could not open {CABLE_DOWNLOAD}: {e}");
        }
    });
    {
        let weak = ui.as_weak();
        ui.on_mixer_delete_channel(move |id| {
            let mut s = shared.lock();
            s.preset.mixer.channels.retain(|c| c.id as i32 != id);
            commit(&mut s);
            if let Some(ui) = weak.upgrade() {
                push_channels(&ui, &s.preset.mixer);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn cable_pairs_its_recording_and_playback_ends() {
        let mics = v(&["Mikrofon (Wireless Microphone RX)", "CABLE Output (VB-Audio Virtual Cable)"]);
        let outs = v(&[
            "CABLE In 16ch (VB-Audio Virtual Cable)",
            "CABLE Input (VB-Audio Virtual Cable)",
            "LG ULTRAGEAR (NVIDIA High Definition Audio)",
        ]);
        assert_eq!(
            cables(&mics, &outs),
            vec![("CABLE Output (VB-Audio Virtual Cable)".to_string(), "CABLE Input (VB-Audio Virtual Cable)".to_string())]
        );
    }

    #[test]
    fn real_devices_are_not_cables() {
        let mics = v(&["Mikrofon (USB Audio CODEC )"]);
        let outs = v(&["Lautsprecher (USB Audio CODEC )"]);
        assert!(cables(&mics, &outs).is_empty());
        assert_eq!(adapter("Mikrofon (2- USB Audio CODEC )"), Some("2- USB Audio CODEC"));
        assert_eq!(adapter("no parens"), None);
    }
}
