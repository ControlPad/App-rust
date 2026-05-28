//! Slider value translation: raw ADC (1..=1024) → normalized [0,1] → curve-shaped [0,1].
//!
//! Matches the reference Bézier behavior including the 0.005 dead-zone snap-to-zero.

use serde::{Deserialize, Serialize};

pub const SLIDER_RAW_MAX: i32 = 1024;
pub const DEAD_ZONE_NORM: f32 = 0.005;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CurvePreset {
    Linear,
    Ease,
    EaseIn,
    EaseOut,
    EaseInOut,
    Custom,
}

impl Default for CurvePreset {
    fn default() -> Self {
        Self::Linear
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BezierPoints {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}

impl BezierPoints {
    pub const LINEAR: Self = Self { x1: 0.0, y1: 0.0, x2: 1.0, y2: 1.0 };

    pub fn for_preset(preset: CurvePreset, custom: Self) -> Self {
        match preset {
            CurvePreset::Linear => Self::LINEAR,
            CurvePreset::Ease => Self { x1: 0.25, y1: 0.1, x2: 0.25, y2: 1.0 },
            CurvePreset::EaseIn => Self { x1: 0.42, y1: 0.0, x2: 1.0, y2: 1.0 },
            CurvePreset::EaseOut => Self { x1: 0.0, y1: 0.0, x2: 0.58, y2: 1.0 },
            CurvePreset::EaseInOut => Self { x1: 0.42, y1: 0.0, x2: 0.58, y2: 1.0 },
            CurvePreset::Custom => custom,
        }
    }
}

impl Default for BezierPoints {
    fn default() -> Self {
        Self::LINEAR
    }
}

/// Normalize a raw slider reading (1..=1024) to [0,1].
pub fn normalize_raw(raw: i32) -> f32 {
    let v = (raw - 1).max(0) as f32 / 1022.0;
    v.clamp(0.0, 1.0)
}

/// Apply curve: dead-zone first, then cubic Bézier solved by bisection on x.
pub fn apply(norm: f32, pts: BezierPoints) -> f32 {
    if norm < DEAD_ZONE_NORM {
        return 0.0;
    }
    bezier_y_at_x(norm, pts).clamp(0.0, 1.0)
}

fn bezier_y_at_x(x: f32, p: BezierPoints) -> f32 {
    // P0=(0,0), P1=(x1,y1), P2=(x2,y2), P3=(1,1).
    let mut lo = 0.0f32;
    let mut hi = 1.0f32;
    let mut t = x; // initial guess
    for _ in 0..24 {
        let cx = cubic(t, 0.0, p.x1, p.x2, 1.0);
        let err = cx - x;
        if err.abs() < 1e-5 {
            break;
        }
        if err > 0.0 {
            hi = t;
        } else {
            lo = t;
        }
        t = 0.5 * (lo + hi);
    }
    cubic(t, 0.0, p.y1, p.y2, 1.0)
}

#[inline]
fn cubic(t: f32, a: f32, b: f32, c: f32, d: f32) -> f32 {
    let mt = 1.0 - t;
    mt * mt * mt * a + 3.0 * mt * mt * t * b + 3.0 * mt * t * t * c + t * t * t * d
}

/// Raw → final volume in one shot.
pub fn raw_to_volume(raw: i32, pts: BezierPoints) -> f32 {
    apply(normalize_raw(raw), pts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_endpoints() {
        assert_eq!(normalize_raw(1), 0.0);
        assert!((normalize_raw(1024) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn linear_passes_through() {
        for raw in [1, 100, 512, 1024] {
            let v = raw_to_volume(raw, BezierPoints::LINEAR);
            let expect = normalize_raw(raw);
            // Snapped to 0 for tiny values.
            let expected = if expect < DEAD_ZONE_NORM { 0.0 } else { expect };
            assert!((v - expected).abs() < 1e-3, "raw={raw} v={v} expected={expected}");
        }
    }

    #[test]
    fn dead_zone_snaps_to_zero() {
        assert_eq!(apply(0.0, BezierPoints::LINEAR), 0.0);
        assert_eq!(apply(0.004, BezierPoints::LINEAR), 0.0);
    }

    #[test]
    fn ease_in_below_diagonal() {
        let p = BezierPoints::for_preset(CurvePreset::EaseIn, BezierPoints::LINEAR);
        let v = apply(0.5, p);
        assert!(v < 0.5, "ease-in at 0.5 should be < 0.5, got {v}");
    }
}
