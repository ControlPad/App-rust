//! Mixer page ↔ profile/settings. The page edits the active profile's
//! `MixerConfig` and the global `MixerDevices`; every edit is saved and handed
//! to the mixer right away.

use std::sync::Arc;

use parking_lot::Mutex;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::glue::Shared;
use crate::model::{
    Effects, EqBand, EqBandKind, EqPreset, Fx, FxPreset, FxSlot, MixChannel, MixTarget, MixerConfig, MIC_PREFIX,
};
use crate::{AppWindow, EqBandUI, FxSlotUI, MixChannelUI, PickerEntry};

/// Device names behind the page's selectors, as last shown — an index from
/// the UI is resolved against exactly the list it was picked from.
#[derive(Default)]
struct Lists {
    /// Real microphones (virtual cables left out: recording one would loop).
    mics: Vec<String>,
    /// Silent-output candidates (never the Slidr Mic's own cable).
    sinks: Vec<String>,
    /// Slidr Mic choices: (name Discord shows, output Slidr plays into).
    cables: Vec<(String, String)>,
}

static LISTS: Mutex<Lists> = Mutex::new(Lists { mics: Vec::new(), sinks: Vec::new(), cables: Vec::new() });

/// What the picker lists: programs (0) or microphones (1).
static PICKER_TAB: Mutex<i32> = Mutex::new(0);

/// Where VB-CABLE comes from; opened by the hint's download button.
const CABLE_DOWNLOAD: &str = "https://vb-audio.com/Cable/";

/// Strip whose effects the effects popup is editing.
static EDITING_FX: Mutex<Option<MixTarget>> = Mutex::new(None);

/// Strip and EQ slot the EQ editor is editing.
static EDITING_EQ: Mutex<Option<(MixTarget, u32)>> = Mutex::new(None);

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
    // Profiles that never went through a load (a brand-new one) still carry
    // the old voice strip.
    let mic = s.settings.mixer_devices.mic_input.clone();
    if s.preset.move_voice_to_channel(mic.as_deref()) {
        commit(s);
    }
    let mut outputs = s.audio.list_outputs();
    let all_mics = s.audio.list_mics();
    let d = &s.settings.mixer_devices;
    // A configured device that is unplugged right now stays selectable, so
    // opening the page doesn't silently drop the choice.
    for name in [&d.sink, &d.mic_out].into_iter().flatten() {
        if !outputs.contains(name) {
            outputs.push(name.clone());
        }
    }
    let mut cables = cables(&all_mics, &outputs);
    // Sensible defaults the first time: the only cable is the Slidr Mic, and
    // Steam's streaming speakers (installed with Steam, silent unless
    // streaming) are the silent output.
    let mut picked = false;
    {
        let d = &mut s.settings.mixer_devices;
        if d.mic_out.is_none() && cables.len() == 1 {
            d.mic_out = Some(cables[0].1.clone());
            picked = true;
        }
        if d.sink.is_none() {
            if let Some(steam) = outputs.iter().find(|o| o.to_lowercase().contains("steam streaming speakers")) {
                d.sink = Some(steam.clone());
                picked = true;
            }
        }
    }
    if picked {
        commit(s);
    }
    let d = &s.settings.mixer_devices;
    if let Some(out) = &d.mic_out {
        if !cables.iter().any(|(_, o)| o == out) {
            cables.push((out.clone(), out.clone()));
        }
    }
    let mics: Vec<String> = all_mics.into_iter().filter(|m| !cables.iter().any(|(c, _)| c == m)).collect();
    let cable_adapter = d.mic_out.as_deref().and_then(adapter);
    let sinks: Vec<String> = outputs
        .iter()
        .filter(|o| cable_adapter.is_none() || adapter(o) != cable_adapter || Some(o.as_str()) == d.sink.as_deref())
        .cloned()
        .collect();

    // No "Not used" here: a silent output is what lets the mix replace
    // programs, and there is always one to offer.
    ui.set_mixer_spare_names(ModelRc::new(VecModel::from(
        sinks.iter().map(|n| SharedString::from(n.as_str())).collect::<Vec<_>>(),
    )));
    ui.set_mixer_cable_names(names("Not used", cables.iter().map(|(c, _)| c.as_str())));
    ui.set_mixer_sink_idx(d.sink.as_ref().and_then(|v| sinks.iter().position(|n| n == v)).map_or(-1, |i| i as i32));
    ui.set_mixer_cable_idx(
        d.mic_out.as_ref().and_then(|o| cables.iter().position(|(_, c)| c == o)).map_or(0, |i| i as i32 + 1),
    );

    let m = &s.preset.mixer;
    ui.set_mixer_enabled(m.enabled);
    push_channels(ui, m);

    let no_cable = cables.is_empty();
    ui.set_mixer_hint_download(no_cable);
    ui.set_mixer_hint(
        if no_cable {
            "No virtual cable found. Slidr needs one for the Slidr Mic: install VB-CABLE by VB-Audio (donationware, all participations are welcome), restart Windows, then choose \"CABLE Output\" as the microphone in your voice chat app."
        } else if m.enabled && d.mic_out.is_none() {
            "Choose the Slidr Mic under Settings → Mixer devices so other apps can hear your voice through Slidr."
        } else {
            ""
        }
        .into(),
    );
    *LISTS.lock() = Lists { mics, sinks, cables };
}

/// Selector model: a leading "none" entry, then the names.
fn names<'a>(first: &'a str, list: impl Iterator<Item = &'a str>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(std::iter::once(first).chain(list).map(SharedString::from).collect::<Vec<_>>()))
}

/// What a channel source is called on the page: a program (or program
/// category), or a microphone.
fn source_label(p: &str) -> String {
    match p.strip_prefix(MIC_PREFIX) {
        Some("") => "Default microphone".into(),
        Some(mic) => mic.to_string(),
        None => crate::app_groups::label_for(p),
    }
}

fn channel_ui(m: &MixerConfig, c: &MixChannel) -> MixChannelUI {
    let shown = |t: MixTarget, saved: f32| crate::mixer::shown_level(m, t).unwrap_or(saved) * 100.0;
    MixChannelUI {
        id: c.id as i32,
        name: c.name.as_str().into(),
        programs: c.programs.iter().map(|p| source_label(p)).collect::<Vec<_>>().join(", ").into(),
        mics_only: c.mics_only(),
        level: shown(MixTarget::Channel(c.id), c.level),
        muted: c.muted,
        fx_count: c.fx.active() as i32,
        fx_on: c.fx.on,
        reroute: c.reroute,
        send_on: c.mic_send.is_some(),
        send: shown(MixTarget::Send(c.id), c.mic_send.unwrap_or(1.0)),
        collapsed: c.collapsed,
    }
}

fn push_channels(ui: &AppWindow, m: &MixerConfig) {
    let rows: Vec<MixChannelUI> = m.channels.iter().map(|c| channel_ui(m, c)).collect();
    ui.set_mixer_channels(ModelRc::new(VecModel::from(rows)));
}

/// Let the page's faders follow the board. Rows are updated in place, and
/// only where a level actually moved, so nothing on the page is rebuilt.
pub fn follow_levels(ui: &AppWindow, s: &Shared) {
    let m = &s.preset.mixer;
    let model = ui.get_mixer_channels();
    for (i, c) in m.channels.iter().enumerate() {
        let Some(row) = model.row_data(i).filter(|r| r.id == c.id as i32) else { return };
        let new = channel_ui(m, c);
        if (row.level - new.level).abs() > 0.5 || (row.send - new.send).abs() > 0.5 {
            model.set_row_data(i, MixChannelUI { level: new.level, send: new.send, ..row });
        }
    }
}

// ── Effects rack ────────────────────────────────────────────────────────────

/// Position of an effect in [`Fx::catalog`] — the UI's `kind`.
fn fx_kind(fx: &Fx) -> i32 {
    match fx {
        Fx::Gate(_) => 0,
        Fx::Eq { .. } => 1,
        Fx::Compressor(_) => 2,
        Fx::Normalizer(_) => 3,
        Fx::Limiter(_) => 4,
    }
}

fn slot_ui(slot: &FxSlot) -> FxSlotUI {
    let mut p = [0f32; 5];
    let mut summary = String::new();
    match &slot.fx {
        Fx::Gate(g) => p[..2].copy_from_slice(&[g.threshold_db, g.release_ms]),
        Fx::Eq { bands } => {
            summary = match bands.len() {
                0 => "No bands yet".into(),
                1 => "1 band".into(),
                n => format!("{n} bands"),
            }
        }
        Fx::Compressor(c) => p.copy_from_slice(&[c.threshold_db, c.ratio, c.attack_ms, c.release_ms, c.makeup_db]),
        Fx::Normalizer(n) => p[..2].copy_from_slice(&[n.target_db, n.max_gain_db]),
        Fx::Limiter(l) => p[0] = l.ceiling_db,
    }
    FxSlotUI {
        id: slot.id as i32,
        kind: fx_kind(&slot.fx),
        name: slot.fx.name().into(),
        on: slot.on,
        p1: p[0],
        p2: p[1],
        p3: p[2],
        p4: p[3],
        p5: p[4],
        summary: summary.into(),
    }
}

/// Parameter `n` (1-based, in the rack's row order) of an effect.
fn set_param(fx: &mut Fx, n: i32, v: f32) {
    match fx {
        Fx::Gate(g) => match n {
            1 => g.threshold_db = v,
            2 => g.release_ms = v,
            _ => {}
        },
        Fx::Compressor(c) => match n {
            1 => c.threshold_db = v,
            2 => c.ratio = v.max(1.0),
            3 => c.attack_ms = v,
            4 => c.release_ms = v,
            5 => c.makeup_db = v,
            _ => {}
        },
        Fx::Normalizer(x) => match n {
            1 => x.target_db = v,
            2 => x.max_gain_db = v,
            _ => {}
        },
        Fx::Limiter(l) => {
            if n == 1 {
                l.ceiling_db = v.min(0.0);
            }
        }
        Fx::Eq { .. } => {}
    }
}

/// Built-in chains first, then the user's.
fn fx_presets(s: &Shared) -> Vec<FxPreset> {
    FxPreset::built_in().into_iter().chain(s.settings.fx_presets.iter().cloned()).collect()
}

/// Built-in EQ curves first, then the user's.
fn eq_presets(s: &Shared) -> Vec<EqPreset> {
    MixerConfig::default_eq_presets().into_iter().chain(s.settings.eq_presets.iter().cloned()).collect()
}

/// Names for a preset selector: the user's own marked "(mine)".
fn preset_names(all: Vec<String>, builtin: usize) -> ModelRc<SharedString> {
    let names: Vec<String> =
        all.into_iter().enumerate().map(|(i, n)| if i < builtin { n } else { format!("{n} (mine)") }).collect();
    self::names("Load preset…", names.iter().map(String::as_str))
}

fn push_eq_presets(ui: &AppWindow, s: &Shared) {
    let builtin = MixerConfig::default_eq_presets().len();
    ui.set_mixer_eq_preset_names(preset_names(eq_presets(s).into_iter().map(|p| p.name).collect(), builtin));
}

fn push_rack(ui: &AppWindow, s: &Shared, fx: &Effects) {
    ui.set_mixer_fx_slots(ModelRc::new(VecModel::from(fx.chain.iter().map(slot_ui).collect::<Vec<_>>())));
    let builtin = FxPreset::built_in().len();
    ui.set_mixer_fx_preset_names(preset_names(fx_presets(s).into_iter().map(|p| p.name).collect(), builtin));
    let catalog: Vec<&str> = Fx::catalog().iter().map(Fx::name).collect();
    ui.set_mixer_fx_add_names(names("Add effect…", catalog.into_iter()));
}

/// Change the chain being edited, save, and (for structural changes) redraw
/// the rack. Parameter moves don't redraw: the slider shows its own value.
fn edit_rack(ui: &AppWindow, s: &mut Shared, redraw: bool, edit: impl FnOnce(&mut Effects)) {
    let Some(target) = *EDITING_FX.lock() else { return };
    let Some(fx) = effects_mut(&mut s.preset.mixer, target) else { return };
    edit(fx);
    let fx = fx.clone();
    commit(s);
    if redraw {
        push_rack(ui, s, &fx);
    }
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
fn push_eq(ui: &AppWindow, on: bool, bands: &[EqBand], rebuild: bool) {
    let rows: Vec<EqBandUI> = bands.iter().map(band_ui).collect();
    let model = ui.get_mixer_eq_bands();
    if !rebuild && model.row_count() == rows.len() {
        for (i, r) in rows.into_iter().enumerate() {
            model.set_row_data(i, r);
        }
    } else {
        ui.set_mixer_eq_bands(ModelRc::new(VecModel::from(rows)));
    }
    ui.set_mixer_eq_on(on);
    ui.set_mixer_eq_curve(curve(bands).into());
}

/// Apply `edit` to the EQ slot being edited (its on switch and bands), then
/// refresh the editor and the mixer. `save` is false while a handle is
/// dragged (saved when it's let go).
fn edit_eq(ui: &AppWindow, s: &mut Shared, rebuild: bool, save: bool, edit: impl FnOnce(&mut bool, &mut Vec<EqBand>)) {
    let Some((target, id)) = *EDITING_EQ.lock() else { return };
    let Some(slot) = effects_mut(&mut s.preset.mixer, target).and_then(|fx| fx.slot_mut(id)) else { return };
    let Fx::Eq { bands } = &mut slot.fx else { return };
    edit(&mut slot.on, bands);
    let (on, bands) = (slot.on, bands.clone());
    push_eq(ui, on, &bands, rebuild);
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
    // Microphones tab: the default one, every real microphone, and whatever
    // the channel already holds (unplugged ones stay removable).
    if *PICKER_TAB.lock() == 1 {
        add(source_label(MIC_PREFIX), MIC_PREFIX.into(), 1, false);
        for m in c.mics() {
            add(source_label(&format!("{MIC_PREFIX}{m}")), format!("{MIC_PREFIX}{m}"), 1, false);
        }
        for m in &LISTS.lock().mics {
            add(m.clone(), format!("{MIC_PREFIX}{m}"), 1, false);
        }
        rows.sort_by_key(|r| !r.selected);
        ui.set_mixer_picker_options(ModelRc::new(VecModel::from(rows)));
        return;
    }
    for g in crate::app_groups::groups() {
        add(g.name.clone(), crate::app_groups::make_ref(&g.id), g.icon, true);
    }
    for p in c.programs.iter().filter(|p| !p.starts_with(MIC_PREFIX)) {
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
            let Some(ui) = weak.upgrade() else { return };
            match page {
                4 => push(&ui, &mut shared.lock()),
                // Settings holds the mixer's devices, and the presets saved in the mixer.
                3 => {
                    let mut s = shared.lock();
                    push(&ui, &mut s);
                    crate::presets_ui::push(&ui, &s.settings);
                }
                _ => {}
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
            let d = &mut s.settings.mixer_devices;
            // The mix follows the default output; an old pick would only confuse.
            d.monitor = None;
            if let Some(sink) = usize::try_from(ui.get_mixer_sink_idx()).ok().and_then(|i| lists.sinks.get(i)) {
                d.sink = Some(sink.clone());
            }
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
            s.preset.mixer.enabled = ui.get_mixer_enabled();
            commit(&mut s);
            // Lists depend on each other (the silent output excludes the
            // Slidr Mic's cable) and the hint on the switch.
            push(&ui, &mut s);
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_move_channel(move |id, to| {
            let mut s = shared.lock();
            let channels = &mut s.preset.mixer.channels;
            let Some(from) = channels.iter().position(|c| c.id as i32 == id) else { return };
            // `to` is an insertion slot in the list as it was, before this channel left it.
            let to = (to.max(0) as usize).min(channels.len());
            let to = if to > from { to - 1 } else { to };
            if to == from {
                return;
            }
            let c = channels.remove(from);
            channels.insert(to, c);
            commit(&mut s);
            if let Some(ui) = weak.upgrade() {
                push_channels(&ui, &s.preset.mixer);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_channel_set(move |id, field, num, text| {
            let mut s = shared.lock();
            let Some(c) = s.preset.mixer.channels.iter_mut().find(|c| c.id as i32 == id) else { return };
            match field.as_str() {
                "collapsed" => {
                    c.collapsed = num > 0.5;
                    commit(&mut s);
                    if let Some(ui) = weak.upgrade() {
                        push_channels(&ui, &s.preset.mixer);
                    }
                    return;
                }
                "name" => c.name = text.to_string(),
                "level" => c.level = (num / 100.0).clamp(0.0, 1.0),
                "muted" => c.muted = num > 0.5,
                "reroute" => c.reroute = num > 0.5,
                "fx-on" => c.fx.on = num > 0.5,
                "send-on" => c.mic_send = (num > 0.5).then_some(c.mic_send.unwrap_or(1.0)),
                "send" => c.mic_send = Some((num / 100.0).clamp(0.0, 1.0)),
                other => log::warn!("mixer page: unknown channel field {other:?}"),
            }
            commit(&mut s);
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
                collapsed: false,
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
            // Opens on microphones for a channel that only has those.
            let tab = i32::from(c.mics_only());
            *PICKER_TAB.lock() = tab;
            ui.set_mixer_picker_tab_index(tab);
            ui.set_mixer_picker_title(format!("Sources for {}", c.name).into());
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
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_picker_tab(move |tab| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(id) = *PICKING.lock() else { return };
            *PICKER_TAB.lock() = tab;
            push_picker(&ui, &shared.lock(), id);
        });
    }
    ui.on_mixer_picker_closed(|| *PICKING.lock() = None);
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_edit_fx(move |which| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(target) = MixTarget::from_value(&which) else { return };
            let mut s = shared.lock();
            let title = format!("Effects – {}", s.preset.mixer.label(target));
            let Some(fx) = effects_mut(&mut s.preset.mixer, target).cloned() else { return };
            *EDITING_FX.lock() = Some(target);
            ui.set_mixer_fx_title(title.into());
            ui.set_mixer_fx_preset_name("".into());
            push_rack(&ui, &s, &fx);
            ui.set_mixer_fx_open(true);
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_fx_param(move |id, n, v| {
            let Some(ui) = weak.upgrade() else { return };
            edit_rack(&ui, &mut shared.lock(), false, |fx| {
                if let Some(slot) = fx.slot_mut(id as u32) {
                    set_param(&mut slot.fx, n, v);
                }
            });
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_fx_toggle(move |id, on| {
            let Some(ui) = weak.upgrade() else { return };
            edit_rack(&ui, &mut shared.lock(), true, |fx| {
                if let Some(slot) = fx.slot_mut(id as u32) {
                    slot.on = on;
                }
            });
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_fx_move(move |id, dir| {
            let Some(ui) = weak.upgrade() else { return };
            edit_rack(&ui, &mut shared.lock(), true, |fx| {
                let Some(i) = fx.chain.iter().position(|s| s.id == id as u32) else { return };
                let j = i as i32 + dir;
                if j >= 0 && (j as usize) < fx.chain.len() {
                    fx.chain.swap(i, j as usize);
                }
            });
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_fx_remove(move |id| {
            let Some(ui) = weak.upgrade() else { return };
            edit_rack(&ui, &mut shared.lock(), true, |fx| fx.chain.retain(|s| s.id != id as u32));
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_fx_add(move |kind| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(new) = Fx::catalog().into_iter().nth(kind as usize) else { return };
            edit_rack(&ui, &mut shared.lock(), true, |fx| {
                let id = fx.next_id();
                fx.chain.push(FxSlot { id, on: true, fx: new });
            });
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_fx_load_preset(move |i| {
            let Some(ui) = weak.upgrade() else { return };
            let mut s = shared.lock();
            let Some(preset) = fx_presets(&s).into_iter().nth(i as usize) else { return };
            edit_rack(&ui, &mut s, true, |fx| fx.chain = preset.chain);
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_fx_save_preset(move |name| {
            let Some(ui) = weak.upgrade() else { return };
            let name = name.trim().to_string();
            let Some(target) = *EDITING_FX.lock() else { return };
            let mut s = shared.lock();
            let Some(fx) = effects_mut(&mut s.preset.mixer, target).cloned() else { return };
            if name.is_empty() {
                return;
            }
            // Same name again replaces the old one.
            let presets = &mut s.settings.fx_presets;
            presets.retain(|p| p.name != name);
            presets.push(FxPreset { name: name.clone(), chain: fx.chain.clone() });
            commit(&mut s);
            push_rack(&ui, &s, &fx);
            crate::glue::toast(&ui, &format!("Saved preset \"{name}\""));
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_fx_closed(move || {
            *EDITING_FX.lock() = None;
            // The strip buttons show how many effects are on.
            if let Some(ui) = weak.upgrade() {
                let s = shared.lock();
                push_channels(&ui, &s.preset.mixer);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_fx_edit_eq(move |id| {
            let Some(ui) = weak.upgrade() else { return };
            let Some(target) = *EDITING_FX.lock() else { return };
            let mut s = shared.lock();
            push_eq_presets(&ui, &s);
            ui.set_mixer_eq_preset_name("".into());
            let m = &mut s.preset.mixer;
            ui.set_mixer_eq_title(format!("EQ – {}", m.label(target)).into());
            let Some(slot) = effects_mut(m, target).and_then(|fx| fx.slot_mut(id as u32)) else { return };
            let Fx::Eq { bands } = &slot.fx else { return };
            *EDITING_EQ.lock() = Some((target, slot.id));
            push_eq(&ui, slot.on, &bands.clone(), true);
            ui.set_mixer_eq_open(true);
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_eq_toggled(move |on| {
            let Some(ui) = weak.upgrade() else { return };
            edit_eq(&ui, &mut shared.lock(), false, true, |slot_on, _| *slot_on = on);
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_eq_set(move |i, field, v| {
            let Some(ui) = weak.upgrade() else { return };
            edit_eq(&ui, &mut shared.lock(), false, true, |on, bands| {
                let Some(b) = bands.get_mut(i as usize) else { return };
                match field.as_str() {
                    "kind" => b.kind = EQ_KINDS.get(v as usize).copied().unwrap_or(EqBandKind::Peak),
                    "freq" => b.freq = freq_from_pos(v),
                    "gain" => b.gain_db = v.clamp(-EQ_RANGE_DB, EQ_RANGE_DB),
                    "q" => b.q = q_from_pos(v),
                    other => log::warn!("EQ editor: unknown field {other:?}"),
                }
                // Editing a band means wanting to hear it.
                *on = true;
            });
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_eq_drag(move |i, x, y| {
            let Some(ui) = weak.upgrade() else { return };
            edit_eq(&ui, &mut shared.lock(), false, false, |on, bands| {
                let Some(b) = bands.get_mut(i as usize) else { return };
                b.freq = freq_from_pos(x);
                if has_gain(b.kind) {
                    b.gain_db = ((0.5 - y.clamp(0.0, 1.0)) * 2.0 * EQ_RANGE_DB).clamp(-EQ_RANGE_DB, EQ_RANGE_DB);
                }
                *on = true;
            });
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_eq_drag_end(move || {
            let Some(ui) = weak.upgrade() else { return };
            edit_eq(&ui, &mut shared.lock(), true, true, |_, _| {});
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_eq_add(move || {
            let Some(ui) = weak.upgrade() else { return };
            edit_eq(&ui, &mut shared.lock(), true, true, |on, bands| {
                bands.push(EqBand { kind: EqBandKind::Peak, freq: 1000.0, gain_db: 0.0, q: 1.0 });
                *on = true;
            });
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_eq_delete(move |i| {
            let Some(ui) = weak.upgrade() else { return };
            edit_eq(&ui, &mut shared.lock(), true, true, |_, bands| {
                if (i as usize) < bands.len() {
                    bands.remove(i as usize);
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
            let Some(preset) = eq_presets(&s).into_iter().nth(i as usize).map(|p| p.bands) else { return };
            edit_eq(&ui, &mut s, true, true, |on, bands| {
                *bands = preset;
                *on = true;
            });
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_eq_save_preset(move |name| {
            let Some(ui) = weak.upgrade() else { return };
            let name = name.trim().to_string();
            let Some((target, id)) = *EDITING_EQ.lock() else { return };
            if name.is_empty() {
                return;
            }
            let mut s = shared.lock();
            let Some(slot) = effects_mut(&mut s.preset.mixer, target).and_then(|fx| fx.slot_mut(id)) else { return };
            let Fx::Eq { bands } = &slot.fx else { return };
            let bands = bands.clone();
            // Same name again replaces the old one.
            let presets = &mut s.settings.eq_presets;
            presets.retain(|p| p.name != name);
            let id = presets.iter().map(|p| p.id).max().unwrap_or(1000) + 1;
            presets.push(EqPreset { id, name: name.clone(), bands });
            commit(&mut s);
            push_eq_presets(&ui, &s);
            crate::glue::toast(&ui, &format!("Saved preset \"{name}\""));
        });
    }
    {
        let weak = ui.as_weak();
        let shared = shared.clone();
        ui.on_mixer_eq_closed(move || {
            *EDITING_EQ.lock() = None;
            // The rack shows the EQ's switch and band count.
            let Some(ui) = weak.upgrade() else { return };
            let Some(target) = *EDITING_FX.lock() else { return };
            let mut s = shared.lock();
            if let Some(fx) = effects_mut(&mut s.preset.mixer, target).cloned() {
                push_rack(&ui, &s, &fx);
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
