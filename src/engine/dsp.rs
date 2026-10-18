//! Small realtime DSP building blocks. Nothing here allocates.

use std::f32::consts::PI;

/// Topology-preserving-transform state variable filter (Cytomic / Simper).
/// Stable under fast cutoff modulation, which makes it a good fit for filter knobs.
#[derive(Clone, Copy, Default)]
pub struct Svf {
    ic1: f32,
    ic2: f32,
    a1: f32,
    a2: f32,
    a3: f32,
    k: f32,
}

pub struct SvfOut {
    pub low: f32,
    pub high: f32,
}

impl Svf {
    pub fn set(&mut self, cutoff: f32, q: f32, sr: f32) {
        let fc = cutoff.clamp(10.0, sr * 0.49);
        let g = (PI * fc / sr).tan();
        self.k = 1.0 / q;
        self.a1 = 1.0 / (1.0 + g * (g + self.k));
        self.a2 = g * self.a1;
        self.a3 = g * self.a2;
    }

    #[inline]
    pub fn tick(&mut self, v0: f32) -> SvfOut {
        let v3 = v0 - self.ic2;
        let v1 = self.a1 * self.ic1 + self.a2 * v3;
        let v2 = self.ic2 + self.a2 * self.ic1 + self.a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        SvfOut {
            low: v2,
            high: v0 - self.k * v1 - v2,
        }
    }

    #[allow(dead_code)]
    pub fn reset(&mut self) {
        self.ic1 = 0.0;
        self.ic2 = 0.0;
    }
}

const BUTTERWORTH_Q: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// 4th-order Linkwitz-Riley split (two cascaded Butterworth SVFs per side).
#[derive(Clone, Copy, Default)]
struct Lr4 {
    lp: [Svf; 2],
    hp: [Svf; 2],
}

impl Lr4 {
    fn set(&mut self, fc: f32, sr: f32) {
        for f in self.lp.iter_mut().chain(self.hp.iter_mut()) {
            f.set(fc, BUTTERWORTH_Q, sr);
        }
    }

    #[inline]
    fn split(&mut self, x: f32) -> (f32, f32) {
        let l = self.lp[0].tick(x).low;
        let l = self.lp[1].tick(l).low;
        let h = self.hp[0].tick(x).high;
        let h = self.hp[1].tick(h).high;
        (l, h)
    }
}

/// DJ-mixer style 3-band isolator EQ for one audio channel.
#[derive(Clone, Copy, Default)]
pub struct Isolator {
    low_x: Lr4,
    high_x: Lr4,
}

pub const EQ_LOW_XOVER: f32 = 220.0;
pub const EQ_HIGH_XOVER: f32 = 2600.0;

impl Isolator {
    pub fn new(sr: f32) -> Self {
        let mut s = Self::default();
        s.low_x.set(EQ_LOW_XOVER, sr);
        s.high_x.set(EQ_HIGH_XOVER, sr);
        s
    }

    #[inline]
    pub fn tick(&mut self, x: f32, g_low: f32, g_mid: f32, g_high: f32) -> f32 {
        let (low, rest) = self.low_x.split(x);
        let (mid, high) = self.high_x.split(rest);
        low * g_low + mid * g_mid + high * g_high
    }
}

/// One-pole parameter smoother (for gains), tick per sample.
#[derive(Clone, Copy)]
pub struct Smooth {
    pub value: f32,
    coef: f32,
}

impl Smooth {
    pub fn new(value: f32, time_s: f32, sr: f32) -> Self {
        Self {
            value,
            coef: 1.0 - (-1.0 / (time_s * sr)).exp(),
        }
    }

    #[inline]
    pub fn tick(&mut self, target: f32) -> f32 {
        self.value += self.coef * (target - self.value);
        self.value
    }
}

pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// 4-point, 3rd-order Hermite interpolation.
#[inline]
pub fn hermite(xm1: f32, x0: f32, x1: f32, x2: f32, t: f32) -> f32 {
    let c = (x1 - xm1) * 0.5;
    let v = x0 - x1;
    let w = c + v;
    let a = w + v + (x2 - x0) * 0.5;
    let b = w + a;
    (((a * t) - b) * t + c) * t + x0
}

/// Gentle limiter so an overdriven master bends instead of hard-clipping.
#[inline]
pub fn soft_clip(x: f32) -> f32 {
    const KNEE: f32 = 0.85;
    let a = x.abs();
    if a <= KNEE {
        x
    } else {
        let over = (a - KNEE) / (1.0 - KNEE);
        let y = KNEE + (1.0 - KNEE) * over.tanh();
        y.copysign(x)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone_gain(freq: f32, gl: f32, gm: f32, gh: f32) -> f32 {
        let sr = 48000.0;
        let mut iso = Isolator::new(sr);
        let (mut ein, mut eout) = (0f64, 0f64);
        for i in 0..48000 {
            let x = (2.0 * PI * freq * i as f32 / sr).sin();
            let y = iso.tick(x, gl, gm, gh);
            if i > 24000 {
                ein += (x * x) as f64;
                eout += (y * y) as f64;
            }
        }
        (eout / ein).sqrt() as f32
    }

    #[test]
    fn isolator_is_flat_at_unity() {
        for f in [50.0, 220.0, 1000.0, 2600.0, 8000.0] {
            let g = tone_gain(f, 1.0, 1.0, 1.0);
            assert!((g - 1.0).abs() < 0.06, "{f} Hz -> {g}");
        }
    }

    #[test]
    fn isolator_kills_bands() {
        assert!(tone_gain(50.0, 0.0, 1.0, 1.0) < 0.05);
        assert!(tone_gain(800.0, 1.0, 0.0, 1.0) < 0.2);
        assert!(tone_gain(12000.0, 1.0, 1.0, 0.0) < 0.05);
    }

    #[test]
    fn hermite_hits_samples() {
        assert_eq!(hermite(0.0, 1.0, 2.0, 3.0, 0.0), 1.0);
        assert!((hermite(0.0, 1.0, 2.0, 3.0, 0.5) - 1.5).abs() < 1e-6);
    }
}
