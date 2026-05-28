//! No-op backend used when no platform implementation is available.

use super::{AudioBackend, MuteTarget, VolumeTarget};

pub struct NullBackend;

impl AudioBackend for NullBackend {
    fn set_volume(&self, target: VolumeTarget<'_>, value: f32) {
        log::debug!("null audio: set_volume({target:?}, {value})");
    }
    fn set_mute(&self, target: MuteTarget<'_>, muted: bool) {
        log::debug!("null audio: set_mute({target:?}, {muted})");
    }
    fn toggle_mute(&self, target: MuteTarget<'_>) {
        log::debug!("null audio: toggle_mute({target:?})");
    }
    fn is_muted(&self, _target: MuteTarget<'_>) -> bool {
        false
    }
}
