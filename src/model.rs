//! Persistent data model: presets, categories, actions, audio streams, settings.

use serde::{Deserialize, Serialize};

use crate::curve::{BezierPoints, CurvePreset};

/// Audio target on a slider.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AudioStream {
    /// Process name (e.g. `firefox`) or absolute path to an executable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<String>,
    /// Microphone friendly name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mic_name: Option<String>,
    /// Output device friendly name. `None` = default endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
}

impl AudioStream {
    pub fn label(&self) -> String {
        if let Some(p) = &self.process {
            p.clone()
        } else if let Some(m) = &self.mic_name {
            format!("mic: {m}")
        } else if let Some(d) = &self.device_name {
            format!("device: {d}")
        } else {
            "default output".into()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    MuteProcess,
    MuteMainAudio,
    MuteMic,
    OpenProcess,
    OpenWebsite,
    KeyPress,
    CycleOutput,
}

impl ActionKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::MuteProcess => "Mute process",
            Self::MuteMainAudio => "Mute main audio",
            Self::MuteMic => "Mute microphone",
            Self::OpenProcess => "Open application",
            Self::OpenWebsite => "Open website",
            Self::KeyPress => "Simulate key",
            Self::CycleOutput => "Cycle output device",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ButtonAction {
    pub kind: ActionKind,
    /// Payload, semantics vary by kind:
    /// * `MuteProcess` / `OpenProcess` → process name or path
    /// * `MuteMainAudio` → optional device name
    /// * `MuteMic` → mic friendly name
    /// * `OpenWebsite` → URL
    /// * `KeyPress` → virtual key code (decimal u32)
    /// * `CycleOutput` → newline-separated output device names (empty = all)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub property: Option<String>,
    /// Optional pretty display string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
}

// ── LED control (experimental: buttons 5/6/7 → board LEDs A0/A1/D13) ─────────

/// Number of controllable LEDs on the board.
pub const NUM_LEDS: usize = 3;
/// Button index (0-based) that owns each LED. Display buttons 5/6/7 → LED 0/1/2.
pub const LED_BUTTONS: [usize; NUM_LEDS] = [4, 5, 6];
/// LED index for a button index, if that button has an LED.
pub fn led_for_button(btn: usize) -> Option<usize> {
    LED_BUTTONS.iter().position(|&b| b == btn)
}

/// HTTP method for an LED "API" condition poll.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    #[default]
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl HttpMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }
    pub fn from_index(i: i32) -> Self {
        match i {
            1 => Self::Post,
            2 => Self::Put,
            3 => Self::Patch,
            4 => Self::Delete,
            _ => Self::Get,
        }
    }
    pub fn to_index(self) -> i32 {
        match self {
            Self::Get => 0,
            Self::Post => 1,
            Self::Put => 2,
            Self::Patch => 3,
            Self::Delete => 4,
        }
    }
}

/// LED visual appearance — one of the firmware modes (see LED-Steuerung.md).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedModeKind {
    #[default]
    Off,
    On,
    Blink,
    Breathe,
}

/// Where an LED state's brightness comes from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrightnessMode {
    /// Use the fixed `brightness` value.
    #[default]
    Fixed,
    /// Track the live volume percent of `brightness_audio` / `brightness_target`.
    Volume,
}

/// A concrete LED appearance with its timing + brightness parameters. Brightness
/// is per-state (each of an LED's active/inactive appearances has its own).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedMode {
    #[serde(default)]
    pub kind: LedModeKind,
    /// Blink on-phase duration (ms).
    #[serde(default = "default_blink_ms")]
    pub on_ms: u32,
    /// Blink off-phase duration (ms).
    #[serde(default = "default_blink_ms")]
    pub off_ms: u32,
    /// Breathe (wabern) cycle duration (ms).
    #[serde(default = "default_breathe_ms")]
    pub cycle_ms: u32,
    /// Fixed value vs. sync-to-volume.
    #[serde(default)]
    pub brightness_mode: BrightnessMode,
    /// Peak brightness 0..=100 (used when `brightness_mode == Fixed`).
    #[serde(default = "default_brightness")]
    pub brightness: u8,
    /// Audio endpoint family to follow when syncing brightness to volume.
    #[serde(default)]
    pub brightness_audio: AudioKind,
    /// Process/mic/output name to follow. None = default output endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brightness_target: Option<String>,
}

impl Default for LedMode {
    fn default() -> Self {
        Self {
            kind: LedModeKind::Off,
            on_ms: 500,
            off_ms: 500,
            cycle_ms: 2000,
            brightness_mode: BrightnessMode::Fixed,
            brightness: 80,
            brightness_audio: AudioKind::Process,
            brightness_target: None,
        }
    }
}

impl LedMode {
    pub fn on() -> Self {
        Self { kind: LedModeKind::On, ..Default::default() }
    }
    /// Serial command line realising this mode on `led` (0..2), no trailing newline.
    pub fn command(&self, led: u8) -> String {
        match self.kind {
            LedModeKind::Off => format!("L,{led},0"),
            LedModeKind::On => format!("L,{led},1"),
            LedModeKind::Blink => format!("L,{led},2,{},{}", self.on_ms.max(1), self.off_ms.max(1)),
            LedModeKind::Breathe => format!("L,{led},3,{}", self.cycle_ms.max(1)),
        }
    }
}

/// How the LED chooses between its active and inactive appearance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedControl {
    #[default]
    Conditional,
    Manual,
}

/// How multiple conditions combine into the LED's active state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CombineLogic {
    #[default]
    Any,
    All,
}

/// Comparison for a volume condition.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Comparison {
    #[default]
    Below,
    Above,
    Exact,
}

/// Which kind of audio endpoint a condition targets.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioKind {
    #[default]
    Process,
    Mic,
    Output,
}

/// What a single toggle condition tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedConditionKind {
    #[default]
    Muted,
    Volume,
    Api,
}

/// HTTP poll used by an `Api` condition. Mirrors a button "API call", plus the
/// poll interval and an optional expected-body match used to derive a boolean.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedApi {
    #[serde(default)]
    pub method: HttpMethod,
    #[serde(default)]
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bearer: Option<String>,
    /// How often (ms) to poll the endpoint to check for a state change.
    #[serde(default = "default_led_interval")]
    pub interval_ms: u32,
    /// Condition is true when the response body contains this text. Empty/None →
    /// any 2xx response counts as true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_body: Option<String>,
}

/// One toggle condition. Fields are interpreted per `kind`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedCondition {
    #[serde(default)]
    pub kind: LedConditionKind,
    /// For `Muted`/`Volume`: which endpoint family.
    #[serde(default)]
    pub audio_kind: AudioKind,
    /// Process/mic/output device name. None = default output endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// For `Volume`: comparison against `value`.
    #[serde(default)]
    pub cmp: Comparison,
    /// For `Volume`: threshold percent 0..=100.
    #[serde(default)]
    pub value: u8,
    /// For `Api`.
    #[serde(default)]
    pub api: LedApi,
}

/// Per-LED configuration. Conditions decide the `active` vs `inactive` look.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedConfig {
    #[serde(default)]
    pub control: LedControl,
    /// Appearance shown when the LED is active (conditions met / manual on).
    #[serde(default = "LedMode::on")]
    pub active: LedMode,
    /// Appearance shown when the LED is inactive.
    #[serde(default)]
    pub inactive: LedMode,
    #[serde(default)]
    pub combine: CombineLogic,
    #[serde(default)]
    pub conditions: Vec<LedCondition>,
    /// Manual-mode persisted state: true = active.
    #[serde(default)]
    pub manual_active: bool,
}

impl Default for LedConfig {
    fn default() -> Self {
        Self {
            control: LedControl::Conditional,
            active: LedMode::on(),
            inactive: LedMode::default(),
            combine: CombineLogic::Any,
            conditions: Vec::new(),
            manual_active: false,
        }
    }
}

fn default_blink_ms() -> u32 {
    500
}
fn default_breathe_ms() -> u32 {
    2000
}
fn default_led_interval() -> u32 {
    1000
}
fn default_brightness() -> u8 {
    80
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ButtonCategory {
    pub id: u32,
    pub name: String,
    #[serde(default)]
    pub actions: Vec<ButtonAction>,
    /// UI-only: whether the card is collapsed in the categories view.
    #[serde(default)]
    pub collapsed: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SliderCategory {
    pub id: u32,
    pub name: String,
    #[serde(default)]
    pub streams: Vec<AudioStream>,
    /// UI-only: whether the card is collapsed in the categories view.
    #[serde(default)]
    pub collapsed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeMode {
    System,
    Light,
    Dark,
}

impl Default for ThemeMode {
    fn default() -> Self {
        Self::System
    }
}

/// Global settings — system-wide, shared across all profiles.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "default_true")]
    pub minimize_to_tray: bool,
    #[serde(default)]
    pub start_with_os: bool,
    #[serde(default)]
    pub start_minimized: bool,
    #[serde(default)]
    pub tray_intro_shown: bool,

    /// Experimental: enable per-button LED control (board buttons 5/6/7).
    #[serde(default)]
    pub led_experimental: bool,

    /// Last loaded profile name (sticky across launches).
    #[serde(default)]
    pub active_preset: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            minimize_to_tray: true,
            start_with_os: false,
            start_minimized: false,
            tray_intro_shown: false,
            led_experimental: false,
            active_preset: "Default".into(),
        }
    }
}

/// Per-profile appearance + slider settings (saved inside each profile).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileSettings {
    #[serde(default)]
    pub theme: ThemeMode,
    #[serde(default)]
    pub accent_preset: i32,

    #[serde(default = "default_dead_zone")]
    pub slider_dead_zone: i32,
    #[serde(default = "default_true")]
    pub unmute_on_change: bool,
    #[serde(default)]
    pub curve_preset: CurvePreset,
    #[serde(default)]
    pub custom_curve: BezierPoints,
}

impl Default for ProfileSettings {
    fn default() -> Self {
        Self {
            theme: ThemeMode::default(),
            accent_preset: 0,
            slider_dead_zone: 4,
            unmute_on_change: true,
            curve_preset: CurvePreset::default(),
            custom_curve: BezierPoints::default(),
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_dead_zone() -> i32 {
    4
}

/// Slider/button → category assignments (indices 0-based).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Assignments {
    /// Slider index → SliderCategory.id (None = unassigned).
    #[serde(default)]
    pub sliders: [Option<u32>; crate::protocol::NUM_SLIDERS],
    /// Button index → ButtonCategory.id.
    #[serde(default)]
    pub buttons: [Option<u32>; crate::protocol::NUM_BUTTONS],
}

/// In-memory preset (also the on-disk shape, split across files when saving).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Preset {
    pub id: u32,
    pub name: String,
    #[serde(default)]
    pub slider_categories: Vec<SliderCategory>,
    #[serde(default)]
    pub button_categories: Vec<ButtonCategory>,
    #[serde(default)]
    pub assignments: Assignments,
    /// Appearance + slider settings carried by this profile.
    #[serde(default)]
    pub settings: ProfileSettings,
    /// Per-LED config, indexed by LED (0/1/2 = buttons 5/6/7). None = not configured.
    #[serde(default)]
    pub leds: [Option<LedConfig>; NUM_LEDS],
}

impl Preset {
    pub fn slider_category(&self, slider_idx: usize) -> Option<&SliderCategory> {
        let id = self.assignments.sliders.get(slider_idx)?.as_ref()?;
        self.slider_categories.iter().find(|c| c.id == *id)
    }

    pub fn button_category(&self, button_idx: usize) -> Option<&ButtonCategory> {
        let id = self.assignments.buttons.get(button_idx)?.as_ref()?;
        self.button_categories.iter().find(|c| c.id == *id)
    }
}

/// Allocate a fresh id not already used in `existing`.
pub fn next_id<T, F: Fn(&T) -> u32>(existing: &[T], id_of: F) -> u32 {
    let mut max = 0u32;
    for e in existing {
        let v = id_of(e);
        if v > max {
            max = v;
        }
    }
    max + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_id_grows() {
        let v: Vec<SliderCategory> = vec![
            SliderCategory { id: 1, name: "a".into(), streams: vec![], collapsed: false },
            SliderCategory { id: 5, name: "b".into(), streams: vec![], collapsed: false },
        ];
        assert_eq!(next_id(&v, |c| c.id), 6);
    }

    #[test]
    fn audio_stream_label() {
        assert_eq!(
            AudioStream { process: Some("firefox".into()), ..Default::default() }.label(),
            "firefox"
        );
        assert_eq!(AudioStream::default().label(), "default output");
    }
}
