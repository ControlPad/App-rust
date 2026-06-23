//! Edge detection + dispatch.
//!
//! Receives parsed [`Frame`]s, diffs against previous state, applies dead-zone &
//! throttle, then emits commands to the audio/key subsystems and UI state updates
//! to the renderer.

use std::time::{Duration, Instant};

use crate::model::{ActionKind, Preset};
use crate::protocol::{Frame, NUM_BUTTONS, NUM_SLIDERS};

/// Owned audio/key target (no borrows, so commands cross thread boundaries).
#[derive(Debug, Clone)]
pub enum Target {
    Process(String),
    Mic(String),
    System(Option<String>),
}

/// A side-effect emitted by the engine, executed off the UI thread by the
/// actuator so rendering never blocks on audio / key I/O.
#[derive(Debug, Clone)]
pub enum Cmd {
    SetVolume { target: Target, value: f32, unmute: bool },
    ToggleMute(Target),
    Open(String),
    KeyDown(u32),
    KeyUp(u32),
    /// Switch the default output to the next device in the list (wrapping).
    /// Empty list = cycle through all active output endpoints.
    CycleOutput(Vec<String>),
    /// Fire an HTTP request. `{value}` placeholders are already substituted.
    ApiCall {
        method: crate::model::HttpMethod,
        url: String,
        payload: Option<String>,
        bearer: Option<String>,
    },

    // ── Experimental LED control ──
    /// Replace the full per-LED configuration set.
    SetLeds(Box<[Option<crate::model::LedConfig>; crate::model::NUM_LEDS]>),
    /// Enable/disable LED output globally (experimental toggle).
    SetLedExperimental(bool),
    /// Toggle a manual-mode LED's active state (led index 0..2).
    LedManualToggle(u8),
    /// The board (re)connected — re-push all LED state.
    SerialConnected,
    /// Persist the current live LED state to the board's EEPROM (`S`).
    LedSaveState,
    /// Turn all LEDs off and persist that to EEPROM (clear the saved look).
    LedClearSaved,
}

/// Snapshot pushed to the UI on every dispatch tick.
#[derive(Debug, Clone, Default)]
pub struct LiveState {
    pub sliders: [i32; NUM_SLIDERS],
    pub buttons: [u8; NUM_BUTTONS],
    pub board: crate::protocol::BoardOrientation,
    pub badge: crate::protocol::Badge,
    pub connected: bool,
}

/// How long after a (re)connect we ignore incoming frames. Opening the serial
/// port toggles DTR, which resets the Arduino; while it boots, its slider/button
/// pins float to garbage (analog pins often read max → 100%, button pins can
/// read HIGH). Swallowing this window stops phantom volume spikes and spurious
/// button actions (e.g. a random "Open Spotify").
const SETTLE_WINDOW: Duration = Duration::from_millis(1000);

pub struct EventEngine {
    prev_sliders: [i32; NUM_SLIDERS],
    prev_buttons: [u8; NUM_BUTTONS],
    last_emit: Instant,
    emit_interval: Duration,
    state: LiveState,
    /// Frames received before this instant are treated as boot noise and dropped.
    settle_deadline: Instant,
    /// Set on (re)connect: the first frame past the settle window re-establishes
    /// the edge-detection baseline instead of firing actions.
    resync: bool,
    /// Per-slider throttle bookkeeping for API actions: the last raw value seen
    /// but not yet sent, and when we last sent one.
    pending_api: [Option<i32>; NUM_SLIDERS],
    last_api_emit: [Instant; NUM_SLIDERS],
}

impl Default for EventEngine {
    fn default() -> Self {
        Self {
            prev_sliders: [-1; NUM_SLIDERS],
            prev_buttons: [0; NUM_BUTTONS],
            last_emit: Instant::now() - Duration::from_secs(1),
            emit_interval: Duration::from_millis(16),
            state: LiveState::default(),
            settle_deadline: Instant::now(),
            resync: false,
            pending_api: [None; NUM_SLIDERS],
            last_api_emit: [Instant::now() - Duration::from_secs(2); NUM_SLIDERS],
        }
    }
}

impl EventEngine {
    pub fn state(&self) -> &LiveState {
        &self.state
    }

    pub fn set_connected(&mut self, connected: bool) {
        // Rising edge: the port just (re)opened, which resets the board. Arm the
        // settle window + force a baseline resync so the boot-time pin garbage
        // can't fire actions or slam volumes to 100%.
        if connected && !self.state.connected {
            self.settle_deadline = Instant::now() + SETTLE_WINDOW;
            self.resync = true;
        }
        self.state.connected = connected;
    }

    /// Diff the frame against previous state. Returns `(changed, cmds)` — `cmds`
    /// are the side effects to run on the actuator thread. This method does no
    /// audio/key I/O itself, so it's cheap to call on the UI thread.
    pub fn ingest(
        &mut self,
        frame: &Frame,
        preset: &Preset,
    ) -> (bool, Vec<Cmd>) {
        let settings = &preset.settings;
        let mut changed = false;
        let mut cmds = Vec::new();
        let now = Instant::now();

        // Always keep raw state mirror up to date.
        self.state.sliders = frame.sliders;
        self.state.buttons = frame.buttons;
        if self.state.board != frame.board {
            self.state.board = frame.board;
            changed = true;
        }
        if self.state.badge != frame.badge {
            self.state.badge = frame.badge;
            changed = true;
        }

        // Settle window: drop boot-time pin garbage after a (re)connect. We still
        // mirror the values to the UI above, but emit no commands and don't let
        // them poison the edge-detection baseline. This is what prevents the
        // phantom "Open Spotify" and the brief all-volumes-to-100% flash.
        if now < self.settle_deadline {
            return (changed, cmds);
        }

        // Throttle action dispatch (matches reference 16ms cadence).
        if now.duration_since(self.last_emit) < self.emit_interval {
            return (changed, cmds);
        }
        self.last_emit = now;

        // First good frame after the settle window: adopt the current button
        // state as the baseline (so a pin that settled HIGH isn't read as a fresh
        // press) and invalidate the slider baseline so volumes re-sync once to the
        // real physical positions.
        if self.resync {
            self.resync = false;
            self.prev_buttons = frame.buttons;
            self.prev_sliders = [-1; NUM_SLIDERS];
            // The board has now finished booting — this is the first frame past
            // the post-reset settle window, so it can finally receive serial
            // commands. The LED push on `SerialEvent::Connected` fired while the
            // board was still in its DTR-reset bootloader and was lost; this is
            // the resend that actually lands, giving the board its initial setup.
            cmds.push(Cmd::SerialConnected);
        }

        // Button edges → fire actions.
        for i in 0..NUM_BUTTONS {
            let prev = self.prev_buttons[i];
            let cur = frame.buttons[i];
            if prev == cur {
                continue;
            }
            self.prev_buttons[i] = cur;
            changed = true;

            // Experimental: a manual-mode LED toggles on its button's press edge,
            // independent of any category action assigned to that button.
            if cur == 1 {
                if let Some(led) = crate::model::led_for_button(i) {
                    if let Some(cfg) = &preset.leds[led] {
                        if cfg.control == crate::model::LedControl::Manual {
                            cmds.push(Cmd::LedManualToggle(led as u8));
                        }
                    }
                }
            }

            let Some(cat) = preset.button_category(i) else { continue };
            for action in &cat.actions {
                push_button_cmds(action, cur == 1, prev == 1, &mut cmds);
            }
        }

        // Slider deltas → volume updates.
        for i in 0..NUM_SLIDERS {
            let prev = self.prev_sliders[i];
            let cur = frame.sliders[i];
            let diff = (cur - prev).abs();
            let first = prev < 0;
            if !first && diff <= settings.slider_dead_zone {
                continue;
            }
            self.prev_sliders[i] = cur;
            changed = true;

            let Some(cat) = preset.slider_category(i) else { continue };
            let pts = crate::curve::BezierPoints::for_preset(settings.curve_preset, settings.custom_curve);
            let volume = crate::curve::raw_to_volume(cur, pts);

            for stream in &cat.streams {
                if stream.api.is_some() {
                    // API streams are throttled + flushed below; just record the
                    // latest position here.
                    self.pending_api[i] = Some(cur);
                } else {
                    cmds.push(Cmd::SetVolume {
                        target: stream_target(stream),
                        value: volume,
                        unmute: settings.unmute_on_change,
                    });
                }
            }
        }

        // Flush throttled slider API calls. Runs every tick (not just on change),
        // so the trailing value lands within one throttle window after the slider
        // stops moving. Each API action carries its own interval (configured in
        // the wizard); a slider with several uses the smallest, clamped to a sane
        // range.
        for i in 0..NUM_SLIDERS {
            let Some(raw) = self.pending_api[i] else { continue };
            let Some(cat) = preset.slider_category(i) else {
                self.pending_api[i] = None;
                continue;
            };
            let Some(throttle_ms) = cat
                .streams
                .iter()
                .filter_map(|s| s.api.as_ref())
                .map(|a| {
                    a.throttle_ms
                        .clamp(crate::model::API_THROTTLE_MIN_MS, crate::model::API_THROTTLE_MAX_MS)
                })
                .min()
            else {
                self.pending_api[i] = None;
                continue;
            };
            if now.duration_since(self.last_api_emit[i]) < Duration::from_millis(throttle_ms as u64) {
                continue;
            }
            self.last_api_emit[i] = now;
            self.pending_api[i] = None;
            let percent = (crate::curve::normalize_raw(raw) * 100.0).round() as i32;
            for stream in &cat.streams {
                if let Some(api) = &stream.api {
                    cmds.push(api_cmd(api, Some(percent)));
                }
            }
        }

        (changed, cmds)
    }
}

/// Build an [`Cmd::ApiCall`], substituting the `{value}` placeholder with the
/// slider percent (for slider actions). Buttons pass `None` (no substitution).
fn api_cmd(api: &crate::model::ApiCall, percent: Option<i32>) -> Cmd {
    let sub = |s: &str| -> String {
        match percent {
            Some(p) => s.replace("{value}", &p.to_string()),
            None => s.to_string(),
        }
    };
    Cmd::ApiCall {
        method: api.method,
        url: sub(&api.url),
        payload: api.payload.as_deref().map(sub),
        bearer: api.bearer.clone(),
    }
}

fn stream_target(s: &crate::model::AudioStream) -> Target {
    if let Some(p) = &s.process {
        Target::Process(p.clone())
    } else if let Some(m) = &s.mic_name {
        Target::Mic(m.clone())
    } else {
        Target::System(s.device_name.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ApiCall, HttpMethod};

    #[test]
    fn slider_api_substitutes_value_placeholder() {
        let api = ApiCall {
            method: HttpMethod::Post,
            url: "https://x/set?v={value}".into(),
            payload: Some("{\"pct\": {value}}".into()),
            bearer: Some("tok".into()),
            ..Default::default()
        };
        let Cmd::ApiCall { method, url, payload, bearer } = api_cmd(&api, Some(73)) else {
            panic!("expected ApiCall");
        };
        assert_eq!(method, HttpMethod::Post);
        assert_eq!(url, "https://x/set?v=73");
        assert_eq!(payload.as_deref(), Some("{\"pct\": 73}"));
        assert_eq!(bearer.as_deref(), Some("tok"));
    }

    #[test]
    fn button_api_leaves_placeholder_untouched() {
        let api = ApiCall { url: "https://x/{value}".into(), ..Default::default() };
        let Cmd::ApiCall { url, .. } = api_cmd(&api, None) else { panic!("expected ApiCall") };
        assert_eq!(url, "https://x/{value}");
    }
}

fn push_button_cmds(
    action: &crate::model::ButtonAction,
    pressed: bool,
    was_pressed: bool,
    out: &mut Vec<Cmd>,
) {
    let just_pressed = pressed && !was_pressed;
    let just_released = !pressed && was_pressed;

    match action.kind {
        ActionKind::MuteProcess => {
            if just_pressed {
                if let Some(p) = &action.property {
                    out.push(Cmd::ToggleMute(Target::Process(p.clone())));
                }
            }
        }
        ActionKind::MuteMainAudio => {
            if just_pressed {
                out.push(Cmd::ToggleMute(Target::System(action.property.clone())));
            }
        }
        ActionKind::MuteMic => {
            if just_pressed {
                if let Some(m) = &action.property {
                    out.push(Cmd::ToggleMute(Target::Mic(m.clone())));
                }
            }
        }
        ActionKind::OpenProcess | ActionKind::OpenWebsite => {
            if just_pressed {
                if let Some(t) = &action.property {
                    out.push(Cmd::Open(t.clone()));
                }
            }
        }
        ActionKind::KeyPress => {
            let Some(prop) = &action.property else { return };
            let Ok(vk) = prop.parse::<u32>() else { return };
            if just_pressed {
                out.push(Cmd::KeyDown(vk));
            } else if just_released {
                out.push(Cmd::KeyUp(vk));
            }
        }
        ActionKind::CycleOutput => {
            if just_pressed {
                // Property is a newline-separated device list; empty = all.
                let devices = action
                    .property
                    .as_deref()
                    .map(|p| p.lines().map(str::to_string).filter(|s| !s.is_empty()).collect())
                    .unwrap_or_default();
                out.push(Cmd::CycleOutput(devices));
            }
        }
        ActionKind::ApiCall => {
            if just_pressed {
                if let Some(api) = &action.api {
                    out.push(api_cmd(api, None));
                }
            }
        }
    }
}
