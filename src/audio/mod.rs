//! Audio backend abstraction.
//!
//! Per-app and per-device volume / mute control across Windows (WASAPI) and
//! Linux (PulseAudio / PipeWire-Pulse). All calls are non-blocking from the
//! caller's perspective — slow lookups are cached by implementations.

#[cfg(target_os = "linux")]
pub mod pulse;
#[cfg(target_os = "windows")]
pub mod wasapi;
pub mod null;

#[derive(Debug, Clone, Copy)]
pub enum VolumeTarget<'a> {
    /// Per-process audio session.
    Process(&'a str),
    /// Microphone (capture endpoint) by friendly name.
    Mic(&'a str),
    /// Output endpoint; `None` = default.
    System(Option<&'a str>),
}

#[derive(Debug, Clone, Copy)]
pub enum MuteTarget<'a> {
    Process(&'a str),
    Mic(&'a str),
    System(Option<&'a str>),
}

pub trait AudioBackend: Send + Sync {
    /// Set volume in [0.0, 1.0].
    fn set_volume(&self, target: VolumeTarget<'_>, value: f32);
    fn set_mute(&self, target: MuteTarget<'_>, muted: bool);
    fn toggle_mute(&self, target: MuteTarget<'_>);
    fn is_muted(&self, target: MuteTarget<'_>) -> bool;

    fn list_processes(&self) -> Vec<String> {
        Vec::new()
    }
    fn list_mics(&self) -> Vec<String> {
        Vec::new()
    }
    fn list_outputs(&self) -> Vec<String> {
        Vec::new()
    }

    /// Advance the default output endpoint to the next device in `devices`
    /// (wrapping). An empty list cycles through *all* active output endpoints.
    /// No-op on backends that don't support switching the default device.
    fn cycle_output(&self, devices: &[String]) {
        let _ = devices;
    }
}

pub fn default_backend() -> Box<dyn AudioBackend> {
    #[cfg(target_os = "linux")]
    {
        match pulse::PulseBackend::new() {
            Ok(b) => return Box::new(b),
            Err(e) => log::warn!("PulseAudio backend unavailable ({e}); audio control disabled"),
        }
    }
    #[cfg(target_os = "windows")]
    {
        match wasapi::WasapiBackend::new() {
            Ok(b) => return Box::new(b),
            Err(e) => log::warn!("WASAPI backend unavailable ({e}); audio control disabled"),
        }
    }
    Box::new(null::NullBackend)
}
