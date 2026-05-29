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

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ButtonCategory {
    pub id: u32,
    pub name: String,
    #[serde(default)]
    pub actions: Vec<ButtonAction>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SliderCategory {
    pub id: u32,
    pub name: String,
    #[serde(default)]
    pub streams: Vec<AudioStream>,
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
            SliderCategory { id: 1, name: "a".into(), streams: vec![] },
            SliderCategory { id: 5, name: "b".into(), streams: vec![] },
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
