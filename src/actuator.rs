//! Actuator thread: owns the audio backend (for applying volume/mute) and the
//! key controller, and executes [`Cmd`]s off the UI thread. This keeps the
//! renderer smooth — per-frame WASAPI session enumeration / `pactl` calls never
//! block the event loop.

use std::collections::HashMap;
use std::time::{Duration, Instant};

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

    // Last volume applied per target. Re-played after an output-device switch
    // (see `OUTPUT_REAPPLY_DELAYS`).
    let mut applied: HashMap<String, (Target, f32)> = HashMap::new();
    let mut last_output = audio.default_output_id();
    let mut next_output_check = Instant::now() + OUTPUT_POLL;
    // Scheduled re-apply passes, earliest first.
    let mut reapply: Vec<Instant> = Vec::new();

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
                    apply(&*audio, keys.as_mut(), &mut led, &mut applied, cmd);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        led.tick(&*audio);

        let now = Instant::now();
        if now >= next_output_check {
            next_output_check = now + OUTPUT_POLL;
            let cur = audio.default_output_id();
            // `None` means the backend can't report an id — never treat that as
            // a switch, or we'd re-apply on every poll.
            if cur.is_some() && cur != last_output {
                log::info!("default output changed; re-applying volumes");
                last_output = cur;
                reapply = OUTPUT_REAPPLY_DELAYS.iter().map(|d| now + *d).collect();
            }
        }
        if reapply.first().is_some_and(|t| now >= *t) {
            reapply.remove(0);
            reapply_volumes(&*audio, &applied);
        }
    }
}

/// How often the default output endpoint is checked for a switch.
const OUTPUT_POLL: Duration = Duration::from_millis(400);

/// When to re-apply volumes after the default output changed, measured from the
/// moment we noticed. Sessions don't all migrate to the new endpoint at once —
/// an app can take a beat to reopen its stream — so we replay twice: once for
/// the streams that moved immediately, once for the stragglers.
const OUTPUT_REAPPLY_DELAYS: [Duration; 2] =
    [Duration::from_millis(350), Duration::from_millis(1500)];

/// Re-send the last volume we set for every target.
///
/// Why this is needed: per-app volumes live on the audio *session*, and a
/// session that follows the default device to a new endpoint arrives carrying
/// that endpoint's remembered level, not the one the slider is sitting at. The
/// physical slider and the actual volume then disagree until the next nudge —
/// which is the jump the user sees. Mute state is deliberately left alone: the
/// switch shouldn't un-mute anything the user muted on purpose.
fn reapply_volumes(audio: &dyn AudioBackend, applied: &HashMap<String, (Target, f32)>) {
    for (target, value) in applied.values() {
        audio.set_volume(vol_target(target), *value);
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

fn apply(
    audio: &dyn AudioBackend,
    keys: Option<&mut KeyController>,
    led: &mut LedEngine,
    applied: &mut HashMap<String, (Target, f32)>,
    cmd: Cmd,
) {
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
            applied.insert(target_key(&target), (target, value));
        }
        Cmd::ToggleMute(target) => audio.toggle_mute(mute_target(&target)),
        Cmd::Open(t) => {
            // A program group resolves to whichever member is running right now
            // ("Games" → the game you're in). Nothing running means there is
            // nothing to open: a group has no single executable of its own.
            let target = match crate::app_groups::parse_ref(&t) {
                Some(id) => match crate::app_groups::running_in_group(id)
                    .into_iter()
                    .find_map(|a| a.path)
                {
                    Some(path) => path,
                    None => {
                        log::info!(
                            "open: no running program in group {:?}; nothing to launch",
                            crate::app_groups::display_name(id)
                        );
                        return;
                    }
                },
                None => t,
            };
            if let Err(e) = open::that(&target) {
                log::warn!("open {target:?} failed: {e}");
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
