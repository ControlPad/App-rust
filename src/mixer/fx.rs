//! Dynamics effects and the per-strip chain: noise gate → EQ → compressor →
//! normalizer → limiter. Stereo-linked (both channels get the same gain, so
//! the image doesn't wander), sample by sample on interleaved `f32` blocks.

use crate::model::{Compressor, Effects, EqBand, Gate, Limiter, Normalizer};

use super::dsp::{Eq, RATE};

fn db_to_lin(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

fn lin_to_db(x: f32) -> f32 {
    20.0 * x.max(1e-9).log10()
}

/// One-pole smoothing coefficient for a time constant in milliseconds.
fn coeff(ms: f32) -> f32 {
    (-1.0 / (ms.max(0.01) * 0.001 * RATE as f32)).exp()
}

/// Mutes the signal while it stays below a threshold — room noise between words.
struct GateFx {
    p: Gate,
    env: f32,
    gain: f32,
    hold: u32,
}

impl GateFx {
    /// How long the gate stays open after the signal drops (keeps word ends).
    const HOLD_MS: f32 = 60.0;
    /// Attenuation while closed (-80 dB: silent, but no hard zero).
    const FLOOR: f32 = 1e-4;

    fn new(p: Gate) -> Self {
        Self { p, env: 0.0, gain: Self::FLOOR, hold: 0 }
    }

    fn process(&mut self, buf: &mut [f32]) {
        let env_rel = coeff(20.0);
        let open_c = coeff(1.0);
        let close_c = coeff(self.p.release_ms);
        let threshold = db_to_lin(self.p.threshold_db);
        let hold = (Self::HOLD_MS * 0.001 * RATE as f32) as u32;
        for f in buf.chunks_exact_mut(2) {
            let level = f[0].abs().max(f[1].abs());
            self.env = if level > self.env { level } else { self.env * env_rel + level * (1.0 - env_rel) };
            if self.env > threshold {
                self.hold = hold;
            } else {
                self.hold = self.hold.saturating_sub(1);
            }
            let target = if self.hold > 0 { 1.0 } else { Self::FLOOR };
            let c = if target > self.gain { open_c } else { close_c };
            self.gain = target + (self.gain - target) * c;
            f[0] *= self.gain;
            f[1] *= self.gain;
        }
    }
}

/// Turns loud parts down above a threshold, by `ratio`.
struct CompFx {
    p: Compressor,
    env_db: f32,
}

impl CompFx {
    fn new(p: Compressor) -> Self {
        Self { p, env_db: -120.0 }
    }

    fn process(&mut self, buf: &mut [f32]) {
        let att = coeff(self.p.attack_ms);
        let rel = coeff(self.p.release_ms);
        let slope = 1.0 - 1.0 / self.p.ratio.max(1.0);
        for f in buf.chunks_exact_mut(2) {
            let level = lin_to_db(f[0].abs().max(f[1].abs()));
            let c = if level > self.env_db { att } else { rel };
            self.env_db = level + (self.env_db - level) * c;
            let over = self.env_db - self.p.threshold_db;
            let reduction = if over > 0.0 { over * slope } else { 0.0 };
            let g = db_to_lin(self.p.makeup_db - reduction);
            f[0] *= g;
            f[1] *= g;
        }
    }
}

/// Slowly steers the average loudness to a target — evens out a quiet video
/// next to a loud song. Never lifts silence.
struct NormFx {
    p: Normalizer,
    mean_sq: f32,
    gain_db: f32,
}

impl NormFx {
    /// Loudness is measured over ~3 s and the gain moves with ~1 s.
    const MEASURE_MS: f32 = 3000.0;
    const MOVE_MS: f32 = 1000.0;
    /// Below this the input counts as silence and the gain is left alone.
    const SILENCE_DB: f32 = -50.0;

    fn new(p: Normalizer) -> Self {
        Self { p, mean_sq: 0.0, gain_db: 0.0 }
    }

    fn process(&mut self, buf: &mut [f32]) {
        let m = coeff(Self::MEASURE_MS);
        let g = coeff(Self::MOVE_MS);
        let max = self.p.max_gain_db.max(0.0);
        for f in buf.chunks_exact_mut(2) {
            let sq = (f[0] * f[0] + f[1] * f[1]) * 0.5;
            self.mean_sq = sq + (self.mean_sq - sq) * m;
            let rms_db = 10.0 * self.mean_sq.max(1e-12).log10();
            if rms_db > Self::SILENCE_DB {
                let want = (self.p.target_db - rms_db).clamp(-max, max);
                self.gain_db = want + (self.gain_db - want) * g;
            }
            let lin = db_to_lin(self.gain_db);
            f[0] *= lin;
            f[1] *= lin;
        }
    }
}

/// Keeps peaks under a ceiling: clamps instantly, recovers over 50 ms.
struct LimitFx {
    p: Limiter,
    gain: f32,
}

impl LimitFx {
    fn new(p: Limiter) -> Self {
        Self { p, gain: 1.0 }
    }

    fn process(&mut self, buf: &mut [f32]) {
        let ceiling = db_to_lin(self.p.ceiling_db.min(0.0));
        let rel = coeff(50.0);
        for f in buf.chunks_exact_mut(2) {
            let peak = f[0].abs().max(f[1].abs());
            self.gain = 1.0 + (self.gain - 1.0) * rel;
            if peak * self.gain > ceiling {
                self.gain = ceiling / peak;
            }
            f[0] = (f[0] * self.gain).clamp(-ceiling, ceiling);
            f[1] = (f[1] * self.gain).clamp(-ceiling, ceiling);
        }
    }
}

/// All effects of one strip. Parameter changes keep each effect's running
/// state (envelopes, gains), so turning a knob doesn't click.
#[derive(Default)]
pub struct FxChain {
    gate: Option<GateFx>,
    bands: Vec<EqBand>,
    eq: Option<Eq>,
    comp: Option<CompFx>,
    norm: Option<NormFx>,
    limit: Option<LimitFx>,
}

impl FxChain {
    pub fn update(&mut self, fx: &Effects) {
        fn slot<T, P: Copy>(s: &mut Option<T>, on: bool, p: P, new: fn(P) -> T, set: fn(&mut T, P)) {
            match (on, s.as_mut()) {
                (false, _) => *s = None,
                (true, Some(x)) => set(x, p),
                (true, None) => *s = Some(new(p)),
            }
        }
        slot(&mut self.gate, fx.gate.on, fx.gate, GateFx::new, |x, p| x.p = p);
        slot(&mut self.comp, fx.comp.on, fx.comp, CompFx::new, |x, p| x.p = p);
        slot(&mut self.norm, fx.norm.on, fx.norm, NormFx::new, |x, p| x.p = p);
        slot(&mut self.limit, fx.limit.on, fx.limit, LimitFx::new, |x, p| x.p = p);
        if !fx.eq_on {
            self.eq = None;
        } else if self.eq.is_none() || fx.eq != self.bands {
            self.eq = Some(Eq::new(&fx.eq));
        }
        self.bands = fx.eq.clone();
    }

    pub fn process(&mut self, buf: &mut [f32]) {
        if let Some(x) = &mut self.gate {
            x.process(buf);
        }
        if let Some(x) = &mut self.eq {
            x.process(buf);
        }
        if let Some(x) = &mut self.comp {
            x.process(buf);
        }
        if let Some(x) = &mut self.norm {
            x.process(buf);
        }
        if let Some(x) = &mut self.limit {
            x.process(buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(amp: f32, secs: f32) -> Vec<f32> {
        (0..(RATE as f32 * secs) as usize)
            .flat_map(|i| {
                let v = (2.0 * std::f32::consts::PI * 440.0 * i as f32 / RATE as f32).sin() * amp;
                [v, v]
            })
            .collect()
    }

    fn peak_db(b: &[f32]) -> f32 {
        lin_to_db(b.iter().fold(0f32, |a, s| a.max(s.abs())))
    }

    fn rms_db(b: &[f32]) -> f32 {
        10.0 * (b.iter().map(|s| s * s).sum::<f32>() / b.len() as f32).log10()
    }

    fn chain(fx: Effects) -> FxChain {
        let mut c = FxChain::default();
        c.update(&fx);
        c
    }

    /// Run `secs` of a sine through the chain; return the last quarter.
    fn run(c: &mut FxChain, amp: f32, secs: f32) -> Vec<f32> {
        let mut b = sine(amp, secs);
        for block in b.chunks_mut(960) {
            c.process(block);
        }
        b[b.len() * 3 / 4..].to_vec()
    }

    #[test]
    fn gate_silences_noise_and_passes_speech() {
        let mut c = chain(Effects { gate: Gate { on: true, threshold_db: -40.0, release_ms: 100.0 }, ..Default::default() });
        assert!(peak_db(&run(&mut c, 0.003, 1.0)) < -100.0, "-50 dB hum is gated");
        assert!((peak_db(&run(&mut c, 0.3, 1.0)) - lin_to_db(0.3)).abs() < 0.1, "speech level passes");
    }

    #[test]
    fn compressor_reduces_by_its_ratio_above_threshold() {
        let comp = Compressor { on: true, threshold_db: -20.0, ratio: 4.0, attack_ms: 5.0, release_ms: 200.0, makeup_db: 0.0 };
        let mut c = chain(Effects { comp, ..Default::default() });
        // -6 dBFS is 14 dB over: 14 * (1 - 1/4) = 10.5 dB less.
        let out = peak_db(&run(&mut c, 0.5, 1.0));
        assert!((out - (-6.02 - 10.5)).abs() < 1.5, "{out}");
        // Below the threshold nothing changes.
        let mut c = chain(Effects { comp, ..Default::default() });
        assert!((peak_db(&run(&mut c, 0.05, 1.0)) - lin_to_db(0.05)).abs() < 0.1);
    }

    #[test]
    fn normalizer_brings_quiet_audio_to_target_and_ignores_silence() {
        let norm = Normalizer { on: true, target_db: -18.0, max_gain_db: 15.0 };
        let mut c = chain(Effects { norm, ..Default::default() });
        // Sine at 0.1 peak = -23 dB RMS; should end near -18.
        let out = rms_db(&run(&mut c, 0.1, 12.0));
        assert!((out + 18.0).abs() < 1.0, "{out}");
        let mut c = chain(Effects { norm, ..Default::default() });
        assert!(peak_db(&run(&mut c, 0.0005, 5.0)) < -60.0, "silence is not lifted");
    }

    #[test]
    fn limiter_holds_the_ceiling() {
        let mut c = chain(Effects { limit: Limiter { on: true, ceiling_db: -3.0 }, ..Default::default() });
        let out = run(&mut c, 0.95, 1.0);
        assert!(peak_db(&out) <= -3.0 + 0.01);
        assert!(peak_db(&out) > -4.0, "doesn't squash more than needed");
    }

    #[test]
    fn off_chain_is_transparent_and_updates_keep_state() {
        let mut c = chain(Effects::default());
        let input = sine(0.4, 0.1);
        let mut out = input.clone();
        c.process(&mut out);
        assert_eq!(input, out);
        let comp = Compressor { on: true, ..Default::default() };
        c.update(&Effects { comp, ..Default::default() });
        run(&mut c, 0.9, 0.5);
        let env = c.comp.as_ref().unwrap().env_db;
        c.update(&Effects { comp: Compressor { ratio: 6.0, ..comp }, ..Default::default() });
        assert_eq!(c.comp.as_ref().unwrap().env_db, env, "a knob turn keeps the envelope");
    }
}
