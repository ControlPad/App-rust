//! Feedback sounds for the mixer's switches, and how long the switched thing
//! fades: a switch takes as long as its sound, so you hear the click and the
//! change together.
//!
//! Sounds are `<name>_on.wav` / `<name>_off.wav` (16-bit PCM, 48 kHz, mono or
//! stereo) read from a folder and re-read when a file changes, so they can be
//! tuned while Slidr runs. They play on the monitor only, never into the
//! Slidr Mic.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use crate::model::{MixTarget, MixerConfig};

use super::dsp::{BLOCK_FRAMES, RATE};

/// Mono samples at [`RATE`].
pub type Sound = Arc<Vec<f32>>;
pub type Sounds = HashMap<String, Sound>;

/// Fade for a switch whose sound file is missing.
const SILENT_FADE: u32 = RATE / 4;
/// Default level of the feedback sounds.
pub const SFX_GAIN: f32 = 0.8;

/// The sound sets to choose from in Settings: (folder under `sets/`, name).
pub const SETS: [(&str, &str); 3] = [("tide", "Tide"), ("grid", "Grid"), ("grain", "Grain")];
pub const DEFAULT_SET: &str = "tide";

/// Where the sounds are read from: `SLIDR_SOUND_DIR`, else `~/mixer-sounds`
/// (while they are being designed; they move into the repo once settled).
/// Each set is a folder `sets/<id>` in there.
pub fn sound_dir() -> Option<PathBuf> {
    std::env::var_os("SLIDR_SOUND_DIR").map(PathBuf::from).or_else(|| dirs::home_dir().map(|h| h.join("mixer-sounds")))
}

/// 16-bit PCM WAV at 48 kHz → mono samples. Anything else is refused.
pub fn parse_wav(bytes: &[u8]) -> Option<Vec<f32>> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let (mut channels, mut rate, mut bits, mut data) = (0u16, 0u32, 0u16, None);
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().ok()?) as usize;
        let body = bytes.get(at + 8..at + 8 + len)?;
        match id {
            b"fmt " if len >= 16 => {
                if u16::from_le_bytes([body[0], body[1]]) != 1 {
                    return None; // not PCM
                }
                channels = u16::from_le_bytes([body[2], body[3]]);
                rate = u32::from_le_bytes(body[4..8].try_into().ok()?);
                bits = u16::from_le_bytes([body[14], body[15]]);
            }
            b"data" => data = Some(body),
            _ => {}
        }
        at += 8 + len + (len & 1);
    }
    let data = data?;
    if bits != 16 || rate != RATE || !(1..=2).contains(&channels) {
        return None;
    }
    let samples = data.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0);
    Some(if channels == 1 {
        samples.collect()
    } else {
        let s: Vec<f32> = samples.collect();
        s.chunks_exact(2).map(|f| (f[0] + f[1]) * 0.5).collect()
    })
}

/// The chosen set's folder, re-read file by file when something changed.
pub struct Library {
    set: String,
    files: HashMap<String, (SystemTime, Sound)>,
}

impl Default for Library {
    fn default() -> Self {
        Self { set: DEFAULT_SET.into(), files: HashMap::new() }
    }
}

impl Library {
    /// Switch to another set (an unknown one falls back to the default);
    /// the next [`Library::refresh`] loads it. `true` if it changed.
    pub fn select(&mut self, set: &str) -> bool {
        let set = SETS.iter().find(|s| s.0 == set).map_or(DEFAULT_SET, |s| s.0);
        if self.set == set {
            return false;
        }
        self.set = set.into();
        self.files.clear();
        true
    }

    /// Pick up added, changed and removed files. `true` if anything changed.
    pub fn refresh(&mut self) -> bool {
        let Some(dir) = sound_dir().map(|d| d.join("sets").join(&self.set)) else { return false };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            let had = !self.files.is_empty();
            self.files.clear();
            return had;
        };
        let mut seen = Vec::new();
        let mut changed = false;
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("wav") {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|s| s.to_str()).map(str::to_string) else { continue };
            let Ok(modified) = e.metadata().and_then(|m| m.modified()) else { continue };
            seen.push(name.clone());
            if self.files.get(&name).is_some_and(|(t, _)| *t == modified) {
                continue;
            }
            match std::fs::read(&path).ok().and_then(|b| parse_wav(&b)) {
                Some(s) => {
                    self.files.insert(name, (modified, Arc::new(s)));
                    changed = true;
                }
                None => log::warn!("mixer: {} is not 16-bit PCM at 48 kHz; skipped", path.display()),
            }
        }
        let before = self.files.len();
        self.files.retain(|n, _| seen.contains(n));
        changed || self.files.len() != before
    }

    pub fn sounds(&self) -> Sounds {
        self.files.iter().map(|(n, (_, s))| (n.clone(), s.clone())).collect()
    }
}

/// Fade lengths (in frames) for what a layout change switched. Everything
/// not listed changes within one block.
#[derive(Default)]
pub struct Fades {
    /// A strip's gains (mute, hear-yourself), keyed by `Voice` / `Channel`.
    pub gain: HashMap<MixTarget, u32>,
    /// One effect slot: (strip, slot id).
    pub slots: HashMap<(MixTarget, u32), u32>,
}

impl Fades {
    pub fn gain(&self, strip: MixTarget) -> u32 {
        self.gain.get(&strip).copied().unwrap_or(BLOCK_FRAMES as u32)
    }

    pub fn slot(&self, strip: MixTarget, id: u32) -> u32 {
        self.slots.get(&(strip, id)).copied().unwrap_or(BLOCK_FRAMES as u32)
    }
}

pub fn switch_sound(base: &str, on: bool) -> String {
    format!("{base}_{}", if on { "on" } else { "off" })
}

/// What a layout change switched: the sounds to play and how long each
/// switched thing fades. `muted_*` are the effective mutes (board toggles
/// included) before and after.
pub fn diff(
    old: &MixerConfig,
    new: &MixerConfig,
    muted_before: &dyn Fn(MixTarget) -> Option<bool>,
    muted_after: &dyn Fn(MixTarget) -> Option<bool>,
    sounds: &Sounds,
) -> (Vec<Sound>, Fades) {
    let mut play = Vec::new();
    let mut fades = Fades::default();
    let mut switched = |name: String, fallback: u32| -> u32 {
        match sounds.get(&name) {
            Some(s) => {
                play.push(s.clone());
                s.len() as u32
            }
            None => fallback,
        }
    };

    let mut strips = vec![(MixTarget::Voice, &old.voice.fx, &new.voice.fx)];
    for c in &new.channels {
        if let Some(o) = old.channels.iter().find(|o| o.id == c.id) {
            strips.push((MixTarget::Channel(c.id), &o.fx, &c.fx));
        }
    }
    for (strip, _, _) in &strips {
        if let (Some(a), Some(b)) = (muted_before(*strip), muted_after(*strip)) {
            if a != b {
                let len = switched(switch_sound("mute", b), BLOCK_FRAMES as u32);
                fades.gain.insert(*strip, len);
            }
        }
    }
    for c in &new.channels {
        let Some(o) = old.channels.iter().find(|o| o.id == c.id) else { continue };
        if o.reroute != c.reroute {
            switched(switch_sound("replace", c.reroute), 0);
        }
        // Into the Slidr Mic: the send fades in / out with its sound.
        if o.mic_send.is_some() != c.mic_send.is_some() {
            let len = switched(switch_sound("mic", c.mic_send.is_some()), BLOCK_FRAMES as u32);
            let e = fades.gain.entry(MixTarget::Channel(c.id)).or_insert(0);
            *e = (*e).max(len);
        }
    }
    if old.voice.to_monitor != new.voice.to_monitor {
        let len = switched(switch_sound("monitor", new.voice.to_monitor), BLOCK_FRAMES as u32);
        let e = fades.gain.entry(MixTarget::Voice).or_insert(0);
        *e = (*e).max(len);
    }
    for (strip, a, b) in strips {
        let master = a.on != b.on;
        // The master switch clicks once for the whole chain: `effects_on/off`,
        // or the EQ's sound while there is no file of its own.
        let master_len = if master {
            let own = switch_sound("effects", b.on);
            let name = if sounds.contains_key(&own) { own } else { switch_sound("eq", b.on) };
            switched(name, SILENT_FADE)
        } else {
            0
        };
        for slot in &b.chain {
            let Some(was) = a.chain.iter().find(|s| s.id == slot.id) else { continue };
            let (before, after) = (was.on && a.on, slot.on && b.on);
            if before == after {
                continue;
            }
            // Switches inside the effects menu stay quiet; only the master clicks.
            let len = if master { master_len } else { SILENT_FADE };
            fades.slots.insert((strip, slot.id), len);
        }
    }
    (play, fades)
}

/// Sounds currently playing, mixed onto the monitor.
pub struct Player {
    playing: Vec<(Sound, usize)>,
    pub gain: f32,
}

impl Default for Player {
    fn default() -> Self {
        Self { playing: Vec::new(), gain: SFX_GAIN }
    }
}

impl Player {
    pub fn play(&mut self, s: Sound) {
        self.playing.push((s, 0));
    }

    /// Add the next stretch of every playing sound to a stereo block.
    pub fn mix_into(&mut self, out: &mut [f32]) {
        let gain = self.gain;
        for (s, pos) in &mut self.playing {
            for frame in out.chunks_exact_mut(2) {
                let Some(v) = s.get(*pos) else { break };
                frame[0] += v * gain;
                frame[1] += v * gain;
                *pos += 1;
            }
        }
        self.playing.retain(|(s, pos)| *pos < s.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Effects, Fx, FxSlot, Limiter, MixChannel};

    fn wav(channels: u16, rate: u32, samples: &[i16]) -> Vec<u8> {
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut b = b"RIFF".to_vec();
        b.extend((36 + data.len() as u32).to_le_bytes());
        b.extend(b"WAVEfmt ");
        b.extend(16u32.to_le_bytes());
        b.extend(1u16.to_le_bytes());
        b.extend(channels.to_le_bytes());
        b.extend(rate.to_le_bytes());
        b.extend((rate * channels as u32 * 2).to_le_bytes());
        b.extend((channels * 2).to_le_bytes());
        b.extend(16u16.to_le_bytes());
        b.extend(b"data");
        b.extend((data.len() as u32).to_le_bytes());
        b.extend(data);
        b
    }

    #[test]
    fn reads_mono_and_folds_stereo() {
        assert_eq!(parse_wav(&wav(1, 48000, &[16384, -16384])).unwrap(), vec![0.5, -0.5]);
        assert_eq!(parse_wav(&wav(2, 48000, &[16384, 0])).unwrap(), vec![0.25]);
        assert!(parse_wav(&wav(1, 44100, &[0])).is_none(), "other rates are refused");
        assert!(parse_wav(b"not a wav").is_none());
    }

    fn cfg(muted: bool, slot_on: bool, fx_on: bool) -> MixerConfig {
        let fx = Effects { on: fx_on, chain: vec![FxSlot { id: 7, on: slot_on, fx: Fx::Limiter(Limiter::default()) }] };
        MixerConfig {
            enabled: true,
            channels: vec![MixChannel {
                id: 1,
                name: "c".into(),
                programs: vec![],
                level: 1.0,
                muted,
                fx,
                reroute: true,
                mic_send: None,
                collapsed: false,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn a_switch_plays_its_sound_and_fades_for_as_long() {
        let sounds: Sounds = [("mute_on", 300usize), ("limiter_off", 500)]
            .into_iter()
            .map(|(n, l)| (n.to_string(), Arc::new(vec![0.0; l])))
            .collect();
        let (a, b) = (cfg(false, true, true), cfg(true, false, true));
        let muted = |c: &MixerConfig| {
            let m = c.channels[0].muted;
            move |t| (t == MixTarget::Channel(1)).then_some(m)
        };
        let (play, fades) = diff(&a, &b, &muted(&a), &muted(&b), &sounds);
        assert_eq!(play.len(), 1, "effects inside the menu switch quietly");
        assert_eq!(fades.gain(MixTarget::Channel(1)), 300);
        assert_eq!(fades.slot(MixTarget::Channel(1), 7), SILENT_FADE);
        // Nothing switched: nothing plays, quick changes.
        let (play, fades) = diff(&a, &a, &muted(&a), &muted(&a), &sounds);
        assert!(play.is_empty());
        assert_eq!(fades.slot(MixTarget::Channel(1), 7), BLOCK_FRAMES as u32);
    }

    #[test]
    fn replace_and_mic_switches_have_their_sounds() {
        let sounds: Sounds = [("replace_off", 100usize), ("mic_on", 700)]
            .into_iter()
            .map(|(n, l)| (n.to_string(), Arc::new(vec![0.0; l])))
            .collect();
        let a = cfg(false, true, true);
        let mut b = a.clone();
        b.channels[0].reroute = false;
        b.channels[0].mic_send = Some(0.5);
        let (play, fades) = diff(&a, &b, &|_| None, &|_| None, &sounds);
        assert_eq!(play.len(), 2);
        assert_eq!(fades.gain(MixTarget::Channel(1)), 700, "the send fades with its sound");
    }

    #[test]
    fn master_switch_clicks_once_and_fades_every_effect() {
        let (a, b) = (cfg(false, true, true), cfg(false, true, false));
        let (play, fades) = diff(&a, &b, &|_| None, &|_| None, &Sounds::new());
        assert!(play.is_empty(), "no file, no sound");
        assert_eq!(fades.slot(MixTarget::Channel(1), 7), SILENT_FADE);
        let eq: Sounds = [("eq_off".to_string(), Arc::new(vec![0.0; 900]))].into();
        let (play, fades) = diff(&a, &b, &|_| None, &|_| None, &eq);
        assert_eq!(play.len(), 1, "falls back to the EQ sound");
        assert_eq!(fades.slot(MixTarget::Channel(1), 7), 900);
        let own: Sounds = [("effects_off".to_string(), Arc::new(vec![0.0; 400])), ("eq_off".to_string(), Arc::new(vec![0.0; 900]))].into();
        assert_eq!(diff(&a, &b, &|_| None, &|_| None, &own).1.slot(MixTarget::Channel(1), 7), 400);
    }

    #[test]
    fn selecting_a_set_drops_the_old_sounds_and_unknown_sets_fall_back() {
        let mut lib = Library::default();
        lib.files.insert("mute_on".into(), (SystemTime::UNIX_EPOCH, Arc::new(vec![0.0])));
        assert!(!lib.select("tide"), "already the default");
        assert!(lib.select("grid"));
        assert!(lib.sounds().is_empty());
        assert!(lib.select("no-such-set"));
        assert_eq!(lib.set, DEFAULT_SET);
    }

    #[test]
    fn player_mixes_to_both_sides_and_finishes() {
        let mut p = Player::default();
        p.play(Arc::new(vec![1.0; 3]));
        let mut out = vec![0.0; 8];
        p.mix_into(&mut out);
        assert_eq!(&out[..6], &[SFX_GAIN; 6]);
        assert_eq!(&out[6..], &[0.0, 0.0]);
        assert!(p.playing.is_empty());
    }
}
