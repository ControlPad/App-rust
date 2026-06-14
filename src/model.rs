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
    /// When set, this slider entry fires an HTTP request (with the slider
    /// percent substituted for `{value}`) instead of controlling audio volume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<ApiCall>,
}

impl AudioStream {
    pub fn label(&self) -> String {
        if let Some(api) = &self.api {
            api.label()
        } else if let Some(p) = &self.process {
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
    /// HTTP request config, used when `kind == ApiCall`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<ApiCall>,
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
}
