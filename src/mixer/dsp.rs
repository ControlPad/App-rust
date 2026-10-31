//! Signal processing for the mixer: EQ filters and gain ramps.
//!
//! Everything works on interleaved stereo `f32` blocks at [`RATE`]. Pure Rust
//! with no platform code, so it is unit-tested on any host.

use crate::model::{EqBand, EqBandKind};

/// Sample rate of the whole mixer.
pub const RATE: u32 = 48_000;
/// Frames per processing block (10 ms).
pub const BLOCK_FRAMES: usize = 480;
/// Samples per block (stereo, interleaved).
pub const BLOCK_SAMPLES: usize = BLOCK_FRAMES * 2;

/// Fader position [0, 1] → linear gain. Squared, so the travel feels like a
/// volume knob (half way ≈ −12 dB) rather than a switch at the top end.
pub fn fader_gain(level: f32) -> f32 {
    let l = level.clamp(0.0, 1.0);
    l * l
}

/// A value moving linearly to a target over a number of frames. Setting the
/// target it already heads for changes nothing, so an unrelated update can't
/// cut a running fade short.
#[derive(Debug, Clone, Copy)]
pub struct Fade {
    current: f32,
    target: f32,
    step: f32,
    left: u32,
}

impl Fade {
    pub fn new(v: f32) -> Self {
        Self { current: v, target: v, step: 0.0, left: 0 }
    }

    pub fn set_over(&mut self, target: f32, frames: u32) {
        if target == self.target {
            return;
        }
        self.target = target;
        self.left = frames.max(1);
        self.step = (target - self.current) / self.left as f32;
    }

    /// This frame's value, then advance.
    pub fn next(&mut self) -> f32 {
        let v = self.current;
        if self.left > 0 {
            self.left -= 1;
            self.current = if self.left == 0 { self.target } else { self.current + self.step };
        }
        v
    }

    /// Resting at `v` (no fade running).
    pub fn is_at(&self, v: f32) -> bool {
        self.left == 0 && self.current == v
    }
}

/// A gain applied while mixing: moves to a new value over a block (so fader
/// moves don't click) or longer for a switch (see `sfx`).
#[derive(Debug, Clone, Copy)]
pub struct Ramp(Fade);

impl Ramp {
    pub fn new(gain: f32) -> Self {
        Self(Fade::new(gain))
    }

    pub fn set_over(&mut self, gain: f32, frames: u32) {
        self.0.set_over(gain, frames);
    }

    /// `out += input * gain`, ramping the gain across the block.
    pub fn mix_into(&mut self, input: &[f32], out: &mut [f32]) {
        if self.0.is_at(0.0) {
            return;
        }
        for (i, o) in input.chunks_exact(2).zip(out.chunks_exact_mut(2)) {
            let g = self.0.next();
            o[0] += i[0] * g;
            o[1] += i[1] * g;
        }
    }

    /// Advance as if a block had been mixed (no input this block).
    pub fn skip(&mut self, frames: usize) {
        for _ in 0..frames {
            self.0.next();
        }
    }
}

/// One second-order section (RBJ "Audio EQ Cookbook"), separate state per channel.
#[derive(Debug, Clone, Copy)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    /// Direct form II transposed state, per channel.
    z: [[f32; 2]; 2],
}

impl Biquad {
    pub fn new(band: &EqBand) -> Self {
        let f0 = band.freq.clamp(10.0, RATE as f32 * 0.45);
        let q = band.q.max(0.05);
        let a = 10f32.powf(band.gain_db / 40.0);
        let w0 = 2.0 * std::f32::consts::PI * f0 / RATE as f32;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let (b0, b1, b2, a0, a1, a2) = match band.kind {
            EqBandKind::Peak => (
                1.0 + alpha * a,
                -2.0 * cos,
                1.0 - alpha * a,
                1.0 + alpha / a,
                -2.0 * cos,
                1.0 - alpha / a,
            ),
            EqBandKind::LowShelf => {
                let s = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) - (a - 1.0) * cos + s),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                    a * ((a + 1.0) - (a - 1.0) * cos - s),
                    (a + 1.0) + (a - 1.0) * cos + s,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                    (a + 1.0) + (a - 1.0) * cos - s,
                )
            }
            EqBandKind::HighShelf => {
                let s = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) + (a - 1.0) * cos + s),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                    a * ((a + 1.0) + (a - 1.0) * cos - s),
                    (a + 1.0) - (a - 1.0) * cos + s,
                    2.0 * ((a - 1.0) - (a + 1.0) * cos),
                    (a + 1.0) - (a - 1.0) * cos - s,
                )
            }
            EqBandKind::HighPass => (
                (1.0 + cos) / 2.0,
                -(1.0 + cos),
                (1.0 + cos) / 2.0,
                1.0 + alpha,
                -2.0 * cos,
                1.0 - alpha,
            ),
            EqBandKind::LowPass => (
                (1.0 - cos) / 2.0,
                1.0 - cos,
                (1.0 - cos) / 2.0,
                1.0 + alpha,
                -2.0 * cos,
                1.0 - alpha,
            ),
        };
        Self { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0, z: [[0.0; 2]; 2] }
    }

    /// Gain in dB this section applies at `freq`.
    pub fn response_db(&self, freq: f32) -> f32 {
        let w = 2.0 * std::f32::consts::PI * freq / RATE as f32;
        let (s1, c1) = w.sin_cos();
        let (s2, c2) = (2.0 * w).sin_cos();
        let num = (self.b0 + self.b1 * c1 + self.b2 * c2).powi(2) + (self.b1 * s1 + self.b2 * s2).powi(2);
        let den = (1.0 + self.a1 * c1 + self.a2 * c2).powi(2) + (self.a1 * s1 + self.a2 * s2).powi(2);
        10.0 * (num / den).log10()
    }

    /// Filter an interleaved stereo block in place.
    pub fn process(&mut self, buf: &mut [f32]) {
        for frame in buf.chunks_exact_mut(2) {
            for (ch, x) in frame.iter_mut().enumerate() {
                let z = &mut self.z[ch];
                let y = self.b0 * *x + z[0];
                z[0] = self.b1 * *x - self.a1 * y + z[1];
                z[1] = self.b2 * *x - self.a2 * y;
                *x = y;
            }
        }
    }
}

/// A chain of bands.
#[derive(Debug, Clone, Default)]
pub struct Eq {
    filters: Vec<Biquad>,
}

impl Eq {
    pub fn new(bands: &[EqBand]) -> Self {
        Self { filters: bands.iter().map(Biquad::new).collect() }
    }

    pub fn process(&mut self, buf: &mut [f32]) {
        for f in &mut self.filters {
            f.process(buf);
        }
    }

    /// Combined gain in dB at `freq` (what the editor draws).
    pub fn response_db(&self, freq: f32) -> f32 {
        self.filters.iter().map(|f| f.response_db(freq)).sum()
    }
}

/// Keep a bus inside [-1, 1] without the harsh edge of a hard clip: linear up
/// to 0.9, then a tanh knee into the remaining headroom.
pub fn soft_clip(buf: &mut [f32]) {
    const KNEE: f32 = 0.9;
    for s in buf {
        let a = s.abs();
        if a > KNEE {
            *s = s.signum() * (KNEE + (1.0 - KNEE) * ((a - KNEE) / (1.0 - KNEE)).tanh());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let v = (2.0 * std::f32::consts::PI * freq * i as f32 / RATE as f32).sin() * 0.5;
                [v, v]
            })
            .collect()
    }

    fn rms_db(buf: &[f32]) -> f32 {
        let ms = buf.iter().map(|s| s * s).sum::<f32>() / buf.len() as f32;
        10.0 * ms.log10()
    }

    /// Level change in dB the EQ applies to a steady sine (after settling).
    fn eq_gain_db(bands: &[EqBand], freq: f32) -> f32 {
        let input = sine(freq, RATE as usize / 2);
        let mut out = input.clone();
        Eq::new(bands).process(&mut out);
        let tail = input.len() / 2;
        rms_db(&out[tail..]) - rms_db(&input[tail..])
    }

    fn band(kind: EqBandKind, freq: f32, gain_db: f32) -> EqBand {
        EqBand { kind, freq, gain_db, q: std::f32::consts::FRAC_1_SQRT_2 }
    }

    #[test]
    fn peak_hits_its_gain_at_the_centre_and_leaves_far_bands_alone() {
        let b = [EqBand { q: 1.0, ..band(EqBandKind::Peak, 1000.0, 6.0) }];
        assert!((eq_gain_db(&b, 1000.0) - 6.0).abs() < 0.3);
        assert!(eq_gain_db(&b, 50.0).abs() < 0.3);
        assert!(eq_gain_db(&b, 15000.0).abs() < 0.5);
    }

    #[test]
    fn drawn_response_matches_what_the_filter_does() {
        let b = [EqBand { q: 1.0, ..band(EqBandKind::Peak, 1000.0, 6.0) }, band(EqBandKind::HighPass, 100.0, 0.0)];
        let eq = Eq::new(&b);
        for f in [40.0, 300.0, 1000.0, 5000.0] {
            assert!((eq.response_db(f) - eq_gain_db(&b, f)).abs() < 0.3, "{f} Hz");
        }
    }

    #[test]
    fn flat_eq_is_transparent() {
        let b = [band(EqBandKind::Peak, 1000.0, 0.0), band(EqBandKind::LowShelf, 200.0, 0.0)];
        for f in [60.0, 1000.0, 8000.0] {
            assert!(eq_gain_db(&b, f).abs() < 0.05, "{f} Hz");
        }
    }

    #[test]
    fn high_pass_removes_rumble_and_keeps_voice() {
        let b = [band(EqBandKind::HighPass, 100.0, 0.0)];
        assert!(eq_gain_db(&b, 20.0) < -20.0);
        assert!(eq_gain_db(&b, 1000.0).abs() < 0.2);
    }

    #[test]
    fn shelves_lift_their_side() {
        let low = [band(EqBandKind::LowShelf, 200.0, 6.0)];
        assert!((eq_gain_db(&low, 40.0) - 6.0).abs() < 0.5);
        assert!(eq_gain_db(&low, 5000.0).abs() < 0.3);
        let high = [band(EqBandKind::HighShelf, 4000.0, -6.0)];
        assert!((eq_gain_db(&high, 16000.0) + 6.0).abs() < 0.6);
        assert!(eq_gain_db(&high, 200.0).abs() < 0.3);
    }

    #[test]
    fn ramp_reaches_target_without_jumping() {
        let mut r = Ramp::new(0.0);
        r.set_over(1.0, BLOCK_FRAMES as u32);
        let input = vec![1.0; BLOCK_SAMPLES];
        let mut out = vec![0.0; BLOCK_SAMPLES];
        r.mix_into(&input, &mut out);
        assert_eq!(out[0], 0.0);
        assert!(out.windows(2).all(|w| w[1] >= w[0]));
        let mut out2 = vec![0.0; BLOCK_SAMPLES];
        r.mix_into(&input, &mut out2);
        assert!(out2.iter().all(|&s| (s - 1.0).abs() < 1e-6));
    }

    #[test]
    fn long_fade_spans_blocks_and_isnt_cut_short() {
        let mut f = Fade::new(1.0);
        f.set_over(0.0, 1000);
        for _ in 0..500 {
            f.next();
        }
        f.set_over(0.0, 10); // same target again: keeps going at the old pace
        let v = f.next();
        assert!((v - 0.5).abs() < 0.01, "{v}");
        for _ in 0..499 {
            f.next();
        }
        assert!(f.is_at(0.0));
    }

    #[test]
    fn fader_taper() {
        assert_eq!(fader_gain(0.0), 0.0);
        assert_eq!(fader_gain(1.0), 1.0);
        assert!((20.0 * fader_gain(0.5).log10() + 12.04).abs() < 0.1);
        assert_eq!(fader_gain(2.0), 1.0);
    }

    #[test]
    fn soft_clip_bounds_and_keeps_quiet_signals() {
        let mut b = vec![0.5, -0.5, 3.0, -3.0, 0.95];
        soft_clip(&mut b);
        assert_eq!(&b[..2], &[0.5, -0.5]);
        assert!(b[2] <= 1.0 && b[2] > 0.9 && b[3] >= -1.0);
        assert!(b[4] > 0.9 && b[4] < 0.95);
    }
}
