//! Hand-off between a capture thread and the mixer thread.
//!
//! The two sides run on different device clocks, so the fill level drifts. The
//! reader waits for a small cushion before it starts consuming, pads with
//! silence when the writer falls behind, and trims the backlog when it runs
//! ahead — a dropped or doubled sliver now and then instead of growing latency.

use std::collections::VecDeque;

use parking_lot::Mutex;

use super::dsp::BLOCK_SAMPLES;

/// Cushion the reader waits for before (re)starting: 20 ms.
const PRIME: usize = BLOCK_SAMPLES * 2;
/// Backlog above which the oldest audio is dropped back down to `PRIME`: 60 ms.
const HIGH: usize = BLOCK_SAMPLES * 6;
/// Hard cap while nobody reads.
const MAX: usize = BLOCK_SAMPLES * 20;

#[derive(Default)]
pub struct Ring {
    inner: Mutex<State>,
}

#[derive(Default)]
struct State {
    q: VecDeque<f32>,
    primed: bool,
}

impl Ring {
    pub fn push(&self, samples: &[f32]) {
        let mut s = self.inner.lock();
        s.q.extend(samples);
        let len = s.q.len();
        if len > MAX {
            s.q.drain(..len - MAX);
        }
    }

    /// Fill `out` with the next block. Returns `false` (and writes silence for
    /// the missing part) when there was not enough audio.
    pub fn pop(&self, out: &mut [f32]) -> bool {
        let mut s = self.inner.lock();
        if !s.primed {
            if s.q.len() < PRIME {
                out.fill(0.0);
                return false;
            }
            s.primed = true;
        }
        let len = s.q.len();
        if len > HIGH {
            s.q.drain(..len - PRIME);
        }
        let n = s.q.len().min(out.len());
        for (o, v) in out.iter_mut().zip(s.q.drain(..n)) {
            *o = v;
        }
        out[n..].fill(0.0);
        if n < out.len() {
            s.primed = false;
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_for_cushion_then_delivers_in_order() {
        let r = Ring::default();
        let mut out = vec![9.0; BLOCK_SAMPLES];
        r.push(&vec![1.0; BLOCK_SAMPLES]);
        assert!(!r.pop(&mut out));
        assert!(out.iter().all(|&s| s == 0.0));
        r.push(&vec![2.0; BLOCK_SAMPLES]);
        assert!(r.pop(&mut out));
        assert!(out.iter().all(|&s| s == 1.0));
        assert!(r.pop(&mut out));
        assert!(out.iter().all(|&s| s == 2.0));
    }

    #[test]
    fn underrun_pads_and_reprimes() {
        let r = Ring::default();
        r.push(&vec![1.0; PRIME + BLOCK_SAMPLES / 2]);
        let mut out = vec![0.0; BLOCK_SAMPLES];
        assert!(r.pop(&mut out));
        assert!(r.pop(&mut out));
        assert!(!r.pop(&mut out));
        assert_eq!(out[BLOCK_SAMPLES / 2], 0.0);
        r.push(&vec![1.0; BLOCK_SAMPLES]);
        assert!(!r.pop(&mut out), "must wait for the cushion again");
    }

    #[test]
    fn backlog_is_trimmed() {
        let r = Ring::default();
        r.push(&vec![0.0; HIGH * 2]);
        r.push(&vec![5.0; PRIME]);
        let mut out = vec![0.0; BLOCK_SAMPLES];
        assert!(r.pop(&mut out));
        assert!(out.iter().all(|&s| s == 5.0), "oldest audio dropped first");
    }
}
