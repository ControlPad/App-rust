//! Software mixer: programs grouped into channels, mixed with faders and EQ
//! onto the user's headset ("monitor"), plus a "Slidr Mic" carrying the real
//! microphone and whatever channels are sent to it. See `docs/mixer-plan.md`.
//!
//! Runs on its own thread, independent of the board; the board (via the
//! actuator) and the UI only send it commands. The layout comes from the
//! active profile, the devices from the global settings.

pub mod dsp;
mod engine;
mod ring;
#[cfg(target_os = "windows")]
mod route;
#[cfg(target_os = "windows")]
mod windows_io;

use std::time::Duration;

use crossbeam_channel::Sender;
use once_cell::sync::OnceCell;

use crate::model::{MixTarget, MixerConfig, MixerDevices};

/// Journal of programs rerouted by the mixer, in the config folder.
const JOURNAL_FILE: &str = "mixer-routing.json";

pub enum Cmd {
    Configure(MixerConfig, MixerDevices),
    SetLevel(MixTarget, f32),
    SetMute(MixTarget, bool),
    ToggleMute(MixTarget),
    /// Restore rerouted programs and stop; acknowledged on the sender.
    Shutdown(Sender<()>),
}

static TX: OnceCell<Option<Sender<Cmd>>> = OnceCell::new();

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

#[cfg(not(target_os = "windows"))]
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
    // Nothing to start for a profile that never used the mixer.
    if !mixer.enabled && TX.get().is_none() && !journal_pending() {
        return;
    }
    send(Cmd::Configure(mixer, devices));
}

pub fn set_level(target: MixTarget, value: f32) {
    send(Cmd::SetLevel(target, value));
}

pub fn set_mute(target: MixTarget, muted: bool) {
    send(Cmd::SetMute(target, muted));
}

pub fn toggle_mute(target: MixTarget) {
    send(Cmd::ToggleMute(target));
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
