//! LED control engine (experimental).
//!
//! Evaluates each configured LED's conditions and pushes `L`/`B`/`H` commands to
//! the board over the [`SerialLink`] (see `LED-Steuerung.md`). Runs on the
//! actuator thread, ticked ~10×/s, so audio-state polling and HTTP checks never
//! block the UI. API conditions are polled on detached threads at their own
//! interval; the tick only reads the cached boolean.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::audio::{AudioBackend, MuteTarget, VolumeTarget};
use crate::model::{
    AudioKind, BrightnessMode, CombineLogic, Comparison, LedApi, LedCondition, LedConditionKind,
    LedConfig, LedControl, LedMode, LedModeKind, NUM_LEDS,
};
use crate::serial::SerialLink;

/// Live per-LED state exposed to the UI (for the configure popup readback).
#[derive(Clone, Copy, Default)]
pub struct LedLive {
    pub configured: bool,
    pub manual: bool,
    pub active: bool,
    /// Resolved appearance: 0 off, 1 on, 2 blink, 3 breathe.
    pub mode_kind: u8,
}

pub type LedStateHandle = Arc<Mutex<[LedLive; NUM_LEDS]>>;

pub fn new_state() -> LedStateHandle {
    Arc::new(Mutex::new([LedLive::default(); NUM_LEDS]))
}

/// Cached result of an async API poll for one (led, condition) pair.
struct ApiSlot {
    last_poll: Option<Instant>,
    value: Arc<Mutex<bool>>,
    inflight: Arc<AtomicBool>,
}

pub struct LedEngine {
    link: SerialLink,
    state: LedStateHandle,
    configs: [Option<LedConfig>; NUM_LEDS],
    experimental: bool,
    last_mode: [Option<LedMode>; NUM_LEDS],
    last_brightness: [Option<u8>; NUM_LEDS],
    api: HashMap<(usize, usize), ApiSlot>,
}

impl LedEngine {
    pub fn new(link: SerialLink, state: LedStateHandle) -> Self {
        Self {
            link,
            state,
            configs: Default::default(),
            experimental: false,
            last_mode: Default::default(),
            last_brightness: Default::default(),
            api: HashMap::new(),
        }
    }

    /// Persist the current live LED state to the board's EEPROM (`S`).
    pub fn save_state(&self) {
        if self.link.write_line("S") {
            log::info!("led: saved current state to board EEPROM");
        }
    }

    /// Turn all LEDs off and persist that — clears the board's standalone look.
    /// Live control resumes on the next tick if experimental is still enabled.
    pub fn clear_and_save(&mut self) {
        for led in 0..NUM_LEDS {
            self.link.write_line(&format!("L,{led},0"));
        }
        self.link.write_line("S");
        self.force_resend();
        log::info!("led: cleared saved state (all off) on board EEPROM");
    }

    pub fn set_configs(&mut self, mut configs: [Option<LedConfig>; NUM_LEDS]) {
        // Preserve runtime manual state across config pushes so editing a
        // condition doesn't reset a manually-toggled LED.
        for i in 0..NUM_LEDS {
            if let (Some(new), Some(old)) = (configs[i].as_mut(), self.configs[i].as_ref()) {
                if new.control == LedControl::Manual && old.control == LedControl::Manual {
                    new.manual_active = old.manual_active;
                }
            }
        }
        self.configs = configs;
        self.api.clear();
        self.force_resend();
    }

    pub fn set_experimental(&mut self, on: bool) {
        if self.experimental == on {
            return;
        }
        self.experimental = on;
        self.force_resend();
        if !on {
            // Turn everything off when the feature is disabled.
            for led in 0..NUM_LEDS {
                self.link.write_line(&format!("L,{led},0"));
            }
        }
    }

    pub fn manual_toggle(&mut self, led: usize) {
        if let Some(cfg) = self.configs.get_mut(led).and_then(|c| c.as_mut()) {
            if cfg.control == LedControl::Manual {
                cfg.manual_active = !cfg.manual_active;
            }
        }
    }

    /// True if the given LED is configured for manual control (used to persist
    /// the toggled state back to the preset).
    pub fn manual_state(&self, led: usize) -> Option<bool> {
        self.configs
            .get(led)
            .and_then(|c| c.as_ref())
            .filter(|c| c.control == LedControl::Manual)
            .map(|c| c.manual_active)
    }

    /// Force a full resend of mode + brightness on the next tick.
    pub fn force_resend(&mut self) {
        self.last_mode = Default::default();
        self.last_brightness = Default::default();
    }

    pub fn tick(&mut self, audio: &dyn AudioBackend) {
        if !self.experimental || !self.link.is_connected() {
            return;
        }
        for led in 0..NUM_LEDS {
            let Some(cfg) = self.configs[led].clone() else {
                self.state.lock()[led] = LedLive::default();
                continue;
            };
            let active = self.resolve(led, &cfg, audio);
            let mode = if active { cfg.active.clone() } else { cfg.inactive.clone() };

            // Mode (L/B) command.
            if self.last_mode[led].as_ref() != Some(&mode)
                && self.link.write_line(&mode.command(led as u8))
            {
                self.last_mode[led] = Some(mode.clone());
            }
            // Brightness (H) — per state, fixed or synced to a device's volume.
            // Skipped for the off mode (nothing to light).
            if mode.kind != LedModeKind::Off {
                let bb = mode_brightness(&mode, audio);
                if self.last_brightness[led] != Some(bb)
                    && self.link.write_line(&format!("H,{led},{bb}"))
                {
                    self.last_brightness[led] = Some(bb);
                }
            }

            self.state.lock()[led] = LedLive {
                configured: true,
                manual: cfg.control == LedControl::Manual,
                active,
                mode_kind: mode.kind as u8,
            };
        }
    }

    fn resolve(&mut self, led: usize, cfg: &LedConfig, audio: &dyn AudioBackend) -> bool {
        match cfg.control {
            LedControl::Manual => cfg.manual_active,
            LedControl::Conditional => {
                if cfg.conditions.is_empty() {
                    return false;
                }
                let mut results = Vec::with_capacity(cfg.conditions.len());
                for (i, cond) in cfg.conditions.iter().enumerate() {
                    results.push(self.eval(led, i, cond, audio));
                }
                match cfg.combine {
                    CombineLogic::Any => results.iter().any(|&b| b),
                    CombineLogic::All => results.iter().all(|&b| b),
                }
            }
        }
    }

    fn eval(&mut self, led: usize, idx: usize, cond: &LedCondition, audio: &dyn AudioBackend) -> bool {
        match cond.kind {
            LedConditionKind::Muted => audio.is_muted(mute_target(cond)),
            LedConditionKind::Volume => {
                let Some(v) = audio.get_volume(vol_target(cond)) else { return false };
                let pct = (v * 100.0).round() as i32;
                let target = cond.value as i32;
                match cond.cmp {
                    Comparison::Below => pct < target,
                    Comparison::Above => pct > target,
                    Comparison::Exact => pct == target,
                }
            }
            LedConditionKind::Api => self.eval_api(led, idx, cond),
        }
    }

    fn eval_api(&mut self, led: usize, idx: usize, cond: &LedCondition) -> bool {
        let api = &cond.api;
        if api.url.is_empty() {
            return false;
        }
        let interval = Duration::from_millis(api.interval_ms.max(100) as u64);
        let slot = self.api.entry((led, idx)).or_insert_with(|| ApiSlot {
            last_poll: None,
            value: Arc::new(Mutex::new(false)),
            inflight: Arc::new(AtomicBool::new(false)),
        });
        let due = slot.last_poll.map_or(true, |t| t.elapsed() >= interval);
        if due && !slot.inflight.load(Ordering::Acquire) {
            slot.inflight.store(true, Ordering::Release);
            slot.last_poll = Some(Instant::now());
            let value = slot.value.clone();
            let inflight = slot.inflight.clone();
            let api = api.clone();
            let _ = std::thread::Builder::new()
                .name("slidr-led-api".into())
                .spawn(move || {
                    let result = poll_api(&api);
                    *value.lock() = result;
                    inflight.store(false, Ordering::Release);
                });
        }
        *slot.value.lock()
    }
}

/// Poll the endpoint and derive a boolean: true on a 2xx response, optionally
/// further requiring the body to contain `expect_body`.
fn poll_api(api: &LedApi) -> bool {
    let mut req = ureq::request(api.method.as_str(), &api.url);
    if let Some(token) = &api.bearer {
        req = req.set("Authorization", &format!("Bearer {token}"));
    }
    let resp = match &api.payload {
        Some(body) if !body.is_empty() => {
            req.set("Content-Type", "application/json").send_string(body)
        }
        _ => req.call(),
    };
    match resp {
        Ok(r) => match &api.expect_body {
            Some(want) if !want.is_empty() => r.into_string().unwrap_or_default().contains(want.as_str()),
            _ => true, // any non-error (2xx) response
        },
        Err(ureq::Error::Status(_, r)) => match &api.expect_body {
            // Non-2xx: only true if a body match was requested and matches.
            Some(want) if !want.is_empty() => {
                r.into_string().unwrap_or_default().contains(want.as_str())
            }
            _ => false,
        },
        Err(e) => {
            log::warn!("led api {} {} failed: {e}", api.method.as_str(), api.url);
            false
        }
    }
}

/// Resolve a lit mode's brightness byte (0..=255): a fixed value, or the live
/// volume percent of the chosen device when syncing.
fn mode_brightness(mode: &LedMode, audio: &dyn AudioBackend) -> u8 {
    let pct: f32 = match mode.brightness_mode {
        BrightnessMode::Fixed => mode.brightness as f32 / 100.0,
        BrightnessMode::Volume => audio
            .get_volume(mode_vol_target(mode))
            .unwrap_or(mode.brightness as f32 / 100.0),
    };
    (pct.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn mode_vol_target(mode: &LedMode) -> VolumeTarget<'_> {
    match mode.brightness_audio {
        AudioKind::Process => VolumeTarget::Process(mode.brightness_target.as_deref().unwrap_or("")),
        AudioKind::Mic => VolumeTarget::Mic(mode.brightness_target.as_deref().unwrap_or("")),
        AudioKind::Output => VolumeTarget::System(mode.brightness_target.as_deref()),
    }
}

fn vol_target(cond: &LedCondition) -> VolumeTarget<'_> {
    match cond.audio_kind {
        AudioKind::Process => VolumeTarget::Process(cond.target.as_deref().unwrap_or("")),
        AudioKind::Mic => VolumeTarget::Mic(cond.target.as_deref().unwrap_or("")),
        AudioKind::Output => VolumeTarget::System(cond.target.as_deref()),
    }
}

fn mute_target(cond: &LedCondition) -> MuteTarget<'_> {
    match cond.audio_kind {
        AudioKind::Process => MuteTarget::Process(cond.target.as_deref().unwrap_or("")),
        AudioKind::Mic => MuteTarget::Mic(cond.target.as_deref().unwrap_or("")),
        AudioKind::Output => MuteTarget::System(cond.target.as_deref()),
    }
}
