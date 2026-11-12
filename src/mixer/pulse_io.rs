//! Linux: the mixer over PulseAudio / PipeWire-Pulse.
//!
//! Like the rest of the Linux audio code (`audio/pulse.rs`) this drives the
//! sound server through its command-line tools — `pactl` to manage devices
//! and move streams, `parec` / `pacat` to record and play raw PCM — so the
//! binary needs no native library (they all come with `pulseaudio-utils`).
//!
//! * Each channel gets a null sink, `slidr_mixer_ch_<id>`. Its programs' streams
//!   are moved there and its monitor is recorded into the channel. Nothing of
//!   them reaches the speakers directly, so this is "Replace original" by
//!   itself; with that switch off a loopback plays the original too.
//! * The mix plays as one stream on the default output (and follows it).
//! * The Slidr Mic is a null sink the mic bus plays into, plus a source
//!   remapped from its monitor and named "Slidr Mic" — no driver needed.
//! * Everything Slidr loads carries `slidr_mixer` in its arguments. Unloading
//!   a null sink moves its streams back to the default output, so switching
//!   off, quitting, and cleaning up after a crash are all "unload ours".
//!
//! Compiled on every platform (it uses only `std::process`) so it is
//! type-checked and its pure parts tested anywhere; only Linux starts it.

#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};

use crate::audio::pulse_parse::{modules_with, parse_defaults, parse_records, parse_short, strip_exe, Record};
use crate::model::{MixTarget, MixerConfig, MixerDevices};

use super::dsp::{Fade, BLOCK_FRAMES, BLOCK_SAMPLES, RATE};
use super::engine::Engine;
use super::ring::Ring;
use super::sfx::{self, Library, Player, Sounds};
use super::Cmd;

/// In the arguments of every module Slidr loads.
const MARKER: &str = "slidr_mixer";
const MIC_SINK: &str = "slidr_mixer_mic";
const MIC_SOURCE: &str = "slidr_mixer_mic_source";
/// `media.name` of the two streams Slidr plays.
const MIX_STREAM: &str = "Slidr mix";
const MIC_STREAM: &str = "Slidr Mic";
/// How far ahead of the device the played streams should run.
const TARGET_MS: u32 = 40;
const SCAN_EVERY: Duration = Duration::from_secs(1);

fn channel_sink(id: u32) -> String {
    format!("{MARKER}_ch_{id}")
}

/// A device description safe inside module arguments (no quotes).
fn description(text: &str) -> String {
    text.chars().filter(|c| c.is_alphanumeric() || " -_.()".contains(*c)).collect()
}

/// `<key>="device.description='<label>'"`: the double quotes keep the label
/// one argument (module arguments split at spaces), the single ones are the
/// property list's own quoting.
fn description_arg(key: &str, label: &str) -> String {
    format!("{key}=\"device.description='{}'\"", description(label))
}

fn null_sink_args(name: &str, label: &str) -> Vec<String> {
    vec![format!("sink_name={name}"), description_arg("sink_properties", label)]
}

/// Whether a playback stream belongs to `program` (a process name or a
/// program category reference).
fn stream_matches(program: &str, s: &Record) -> bool {
    let exe = if s.process_binary.is_empty() { &s.app_name } else { &s.process_binary };
    if exe.is_empty() {
        return false;
    }
    match crate::app_groups::parse_ref(program) {
        Some(id) => crate::app_groups::matches(id, exe, None),
        None => {
            let needle = strip_exe(&program.to_lowercase());
            let name = strip_exe(&exe.to_lowercase());
            !needle.is_empty() && (name.contains(&needle) || needle.contains(&name))
        }
    }
}

/// Slidr's own streams (the mix, the Slidr Mic, loopbacks) are never moved.
fn is_ours(s: &Record) -> bool {
    s.media_name == MIX_STREAM || s.media_name == MIC_STREAM || s.media_name.contains(MARKER) || s.app_name == "Slidr"
}

/// What to do about the played streams' lead over the device: drop a block
/// (too far ahead: the clocks drift and the pipe fills), add one (about to
/// run dry), or nothing.
fn drift_step(buffer_ms: Option<u32>) -> i32 {
    match buffer_ms {
        Some(ms) if ms > TARGET_MS + 40 => -1,
        Some(ms) if ms + 20 < TARGET_MS => 1,
        _ => 0,
    }
}

// ── pactl ──────────────────────────────────────────────────────────────────

fn tool(name: &str) -> Command {
    let mut c = Command::new(name);
    // Parsed output must not be translated (see audio/pulse.rs).
    c.env("LC_ALL", "C").env("LANG", "C").env_remove("LANGUAGE");
    c
}

fn pactl(args: &[&str]) -> Option<String> {
    let out = tool("pactl").args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn load_module(name: &str, args: &[String]) -> Option<u32> {
    let mut all = vec!["load-module", name];
    all.extend(args.iter().map(String::as_str));
    let id = pactl(&all).and_then(|o| o.trim().parse().ok());
    if id.is_none() {
        log::warn!("mixer: could not load {name} {args:?}");
    }
    id
}

fn unload_module(id: u32) {
    let _ = pactl(&["unload-module", &id.to_string()]);
}

/// Unload every module Slidr loaded — also ones left by a crash.
fn unload_ours() {
    if let Some(text) = pactl(&["list", "short", "modules"]) {
        for id in modules_with(&text, MARKER) {
            unload_module(id);
        }
    }
}

// ── Streams ────────────────────────────────────────────────────────────────

fn pcm_args(c: &mut Command) {
    c.args(["--raw", "--format=s16le", "--channels=2", "--client-name=Slidr"]).arg(format!("--rate={RATE}"));
}

/// A recording (`parec`) feeding a ring buffer.
struct Capture {
    ring: Arc<Ring>,
    child: Child,
}

impl Capture {
    fn spawn(device: &str) -> Option<Self> {
        let mut c = tool("parec");
        pcm_args(&mut c);
        c.args(["--latency-msec=20", &format!("--device={device}"), &format!("--stream-name={MARKER} capture")]);
        let mut child = c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
        let mut out = child.stdout.take()?;
        let ring = Arc::new(Ring::default());
        let r = ring.clone();
        let device = device.to_string();
        let spawned = std::thread::Builder::new().name("mixer-parec".into()).spawn(move || {
            let mut bytes = vec![0u8; BLOCK_SAMPLES * 2];
            let mut samples = vec![0f32; BLOCK_SAMPLES];
            while out.read_exact(&mut bytes).is_ok() {
                for (s, b) in samples.iter_mut().zip(bytes.chunks_exact(2)) {
                    *s = i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0;
                }
                r.push(&samples);
            }
            log::debug!("mixer: recording {device} ended");
        });
        if spawned.is_err() {
            let _ = child.kill();
            return None;
        }
        Some(Self { ring, child })
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A played stream (`pacat`), written block by block.
struct Output {
    child: Child,
    stdin: ChildStdin,
    bytes: Vec<u8>,
}

impl Output {
    /// `device` `None` = the default output, followed when it changes.
    fn open(device: Option<&str>, stream: &str) -> Option<Self> {
        let mut c = tool("pacat");
        c.arg("--playback");
        pcm_args(&mut c);
        c.arg(format!("--latency-msec={TARGET_MS}")).arg(format!("--stream-name={stream}"));
        if let Some(d) = device {
            c.arg(format!("--device={d}"));
        }
        let mut child = match c.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
            Ok(ch) => ch,
            Err(e) => {
                log::warn!("mixer: pacat could not start ({e}); is pulseaudio-utils installed?");
                return None;
            }
        };
        let stdin = child.stdin.take()?;
        Some(Self { child, stdin, bytes: Vec::with_capacity(BLOCK_SAMPLES * 2) })
    }

    fn write(&mut self, block: &[f32]) -> std::io::Result<()> {
        self.bytes.clear();
        for s in block {
            self.bytes.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
        }
        self.stdin.write_all(&self.bytes)
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ── Audio thread ───────────────────────────────────────────────────────────

enum AudioCmd {
    Mixer(Cmd),
    /// Recordings: channel → its sink's monitor; (channel, microphone source).
    Streams(HashMap<u32, String>, HashSet<(u32, String)>),
    /// Channels heard through the mix (their programs were replaced).
    Monitored(Vec<(u32, bool)>),
    Sounds(Sounds),
    /// Lead of the played streams over their devices (mix, Slidr Mic), ms.
    Latency(Option<u32>, Option<u32>),
    Stop(Sender<()>),
}

struct Audio {
    engine: Engine,
    cfg: MixerConfig,
    live_levels: HashMap<MixTarget, f32>,
    live_mutes: HashMap<MixTarget, bool>,
    channels: HashMap<u32, (String, Capture)>,
    mics: HashMap<(u32, String), Capture>,
    monitor: Option<Output>,
    mic_out: Option<Output>,
    sounds: Sounds,
    sfx_on: bool,
    sound_set: Option<String>,
    quiet_start: bool,
    player: Player,
    bus: Fade,
    closing: Option<Instant>,
    /// Blocks to drop (< 0) or add (> 0) to keep each played stream's lead.
    drift: (i32, i32),
    /// When to try again to open the played streams that aren't open (the
    /// Slidr Mic's sink may not exist yet, or a stream ended).
    reopen_at: Option<Instant>,
    tmp: Vec<f32>,
    monitor_buf: Vec<f32>,
    mic_buf: Vec<f32>,
}

impl Audio {
    fn new() -> Self {
        Self {
            engine: Engine::default(),
            cfg: MixerConfig::default(),
            live_levels: HashMap::new(),
            live_mutes: HashMap::new(),
            channels: HashMap::new(),
            mics: HashMap::new(),
            monitor: None,
            mic_out: None,
            sounds: Sounds::new(),
            sfx_on: true,
            sound_set: None,
            quiet_start: false,
            player: Player::default(),
            bus: Fade::new(1.0),
            closing: None,
            drift: (0, 0),
            reopen_at: None,
            tmp: vec![0.0; BLOCK_SAMPLES],
            monitor_buf: vec![0.0; BLOCK_SAMPLES],
            mic_buf: vec![0.0; BLOCK_SAMPLES],
        }
    }

    /// Switched on (or still fading out). Mixing also needs the mix stream open.
    fn running(&self) -> bool {
        self.cfg.enabled || self.closing.is_some()
    }

    fn play(&mut self, name: &str) -> u32 {
        match self.sounds.get(name) {
            Some(s) => {
                if self.sfx_on {
                    self.player.play(s.clone());
                }
                s.len() as u32
            }
            None => 0,
        }
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Configure(cfg, _devices) => self.configure(cfg),
            Cmd::QuietStart => self.quiet_start = true,
            Cmd::SetLevel(t, v) => {
                self.live_levels.insert(t, v);
                self.engine.set_level(t, v);
            }
            Cmd::SetMute(t, m) => {
                self.live_mutes.insert(t, m);
                self.engine.set_mute(t, m, BLOCK_FRAMES as u32);
            }
            Cmd::ToggleMute(t) => {
                if let Some(m) = self.engine.muted(t) {
                    self.live_mutes.insert(t, !m);
                    let len = self.play(&sfx::switch_sound("mute", !m)).max(BLOCK_FRAMES as u32);
                    self.engine.set_mute(t, !m, len);
                }
            }
            Cmd::SoundPrefs(on, volume, set) => {
                self.sfx_on = on;
                self.player.gain = volume.clamp(0.0, 1.0);
                let changed = self.sound_set.as_ref().is_some_and(|s| *s != set);
                self.sound_set = Some(set);
                if changed && self.running() && self.closing.is_none() {
                    self.play("mute_on");
                }
            }
            Cmd::Shutdown(_) => unreachable!("the control thread handles shutdown"),
        }
    }

    fn configure(&mut self, cfg: MixerConfig) {
        let was_running = self.running();
        let old = std::mem::take(&mut self.cfg);
        self.live_levels.retain(|t, _| old.level(*t) == cfg.level(*t));
        self.live_mutes.retain(|t, _| muted_of(&old, *t) == muted_of(&cfg, *t));
        let live = was_running && cfg.enabled;
        let none = Sounds::new();
        let engine = &self.engine;
        let live_mutes = &self.live_mutes;
        let (sounds, fades) = sfx::diff(
            &old,
            &cfg,
            &|t| engine.muted(t),
            &|t| live_mutes.get(&t).copied().or_else(|| muted_of(&cfg, t)),
            if live { &self.sounds } else { &none },
        );
        if self.sfx_on {
            for s in sounds {
                self.player.play(s);
            }
        }
        self.engine.configure(&cfg, &fades);
        for (t, v) in &self.live_levels {
            self.engine.set_level(*t, *v);
        }
        for (t, m) in &self.live_mutes {
            self.engine.set_mute(*t, *m, fades.gain(*t));
        }
        let (was_on, now_on) = (old.enabled, cfg.enabled);
        self.cfg = cfg;
        let starting = std::mem::take(&mut self.quiet_start);
        if now_on && !was_on {
            self.closing = None;
            self.open_outputs();
            self.bus = Fade::new(0.0);
            let len = if starting { 0 } else { self.play("power_on") };
            self.bus.set_over(1.0, len.max(BLOCK_FRAMES as u32));
        } else if was_on && !now_on && was_running {
            let len = self.play("power_off").max(BLOCK_FRAMES as u32);
            self.bus.set_over(0.0, len);
            self.closing = Some(Instant::now() + Duration::from_secs_f32(len as f32 / RATE as f32));
        } else if now_on {
            self.closing = None;
            self.bus.set_over(1.0, BLOCK_FRAMES as u32);
        }
    }

    /// Open whichever played stream isn't open; try again in a second if
    /// one doesn't come up.
    fn open_outputs(&mut self) {
        if self.monitor.is_none() {
            self.monitor = Output::open(None, MIX_STREAM);
            self.drift.0 = 0;
        }
        if self.mic_out.is_none() {
            self.mic_out = Output::open(Some(MIC_SINK), MIC_STREAM);
            self.drift.1 = 0;
        }
        let missing = self.monitor.is_none() || self.mic_out.is_none();
        self.reopen_at = missing.then(|| Instant::now() + Duration::from_secs(1));
    }

    fn reopen_if_due(&mut self) {
        if self.reopen_at.is_some_and(|t| Instant::now() >= t) {
            self.open_outputs();
        }
    }

    fn close(&mut self) {
        self.channels.clear();
        self.mics.clear();
        self.monitor = None;
        self.mic_out = None;
        self.reopen_at = None;
    }

    fn finish_closing(&mut self) {
        if self.closing.is_some_and(|t| Instant::now() >= t) {
            self.closing = None;
            self.close();
            self.bus = Fade::new(1.0);
        }
    }

    fn set_streams(&mut self, channels: HashMap<u32, String>, mics: HashSet<(u32, String)>) {
        self.channels.retain(|id, (dev, _)| channels.get(id) == Some(dev));
        for (id, dev) in channels {
            if !self.channels.contains_key(&id) {
                if let Some(cap) = Capture::spawn(&dev) {
                    self.channels.insert(id, (dev, cap));
                }
            }
        }
        self.mics.retain(|k, _| mics.contains(k));
        for key in mics {
            if !self.mics.contains_key(&key) {
                match Capture::spawn(&key.1) {
                    Some(cap) => {
                        self.mics.insert(key, cap);
                    }
                    None => log::warn!("mixer: could not record {:?}", key.1),
                }
            }
        }
    }

    fn mix_block(&mut self) {
        let mut sums: HashMap<u32, Vec<f32>> = HashMap::new();
        let inputs = self.channels.iter().map(|(id, (_, c))| (*id, c)).chain(self.mics.iter().map(|((id, _), c)| (*id, c)));
        for (id, cap) in inputs {
            cap.ring.pop(&mut self.tmp);
            let sum = sums.entry(id).or_insert_with(|| vec![0.0; BLOCK_SAMPLES]);
            for (s, t) in sum.iter_mut().zip(&self.tmp) {
                *s += t;
            }
        }
        self.engine.process(&mut sums, None, &mut self.monitor_buf, &mut self.mic_buf);
        if !self.bus.is_at(1.0) {
            for (m, k) in self.monitor_buf.chunks_exact_mut(2).zip(self.mic_buf.chunks_exact_mut(2)) {
                let g = self.bus.next();
                m[0] *= g;
                m[1] *= g;
                k[0] *= g;
                k[1] *= g;
            }
        }
        self.player.mix_into(&mut self.monitor_buf);
    }

    /// Write one block to each played stream, dropping or doubling a block
    /// now and then to hold its lead (see [`drift_step`]).
    fn write(&mut self) {
        let mut failed = false;
        for (out, buf, drift) in [
            (&mut self.monitor, &self.monitor_buf, &mut self.drift.0),
            (&mut self.mic_out, &self.mic_buf, &mut self.drift.1),
        ] {
            let Some(o) = out.as_mut() else { continue };
            let times = match *drift {
                d if d < 0 => 0,
                d if d > 0 => 2,
                _ => 1,
            };
            *drift = 0;
            if (0..times).any(|_| o.write(buf).is_err()) {
                *out = None;
                failed = true;
            }
        }
        if failed && self.reopen_at.is_none() {
            log::warn!("mixer: a played stream ended; reopening it in a second");
            self.reopen_at = Some(Instant::now() + Duration::from_secs(1));
        }
    }
}

fn muted_of(cfg: &MixerConfig, t: MixTarget) -> Option<bool> {
    match t {
        MixTarget::Voice => Some(cfg.voice.muted),
        MixTarget::Channel(id) | MixTarget::Send(id) => cfg.channels.iter().find(|c| c.id == id).map(|c| c.muted),
    }
}

fn audio_thread(rx: Receiver<AudioCmd>) {
    let mut audio = Audio::new();
    let block = Duration::from_secs_f64(BLOCK_FRAMES as f64 / RATE as f64);
    let mut next = Instant::now();
    loop {
        let first = if audio.running() {
            rx.try_recv().ok()
        } else {
            match rx.recv() {
                Ok(c) => Some(c),
                Err(_) => break,
            }
        };
        for cmd in first.into_iter().chain(rx.try_iter()) {
            match cmd {
                AudioCmd::Mixer(c) => audio.handle(c),
                AudioCmd::Streams(..) | AudioCmd::Monitored(_) if audio.closing.is_some() => {}
                AudioCmd::Streams(ch, mics) => audio.set_streams(ch, mics),
                AudioCmd::Monitored(list) => {
                    for (ch, on) in list {
                        audio.engine.set_monitored(ch, on);
                    }
                }
                AudioCmd::Sounds(s) => audio.sounds = s,
                AudioCmd::Latency(mix, mic) => audio.drift = (drift_step(mix), drift_step(mic)),
                AudioCmd::Stop(ack) => {
                    audio.close();
                    let _ = ack.send(());
                    return;
                }
            }
        }
        if audio.running() {
            audio.reopen_if_due();
        }
        if audio.running() && audio.monitor.is_none() {
            // Waiting for the mix stream to come up.
            std::thread::sleep(block);
            next = Instant::now();
        } else if audio.running() {
            // Mix on the wall clock, one block per 10 ms; the latency
            // feedback corrects the clocks drifting apart.
            let now = Instant::now();
            if next + block * 10 < now {
                next = now; // fell far behind (suspend): start over
            }
            while next <= now {
                audio.mix_block();
                audio.write();
                next += block;
            }
            std::thread::sleep(next.saturating_duration_since(Instant::now()).min(block));
        } else {
            next = Instant::now();
        }
        audio.finish_closing();
    }
    audio.close();
}

// ── Control thread ─────────────────────────────────────────────────────────

/// The devices Slidr loaded for the mixer: module ids by what they are.
#[derive(Default)]
struct Loaded {
    /// channel → (null sink module, loopback module when not replacing).
    channels: HashMap<u32, (u32, Option<u32>)>,
    /// The Slidr Mic: null sink and remapped source.
    mic: Option<(u32, u32)>,
}

struct Control {
    audio: Sender<AudioCmd>,
    cfg: MixerConfig,
    loaded: Loaded,
    library: Library,
    /// Streams Slidr moved, by sink-input index → channel.
    moved: HashMap<String, u32>,
    sent: (HashMap<u32, String>, HashSet<(u32, String)>),
    restore_at: Option<Instant>,
}

impl Control {
    fn configure(&mut self, cfg: MixerConfig, devices: MixerDevices) {
        self.refresh_sounds();
        if self.cfg.enabled && !cfg.enabled {
            let fade = self.library.sounds().get("power_off").map_or(0, |s| s.len());
            self.restore_at = Some(Instant::now() + Duration::from_secs_f32(fade as f32 / RATE as f32));
        } else if cfg.enabled {
            self.restore_at = None;
        }
        // Renamed channels: the sinks carry the name, reload them.
        for c in &cfg.channels {
            if self.cfg.channels.iter().any(|o| o.id == c.id && o.name != c.name) {
                self.drop_channel(c.id);
            }
        }
        self.cfg = cfg.clone();
        let _ = self.audio.send(AudioCmd::Mixer(Cmd::Configure(cfg, devices)));
    }

    fn refresh_sounds(&mut self) {
        if self.library.refresh() {
            let _ = self.audio.send(AudioCmd::Sounds(self.library.sounds()));
        }
    }

    fn drop_channel(&mut self, id: u32) {
        if let Some((sink, loopback)) = self.loaded.channels.remove(&id) {
            if let Some(l) = loopback {
                unload_module(l);
            }
            unload_module(sink);
        }
        self.moved.retain(|_, ch| *ch != id);
    }

    /// Unload everything; the sound server moves the programs back.
    fn restore(&mut self) {
        let ids: Vec<u32> = self.loaded.channels.keys().copied().collect();
        for id in ids {
            self.drop_channel(id);
        }
        if let Some((sink, source)) = self.loaded.mic.take() {
            unload_module(source);
            unload_module(sink);
        }
        self.moved.clear();
    }

    fn restore_if_due(&mut self) {
        if self.restore_at.is_some_and(|t| Instant::now() >= t) {
            self.restore_at = None;
            self.restore();
        }
    }

    fn scan(&mut self) {
        let on = self.cfg.enabled && self.restore_at.is_none();
        if !on {
            if self.restore_at.is_none() && (!self.loaded.channels.is_empty() || self.loaded.mic.is_some()) {
                self.restore();
            }
            self.send_streams(HashMap::new(), HashSet::new());
            return;
        }
        // The Slidr Mic.
        if self.loaded.mic.is_none() {
            let sink = load_module("module-null-sink", &null_sink_args(MIC_SINK, "Slidr Mic (output)"));
            let source = sink.and_then(|_| {
                load_module(
                    "module-remap-source",
                    &[
                        format!("master={MIC_SINK}.monitor"),
                        format!("source_name={MIC_SOURCE}"),
                        description_arg("source_properties", "Slidr Mic"),
                    ],
                )
            });
            match (sink, source) {
                (Some(a), Some(b)) => self.loaded.mic = Some((a, b)),
                (Some(a), None) => unload_module(a),
                _ => {}
            }
        }
        // One sink per channel; a loopback plays the original when it isn't replaced.
        let gone: Vec<u32> =
            self.loaded.channels.keys().filter(|id| !self.cfg.channels.iter().any(|c| c.id == **id)).copied().collect();
        for id in gone {
            self.drop_channel(id);
        }
        for c in self.cfg.channels.clone() {
            let sink = channel_sink(c.id);
            if !self.loaded.channels.contains_key(&c.id) {
                if let Some(m) = load_module("module-null-sink", &null_sink_args(&sink, &format!("Slidr {}", c.name))) {
                    self.loaded.channels.insert(c.id, (m, None));
                }
            }
            let Some(entry) = self.loaded.channels.get_mut(&c.id) else { continue };
            match (c.reroute, entry.1) {
                (false, None) => {
                    entry.1 = load_module(
                        "module-loopback",
                        &[format!("source={sink}.monitor"), "latency_msec=30".into(), format!("sink_input_properties=media.name={MARKER}_loopback")],
                    );
                }
                (true, Some(l)) => {
                    unload_module(l);
                    entry.1 = None;
                }
                _ => {}
            }
        }

        // Move matching programs into their channel's sink.
        let inputs = pactl(&["list", "sink-inputs"]).map(|t| parse_records(&t, "Sink Input #")).unwrap_or_default();
        let sinks = pactl(&["list", "short", "sinks"]).map(|t| parse_short(&t)).unwrap_or_default();
        let sink_index = |name: &str| sinks.iter().find(|(_, n)| n == name).map(|(i, _)| i.clone());
        self.moved.retain(|idx, _| inputs.iter().any(|s| &s.id == idx));
        // Programs taken out of a channel go back to the default output.
        let stale: Vec<String> = self
            .moved
            .iter()
            .filter(|(idx, ch)| {
                let Some(s) = inputs.iter().find(|s| &s.id == *idx) else { return false };
                !self.cfg.channels.iter().any(|c| c.id == **ch && c.programs.iter().any(|p| stream_matches(p, s)))
            })
            .map(|(idx, _)| idx.clone())
            .collect();
        for idx in stale {
            let _ = pactl(&["move-sink-input", &idx, "@DEFAULT_SINK@"]);
            self.moved.remove(&idx);
        }
        for s in inputs.iter().filter(|s| !is_ours(s)) {
            let program_of = |c: &crate::model::MixChannel| c.programs.iter().any(|p| stream_matches(p, s));
            let Some(c) = self.cfg.channels.iter().find(|c| program_of(c)) else { continue };
            let sink = channel_sink(c.id);
            let there = sink_index(&sink).is_some_and(|i| i == s.sink);
            if !there && self.loaded.channels.contains_key(&c.id) {
                if pactl(&["move-sink-input", &s.id, &sink]).is_some() {
                    log::info!("mixer: {} into channel {}", s.process_label(), c.id);
                    self.moved.insert(s.id.clone(), c.id);
                }
            }
        }
        let latency = |name: &str| inputs.iter().find(|s| s.media_name == name).and_then(|s| s.buffer_ms);
        let _ = self.audio.send(AudioCmd::Latency(latency(MIX_STREAM), latency(MIC_STREAM)));

        // What the audio thread records, and which channels it plays.
        let channels: HashMap<u32, String> = self
            .cfg
            .channels
            .iter()
            .filter(|c| self.loaded.channels.contains_key(&c.id))
            .map(|c| (c.id, format!("{}.monitor", channel_sink(c.id))))
            .collect();
        let mics = self.mic_sources();
        self.send_streams(channels, mics);
        let monitored = self.cfg.channels.iter().map(|c| (c.id, c.reroute || c.mics_only())).collect();
        let _ = self.audio.send(AudioCmd::Monitored(monitored));
    }

    /// The channels' microphones as source names. The page stores what the
    /// user picked (a description); "" is the default source.
    fn mic_sources(&self) -> HashSet<(u32, String)> {
        let wanted: Vec<(u32, String)> =
            self.cfg.channels.iter().flat_map(|c| c.mics().map(move |m| (c.id, m.to_string()))).collect();
        if wanted.is_empty() {
            return HashSet::new();
        }
        let sources = pactl(&["list", "sources"]).map(|t| parse_records(&t, "Source #")).unwrap_or_default();
        let default = pactl(&["info"]).map(|t| parse_defaults(&t).1).unwrap_or_default();
        wanted
            .into_iter()
            .filter_map(|(ch, mic)| {
                let name = if mic.is_empty() {
                    default.clone()
                } else {
                    sources.iter().find(|s| s.label() == mic || s.name == mic).map(|s| s.name.clone())?
                };
                // Recording the Slidr Mic (say, as the default source) would feed it back into itself.
                if name.is_empty() || name.contains(MARKER) {
                    log::error!("mixer: microphone {mic:?} is the Slidr Mic itself; not recorded");
                    return None;
                }
                Some((ch, name))
            })
            .collect()
    }

    fn send_streams(&mut self, channels: HashMap<u32, String>, mics: HashSet<(u32, String)>) {
        if (&channels, &mics) != (&self.sent.0, &self.sent.1) {
            self.sent = (channels.clone(), mics.clone());
            let _ = self.audio.send(AudioCmd::Streams(channels, mics));
        }
    }

    fn shutdown(&mut self) {
        let (tx, rx) = crossbeam_channel::bounded(1);
        if self.audio.send(AudioCmd::Stop(tx)).is_ok() {
            let _ = rx.recv_timeout(Duration::from_secs(2));
        }
        self.restore();
    }
}

pub fn run(rx: Receiver<Cmd>) {
    if pactl(&["info"]).is_none() {
        log::error!("mixer: no PulseAudio / PipeWire server reachable through pactl; mixer disabled");
        return;
    }
    // Devices a crashed run left behind.
    unload_ours();
    let (audio_tx, audio_rx) = crossbeam_channel::unbounded();
    if let Err(e) = std::thread::Builder::new().name("mixer-audio".into()).spawn(move || audio_thread(audio_rx)) {
        log::error!("mixer: could not start the audio thread: {e}");
        return;
    }
    let mut control = Control {
        audio: audio_tx,
        cfg: MixerConfig::default(),
        loaded: Loaded::default(),
        library: Library::default(),
        moved: HashMap::new(),
        sent: (HashMap::new(), HashSet::new()),
        restore_at: None,
    };
    control.refresh_sounds();
    let mut next_scan = Instant::now();
    loop {
        let due = control.restore_at.map_or(next_scan, |t| t.min(next_scan));
        match rx.recv_timeout(due.saturating_duration_since(Instant::now())) {
            Ok(Cmd::Shutdown(ack)) => {
                control.shutdown();
                let _ = ack.send(());
                return;
            }
            Ok(Cmd::Configure(cfg, devices)) => {
                control.configure(cfg, devices);
                next_scan = Instant::now();
            }
            Ok(Cmd::SoundPrefs(on, volume, set)) => {
                let switched = control.library.select(&set);
                if control.library.refresh() || switched {
                    let _ = control.audio.send(AudioCmd::Sounds(control.library.sounds()));
                }
                let _ = control.audio.send(AudioCmd::Mixer(Cmd::SoundPrefs(on, volume, set)));
            }
            Ok(cmd) => {
                let _ = control.audio.send(AudioCmd::Mixer(cmd));
            }
            Err(RecvTimeoutError::Timeout) => {
                control.restore_if_due();
                if Instant::now() >= next_scan {
                    control.refresh_sounds();
                    control.scan();
                    next_scan = Instant::now() + SCAN_EVERY;
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    control.shutdown();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(binary: &str, media: &str) -> Record {
        Record { process_binary: binary.into(), media_name: media.into(), ..Default::default() }
    }

    #[test]
    fn programs_match_their_streams_but_never_ours() {
        assert!(stream_matches("spotify", &stream("spotify", "Spotify")));
        assert!(stream_matches("Firefox.exe", &stream("firefox", "Video")));
        assert!(!stream_matches("discord", &stream("spotify", "x")));
        assert!(!stream_matches("anything", &stream("", "")), "a stream without a program");
        assert!(is_ours(&stream("pacat", MIX_STREAM)));
        assert!(is_ours(&stream("pipewire", "slidr_mixer_loopback")));
        assert!(!is_ours(&stream("pacat", "something else")));
    }

    #[test]
    fn device_names_and_arguments() {
        assert_eq!(channel_sink(3), "slidr_mixer_ch_3");
        assert_eq!(
            null_sink_args("slidr_mixer_ch_3", "Slidr Mu'sic \"x\""),
            vec!["sink_name=slidr_mixer_ch_3", "sink_properties=\"device.description='Slidr Music x'\""]
        );
    }

    #[test]
    fn drift_keeps_the_lead_near_the_target() {
        assert_eq!(drift_step(None), 0);
        assert_eq!(drift_step(Some(TARGET_MS)), 0);
        assert_eq!(drift_step(Some(TARGET_MS + 60)), -1, "pipe filling up: drop a block");
        assert_eq!(drift_step(Some(5)), 1, "about to run dry: add one");
    }
}
