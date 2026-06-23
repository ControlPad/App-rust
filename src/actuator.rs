//! Actuator thread: owns the audio backend (for applying volume/mute) and the
//! key controller, and executes [`Cmd`]s off the UI thread. This keeps the
//! renderer smooth — per-frame WASAPI session enumeration / `pactl` calls never
//! block the event loop.

use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};

use crate::audio::{self, AudioBackend, MuteTarget, VolumeTarget};
use crate::events::{Cmd, Target};
use crate::keys::KeyController;
use crate::led::{LedEngine, LedStateHandle};
use crate::model::HttpMethod;
use crate::serial::SerialLink;

pub fn spawn(serial: SerialLink, led_state: LedStateHandle) -> Sender<Cmd> {
    let (tx, rx) = crossbeam_channel::unbounded::<Cmd>();
    std::thread::Builder::new()
        .name("slidr-actuator".into())
        .spawn(move || run(rx, serial, led_state))
        .expect("spawn actuator thread");
    tx
}

fn run(rx: Receiver<Cmd>, serial: SerialLink, led_state: LedStateHandle) {
    // On Windows, run this worker in an MTA apartment so WASAPI calls work
    // without a message pump. The backend's own CoInitializeEx(STA) will return
    // RPC_E_CHANGED_MODE and be ignored, leaving us in MTA.
    #[cfg(target_os = "windows")]
    unsafe {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }

    let audio: Box<dyn AudioBackend> = audio::default_backend();
    let mut keys = match KeyController::new() {
        Ok(k) => Some(k),
        Err(e) => {
            log::warn!("actuator: key controller unavailable ({e}); key actions disabled");
            None
        }
    };
    let mut led = LedEngine::new(serial, led_state);

    // Block for commands but wake regularly so the LED engine can re-evaluate
    // its conditions (mute/volume polling, cached API results) and push updates.
    let tick = Duration::from_millis(100);
    loop {
        match rx.recv_timeout(tick) {
            Ok(first) => {
                // Coalesce volume commands: if several frames queue up, only the
                // latest volume per target matters, so drain and keep the last.
                let mut batch = vec![first];
                while let Ok(more) = rx.try_recv() {
                    batch.push(more);
                }
                for cmd in coalesce(batch) {
                    apply(&*audio, keys.as_mut(), &mut led, cmd);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        led.tick(&*audio);
    }
}

/// Keep only the most recent SetVolume per target; preserve order of other cmds.
fn coalesce(batch: Vec<Cmd>) -> Vec<Cmd> {
    use std::collections::HashMap;
    // Walk backwards, emitting the first (latest) SetVolume seen per target key.
    let mut seen: HashMap<String, ()> = HashMap::new();
    let mut rev_out: Vec<Cmd> = Vec::with_capacity(batch.len());
    for cmd in batch.into_iter().rev() {
        match &cmd {
            Cmd::SetVolume { target, .. } => {
                let key = format!("v:{}", target_key(target));
                if seen.insert(key, ()).is_none() {
                    rev_out.push(cmd);
                }
            }
            _ => rev_out.push(cmd),
        }
    }
    rev_out.reverse();
    rev_out
}

fn target_key(t: &Target) -> String {
    match t {
        Target::Process(p) => format!("p:{p}"),
        Target::Mic(m) => format!("m:{m}"),
        Target::System(d) => format!("s:{}", d.as_deref().unwrap_or("")),
    }
}

fn apply(audio: &dyn AudioBackend, keys: Option<&mut KeyController>, led: &mut LedEngine, cmd: Cmd) {
    match cmd {
        Cmd::SetLeds(cfgs) => led.set_configs(*cfgs),
        Cmd::SetLedExperimental(on) => led.set_experimental(on),
        Cmd::LedManualToggle(i) => led.manual_toggle(i as usize),
        Cmd::SerialConnected => led.on_connected(),
        Cmd::LedSaveState => led.save_state(),
        Cmd::LedClearSaved => led.clear_and_save(),
        Cmd::SetVolume { target, value, unmute } => {
            if unmute {
                audio.set_mute(mute_target(&target), false);
            }
            audio.set_volume(vol_target(&target), value);
        }
        Cmd::ToggleMute(target) => audio.toggle_mute(mute_target(&target)),
        Cmd::Open(t) => {
            if let Err(e) = open::that(&t) {
                log::warn!("open {t:?} failed: {e}");
            }
        }
        Cmd::KeyDown(vk) => {
            if let Some(k) = keys {
                k.hold_start(vk);
            }
        }
        Cmd::KeyUp(vk) => {
            if let Some(k) = keys {
                k.hold_stop(vk);
            }
        }
        Cmd::CycleOutput(devices) => audio.cycle_output(&devices),
        Cmd::ApiCall { method, url, payload, bearer } => {
            spawn_api_call(method, url, payload, bearer);
        }
    }
}

/// Fire the HTTP request on a short-lived detached thread so a slow endpoint can
/// never stall the actuator loop (which also drives audio).
fn spawn_api_call(method: HttpMethod, url: String, payload: Option<String>, bearer: Option<String>) {
    let _ = std::thread::Builder::new()
        .name("slidr-api".into())
        .spawn(move || {
            let mut req = ureq::request(method.as_str(), &url);
            if let Some(token) = &bearer {
                req = req.set("Authorization", &format!("Bearer {token}"));
            }
            let result = match &payload {
                Some(body) if !body.is_empty() => {
                    req.set("Content-Type", "application/json").send_string(body)
                }
                _ => req.call(),
            };
            match result {
                Ok(resp) => log::info!("api {} {url} -> {}", method.as_str(), resp.status()),
                Err(e) => log::warn!("api {} {url} failed: {e}", method.as_str()),
            }
        });
}

fn vol_target(t: &Target) -> VolumeTarget<'_> {
    match t {
        Target::Process(p) => VolumeTarget::Process(p),
        Target::Mic(m) => VolumeTarget::Mic(m),
        Target::System(d) => VolumeTarget::System(d.as_deref()),
    }
}

fn mute_target(t: &Target) -> MuteTarget<'_> {
    match t {
        Target::Process(p) => MuteTarget::Process(p),
        Target::Mic(m) => MuteTarget::Mic(m),
        Target::System(d) => MuteTarget::System(d.as_deref()),
    }
}
