//! Persistent data model: presets, categories, actions, audio streams, settings.

use serde::{Deserialize, Serialize};

use crate::curve::{BezierPoints, CurvePreset};

/// Audio target on a slider.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AudioStream {
    /// Process name (e.g. `firefox`), absolute path to an executable, or a
    /// program-category reference (`@group:<id>`, see [`crate::app_groups`])
    /// that follows every member of the category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<String>,
    /// Microphone friendly name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mic_name: Option<String>,
    /// Output device friendly name. `None` = default endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    /// When set, this slider entry fires an HTTP request (with the slider
    /// percent substituted for `{value}`) instead of controlling audio volume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<ApiCall>,
    /// When set, the slider drives a level inside Slidr's own mixer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mixer: Option<MixTarget>,
}

impl AudioStream {
    pub fn label(&self) -> String {
        if let Some(api) = &self.api {
            api.label()
        } else if let Some(m) = &self.mixer {
            format!("mixer: {}", m.key())
        } else if let Some(p) = &self.process {
            crate::app_groups::label_for(p)
        } else if let Some(m) = &self.mic_name {
            format!("mic: {m}")
        } else if let Some(d) = &self.device_name {
            format!("device: {d}")
        } else {
            "default output".into()
        }
    }
}

/// HTTP method for an [`ApiCall`] action.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    #[default]
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
    /// Dropdown index used by the wizard UI (must match the slint method list).
    pub fn from_index(i: i32) -> Self {
        match i {
            0 => Self::Get,
            2 => Self::Put,
            3 => Self::Patch,
            4 => Self::Delete,
            _ => Self::Post,
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

/// An HTTP request fired by a button press or slider movement. For sliders, the
/// literal `{value}` placeholder in `url` / `payload` is replaced with the
/// current slider position as a percent (0–100) before the request is sent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiCall {
    #[serde(default)]
    pub method: HttpMethod,
    pub url: String,
    /// Request body (typically JSON). Sent as `application/json`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<String>,
    /// Optional bearer token, sent as `Authorization: Bearer <token>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bearer: Option<String>,
    /// Minimum spacing (ms) between requests when fired by a moving slider.
    /// Clamped to [`API_THROTTLE_MIN_MS`, `API_THROTTLE_MAX_MS`]. Ignored for
    /// button actions (they fire once per press).
    #[serde(default = "default_api_throttle_ms")]
    pub throttle_ms: i32,
}

impl Default for ApiCall {
    fn default() -> Self {
        Self {
            method: HttpMethod::default(),
            url: String::new(),
            payload: None,
            bearer: None,
            throttle_ms: default_api_throttle_ms(),
        }
    }
}

impl ApiCall {
    pub fn label(&self) -> String {
        if self.url.is_empty() {
            "API call".into()
        } else {
            format!("{} {}", self.method.as_str(), self.url)
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
    ApiCall,
    /// Toggle mute of a mixer strip (`ButtonAction::mixer`).
    MuteMixer,
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
            Self::ApiCall => "API call",
            Self::MuteMixer => "Mute mixer channel",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ButtonAction {
    pub kind: ActionKind,
    /// Payload, semantics vary by kind:
    /// * `MuteProcess` / `OpenProcess` → process name, path, or `@group:<id>`
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
    /// HTTP request config, used when `kind == ApiCall`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<ApiCall>,
    /// Mixer strip, used when `kind == MuteMixer`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mixer: Option<MixTarget>,
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
    /// The OS media session's playback state (Spotify, a browser tab, any
    /// player that publishes transport controls).
    Media,
    /// Discord voice state (self-mute / deafen / in a voice channel).
    Discord,
}

/// HTTP poll used by an `Api` condition. Mirrors a button "API call", plus the
/// poll interval and an optional expected-body match used to derive a boolean.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedApi {
    #[serde(default = "led_api_default_method")]
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

/// An LED state-check poll defaults to GET (unlike a button "API call" action,
/// whose [`HttpMethod`] default is POST).
fn led_api_default_method() -> HttpMethod {
    HttpMethod::Get
}

impl Default for LedApi {
    fn default() -> Self {
        Self {
            method: led_api_default_method(),
            url: String::new(),
            payload: None,
            bearer: None,
            interval_ms: default_led_interval(),
            expect_body: None,
        }
    }
}

/// One toggle condition. Fields are interpreted per `kind`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedCondition {
    #[serde(default)]
    pub kind: LedConditionKind,
    /// For `Muted`/`Volume`: which endpoint family.
    #[serde(default)]
    pub audio_kind: AudioKind,
    /// Process/mic/output device name, or — for `Media` — the media source to
    /// watch (empty/None = any source). None = default output endpoint.
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
    /// For `Discord`: which part of the voice state to follow.
    #[serde(default)]
    pub discord: crate::discord::DiscordSignal,
    /// For `Media`: the playback state to match.
    #[serde(default)]
    pub media: crate::media::MediaStatus,
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

    /// Discord application used for the LED "Discord" condition source. The id
    /// is not a secret; the secret is only needed until a token is cached, and
    /// `SLIDR_DISCORD_CLIENT_SECRET` overrides whatever is stored here.
    #[serde(default)]
    pub discord_client_id: String,
    #[serde(default)]
    pub discord_client_secret: String,

    /// Ask GitHub once per launch whether a newer release exists. Opt-out: the
    /// check is a single outbound request and nothing is ever downloaded or
    /// installed automatically, but it is still the app talking to the network
    /// unprompted, so it can be switched off.
    #[serde(default = "default_true")]
    pub update_check: bool,

    /// Last loaded profile name (sticky across launches).
    #[serde(default)]
    pub active_preset: String,

    /// Devices the software mixer plays to and records from. Machine-specific,
    /// so global rather than per profile.
    #[serde(default)]
    pub mixer_devices: MixerDevices,

    /// Effect chains saved by the user, offered on every profile's strips.
    #[serde(default)]
    pub fx_presets: Vec<FxPreset>,

    /// Short sounds when mixer things are switched (see `mixer::sfx`).
    #[serde(default = "default_true")]
    pub mixer_sounds: bool,
    /// Their volume, 0..1.
    #[serde(default = "default_sound_volume")]
    pub mixer_sound_volume: f32,

    /// Sidebar tabs in display order, by name (see `crate::sidebar`).
    #[serde(default)]
    pub nav_order: Vec<String>,
    /// Tabs hidden from the sidebar. Settings can't be hidden.
    #[serde(default)]
    pub nav_hidden: Vec<String>,
}

fn default_sound_volume() -> f32 {
    0.8
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            minimize_to_tray: true,
            start_with_os: false,
            start_minimized: false,
            tray_intro_shown: false,
            led_experimental: false,
            discord_client_id: String::new(),
            discord_client_secret: String::new(),
            update_check: true,
            active_preset: "Default".into(),
            mixer_devices: MixerDevices::default(),
            fx_presets: Vec::new(),
            mixer_sounds: true,
            mixer_sound_volume: default_sound_volume(),
            nav_order: Vec::new(),
            nav_hidden: Vec::new(),
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
pub fn default_api_throttle_ms() -> i32 {
    500
}

/// Allowed range for [`ApiCall::throttle_ms`].
pub const API_THROTTLE_MIN_MS: i32 = 50;
pub const API_THROTTLE_MAX_MS: i32 = 1000;

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
    /// Newest Slidr version that has written this profile. Raised on save,
    /// never lowered; empty for profiles written before stamping existed.
    /// See [`crate::version`] for why the comparison only matters downwards.
    #[serde(default)]
    pub app_version: String,
    /// Software mixer channels of this profile (see `docs/mixer-plan.md`).
    #[serde(default)]
    pub mixer: MixerConfig,
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
    fn http_method_index_round_trip() {
        for m in [HttpMethod::Get, HttpMethod::Post, HttpMethod::Put, HttpMethod::Patch, HttpMethod::Delete] {
            assert_eq!(HttpMethod::from_index(m.to_index()), m);
        }
        // Out-of-range index falls back to the default (POST).
        assert_eq!(HttpMethod::from_index(99), HttpMethod::Post);
    }

    #[test]
    fn api_call_label_and_stream() {
        let api = ApiCall { method: HttpMethod::Post, url: "https://x/y".into(), ..Default::default() };
        assert_eq!(api.label(), "POST https://x/y");
        assert_eq!(ApiCall::default().label(), "API call");
        // An API stream's label comes from the API config, not audio fields.
        let s = AudioStream { api: Some(api), ..Default::default() };
        assert_eq!(s.label(), "POST https://x/y");
    }

    #[test]
    fn audio_stream_label() {
        assert_eq!(
            AudioStream { process: Some("firefox".into()), ..Default::default() }.label(),
            "firefox"
        );
        assert_eq!(AudioStream::default().label(), "default output");
    }

    #[test]
    fn effects_from_before_chains_become_a_chain() {
        let old = r#"{"eq_on": true, "eq": [{"kind": "peak", "freq": 1000.0, "gain_db": 3.0}],
            "gate": {"on": true, "threshold_db": -50.0, "release_ms": 150.0},
            "comp": {"on": false, "threshold_db": -20.0},
            "limit": {"on": true, "ceiling_db": -2.0}}"#;
        let fx: Effects = serde_json::from_str(old).unwrap();
        let kinds: Vec<&str> = fx.chain.iter().map(|s| s.fx.name()).collect();
        assert_eq!(kinds, ["Noise gate", "EQ", "Limiter"], "only what was on, in the old order");
        assert!(matches!(fx.chain[0].fx, Fx::Gate(Gate { threshold_db, .. }) if threshold_db == -50.0));
        assert!(matches!(fx.chain[2].fx, Fx::Limiter(Limiter { ceiling_db }) if ceiling_db == -2.0));
    }

    #[test]
    fn chain_round_trips() {
        let fx = Effects { on: false, chain: FxPreset::built_in().remove(1).chain };
        let json = serde_json::to_string(&fx).unwrap();
        assert!(json.contains("\"type\":\"compressor\""), "{json}");
        assert_eq!(serde_json::from_str::<Effects>(&json).unwrap(), fx);
    }
}

// ── Software mixer (see docs/mixer-plan.md) ─────────────────────────────────

/// One strip of the mixer a slider or button can address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MixTarget {
    /// The real microphone's strip (what goes into the Slidr Mic).
    Voice,
    /// A program channel's level (by `MixChannel::id`).
    Channel(u32),
    /// How much of a program channel goes into the Slidr Mic.
    Send(u32),
}

impl MixTarget {
    /// Stable short form, also used as a map key.
    pub fn key(&self) -> String {
        match self {
            Self::Voice => "voice".into(),
            Self::Channel(id) => format!("channel {id}"),
            Self::Send(id) => format!("send {id}"),
        }
    }

    /// Compact text form for the UI's string-valued pickers: `voice`,
    /// `channel:<id>`, `send:<id>`.
    pub fn to_value(&self) -> String {
        match self {
            Self::Voice => "voice".into(),
            Self::Channel(id) => format!("channel:{id}"),
            Self::Send(id) => format!("send:{id}"),
        }
    }

    pub fn from_value(v: &str) -> Option<Self> {
        if v == "voice" {
            return Some(Self::Voice);
        }
        let (kind, id) = v.split_once(':')?;
        let id = id.parse().ok()?;
        match kind {
            "channel" => Some(Self::Channel(id)),
            "send" => Some(Self::Send(id)),
            _ => None,
        }
    }
}

impl MixerConfig {
    /// What a strip is called in lists ("Music", "Music → Slidr Mic", …).
    pub fn label(&self, target: MixTarget) -> String {
        let name = |id: u32| {
            self.channels
                .iter()
                .find(|c| c.id == id)
                .map(|c| c.name.clone())
                .unwrap_or_else(|| format!("Channel {id}"))
        };
        match target {
            MixTarget::Voice => "Voice (microphone)".into(),
            MixTarget::Channel(id) => name(id),
            MixTarget::Send(id) => format!("{} → Slidr Mic", name(id)),
        }
    }

    /// Built-in EQ starting points, offered by the EQ editor.
    pub fn default_eq_presets() -> Vec<EqPreset> {
        use EqBandKind::*;
        let b = |kind, freq, gain_db, q| EqBand { kind, freq, gain_db, q };
        let q = default_q();
        vec![
            EqPreset {
                id: 1,
                name: "Voice clarity".into(),
                bands: vec![b(HighPass, 80.0, 0.0, q), b(LowShelf, 200.0, -2.0, q), b(Peak, 3000.0, 3.0, 1.0), b(HighShelf, 10000.0, 2.0, q)],
            },
            EqPreset {
                id: 2,
                name: "Warm voice".into(),
                bands: vec![b(HighPass, 70.0, 0.0, q), b(Peak, 250.0, 2.0, 0.8), b(HighShelf, 8000.0, -2.0, q)],
            },
            EqPreset { id: 3, name: "Bass boost".into(), bands: vec![b(LowShelf, 120.0, 6.0, q)] },
            EqPreset { id: 4, name: "Treble boost".into(), bands: vec![b(HighShelf, 6000.0, 5.0, q)] },
            EqPreset {
                id: 5,
                name: "Loudness".into(),
                bands: vec![b(LowShelf, 100.0, 4.0, q), b(HighShelf, 8000.0, 3.0, q)],
            },
        ]
    }
}

/// Mixer layout of a profile.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MixerConfig {
    /// Off by default: the mixer reroutes programs, which nobody should get by
    /// surprise after an update.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub channels: Vec<MixChannel>,
    #[serde(default)]
    pub voice: VoiceStrip,
    #[serde(default)]
    pub eq_presets: Vec<EqPreset>,
}

/// A group of programs mixed as one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MixChannel {
    pub id: u32,
    pub name: String,
    /// Process names, paths or `@group:<id>` refs, as for sliders.
    #[serde(default)]
    pub programs: Vec<String>,
    /// Fader position in [0, 1].
    #[serde(default = "default_level")]
    pub level: f32,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub fx: Effects,
    /// Move the programs to the sink so only Slidr's mix is heard. Off = the
    /// program keeps playing normally and is only copied into the Slidr Mic.
    #[serde(default = "default_true")]
    pub reroute: bool,
    /// Share of this channel in the Slidr Mic, [0, 1]; `None` = not sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mic_send: Option<f32>,
}

/// The real microphone's strip.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VoiceStrip {
    #[serde(default = "default_level")]
    pub level: f32,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub fx: Effects,
    /// Also play the voice on the monitor (hear yourself).
    #[serde(default)]
    pub to_monitor: bool,
}

impl Default for VoiceStrip {
    fn default() -> Self {
        Self { level: default_level(), muted: false, fx: Effects::default(), to_monitor: false }
    }
}

/// A strip's effect chain, processed top to bottom. The same effect may
/// appear more than once (two compressors, an EQ on each side of one).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(from = "EffectsFile")]
pub struct Effects {
    /// Master switch: off bypasses the whole chain, keeping every setting.
    #[serde(default = "default_true")]
    pub on: bool,
    #[serde(default)]
    pub chain: Vec<FxSlot>,
}

impl Default for Effects {
    fn default() -> Self {
        Self { on: true, chain: Vec::new() }
    }
}

impl Effects {
    /// How many effects are on (shown on the strip's button).
    pub fn active(&self) -> usize {
        self.chain.iter().filter(|s| s.on).count()
    }

    /// A free slot id within this chain.
    pub fn next_id(&self) -> u32 {
        self.chain.iter().map(|s| s.id).max().unwrap_or(0) + 1
    }

    pub fn slot_mut(&mut self, id: u32) -> Option<&mut FxSlot> {
        self.chain.iter_mut().find(|s| s.id == id)
    }
}

/// One effect in a chain.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FxSlot {
    /// Unique within its chain; lets the running effect keep its state when
    /// the chain is reordered or edited.
    pub id: u32,
    #[serde(default = "default_true")]
    pub on: bool,
    #[serde(flatten)]
    pub fx: Fx,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Fx {
    Gate(Gate),
    Eq { bands: Vec<EqBand> },
    Compressor(Compressor),
    Normalizer(Normalizer),
    Limiter(Limiter),
}

impl Fx {
    /// Every effect the "Add effect" list offers, with default settings.
    pub fn catalog() -> Vec<Fx> {
        vec![
            Fx::Gate(Gate::default()),
            Fx::Eq { bands: Vec::new() },
            Fx::Compressor(Compressor::default()),
            Fx::Normalizer(Normalizer::default()),
            Fx::Limiter(Limiter::default()),
        ]
    }

    pub fn name(&self) -> &'static str {
        match self {
            Fx::Gate(_) => "Noise gate",
            Fx::Eq { .. } => "EQ",
            Fx::Compressor(_) => "Compressor",
            Fx::Normalizer(_) => "Normalizer",
            Fx::Limiter(_) => "Limiter",
        }
    }
}

/// Silences the strip while it stays below `threshold_db` (room noise between words).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Gate {
    pub threshold_db: f32,
    pub release_ms: f32,
}

impl Default for Gate {
    fn default() -> Self {
        Self { threshold_db: -45.0, release_ms: 150.0 }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Compressor {
    pub threshold_db: f32,
    pub ratio: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub makeup_db: f32,
}

impl Default for Compressor {
    fn default() -> Self {
        Self { threshold_db: -20.0, ratio: 3.0, attack_ms: 10.0, release_ms: 150.0, makeup_db: 0.0 }
    }
}

/// Slowly steers the average loudness to `target_db` (RMS), by at most `max_gain_db`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Normalizer {
    pub target_db: f32,
    pub max_gain_db: f32,
}

impl Default for Normalizer {
    fn default() -> Self {
        Self { target_db: -18.0, max_gain_db: 12.0 }
    }
}

/// Keeps peaks under `ceiling_db`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Limiter {
    pub ceiling_db: f32,
}

impl Default for Limiter {
    fn default() -> Self {
        Self { ceiling_db: -1.0 }
    }
}

/// On-disk form of [`Effects`]: the chain, or — in profiles written before
/// chains existed — one fixed slot per effect, which is turned into a chain.
#[derive(Deserialize)]
struct EffectsFile {
    #[serde(default = "default_true")]
    on: bool,
    #[serde(default)]
    chain: Option<Vec<FxSlot>>,
    #[serde(default)]
    eq_on: bool,
    #[serde(default)]
    eq: Vec<EqBand>,
    #[serde(default)]
    gate: Option<OldSlot<Gate>>,
    #[serde(default)]
    comp: Option<OldSlot<Compressor>>,
    #[serde(default)]
    norm: Option<OldSlot<Normalizer>>,
    #[serde(default)]
    limit: Option<OldSlot<Limiter>>,
}

#[derive(Deserialize)]
struct OldSlot<T> {
    #[serde(default)]
    on: bool,
    #[serde(flatten)]
    params: T,
}

impl From<EffectsFile> for Effects {
    fn from(f: EffectsFile) -> Self {
        if let Some(chain) = f.chain {
            return Self { on: f.on, chain };
        }
        // Old fixed order: gate → EQ → compressor → normalizer → limiter.
        // Only what was in use carries over.
        let mut chain = Vec::new();
        let mut push = |on: bool, fx: Fx| chain.push(FxSlot { id: chain.len() as u32 + 1, on, fx });
        if let Some(g) = f.gate.filter(|g| g.on) {
            push(true, Fx::Gate(g.params));
        }
        if f.eq_on || !f.eq.is_empty() {
            push(f.eq_on, Fx::Eq { bands: f.eq });
        }
        if let Some(c) = f.comp.filter(|c| c.on) {
            push(true, Fx::Compressor(c.params));
        }
        if let Some(n) = f.norm.filter(|n| n.on) {
            push(true, Fx::Normalizer(n.params));
        }
        if let Some(l) = f.limit.filter(|l| l.on) {
            push(true, Fx::Limiter(l.params));
        }
        Self { on: true, chain }
    }
}

/// A saved effect chain.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FxPreset {
    pub name: String,
    pub chain: Vec<FxSlot>,
}

impl FxPreset {
    /// Chains that ship with Slidr.
    pub fn built_in() -> Vec<FxPreset> {
        use EqBandKind::*;
        let q = default_q();
        let b = |kind, freq, gain_db, q| EqBand { kind, freq, gain_db, q };
        let preset = |name: &str, fx: Vec<Fx>| FxPreset {
            name: name.into(),
            chain: fx.into_iter().enumerate().map(|(i, fx)| FxSlot { id: i as u32 + 1, on: true, fx }).collect(),
        };
        vec![
            preset("Voice – clean", vec![
                Fx::Gate(Gate { threshold_db: -45.0, release_ms: 150.0 }),
                Fx::Eq { bands: vec![b(HighPass, 80.0, 0.0, q), b(LowShelf, 200.0, -2.0, q), b(Peak, 3000.0, 3.0, 1.0)] },
                Fx::Compressor(Compressor { threshold_db: -20.0, ratio: 3.0, ..Compressor::default() }),
                Fx::Limiter(Limiter { ceiling_db: -1.0 }),
            ]),
            preset("Voice – broadcast", vec![
                Fx::Gate(Gate { threshold_db: -50.0, release_ms: 200.0 }),
                Fx::Eq { bands: vec![b(HighPass, 90.0, 0.0, q), b(Peak, 180.0, 2.5, 0.9), b(Peak, 4000.0, 3.0, 1.0), b(HighShelf, 10000.0, 2.0, q)] },
                Fx::Compressor(Compressor { threshold_db: -24.0, ratio: 4.0, attack_ms: 5.0, release_ms: 120.0, makeup_db: 4.0 }),
                Fx::Limiter(Limiter { ceiling_db: -1.0 }),
            ]),
            preset("Music – even loudness", vec![
                Fx::Normalizer(Normalizer { target_db: -18.0, max_gain_db: 12.0 }),
                Fx::Limiter(Limiter { ceiling_db: -1.0 }),
            ]),
            preset("Game – night mode", vec![
                Fx::Compressor(Compressor { threshold_db: -30.0, ratio: 4.0, attack_ms: 5.0, release_ms: 250.0, makeup_db: 6.0 }),
                Fx::Limiter(Limiter { ceiling_db: -3.0 }),
            ]),
            preset("Bass boost", vec![Fx::Eq { bands: vec![b(LowShelf, 120.0, 6.0, q)] }, Fx::Limiter(Limiter { ceiling_db: -1.0 })]),
            // Dialogue forward, impact kept: lifts the speech range, then a
            // gentle, slow compressor brings quiet lines up against loud scenes
            // without pumping the score.
            preset("Movies & series", vec![
                Fx::Eq { bands: vec![
                    b(LowShelf, 80.0, 3.0, q),
                    b(Peak, 300.0, -1.5, 0.8),
                    b(Peak, 2500.0, 3.5, 1.0),
                    b(HighShelf, 10000.0, 1.5, q),
                ] },
                Fx::Compressor(Compressor { threshold_db: -28.0, ratio: 2.5, attack_ms: 15.0, release_ms: 300.0, makeup_db: 4.0 }),
                Fx::Limiter(Limiter { ceiling_db: -1.0 }),
            ]),
            // An approximation of the consumer JBL voicing (not an official
            // profile): warm, forward bass, slightly recessed mids, crisp top.
            preset("JBL-style", vec![
                Fx::Eq { bands: vec![
                    b(LowShelf, 105.0, 6.0, q),
                    b(Peak, 60.0, 1.5, 1.2),
                    b(Peak, 1500.0, -1.5, 0.9),
                    b(HighShelf, 8000.0, 3.0, q),
                ] },
                Fx::Limiter(Limiter { ceiling_db: -1.0 }),
            ]),
        ]
    }
}

/// A starting point for a strip's EQ.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EqPreset {
    pub id: u32,
    pub name: String,
    #[serde(default)]
    pub bands: Vec<EqBand>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EqBandKind {
    Peak,
    LowShelf,
    HighShelf,
    HighPass,
    LowPass,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EqBand {
    pub kind: EqBandKind,
    /// Centre / corner frequency in Hz.
    pub freq: f32,
    /// Boost or cut in dB (ignored by the pass filters).
    #[serde(default)]
    pub gain_db: f32,
    #[serde(default = "default_q")]
    pub q: f32,
}

/// Mixer devices by friendly name. `None` = the system default for `monitor`
/// and `mic_input`; without `sink` nothing is rerouted, without `mic_out`
/// there is no Slidr Mic.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MixerDevices {
    /// Where the monitor mix plays (the user's headset).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor: Option<String>,
    /// The real microphone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mic_input: Option<String>,
    /// Output nobody listens to; rerouted programs play here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sink: Option<String>,
    /// Render side of the virtual cable that is the Slidr Mic (e.g. "CABLE Input").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mic_out: Option<String>,
}

fn default_level() -> f32 {
    1.0
}
fn default_q() -> f32 {
    std::f32::consts::FRAC_1_SQRT_2
}
