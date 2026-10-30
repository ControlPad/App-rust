//! Mixer page ↔ profile/settings. The page edits the active profile's
//! `MixerConfig` and the global `MixerDevices`; every edit is saved and handed
//! to the mixer right away.

use std::sync::Arc;

use parking_lot::Mutex;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::glue::Shared;
use crate::model::{Effects, EqBand, EqBandKind, MixChannel, MixTarget, MixerConfig};
use crate::{AppWindow, EqBandUI, MixChannelUI, PickerEntry};

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

/// Strip whose EQ the editor popup is editing.
static EDITING_EQ: Mutex<Option<MixTarget>> = Mutex::new(None);

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
    ui.set_mixer_voice_level(m.voice.level * 100.0);
    ui.set_mixer_voice_muted(m.voice.muted);
    ui.set_mixer_voice_monitor(m.voice.to_monitor);
    ui.set_mixer_voice_eq_on(m.voice.fx.eq_on);
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
            eq_on: c.fx.eq_on,
            reroute: c.reroute,
            send_on: c.mic_send.is_some(),
            send: c.mic_send.unwrap_or(1.0) * 100.0,
        })
        .collect();
    ui.set_mixer_channels(ModelRc::new(VecModel::from(rows)));
}

// ── EQ editor ──────────────────────────────────────────────────────────────

const EQ_KINDS: [EqBandKind; 5] =
    [EqBandKind::Peak, EqBandKind::LowShelf, EqBandKind::HighShelf, EqBandKind::HighPass, EqBandKind::LowPass];
/// Editor range: ±15 dB, 20 Hz – 20 kHz, Q 0.3 – 10 (frequency and Q on a log scale).
const EQ_RANGE_DB: f32 = 15.0;
const F_MIN: f32 = 20.0;
const F_SPAN: f32 = 1000.0;
const Q_MIN: f32 = 0.3;
const Q_SPAN: f32 = 10.0 / 0.3;

fn freq_from_pos(p: f32) -> f32 {
    F_MIN * F_SPAN.powf(p.clamp(0.0, 1.0))
}
fn pos_from_freq(f: f32) -> f32 {
    ((f / F_MIN).ln() / F_SPAN.ln()).clamp(0.0, 1.0)
}
fn q_from_pos(p: f32) -> f32 {
    Q_MIN * Q_SPAN.powf(p.clamp(0.0, 1.0))
}
fn pos_from_q(q: f32) -> f32 {
    ((q / Q_MIN).ln() / Q_SPAN.ln()).clamp(0.0, 1.0)
}
fn has_gain(kind: EqBandKind) -> bool {
    !matches!(kind, EqBandKind::HighPass | EqBandKind::LowPass)
}

fn freq_text(f: f32) -> String {
    if f >= 1000.0 {
        format!("{:.1} kHz", f / 1000.0)
    } else {
        format!("{f:.0} Hz")
    }
}

fn band_ui(b: &EqBand) -> EqBandUI {
    let gain = if has_gain(b.kind) { b.gain_db } else { 0.0 };
    EqBandUI {
        kind: EQ_KINDS.iter().position(|k| *k == b.kind).unwrap_or(0) as i32,
        freq_pos: pos_from_freq(b.freq),
        gain,
        q_pos: pos_from_q(b.q),
        freq_text: freq_text(b.freq).into(),
        gain_text: if has_gain(b.kind) { format!("{gain:+.1} dB") } else { "—".into() }.into(),
        q_text: format!("{:.2}", b.q).into(),
        x: pos_from_freq(b.freq),
        y: (EQ_RANGE_DB - gain) / (2.0 * EQ_RANGE_DB),
    }
}

/// SVG path of the combined response in the editor's 1000 × 300 box.
fn curve(bands: &[EqBand]) -> String {
    let eq = crate::mixer::dsp::Eq::new(bands);
    let mut path = String::new();
    for i in 0..=200 {
        let x = i as f32 / 200.0;
        let db = eq.response_db(freq_from_pos(x)).clamp(-EQ_RANGE_DB - 1.0, EQ_RANGE_DB + 1.0);
        let y = 150.0 - db * 150.0 / EQ_RANGE_DB;
        path.push_str(&format!("{}{:.1} {:.1} ", if i == 0 { "M " } else { "L " }, x * 1000.0, y));
    }
    path
}

fn effects_mut(m: &mut MixerConfig, target: MixTarget) -> Option<&mut Effects> {
    match target {
        MixTarget::Voice => Some(&mut m.voice.fx),
        MixTarget::Channel(id) | MixTarget::Send(id) => m.channels.iter_mut().find(|c| c.id == id).map(|c| &mut c.fx),
    }
}

/// Fill the editor. `rebuild` replaces the band rows (after add/delete/load,
/// and when a drag ends so sliders pick up the new values); otherwise rows
/// are updated in place, which keeps a handle that is being dragged alive.
fn push_eq(ui: &AppWindow, fx: &Effects, rebuild: bool) {
    let rows: Vec<EqBandUI> = fx.eq.iter().map(band_ui).collect();
    let model = ui.get_mixer_eq_bands();
    if !rebuild && model.row_count() == rows.len() {
        for (i, r) in rows.into_iter().enumerate() {
            model.set_row_data(i, r);
        }
    } else {
        ui.set_mixer_eq_bands(ModelRc::new(VecModel::from(rows)));
    }
    ui.set_mixer_eq_on(fx.eq_on);
    ui.set_mixer_eq_curve(curve(&fx.eq).into());
}

/// Apply `edit` to the strip being edited, then refresh the editor and the
/// mixer. `save` is false while a handle is dragged (saved when it's let go).
fn edit_eq(ui: &AppWindow, s: &mut Shared, rebuild: bool, save: bool, edit: impl FnOnce(&mut Effects)) {
    let Some(target) = *EDITING_EQ.lock() else { return };
    let Some(fx) = effects_mut(&mut s.preset.mixer, target) else { return };
    edit(fx);
    let fx = fx.clone();
    push_eq(ui, &fx, rebuild);
    if save {
        commit(s);
    } else {
        s.push_mixer();
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
            m.voice.fx.eq_on = ui.get_mixer_voice_eq_on();
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
            let Some(c) = s.preset.mixer.channels.iter_mut().find(|c| c.id as i32 == id) else { return };
            let mut repush = false;
            match field.as_str() {
                "name" => c.name = text.to_string(),
                "level" => c.level = (num / 100.0).clamp(0.0, 1.0),
                "muted" => c.muted = num > 0.5,
                "reroute" => c.reroute = num > 0.5,
                "eq-on" => c.fx.eq_on = num > 0.5,
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
                fx: Effects::default(),
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
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_edit_eq(move |which| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(target) = MixTarget::from_value(&which) else { return };
            let mut s = shared.lock();
            if s.preset.mixer.eq_presets.is_empty() {
                s.preset.mixer.eq_presets = MixerConfig::default_eq_presets();
            }
            let m = &mut s.preset.mixer;
            let title = format!("EQ – {}", m.label(target));
            ui.set_mixer_eq_preset_names(names("Load preset…", m.eq_presets.iter().map(|p| p.name.as_str())));
            let Some(fx) = effects_mut(m, target) else { return };
            let fx = fx.clone();
            *EDITING_EQ.lock() = Some(target);
            ui.set_mixer_eq_title(title.into());
            push_eq(&ui, &fx, true);
            ui.set_mixer_eq_open(true);
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_eq_toggled(move |on| {
            let Some(ui) = weak.upgrade() else { return };
            edit_eq(&ui, &mut shared.lock(), false, true, |fx| fx.eq_on = on);
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_eq_set(move |i, field, v| {
            let Some(ui) = weak.upgrade() else { return };
            edit_eq(&ui, &mut shared.lock(), false, true, |fx| {
                let Some(b) = fx.eq.get_mut(i as usize) else { return };
                match field.as_str() {
                    "kind" => b.kind = EQ_KINDS.get(v as usize).copied().unwrap_or(EqBandKind::Peak),
                    "freq" => b.freq = freq_from_pos(v),
                    "gain" => b.gain_db = v.clamp(-EQ_RANGE_DB, EQ_RANGE_DB),
                    "q" => b.q = q_from_pos(v),
                    other => log::warn!("EQ editor: unknown field {other:?}"),
                }
                // Editing a band means wanting to hear it.
                fx.eq_on = true;
            });
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_eq_drag(move |i, x, y| {
            let Some(ui) = weak.upgrade() else { return };
            edit_eq(&ui, &mut shared.lock(), false, false, |fx| {
                let Some(b) = fx.eq.get_mut(i as usize) else { return };
                b.freq = freq_from_pos(x);
                if has_gain(b.kind) {
                    b.gain_db = ((0.5 - y.clamp(0.0, 1.0)) * 2.0 * EQ_RANGE_DB).clamp(-EQ_RANGE_DB, EQ_RANGE_DB);
                }
                fx.eq_on = true;
            });
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_eq_drag_end(move || {
            let Some(ui) = weak.upgrade() else { return };
            edit_eq(&ui, &mut shared.lock(), true, true, |_| {});
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_eq_add(move || {
            let Some(ui) = weak.upgrade() else { return };
            edit_eq(&ui, &mut shared.lock(), true, true, |fx| {
                fx.eq.push(EqBand { kind: EqBandKind::Peak, freq: 1000.0, gain_db: 0.0, q: 1.0 });
                fx.eq_on = true;
            });
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_eq_delete(move |i| {
            let Some(ui) = weak.upgrade() else { return };
            edit_eq(&ui, &mut shared.lock(), true, true, |fx| {
                if (i as usize) < fx.eq.len() {
                    fx.eq.remove(i as usize);
                }
            });
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_eq_load_preset(move |i| {
            let Some(ui) = weak.upgrade() else { return };
            let mut s = shared.lock();
            let Some(bands) = s.preset.mixer.eq_presets.get(i as usize).map(|p| p.bands.clone()) else { return };
            edit_eq(&ui, &mut s, true, true, |fx| {
                fx.eq = bands;
                fx.eq_on = true;
            });
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_eq_closed(move || {
            *EDITING_EQ.lock() = None;
            // The strip rows show the EQ switch.
            if let Some(ui) = weak.upgrade() {
                let s = shared.lock();
                ui.set_mixer_voice_eq_on(s.preset.mixer.voice.fx.eq_on);
                push_channels(&ui, &s.preset.mixer);
            }
        });
    }
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

    #[test]
    fn eq_scales_round_trip() {
        for f in [20.0, 100.0, 1000.0, 12345.0, 20000.0] {
            assert!((freq_from_pos(pos_from_freq(f)) - f).abs() / f < 1e-3);
        }
        for q in [0.3, 0.707, 1.0, 10.0] {
            assert!((q_from_pos(pos_from_q(q)) - q).abs() / q < 1e-3);
        }
    }

    #[test]
    fn handle_sits_on_its_band() {
        let b = band_ui(&EqBand { kind: EqBandKind::Peak, freq: 1000.0, gain_db: 7.5, q: 1.0 });
        assert!((b.y - 0.25).abs() < 1e-6);
        let hp = band_ui(&EqBand { kind: EqBandKind::HighPass, freq: 80.0, gain_db: 9.0, q: 0.7 });
        assert_eq!(hp.y, 0.5, "pass filters have no gain");
    }

    #[test]
    fn curve_covers_the_box() {
        let c = curve(&[]);
        assert!(c.starts_with("M 0.0 150.0"));
        assert!(c.trim_end().ends_with("1000.0 150.0"));
    }
}
