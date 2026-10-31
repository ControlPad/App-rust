//! Software mixer: programs grouped into channels, mixed with faders and EQ
//! onto the user's headset ("monitor"), plus a "Slidr Mic" carrying the real
//! microphone and whatever channels are sent to it. See `docs/mixer-plan.md`.
//!
//! Runs on its own thread, independent of the board; the board (via the
//! actuator) and the UI only send it commands. The layout comes from the
//! active profile, the devices from the global settings.

pub mod dsp;
mod engine;
mod fx;
// Compiled everywhere so it is type-checked and tested on any host; only
// Linux runs it.
mod pulse_io;
mod sfx;
mod ring;
#[cfg(target_os = "windows")]
mod route;
#[cfg(target_os = "windows")]
mod windows_io;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossbeam_channel::Sender;
use once_cell::sync::{Lazy, OnceCell};
use parking_lot::Mutex;

use crate::model::{MixTarget, MixerConfig, MixerDevices};

pub use sfx::{DEFAULT_SET as DEFAULT_SOUND_SET, SETS as SOUND_SETS};

/// The audio adapter in an endpoint's friendly name — the part in the last
/// parentheses: "CABLE Output (VB-Audio Virtual Cable)" → "VB-Audio Virtual Cable".
/// Both ends of a virtual cable share it.
pub fn adapter(name: &str) -> Option<&str> {
    let open = name.rfind('(')?;
    name[open + 1..].strip_suffix(')').map(str::trim)
}

/// Journal of programs rerouted by the mixer, in the config folder.
const JOURNAL_FILE: &str = "mixer-routing.json";

pub enum Cmd {
    Configure(MixerConfig, MixerDevices),
    /// The next `Configure` is Slidr starting with the mixer on: no power-on sound.
    QuietStart,
    SetLevel(MixTarget, f32),
    SetMute(MixTarget, bool),
    ToggleMute(MixTarget),
    /// Switch sounds on/off, their volume (0..1) and the set they come from.
    SoundPrefs(bool, f32, String),
    /// Restore rerouted programs and stop; acknowledged on the sender.
    Shutdown(Sender<()>),
}

static TX: OnceCell<Option<Sender<Cmd>>> = OnceCell::new();

/// A layout was handed over since launch.
static LAUNCHED: AtomicBool = AtomicBool::new(false);

/// Levels the board set, and the layout they apply over — for the Mixer page,
/// which shows them. Kept by the same rule as the mixer's own: a board move
/// stays until the page changes that same level.
static BOARD: Lazy<Mutex<(HashMap<MixTarget, f32>, MixerConfig)>> = Lazy::new(Default::default);

/// Where a strip's fader really is: the board's last move, else the layout's.
pub fn shown_level(cfg: &MixerConfig, t: MixTarget) -> Option<f32> {
    BOARD.lock().0.get(&t).copied().or_else(|| cfg.level(t))
}

/// The mixer thread's inbox, started on first use. `None` where the mixer
/// isn't implemented yet (Linux, see the plan).
fn sender() -> Option<&'static Sender<Cmd>> {
    TX.get_or_init(spawn).as_ref()
}

#[cfg(target_os = "windows")]
fn spawn() -> Option<Sender<Cmd>> {
    let (tx, rx) = crossbeam_channel::unbounded();
    std::thread::Builder::new()
        .name("mixer".into())
        .spawn(move || windows_io::run(rx))
        .map_err(|e| log::error!("mixer: could not start thread: {e}"))
        .ok()?;
    Some(tx)
}

#[cfg(target_os = "linux")]
fn spawn() -> Option<Sender<Cmd>> {
    let (tx, rx) = crossbeam_channel::unbounded();
    std::thread::Builder::new()
        .name("mixer".into())
        .spawn(move || pulse_io::run(rx))
        .map_err(|e| log::error!("mixer: could not start thread: {e}"))
        .ok()?;
    Some(tx)
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
fn spawn() -> Option<Sender<Cmd>> {
    None
}

fn send(cmd: Cmd) {
    if let Some(tx) = sender() {
        let _ = tx.send(cmd);
    }
}

/// Adopt a profile's layout and the mixer devices.
pub fn configure(mixer: MixerConfig, devices: MixerDevices) {
    {
        let mut board = BOARD.lock();
        let (levels, last) = &mut *board;
        levels.retain(|t, _| last.level(*t) == mixer.level(*t));
        *last = mixer.clone();
    }
    // The first layout after launch: the mixer comes up with the app, which
    // isn't the user switching it on.
    let launching = !LAUNCHED.swap(true, Ordering::Relaxed);
    // Nothing to start for a profile that never used the mixer.
    if !mixer.enabled && TX.get().is_none() && !journal_pending() {
        return;
    }
    if launching && mixer.enabled {
        send(Cmd::QuietStart);
    }
    send(Cmd::Configure(mixer, devices));
}

pub fn set_level(target: MixTarget, value: f32) {
    BOARD.lock().0.insert(target, value);
    send(Cmd::SetLevel(target, value));
}

pub fn set_mute(target: MixTarget, muted: bool) {
    send(Cmd::SetMute(target, muted));
}

pub fn toggle_mute(target: MixTarget) {
    send(Cmd::ToggleMute(target));
}

/// Switch sounds on/off, their volume and set. Only reaches a mixer that is
/// already running (one that isn't has nothing to click).
pub fn sound_prefs(on: bool, volume: f32, set: &str) {
    if let Some(Some(tx)) = TX.get() {
        let _ = tx.send(Cmd::SoundPrefs(on, volume, set.to_string()));
    }
}

/// Hand rerouted programs back to their normal output before the app exits.
pub fn shutdown() {
    let Some(Some(tx)) = TX.get() else { return };
    let (ack_tx, ack_rx) = crossbeam_channel::bounded(1);
    if tx.send(Cmd::Shutdown(ack_tx)).is_ok() {
        let _ = ack_rx.recv_timeout(Duration::from_secs(3));
    }
}

/// Programs rerouted by an earlier run that still need restoring — the mixer
/// thread must run for that even when no profile uses the mixer.
fn journal_pending() -> bool {
    crate::storage::config_root().join(JOURNAL_FILE).exists()
}
