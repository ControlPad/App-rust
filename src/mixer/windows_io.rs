//! Windows side of the mixer: device I/O, per-program capture, routing, and
//! the thread that clocks the engine off the monitor device.
//!
//! Every device runs 16-bit stereo at 48 kHz with Windows converting
//! (`AUTOCONVERTPCM`) — the format the process-loopback proof of concept was
//! verified with; the engine works in `f32` in between.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context};
use crossbeam_channel::{Receiver, RecvTimeoutError};
use windows::core::{implement, Interface, IUnknown, HRESULT, PCWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, ActivateAudioInterfaceAsync, EDataFlow,
    IActivateAudioInterfaceAsyncOperation, IActivateAudioInterfaceCompletionHandler,
    IActivateAudioInterfaceCompletionHandler_Impl, IAudioCaptureClient, IAudioClient,
    IAudioRenderClient, IAudioSessionControl2, IAudioSessionManager2, IMMDevice,
    IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
    AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    DEVICE_STATE_ACTIVE, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
    VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, WAVEFORMATEX,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{CreateEventW, GetCurrentProcessId, WaitForSingleObject};

use crate::model::{MixTarget, MixerConfig, MixerDevices};

use super::dsp::{Fade, BLOCK_FRAMES, BLOCK_SAMPLES, RATE};
use super::engine::Engine;
use super::ring::Ring;
use super::route::Router;
use super::sfx::{self, Library, Player, Sounds};
use super::Cmd;

const CHANNELS: u16 = 2;
const BYTES_PER_FRAME: usize = 4;
/// Device buffer: 100 ms in REFERENCE_TIME (100 ns) units.
const BUFFER_100MS: i64 = 1_000_000;
const SCAN_EVERY: Duration = Duration::from_secs(1);
/// Audio the engine keeps queued on the monitor: 30 ms.
const MONITOR_QUEUE: u32 = (BLOCK_FRAMES * 3) as u32;
/// Above this the Slidr Mic skips a block instead of growing latency: 60 ms.
const MIC_QUEUE_MAX: u32 = (BLOCK_FRAMES * 6) as u32;

fn pcm_format() -> WAVEFORMATEX {
    WAVEFORMATEX {
        wFormatTag: 1, // WAVE_FORMAT_PCM
        nChannels: CHANNELS,
        nSamplesPerSec: RATE,
        nAvgBytesPerSec: RATE * BYTES_PER_FRAME as u32,
        nBlockAlign: BYTES_PER_FRAME as u16,
        wBitsPerSample: 16,
        cbSize: 0,
    }
}

// ── Devices ──────────────────────────────────────────────────────────────────

fn friendly_name(dev: &IMMDevice) -> String {
    unsafe {
        dev.OpenPropertyStore(STGM_READ)
            .and_then(|s| s.GetValue(&PKEY_Device_FriendlyName))
            .map(|v| v.to_string())
            .unwrap_or_default()
    }
}

fn device_id(dev: &IMMDevice) -> Option<String> {
    unsafe {
        let id = dev.GetId().ok()?;
        let s = id.to_string().ok();
        CoTaskMemFree(Some(id.0 as _));
        s
    }
}

/// Endpoint by friendly name (exact, then substring, case-insensitive), or
/// the default for `flow` when `name` is `None`.
fn find_device(en: &IMMDeviceEnumerator, flow: EDataFlow, name: Option<&str>) -> Option<IMMDevice> {
    unsafe {
        let Some(name) = name else {
            return en.GetDefaultAudioEndpoint(flow, eConsole).ok();
        };
        let needle = name.to_lowercase();
        let coll = en.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE).ok()?;
        let devs: Vec<IMMDevice> = (0..coll.GetCount().ok()?).filter_map(|i| coll.Item(i).ok()).collect();
        let named: Vec<(IMMDevice, String)> =
            devs.into_iter().map(|d| { let n = friendly_name(&d).to_lowercase(); (d, n) }).collect();
        named
            .iter()
            .find(|(_, n)| *n == needle)
            .or_else(|| named.iter().find(|(_, n)| n.contains(&needle)))
            .map(|(d, _)| d.clone())
    }
}

/// pid → image name of every program with an audio session on any output.
fn session_programs(en: &IMMDeviceEnumerator) -> HashMap<u32, String> {
    let own = unsafe { GetCurrentProcessId() };
    let mut out = HashMap::new();
    unsafe {
        let Ok(coll) = en.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) else { return out };
        for i in 0..coll.GetCount().unwrap_or(0) {
            let Ok(dev) = coll.Item(i) else { continue };
            let Ok(mgr) = dev.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else { continue };
            let Ok(list) = mgr.GetSessionEnumerator() else { continue };
            for j in 0..list.GetCount().unwrap_or(0) {
                let Ok(ctl) = list.GetSession(j) else { continue };
                let Ok(ctl2) = ctl.cast::<IAudioSessionControl2>() else { continue };
                let pid = ctl2.GetProcessId().unwrap_or(0);
                if pid == 0 || pid == own || out.contains_key(&pid) {
                    continue;
                }
                if let Some(name) = crate::audio::wasapi::process_name(pid) {
                    out.insert(pid, name);
                }
            }
        }
    }
    out
}

/// pid → parent pid of every running process.
fn parent_pids() -> HashMap<u32, u32> {
    let mut out = HashMap::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                out.insert(e.th32ProcessID, e.th32ParentProcessID);
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    out
}

/// Whether one of `pid`'s ancestors is in `set`. Process loopback captures a
/// whole process tree, so capturing both would play the child twice.
fn has_ancestor_in(pid: u32, set: &HashSet<u32>, parents: &HashMap<u32, u32>) -> bool {
    let mut cur = pid;
    for _ in 0..32 {
        match parents.get(&cur) {
            Some(&p) if p != 0 && p != cur => {
                if set.contains(&p) {
                    return true;
                }
                cur = p;
            }
            _ => return false,
        }
    }
    false
}

// ── Render ───────────────────────────────────────────────────────────────────

struct Output {
    client: IAudioClient,
    rc: IAudioRenderClient,
    frames: u32,
    event: HANDLE,
    name: String,
}

impl Output {
    fn open(dev: &IMMDevice) -> anyhow::Result<Self> {
        let name = friendly_name(dev);
        unsafe {
            let client: IAudioClient = dev.Activate(CLSCTX_ALL, None)?;
            client
                .Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                        | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY
                        | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                    BUFFER_100MS,
                    0,
                    &pcm_format(),
                    None,
                )
                .with_context(|| format!("open output {name:?}"))?;
            let event = CreateEventW(None, false, false, None)?;
            client.SetEventHandle(event)?;
            let frames = client.GetBufferSize()?;
            let rc: IAudioRenderClient = client.GetService()?;
            client.Start()?;
            Ok(Self { client, rc, frames, event, name })
        }
    }

    fn queued(&self) -> u32 {
        unsafe { self.client.GetCurrentPadding().unwrap_or(self.frames) }
    }

    /// Queue one block if it fits; a full device buffer drops it.
    fn write(&self, block: &[f32]) -> windows::core::Result<()> {
        let n = (block.len() / 2) as u32;
        unsafe {
            if self.frames - self.client.GetCurrentPadding()? < n {
                return Ok(());
            }
            let buf = self.rc.GetBuffer(n)? as *mut i16;
            let out = std::slice::from_raw_parts_mut(buf, block.len());
            for (o, s) in out.iter_mut().zip(block) {
                *o = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            }
            self.rc.ReleaseBuffer(n, 0)
        }
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
            let _ = CloseHandle(self.event);
        }
    }
}

// ── Capture ──────────────────────────────────────────────────────────────────

enum Source {
    /// A program and its child processes.
    Process(u32),
    /// A capture endpoint, by endpoint id.
    Device(String),
}

/// A capture running on its own thread, feeding `ring`. Dropping stops it.
struct Capture {
    ring: Arc<Ring>,
    stop: Arc<AtomicBool>,
}

impl Capture {
    fn spawn(source: Source) -> Self {
        let ring = Arc::new(Ring::default());
        let stop = Arc::new(AtomicBool::new(false));
        let (r, s) = (ring.clone(), stop.clone());
        let _ = std::thread::Builder::new()
            .name("mixer-capture".into())
            .spawn(move || {
                if let Err(e) = capture_loop(&source, &r, &s) {
                    match source {
                        Source::Process(pid) => log::warn!("mixer: capture of pid {pid} stopped: {e:#}"),
                        Source::Device(_) => log::warn!("mixer: microphone capture stopped: {e:#}"),
                    }
                }
            });
        Self { ring, stop }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        // Not joined: the thread notices within its 100 ms wait and exits on
        // its own, and the audio thread must not sit here meanwhile.
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn capture_loop(source: &Source, ring: &Ring, stop: &AtomicBool) -> anyhow::Result<()> {
    const SILENT: u32 = 0x2; // AUDCLNT_BUFFERFLAGS_SILENT
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let (client, flags) = match source {
            Source::Process(pid) => (
                activate_process_loopback(*pid)?,
                AUDCLNT_STREAMFLAGS_LOOPBACK
                    | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                    | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
            ),
            Source::Device(id) => {
                let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
                let wide: Vec<u16> = id.encode_utf16().chain(Some(0)).collect();
                let dev = en.GetDevice(PCWSTR(wide.as_ptr()))?;
                (
                    dev.Activate::<IAudioClient>(CLSCTX_ALL, None)?,
                    AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                        | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                        | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                )
            }
        };
        client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, BUFFER_100MS, 0, &pcm_format(), None)?;
        let event = CreateEventW(None, false, false, None)?;
        client.SetEventHandle(event)?;
        let cap: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        let mut samples: Vec<f32> = Vec::new();
        let result = (|| -> anyhow::Result<()> {
            while !stop.load(Ordering::Relaxed) {
                if WaitForSingleObject(event, 100) != WAIT_OBJECT_0 {
                    continue;
                }
                loop {
                    let mut data = std::ptr::null_mut();
                    let (mut n, mut flags) = (0u32, 0u32);
                    cap.GetBuffer(&mut data, &mut n, &mut flags, None, None)?;
                    if n == 0 {
                        cap.ReleaseBuffer(0)?;
                        break;
                    }
                    let count = n as usize * CHANNELS as usize;
                    samples.clear();
                    if flags & SILENT != 0 {
                        samples.resize(count, 0.0);
                    } else {
                        let pcm = std::slice::from_raw_parts(data as *const i16, count);
                        samples.extend(pcm.iter().map(|&s| s as f32 / 32768.0));
                    }
                    cap.ReleaseBuffer(n)?;
                    ring.push(&samples);
                }
            }
            Ok(())
        })();
        let _ = client.Stop();
        let _ = CloseHandle(event);
        result
    }
}

#[implement(IActivateAudioInterfaceCompletionHandler)]
struct Activated(mpsc::Sender<()>);

impl IActivateAudioInterfaceCompletionHandler_Impl for Activated_Impl {
    fn ActivateCompleted(&self, _op: Option<&IActivateAudioInterfaceAsyncOperation>) -> windows::core::Result<()> {
        let _ = self.0.send(());
        Ok(())
    }
}

/// Raw PROPVARIANT holding a VT_BLOB (x64 layout: 8-byte header + 16-byte union).
#[repr(C)]
struct BlobVariant {
    vt: u16,
    _reserved: [u16; 3],
    size: u32,
    data: *const u8,
    _pad: u64,
}
const VT_BLOB: u16 = 65;

/// Audio client capturing `pid` and its children (Windows 10 2004+).
fn activate_process_loopback(pid: u32) -> anyhow::Result<IAudioClient> {
    let params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };
    let var = BlobVariant {
        vt: VT_BLOB,
        _reserved: [0; 3],
        size: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
        data: &params as *const _ as *const u8,
        _pad: 0,
    };
    let (tx, rx) = mpsc::channel();
    let handler: IActivateAudioInterfaceCompletionHandler = Activated(tx).into();
    unsafe {
        let op = ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&var as *const _ as *const windows::core::PROPVARIANT),
            &handler,
        )?;
        rx.recv_timeout(Duration::from_secs(5)).map_err(|_| anyhow!("activation timed out"))?;
        let mut hr = HRESULT(0);
        let mut unk: Option<IUnknown> = None;
        op.GetActivateResult(&mut hr, &mut unk)?;
        hr.ok()?;
        Ok(unk.ok_or_else(|| anyhow!("no interface"))?.cast()?)
    }
}

// ── Threads ──────────────────────────────────────────────────────────────────
//
// Two threads, so nothing slow ever sits between the monitor's buffer and the
// next block: the audio thread only mixes and writes, the control thread does
// everything that may take a while (enumerating sessions and processes —
// ~100 ms — and routing) and tells the audio thread what to capture.

/// Control thread → audio thread.
enum AudioCmd {
    Mixer(Cmd),
    /// Programs to capture: pid → channel.
    Capture(HashMap<u32, u32>),
    /// Channels whose programs are rerouted (and so belong on the monitor).
    Monitored(Vec<(u32, bool)>),
    /// Microphones to capture: (channel, device name; "" = system default).
    Mics(HashSet<(u32, String)>),
    /// The system's default output changed: the monitor follows it.
    DefaultOutputChanged,
    /// The feedback sounds, after the folder changed.
    Sounds(Sounds),
    Stop(crossbeam_channel::Sender<()>),
}

struct Audio {
    en: IMMDeviceEnumerator,
    engine: Engine,
    cfg: MixerConfig,
    devices: MixerDevices,
    /// Levels and mutes set from the board since the last layout; re-applied
    /// over a new layout so a profile edit doesn't snap faders back.
    live_levels: HashMap<MixTarget, f32>,
    live_mutes: HashMap<MixTarget, bool>,
    monitor: Option<Output>,
    mic_out: Option<Output>,
    /// pid → (channel, capture).
    captures: HashMap<u32, (u32, Capture)>,
    /// (channel, microphone) → capture; `None` while the microphone isn't
    /// there (tried again when the set of microphones changes).
    mics: HashMap<(u32, String), Option<Capture>>,
    /// Whether devices are open and mixing — the control thread only starts
    /// captures and reroutes while this is set.
    running_flag: Arc<AtomicBool>,
    sounds: Sounds,
    sfx_on: bool,
    /// The set the sounds come from, once the settings named it.
    sound_set: Option<String>,
    /// The next layout is the one Slidr starts with: power on without a sound.
    quiet_start: bool,
    player: Player,
    /// Master gain of both buses: fades in on power-on, out on power-off.
    bus: Fade,
    /// Switched off and fading out; devices close at this moment.
    closing: Option<Instant>,
    stats: Stats,
    tmp: Vec<f32>,
    voice_buf: Vec<f32>,
    monitor_buf: Vec<f32>,
    mic_buf: Vec<f32>,
}

impl Audio {
    fn new(en: IMMDeviceEnumerator, running_flag: Arc<AtomicBool>) -> Self {
        Self {
            en,
            engine: Engine::default(),
            cfg: MixerConfig::default(),
            devices: MixerDevices::default(),
            live_levels: HashMap::new(),
            live_mutes: HashMap::new(),
            monitor: None,
            mic_out: None,
            captures: HashMap::new(),
            mics: HashMap::new(),
            running_flag,
            sounds: Sounds::new(),
            sfx_on: true,
            sound_set: None,
            quiet_start: false,
            player: Player::default(),
            bus: Fade::new(1.0),
            closing: None,
            stats: Stats::new(),
            tmp: vec![0.0; BLOCK_SAMPLES],
            voice_buf: vec![0.0; BLOCK_SAMPLES],
            monitor_buf: vec![0.0; BLOCK_SAMPLES],
            mic_buf: vec![0.0; BLOCK_SAMPLES],
        }
    }

    fn running(&self) -> bool {
        (self.cfg.enabled || self.closing.is_some()) && (self.monitor.is_some() || self.mic_out.is_some())
    }

    /// Start the named feedback sound (if sounds are on); its length in
    /// frames either way, since the switch fades that long (0 if missing).
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
            Cmd::Configure(cfg, devices) => self.configure(cfg, devices),
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
                // Picking another set lets you hear it (not on the first
                // prefs after start, which only restore the saved one).
                let changed = self.sound_set.as_ref().is_some_and(|s| *s != set);
                self.sound_set = Some(set);
                if changed && self.running() && self.closing.is_none() {
                    self.play("mute_on");
                }
            }
            Cmd::Shutdown(_) => unreachable!("the control thread handles shutdown"),
        }
    }

    fn configure(&mut self, cfg: MixerConfig, devices: MixerDevices) {
        let was_running = self.running();
        let old = std::mem::take(&mut self.cfg);
        // A board move stays until the page changes that same level (the
        // page then shows the truth); editing anything else keeps it.
        self.live_levels.retain(|t, _| level_of(&old, *t) == level_of(&cfg, *t));
        self.live_mutes.retain(|t, _| muted_of(&old, *t) == muted_of(&cfg, *t));

        // What got switched: its sound plays, and it fades as long.
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

        let reopen = devices != self.devices;
        let (was_on, now_on) = (old.enabled, cfg.enabled);
        self.cfg = cfg;
        self.devices = devices;
        let starting = std::mem::take(&mut self.quiet_start);
        if now_on && !was_on {
            // Power-on: the mix rises with its sound — but not when Slidr
            // itself starts with the mixer on; nothing was switched then.
            self.closing = None;
            self.close_devices();
            self.open_devices();
            self.bus = Fade::new(0.0);
            let len = if starting { 0 } else { self.play("power_on") };
            self.bus.set_over(1.0, len.max(BLOCK_FRAMES as u32));
        } else if reopen && now_on {
            // Another device: reopen quietly. Program captures don't depend
            // on the devices and keep running (the control thread only sends
            // changes, so dropping them here would leave the mix silent).
            self.close_outputs();
            self.open_devices();
        } else if was_on && !now_on && was_running {
            // Power-off: keep playing while the mix sinks with its sound.
            let len = self.play("power_off").max(BLOCK_FRAMES as u32);
            self.bus.set_over(0.0, len);
            self.closing = Some(Instant::now() + Duration::from_secs_f32(len as f32 / RATE as f32));
        } else if now_on {
            self.closing = None;
            self.bus.set_over(1.0, BLOCK_FRAMES as u32);
        }
        self.running_flag.store(self.running(), Ordering::Relaxed);
    }

    /// Finish a power-off once its fade is over.
    fn finish_closing(&mut self) {
        if self.closing.is_some_and(|t| Instant::now() >= t) {
            self.closing = None;
            self.close_devices();
            self.bus = Fade::new(1.0);
            self.running_flag.store(self.running(), Ordering::Relaxed);
        }
    }

    fn open_devices(&mut self) {
        let d = self.devices.clone();
        // The mix plays on the system's default output and follows it (so
        // switching outputs in Windows or from the board just works). Tests
        // name a silent one instead.
        let monitor = if cfg!(test) { d.monitor.as_deref() } else { None };
        self.monitor = match find_device(&self.en, eRender, monitor).as_ref().map(Output::open) {
            Some(Ok(o)) => Some(o),
            Some(Err(e)) => {
                log::warn!("mixer: {e:#}");
                None
            }
            None => {
                log::warn!("mixer: no output device to play on");
                None
            }
        };
        self.mic_out = d.mic_out.as_deref().and_then(|n| match find_device(&self.en, eRender, Some(n)) {
            Some(dev) => Output::open(&dev).map_err(|e| log::warn!("mixer: {e:#}")).ok(),
            None => {
                log::warn!("mixer: Slidr Mic output {n:?} not found");
                None
            }
        });
        log::info!(
            "mixer: monitor {:?}, Slidr Mic {:?}",
            self.monitor.as_ref().map(|o| &o.name),
            self.mic_out.as_ref().map(|o| &o.name),
        );
    }

    fn close_devices(&mut self) {
        self.captures.clear();
        self.mics.clear();
        self.close_outputs();
    }

    /// Close the outputs, keeping program and microphone captures.
    fn close_outputs(&mut self) {
        self.monitor = None;
        self.mic_out = None;
    }

    /// Start captures for microphones added to channels, drop removed ones.
    fn set_mics(&mut self, wanted: HashSet<(u32, String)>) {
        self.mics.retain(|k, cap| wanted.contains(k) && cap.is_some());
        let cable = self.devices.mic_out.as_deref().and_then(super::adapter).map(str::to_string);
        for key in wanted {
            if self.mics.contains_key(&key) {
                continue;
            }
            let name = (!key.1.is_empty()).then_some(key.1.as_str());
            let dev = find_device(&self.en, eCapture, name);
            // Recording the Slidr Mic's own cable (say, as the default
            // microphone) would feed the mix back into itself.
            if dev.as_ref().map(friendly_name).as_deref().and_then(super::adapter).is_some_and(|a| Some(a) == cable.as_deref()) {
                log::error!("mixer: microphone {:?} is the Slidr Mic itself; not captured", key.1);
                self.mics.insert(key, None);
                continue;
            }
            let cap = dev.as_ref().and_then(device_id).map(|id| Capture::spawn(Source::Device(id)));
            if cap.is_none() {
                log::warn!("mixer: microphone {:?} not found", key.1);
            }
            self.mics.insert(key, cap);
        }
    }

    /// Start captures for new programs, drop those that left. Spawning only
    /// starts a thread, so this is cheap enough for the audio thread.
    fn set_captures(&mut self, wanted: HashMap<u32, u32>) {
        self.captures.retain(|pid, (ch, _)| wanted.get(pid) == Some(ch));
        for (pid, ch) in wanted {
            self.captures.entry(pid).or_insert_with(|| (ch, Capture::spawn(Source::Process(pid))));
        }
    }

    /// Wait for the monitor to want audio, then mix until its queue is topped up.
    fn pump(&mut self) {
        let clock = self.monitor.as_ref().or(self.mic_out.as_ref()).map(|o| o.event);
        match clock {
            Some(ev) => unsafe {
                WaitForSingleObject(ev, 20);
            },
            None => std::thread::sleep(Duration::from_millis(10)),
        }
        for _ in 0..8 {
            if self.monitor.as_ref().is_some_and(|o| o.queued() == 0) {
                self.stats.monitor_dry += 1;
            }
            let wants = self.monitor.as_ref().or(self.mic_out.as_ref()).is_some_and(|o| o.queued() < MONITOR_QUEUE);
            if !wants {
                break;
            }
            self.mix_block();
        }
    }

    fn mix_block(&mut self) {
        let mut channels: HashMap<u32, Vec<f32>> = HashMap::new();
        for (ch, cap) in self.captures.values() {
            if !cap.ring.pop(&mut self.tmp) {
                self.stats.underruns += 1;
            }
            let sum = channels.entry(*ch).or_insert_with(|| vec![0.0; BLOCK_SAMPLES]);
            for (s, t) in sum.iter_mut().zip(&self.tmp) {
                *s += t;
            }
        }
        for ((ch, _), cap) in &self.mics {
            let Some(cap) = cap else { continue };
            // A microphone starting up has nothing yet: silence, not an underrun.
            cap.ring.pop(&mut self.voice_buf);
            let sum = channels.entry(*ch).or_insert_with(|| vec![0.0; BLOCK_SAMPLES]);
            for (s, t) in sum.iter_mut().zip(&self.voice_buf) {
                *s += t;
            }
        }
        self.engine.process(&mut channels, None, &mut self.monitor_buf, &mut self.mic_buf);
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
        if let Some(m) = &self.monitor {
            if let Err(e) = m.write(&self.monitor_buf) {
                log::warn!("mixer: monitor write failed ({e}); reopening devices");
                self.reopen();
                return;
            }
        }
        if let Some(o) = &self.mic_out {
            if o.queued() < MIC_QUEUE_MAX {
                if let Err(e) = o.write(&self.mic_buf) {
                    log::warn!("mixer: Slidr Mic write failed ({e}); reopening devices");
                    self.reopen();
                }
            }
        }
    }

    /// A device went away (unplugged, format change): start over.
    fn reopen(&mut self) {
        self.close_outputs();
        self.open_devices();
        self.running_flag.store(self.running(), Ordering::Relaxed);
    }
}

fn level_of(cfg: &MixerConfig, t: MixTarget) -> Option<f32> {
    cfg.level(t)
}

fn muted_of(cfg: &MixerConfig, t: MixTarget) -> Option<bool> {
    match t {
        MixTarget::Voice => Some(cfg.voice.muted),
        MixTarget::Channel(id) | MixTarget::Send(id) => cfg.channels.iter().find(|c| c.id == id).map(|c| c.muted),
    }
}

fn audio_thread(rx: Receiver<AudioCmd>, running_flag: Arc<AtomicBool>) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let en: IMMDeviceEnumerator = match unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) } {
        Ok(en) => en,
        Err(e) => {
            log::error!("mixer: no audio device enumerator ({e}); mixer disabled");
            return;
        }
    };
    let mut audio = Audio::new(en, running_flag);
    loop {
        // While mixing, commands must not stall the audio; otherwise block.
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
                // While powering off, keep what plays until the fade is over.
                AudioCmd::Capture(_) | AudioCmd::Monitored(_) | AudioCmd::Mics(_) if audio.closing.is_some() => {}
                AudioCmd::Capture(wanted) => audio.set_captures(wanted),
                AudioCmd::Mics(wanted) => audio.set_mics(wanted),
                AudioCmd::DefaultOutputChanged => {
                    if audio.running() && audio.closing.is_none() {
                        log::info!("mixer: default output changed; following it");
                        audio.reopen();
                    }
                }
                AudioCmd::Monitored(list) => {
                    for (ch, on) in list {
                        audio.engine.set_monitored(ch, on);
                    }
                }
                AudioCmd::Sounds(s) => audio.sounds = s,
                AudioCmd::Stop(ack) => {
                    audio.close_devices();
                    audio.running_flag.store(false, Ordering::Relaxed);
                    let _ = ack.send(());
                    return;
                }
            }
        }
        if audio.running() {
            audio.pump();
            audio.stats.report();
        }
        audio.finish_closing();
    }
    audio.close_devices();
}

/// Control side: matches programs to channels, routes them, and feeds the
/// audio thread.
struct Control {
    en: IMMDeviceEnumerator,
    audio: crossbeam_channel::Sender<AudioCmd>,
    audio_running: Arc<AtomicBool>,
    router: Option<Router>,
    cfg: MixerConfig,
    devices: MixerDevices,
    sink: Option<IMMDevice>,
    /// What the audio thread was last told to capture (to log changes only).
    capturing: HashMap<u32, u32>,
    /// Microphones the audio thread was last told to capture.
    mics: HashSet<(u32, String)>,
    /// The system's default output, as last seen (the monitor follows it).
    default_output: Option<String>,
    library: Library,
    /// Switched off: rerouted programs go back when the power-off sound ends.
    restore_at: Option<Instant>,
}

impl Control {
    fn configure(&mut self, cfg: MixerConfig, devices: MixerDevices) {
        if devices != self.devices {
            self.sink = devices.sink.as_deref().and_then(|n| {
                let dev = find_device(&self.en, eRender, Some(n));
                if dev.is_none() {
                    log::warn!("mixer: silent output {n:?} not found; programs are not rerouted");
                }
                dev
            });
            // Rerouting onto the device the mix plays on would feed it back into itself.
            let monitor = find_device(&self.en, eRender, None).as_ref().and_then(device_id);
            if self.sink.as_ref().and_then(device_id).is_some_and(|s| Some(s) == monitor) {
                log::error!("mixer: silent output and headphones are the same device; programs are not rerouted");
                self.sink = None;
            }
            // Nor onto the Slidr Mic's cable: the programs would end up in Discord.
            let sink_adapter = self.sink.as_ref().map(friendly_name);
            if sink_adapter.as_deref().and_then(super::adapter).is_some_and(|a| Some(a) == devices.mic_out.as_deref().and_then(super::adapter)) {
                log::error!("mixer: silent output is the Slidr Mic's cable; programs are not rerouted");
                self.sink = None;
            }
            log::info!("mixer: silent output {:?}", self.sink.as_ref().map(friendly_name));
        }
        self.refresh_sounds();
        if self.cfg.enabled && !cfg.enabled {
            let fade = self.library.sounds().get("power_off").map_or(0, |s| s.len());
            self.restore_at = Some(Instant::now() + Duration::from_secs_f32(fade as f32 / RATE as f32));
        } else if cfg.enabled {
            self.restore_at = None;
        }
        self.cfg = cfg.clone();
        self.devices = devices.clone();
        let _ = self.audio.send(AudioCmd::Mixer(Cmd::Configure(cfg, devices)));
    }

    /// Hand the audio thread the sounds again if a file changed.
    fn refresh_sounds(&mut self) {
        if self.library.refresh() {
            let _ = self.audio.send(AudioCmd::Sounds(self.library.sounds()));
        }
    }

    fn restore_if_due(&mut self) {
        if self.restore_at.is_some_and(|t| Instant::now() >= t) {
            self.restore_at = None;
            if let Some(r) = &mut self.router {
                r.clear_all();
            }
        }
    }

    /// Match running programs to channels; update captures and routes.
    fn scan(&mut self) {
        let sessions = session_programs(&self.en);
        let mut wanted: HashMap<u32, u32> = HashMap::new();
        if self.cfg.enabled && self.audio_running.load(Ordering::Relaxed) {
            for (pid, exe) in &sessions {
                let hit = self.cfg.channels.iter().find(|c| {
                    c.programs.iter().any(|p| crate::audio::wasapi::program_matches(p, *pid, exe))
                });
                if let Some(c) = hit {
                    wanted.insert(*pid, c.id);
                }
            }
            let parents = parent_pids();
            let set: HashSet<u32> = wanted.keys().copied().collect();
            wanted.retain(|pid, _| !has_ancestor_in(*pid, &set, &parents));
        }
        if wanted != self.capturing {
            for (pid, ch) in &wanted {
                if self.capturing.get(pid) != Some(ch) {
                    log::info!("mixer: capturing {} (pid {pid}) into channel {ch}", sessions[pid]);
                }
            }
            self.capturing = wanted.clone();
            let _ = self.audio.send(AudioCmd::Capture(wanted.clone()));
        }

        let reroute: HashSet<u32> = wanted
            .iter()
            .filter(|(_, ch)| self.cfg.channels.iter().any(|c| c.id == **ch && c.reroute))
            .map(|(pid, _)| *pid)
            .collect();
        let can_route = self.router.is_some() && self.sink.is_some();
        if let (Some(router), Some(sink)) = (&mut self.router, &self.sink) {
            for pid in &reroute {
                if !router.is_routed(*pid) {
                    if let Err(e) = router.route(*pid, sink) {
                        log::debug!("mixer: rerouting pid {pid} failed ({e}); retrying");
                    }
                }
            }
        }
        if let Some(router) = &mut self.router {
            if router.pending() {
                let paths: HashMap<u32, String> = sessions
                    .keys()
                    .map(|pid| (*pid, crate::app_groups::process_path(*pid).unwrap_or_default()))
                    .collect();
                router.recover(&paths, &|pid| can_route && reroute.contains(&pid));
            }
        }
        // Microphones play nowhere else: a channel of only microphones is
        // always heard at its fader (that's "hear yourself").
        let monitored =
            self.cfg.channels.iter().map(|c| (c.id, (can_route && c.reroute) || c.mics_only())).collect();
        let _ = self.audio.send(AudioCmd::Monitored(monitored));

        let mics: HashSet<(u32, String)> = if self.cfg.enabled && self.audio_running.load(Ordering::Relaxed) {
            self.cfg.channels.iter().flat_map(|c| c.mics().map(move |m| (c.id, m.to_string()))).collect()
        } else {
            HashSet::new()
        };
        if mics != self.mics {
            self.mics = mics.clone();
            let _ = self.audio.send(AudioCmd::Mics(mics));
        }

        let default_output = find_device(&self.en, eRender, None).as_ref().and_then(device_id);
        if default_output != self.default_output {
            if self.default_output.is_some() {
                let _ = self.audio.send(AudioCmd::DefaultOutputChanged);
            }
            self.default_output = default_output;
        }
    }

    fn shutdown(&mut self) {
        if let Some(r) = &mut self.router {
            r.clear_all();
        }
        let (tx, rx) = crossbeam_channel::bounded(1);
        if self.audio.send(AudioCmd::Stop(tx)).is_ok() {
            let _ = rx.recv_timeout(Duration::from_secs(2));
        }
    }
}

pub fn run(rx: Receiver<Cmd>) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let en: IMMDeviceEnumerator = match unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) } {
        Ok(en) => en,
        Err(e) => {
            log::error!("mixer: no audio device enumerator ({e}); mixer disabled");
            return;
        }
    };
    let (audio_tx, audio_rx) = crossbeam_channel::unbounded();
    let audio_running = Arc::new(AtomicBool::new(false));
    {
        let flag = audio_running.clone();
        if let Err(e) = std::thread::Builder::new().name("mixer-audio".into()).spawn(move || audio_thread(audio_rx, flag)) {
            log::error!("mixer: could not start the audio thread: {e}");
            return;
        }
    }
    let mut control = Control {
        en,
        audio: audio_tx,
        audio_running,
        router: Router::new(),
        cfg: MixerConfig::default(),
        devices: MixerDevices::default(),
        sink: None,
        capturing: HashMap::new(),
        mics: HashSet::new(),
        default_output: None,
        library: Library::default(),
        restore_at: None,
    };
    if let Some(dir) = sfx::sound_dir() {
        log::info!("mixer: feedback sounds from {}", dir.display());
    }
    control.refresh_sounds();
    let mut next_scan = Instant::now();
    loop {
        let due = control.restore_at.map_or(next_scan, |t| t.min(next_scan));
        let wait = due.saturating_duration_since(Instant::now());
        match rx.recv_timeout(wait) {
            Ok(Cmd::Shutdown(ack)) => {
                control.shutdown();
                let _ = ack.send(());
                return;
            }
            Ok(Cmd::Configure(cfg, devices)) => {
                control.configure(cfg, devices);
                // Let the audio thread open its devices before matching programs.
                next_scan = Instant::now() + Duration::from_millis(200);
            }
            Ok(Cmd::SoundPrefs(on, volume, set)) => {
                // The new set's sounds reach the audio thread first, so its
                // preview plays from that set.
                let switched = control.library.select(&set);
                if control.library.refresh() || switched {
                    let _ = control.audio.send(AudioCmd::Sounds(control.library.sounds()));
                }
                let _ = control.audio.send(AudioCmd::Mixer(Cmd::SoundPrefs(on, volume, set)));
            }
            Ok(cmd) => {
                let _ = control.audio.send(AudioCmd::Mixer(cmd));
            }
            Err(RecvTimeoutError::Timeout) if Instant::now() < next_scan => control.restore_if_due(),
            Err(RecvTimeoutError::Timeout) => {
                control.restore_if_due();
                control.refresh_sounds();
                let t = Instant::now();
                control.scan();
                if t.elapsed() > Duration::from_millis(250) {
                    log::debug!("mixer: scan took {} ms", t.elapsed().as_millis());
                }
                next_scan = Instant::now() + SCAN_EVERY;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    control.shutdown();
}

/// Glitch counters, logged every few seconds while something went wrong —
/// the way to tell a starving device from a starving capture.
struct Stats {
    since: Instant,
    /// Program capture had less than a block when the mix needed one.
    underruns: u32,
    /// The monitor had played everything queued before we refilled it.
    monitor_dry: u32,
}

impl Stats {
    fn new() -> Self {
        Self { since: Instant::now(), underruns: 0, monitor_dry: 0 }
    }

    fn report(&mut self) {
        if self.since.elapsed() < Duration::from_secs(5) {
            return;
        }
        if self.underruns > 0 || self.monitor_dry > 0 {
            log::info!(
                "mixer: last 5 s: {} capture underruns, monitor ran dry {} times",
                self.underruns, self.monitor_dry
            );
        }
        *self = Self::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end on real devices, run by hand:
    ///
    ///     SLIDR_TEST_MONITOR="Voicemeeter In 2" SLIDR_TEST_MIC_OUT="Voicemeeter In 1" \
    ///         cargo test --bin slidr windows_io -- --ignored --nocapture
    ///
    /// Both outputs should be ones nobody listens to. `SLIDR_TEST_MIC` picks
    /// the microphone (default: the system default). Measures what this
    /// process plays (its own process loopback) at two voice fader positions.
    #[test]
    #[ignore]
    fn microphone_channel_reaches_the_slidr_mic_and_follows_its_fader() {
        let env = |k: &str| std::env::var(k).ok();
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let en: IMMDeviceEnumerator = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).unwrap() };
        let mut io = Audio::new(en, Default::default());
        let devices = MixerDevices {
            monitor: env("SLIDR_TEST_MONITOR"),
            mic_input: env("SLIDR_TEST_MIC"),
            sink: None,
            mic_out: env("SLIDR_TEST_MIC_OUT"),
        };
        assert!(devices.monitor.is_some() && devices.mic_out.is_some(), "set SLIDR_TEST_MONITOR / SLIDR_TEST_MIC_OUT");
        let mic = env("SLIDR_TEST_MIC").unwrap_or_default();
        let channel = crate::model::MixChannel {
            id: 1,
            name: "Microphone".into(),
            programs: vec![format!("{}{mic}", crate::model::MIC_PREFIX)],
            level: 0.0,
            muted: false,
            fx: Default::default(),
            reroute: false,
            mic_send: Some(1.0),
            collapsed: false,
        };
        io.configure(MixerConfig { enabled: true, channels: vec![channel], ..Default::default() }, devices);
        io.set_mics([(1, mic)].into_iter().collect());
        assert!(io.monitor.is_some() && io.mic_out.is_some() && io.mics.values().all(Option::is_some));
        let tap = Capture::spawn(Source::Process(unsafe { GetCurrentProcessId() }));

        let energy = |b: &[f32]| b.iter().map(|s| (*s as f64).powi(2)).sum::<f64>();
        let db = |e: f64, n: usize| 10.0 * (e / n.max(1) as f64).log10();
        // (what this process played, mic input, Slidr Mic bus) over `secs`.
        // Input and bus are compared block by block, so the room changing
        // between two measurements doesn't matter.
        let measure = |io: &mut Audio, secs: u64| -> (f64, f64, f64) {
            let (mut tapped, mut n_tap, mut input, mut bus, mut n) = (0f64, 0usize, 0f64, 0f64, 0usize);
            let mut block = vec![0.0; BLOCK_SAMPLES];
            let end = Instant::now() + Duration::from_secs(secs);
            while Instant::now() < end {
                unsafe { WaitForSingleObject(io.monitor.as_ref().unwrap().event, 20) };
                while io.monitor.as_ref().unwrap().queued() < MONITOR_QUEUE {
                    io.mix_block();
                    input += energy(&io.voice_buf);
                    bus += energy(&io.mic_buf);
                    n += BLOCK_SAMPLES;
                }
                while tap.ring.pop(&mut block) {
                    tapped += energy(&block);
                    n_tap += block.len();
                }
            }
            (db(tapped, n_tap), db(input, n), db(bus, n))
        };
        measure(&mut io, 1);
        let (full, in_full, bus_full) = measure(&mut io, 2);
        io.handle(Cmd::SetLevel(MixTarget::Send(1), 0.25));
        measure(&mut io, 1);
        let (_, in_q, bus_q) = measure(&mut io, 2);
        io.handle(Cmd::SetMute(MixTarget::Channel(1), true));
        measure(&mut io, 1);
        let (muted, _, _) = measure(&mut io, 2);
        println!(
            "microphone {in_full:.1} dBFS, played {full:.1} dBFS; bus minus input at 100 %: {:.1} dB, at 25 %: {:.1} dB; muted played {muted:.1} dBFS",
            bus_full - in_full,
            bus_q - in_q
        );
        io.close_devices();

        assert!(full > -90.0, "nothing from the microphone was played");
        assert!((bus_full - in_full).abs() < 0.5, "voice at 100 % must pass unchanged");
        // Fader 0.25 → gain 0.0625 → −24 dB.
        assert!((bus_q - in_q + 24.1).abs() < 1.0, "fader did not scale the voice");
        assert!(muted < -90.0, "mute did not silence the voice");
    }
}
