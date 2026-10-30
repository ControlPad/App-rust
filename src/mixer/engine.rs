//! The mix itself: strips, levels, EQ and the two buses. No I/O — the platform
//! layer feeds one block per channel (and the voice) and plays the results.

use std::collections::HashMap;

use crate::model::{MixTarget, MixerConfig};

use super::dsp::{fader_gain, soft_clip, Ramp, BLOCK_SAMPLES};
use super::fx::FxChain;

struct Strip {
    level: f32,
    muted: bool,
    fx: FxChain,
    /// Gain onto the monitor bus.
    monitor: Ramp,
    /// Gain onto the mic bus.
    send: Ramp,
    /// Share sent to the mic, `None` = not sent. Unused for the voice.
    mic_send: Option<f32>,
    /// Whether the strip is heard on the monitor. For channels this is set by
    /// the platform layer: only rerouted programs may be played again, or the
    /// user hears them twice. For the voice it is the "hear yourself" switch.
    to_monitor: bool,
}

impl Strip {
    fn new() -> Self {
        Self {
            level: 1.0,
            muted: false,
            fx: FxChain::default(),
            monitor: Ramp::new(0.0),
            send: Ramp::new(0.0),
            mic_send: None,
            to_monitor: false,
        }
    }


    fn update_gains(&mut self, is_voice: bool) {
        let live = if self.muted { 0.0 } else { 1.0 };
        let level = fader_gain(self.level) * live;
        self.monitor.set(if self.to_monitor { level } else { 0.0 });
        self.send.set(if is_voice { level } else { self.mic_send.map_or(0.0, fader_gain) * live });
    }
}

pub struct Engine {
    channels: HashMap<u32, Strip>,
    voice: Strip,
    scratch: Vec<f32>,
}

impl Default for Engine {
    fn default() -> Self {
        Self { channels: HashMap::new(), voice: Strip::new(), scratch: vec![0.0; BLOCK_SAMPLES] }
    }
}

impl Engine {
    /// Adopt a (new) layout. Strips that already exist keep their ramps and
    /// filter state, so editing one channel doesn't click the others.
    pub fn configure(&mut self, cfg: &MixerConfig) {
        self.channels.retain(|id, _| cfg.channels.iter().any(|c| c.id == *id));
        for c in &cfg.channels {
            let s = self.channels.entry(c.id).or_insert_with(Strip::new);
            s.level = c.level;
            s.muted = c.muted;
            s.mic_send = c.mic_send;
            s.fx.update(&c.fx);
            s.update_gains(false);
        }
        let v = &mut self.voice;
        v.level = cfg.voice.level;
        v.muted = cfg.voice.muted;
        v.to_monitor = cfg.voice.to_monitor;
        v.fx.update(&cfg.voice.fx);
        v.update_gains(true);
    }

    /// Whether a channel's programs were moved to the sink (see `Strip::to_monitor`).
    pub fn set_monitored(&mut self, channel: u32, on: bool) {
        if let Some(s) = self.channels.get_mut(&channel) {
            s.to_monitor = on;
            s.update_gains(false);
        }
    }

    pub fn set_level(&mut self, target: MixTarget, value: f32) {
        let value = value.clamp(0.0, 1.0);
        match target {
            MixTarget::Voice => {
                self.voice.level = value;
                self.voice.update_gains(true);
            }
            MixTarget::Channel(id) => {
                if let Some(s) = self.channels.get_mut(&id) {
                    s.level = value;
                    s.update_gains(false);
                }
            }
            MixTarget::Send(id) => {
                if let Some(s) = self.channels.get_mut(&id) {
                    s.mic_send = Some(value);
                    s.update_gains(false);
                }
            }
        }
    }

    /// Mute state of a strip; a send shares its channel's mute.
    pub fn muted(&self, target: MixTarget) -> Option<bool> {
        match target {
            MixTarget::Voice => Some(self.voice.muted),
            MixTarget::Channel(id) | MixTarget::Send(id) => self.channels.get(&id).map(|s| s.muted),
        }
    }

    pub fn set_mute(&mut self, target: MixTarget, muted: bool) {
        match target {
            MixTarget::Voice => {
                self.voice.muted = muted;
                self.voice.update_gains(true);
            }
            MixTarget::Channel(id) | MixTarget::Send(id) => {
                if let Some(s) = self.channels.get_mut(&id) {
                    s.muted = muted;
                    s.update_gains(false);
                }
            }
        }
    }

    /// Mix one block. `channels` holds each channel's summed input (consumed:
    /// EQ runs in place); `voice` is the real mic. Both buses are overwritten.
    pub fn process(
        &mut self,
        channels: &mut HashMap<u32, Vec<f32>>,
        voice: Option<&mut [f32]>,
        monitor: &mut [f32],
        mic: &mut [f32],
    ) {
        monitor.fill(0.0);
        mic.fill(0.0);
        for (id, strip) in &mut self.channels {
            match channels.get_mut(id) {
                Some(buf) => {
                    strip.fx.process(buf);
                    strip.monitor.mix_into(buf, monitor);
                    strip.send.mix_into(buf, mic);
                }
                None => {
                    // No input this block: still let the ramps arrive.
                    self.scratch.fill(0.0);
                    strip.monitor.mix_into(&self.scratch, monitor);
                    strip.send.mix_into(&self.scratch, mic);
                }
            }
        }
        if let Some(v) = voice {
            self.voice.fx.process(v);
            self.voice.monitor.mix_into(v, monitor);
            self.voice.send.mix_into(v, mic);
        }
        soft_clip(monitor);
        soft_clip(mic);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Effects, MixChannel, VoiceStrip};

    fn channel(id: u32, mic_send: Option<f32>) -> MixChannel {
        MixChannel {
            id,
            name: format!("c{id}"),
            programs: vec![],
            level: 1.0,
            muted: false,
            fx: Effects::default(),
            reroute: true,
            mic_send,
        }
    }

    fn engine(channels: Vec<MixChannel>, voice: VoiceStrip) -> Engine {
        let mut e = Engine::default();
        e.configure(&MixerConfig { enabled: true, channels, voice, eq_presets: vec![] });
        e
    }

    /// Run two blocks (the first ramps) and return the second's bus peaks.
    fn run(e: &mut Engine, inputs: &[u32], voice: bool) -> (f32, f32) {
        let mut out = (0.0, 0.0);
        for _ in 0..2 {
            let mut ch: HashMap<u32, Vec<f32>> =
                inputs.iter().map(|id| (*id, vec![0.25; BLOCK_SAMPLES])).collect();
            let mut v = vec![0.25; BLOCK_SAMPLES];
            let (mut m, mut k) = (vec![0.0; BLOCK_SAMPLES], vec![0.0; BLOCK_SAMPLES]);
            e.process(&mut ch, voice.then_some(&mut v[..]), &mut m, &mut k);
            let peak = |b: &[f32]| b.iter().fold(0f32, |a, s| a.max(s.abs()));
            out = (peak(&m), peak(&k));
        }
        out
    }

    #[test]
    fn channel_reaches_monitor_only_when_rerouted() {
        let mut e = engine(vec![channel(1, None)], VoiceStrip::default());
        assert_eq!(run(&mut e, &[1], false), (0.0, 0.0));
        e.set_monitored(1, true);
        assert_eq!(run(&mut e, &[1], false), (0.25, 0.0));
    }

    #[test]
    fn send_is_independent_of_the_fader_and_mute_silences_both() {
        let mut e = engine(vec![channel(1, Some(1.0))], VoiceStrip::default());
        e.set_monitored(1, true);
        e.set_level(MixTarget::Channel(1), 0.0);
        assert_eq!(run(&mut e, &[1], false), (0.0, 0.25));
        e.set_mute(MixTarget::Send(1), true);
        assert_eq!(run(&mut e, &[1], false), (0.0, 0.0));
        assert_eq!(e.muted(MixTarget::Channel(1)), Some(true));
    }

    #[test]
    fn voice_goes_to_the_mic_and_follows_its_fader() {
        let mut e = engine(vec![], VoiceStrip::default());
        assert_eq!(run(&mut e, &[], true), (0.0, 0.25));
        e.set_level(MixTarget::Voice, 0.5);
        let (_, mic) = run(&mut e, &[], true);
        assert!((mic - 0.0625).abs() < 1e-6);
        e.set_mute(MixTarget::Voice, true);
        assert_eq!(run(&mut e, &[], true), (0.0, 0.0));
    }

    #[test]
    fn hear_yourself_puts_the_voice_on_the_monitor() {
        let mut e = engine(vec![], VoiceStrip { to_monitor: true, ..VoiceStrip::default() });
        assert_eq!(run(&mut e, &[], true), (0.25, 0.25));
    }

    #[test]
    fn reconfigure_keeps_live_channels_and_drops_removed_ones() {
        let mut e = engine(vec![channel(1, Some(1.0)), channel(2, Some(1.0))], VoiceStrip::default());
        e.configure(&MixerConfig {
            enabled: true,
            channels: vec![channel(2, Some(1.0))],
            voice: VoiceStrip::default(),
            eq_presets: vec![],
        });
        assert_eq!(e.muted(MixTarget::Channel(1)), None);
        assert_eq!(run(&mut e, &[1, 2], false), (0.0, 0.25));
    }
}
