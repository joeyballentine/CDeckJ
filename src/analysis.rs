//! Offline analysis run once per loaded track: colour waveform summary and BPM / beat grid.

use crate::track::{WaveBin, WAVE_BINS_PER_SEC};

/// One-pole low-pass coefficient for a cutoff frequency.
fn lp_coef(fc: f32, sr: f32) -> f32 {
    1.0 - (-2.0 * std::f32::consts::PI * fc / sr).exp()
}

/// Streaming 3-band waveform summary, fed frame by frame while a track decodes.
///
/// Each bin stores the absolute peak plus the *relative* strength of low/mid/high, so no
/// whole-track normalisation pass is needed and bins can be shown as soon as they exist.
pub struct WaveBuilder {
    bin_len: f64,
    a_lo: f32,
    a_hi: f32,
    lo1: f32,
    lo2: f32,
    hi1: f32,
    acc: [f32; 5],
    frame: u64,
    next_edge: f64,
}

impl WaveBuilder {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let bin_len = (sample_rate as f64 / WAVE_BINS_PER_SEC).max(1.0);
        Self {
            bin_len,
            a_lo: lp_coef(200.0, sr),
            a_hi: lp_coef(2500.0, sr),
            lo1: 0.0,
            lo2: 0.0,
            hi1: 0.0,
            acc: [0.0; 5],
            frame: 0,
            next_edge: bin_len,
        }
    }

    fn bin(&mut self) -> WaveBin {
        let r = &mut self.acc;
        let n = r[4].max(1.0);
        let (lo, mid, hi) = ((r[1] / n).sqrt(), (r[2] / n).sqrt(), (r[3] / n).sqrt());
        // Highs and mids carry less energy than the kick: weight them so they still show.
        let (lo, mid, hi) = (lo, mid * 1.6, hi * 3.0);
        let m = lo.max(mid).max(hi).max(1e-9);
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0) as u8;
        let b = WaveBin { peak: q(r[0]), low: q(lo / m), mid: q(mid / m), high: q(hi / m) };
        *r = [0.0; 5];
        b
    }

    #[inline]
    pub fn push(&mut self, l: f32, r: f32, out: &mut Vec<WaveBin>) {
        let x = 0.5 * (l + r);
        self.lo1 += self.a_lo * (x - self.lo1);
        self.lo2 += self.a_lo * (self.lo1 - self.lo2);
        self.hi1 += self.a_hi * (x - self.hi1);
        let low = self.lo2;
        let high = x - self.hi1;
        let mid = self.hi1 - self.lo2;
        let a = &mut self.acc;
        a[0] = a[0].max(l.abs().max(r.abs()));
        a[1] += low * low;
        a[2] += mid * mid;
        a[3] += high * high;
        a[4] += 1.0;
        self.frame += 1;
        if self.frame as f64 >= self.next_edge {
            self.next_edge += self.bin_len;
            out.push(self.bin());
        }
    }

    /// Emit the last partial bin.
    pub fn finish(&mut self, out: &mut Vec<WaveBin>) {
        if self.acc[4] > 0.0 {
            out.push(self.bin());
        }
    }
}

/// Whole-buffer convenience wrapper (tests, small tracks).
pub fn waveform(samples: &[f32], sample_rate: u32) -> Vec<WaveBin> {
    let mut b = WaveBuilder::new(sample_rate);
    let mut out = Vec::with_capacity((samples.len() / 2) / (sample_rate as usize / 150).max(1) + 1);
    for f in samples.chunks_exact(2) {
        b.push(f[0], f[1], &mut out);
    }
    b.finish(&mut out);
    out
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BeatGrid {
    pub bpm: f64,
    /// First beat position in frames, in `[0, beat_len)`.
    pub first_beat: f64,
}

const ENV_RATE: f64 = 200.0; // onset envelope samples per second

/// Onset-strength envelope: rectified log-energy rise of a kick band and a hat band.
fn onset_envelope(mono: &[f32], sample_rate: u32) -> (Vec<f32>, f64) {
    let sr = sample_rate as f32;
    let hop = sample_rate as f64 / ENV_RATE;
    let frames = mono.len();
    let n = (frames as f64 / hop) as usize;
    let a_lo = lp_coef(150.0, sr);
    let a_hi = lp_coef(4000.0, sr);
    let (mut l1, mut l2, mut h1) = (0f32, 0f32, 0f32);
    let mut e_lo = vec![0f32; n];
    let mut e_hi = vec![0f32; n];
    for (i, &x) in mono.iter().enumerate() {
        l1 += a_lo * (x - l1);
        l2 += a_lo * (l1 - l2);
        h1 += a_hi * (x - h1);
        let hp = x - h1;
        let b = (i as f64 / hop) as usize;
        if b < n {
            e_lo[b] += l2 * l2;
            e_hi[b] += hp * hp;
        }
    }
    let mut onset = vec![0f32; n];
    for t in 1..n {
        let dl = (1.0 + 1000.0 * e_lo[t]).ln() - (1.0 + 1000.0 * e_lo[t - 1]).ln();
        let dh = (1.0 + 1000.0 * e_hi[t]).ln() - (1.0 + 1000.0 * e_hi[t - 1]).ln();
        onset[t] = dl.max(0.0) + 0.5 * dh.max(0.0);
    }
    // Remove slow trend (moving average over ~0.5 s) so loud sections don't dominate.
    let w = (ENV_RATE * 0.25) as usize;
    let mut prefix = vec![0f64; n + 1];
    for t in 0..n {
        prefix[t + 1] = prefix[t] + onset[t] as f64;
    }
    let out = (0..n)
        .map(|t| {
            let a = t.saturating_sub(w);
            let b = (t + w + 1).min(n);
            let mean = (prefix[b] - prefix[a]) / (b - a) as f64;
            (onset[t] as f64 - mean).max(0.0) as f32
        })
        .collect();
    (out, hop)
}

fn autocorr(env: &[f32], lag: f64) -> f64 {
    let l0 = lag.floor() as usize;
    let f = lag - l0 as f64;
    if l0 + 1 >= env.len() {
        return 0.0;
    }
    let mut s0 = 0f64;
    let mut s1 = 0f64;
    for t in 0..env.len() - l0 - 1 {
        s0 += (env[t] * env[t + l0]) as f64;
        s1 += (env[t] * env[t + l0 + 1]) as f64;
    }
    s0 * (1.0 - f) + s1 * f
}

const PHASE_BINS: usize = 96;

/// Fold the envelope at a period and return (peakiness score, peak phase in envelope samples).
fn comb_fold(env: &[f32], period: f64) -> (f64, f64) {
    let mut hist = [0f64; PHASE_BINS];
    let inv = PHASE_BINS as f64 / period;
    for (t, &v) in env.iter().enumerate() {
        if v > 0.0 {
            let ph = (t as f64 % period) * inv;
            hist[(ph as usize).min(PHASE_BINS - 1)] += v as f64;
        }
    }
    // Light circular smoothing.
    let mut sm = [0f64; PHASE_BINS];
    for i in 0..PHASE_BINS {
        let p = hist[(i + PHASE_BINS - 1) % PHASE_BINS];
        let q = hist[(i + 1) % PHASE_BINS];
        sm[i] = 0.25 * p + 0.5 * hist[i] + 0.25 * q;
    }
    let (imax, &vmax) = sm
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .unwrap();
    let mean = sm.iter().sum::<f64>() / PHASE_BINS as f64;
    // Parabolic peak refinement.
    let y0 = sm[(imax + PHASE_BINS - 1) % PHASE_BINS];
    let y2 = sm[(imax + 1) % PHASE_BINS];
    let denom = y0 - 2.0 * vmax + y2;
    let delta = if denom.abs() > 1e-12 { 0.5 * (y0 - y2) / denom } else { 0.0 };
    let phase_bin = imax as f64 + 0.5 + delta;
    (vmax / mean.max(1e-12), phase_bin / inv)
}

/// Estimate a constant-tempo beat grid from interleaved stereo. `None` for beatless material.
#[allow(dead_code)]
pub fn detect_beat_grid(samples: &[f32], sample_rate: u32) -> Option<BeatGrid> {
    let mono: Vec<f32> = samples.chunks_exact(2).map(|f| 0.5 * (f[0] + f[1])).collect();
    detect_beat_grid_mono(&mono, sample_rate)
}

/// Fold a tempo into `[lo, hi]` by octaves (rekordbox-style BPM range), if it can be.
pub fn fold_bpm(bpm: f64, lo: f64, hi: f64) -> f64 {
    let mut b = bpm;
    while b < lo && b * 2.0 <= hi {
        b *= 2.0;
    }
    while b > hi && b / 2.0 >= lo {
        b /= 2.0;
    }
    b
}

pub fn detect_beat_grid_mono(mono: &[f32], sample_rate: u32) -> Option<BeatGrid> {
    let (env, hop) = onset_envelope(mono, sample_rate);
    if env.len() < (ENV_RATE * 8.0) as usize {
        return None; // need at least ~8 s
    }

    // 1. Coarse tempo from autocorrelation with a mild preference for ~125 BPM.
    let mut best = (0.0f64, 0.0f64); // (score, bpm)
    let mut b = 70.0;
    while b <= 180.0 {
        let lag = 60.0 * ENV_RATE / b;
        let r = autocorr(&env, lag) + 0.5 * autocorr(&env, lag * 2.0);
        let prior = (-0.5 * ((b / 125.0f64).log2() / 0.9).powi(2)).exp();
        let s = r * prior;
        if s > best.0 {
            best = (s, b);
        }
        b += 0.5;
    }
    if best.0 <= 0.0 {
        return None;
    }

    // 2. Fine tempo + phase by folding the whole track at candidate periods.
    let eval = |bpm: f64| comb_fold(&env, 60.0 * ENV_RATE / bpm);
    let mut fine = (0.0f64, best.1, 0.0f64); // (score, bpm, phase)
    let mut b = best.1 - 1.0;
    while b <= best.1 + 1.0 {
        let (s, ph) = eval(b);
        if s > fine.0 {
            fine = (s, b, ph);
        }
        b += 0.01;
    }
    let (mut score, mut bpm, mut phase) = fine;

    // 3. Most dance music sits on whole (or half) BPM values: snap when it costs nothing.
    for snap in [bpm.round(), (bpm * 2.0).round() / 2.0] {
        if (snap - bpm).abs() < 0.08 && (snap - bpm).abs() > 1e-9 {
            let (s, ph) = eval(snap);
            if s >= score * 0.97 {
                score = s;
                bpm = snap;
                phase = ph;
                break;
            }
        }
    }

    if score < 1.5 {
        return None;
    }
    let beat_len = sample_rate as f64 * 60.0 / bpm;
    let mut first = phase * hop;
    first = first.rem_euclid(beat_len);
    Some(BeatGrid { bpm, first_beat: first })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic house loop: kick on every beat, hats on the off-beat.
    pub fn click_track(bpm: f64, offset_s: f64, secs: f64, sr: u32) -> Vec<f32> {
        let n = (secs * sr as f64) as usize;
        let mut out = vec![0f32; n * 2];
        let beat = 60.0 / bpm;
        let mut t0 = offset_s;
        let mut k = 0;
        while t0 < secs {
            let start = (t0 * sr as f64) as usize;
            for i in 0..(0.15 * sr as f64) as usize {
                let idx = start + i;
                if idx >= n {
                    break;
                }
                let t = i as f64 / sr as f64;
                let f = 50.0 + 120.0 * (-t * 40.0).exp();
                let v = (2.0 * std::f64::consts::PI * f * t).sin() * (-t * 18.0).exp() * 0.8;
                out[2 * idx] += v as f32;
                out[2 * idx + 1] += v as f32;
            }
            // off-beat hat (pseudo-random noise burst)
            let hs = ((t0 + beat / 2.0) * sr as f64) as usize;
            let mut seed = 12345u32.wrapping_add(k);
            for i in 0..(0.03 * sr as f64) as usize {
                let idx = hs + i;
                if idx >= n {
                    break;
                }
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let r = (seed >> 9) as f32 / (1u32 << 23) as f32 * 2.0 - 1.0;
                let v = r * 0.2 * (-(i as f32) / (0.008 * sr as f32)).exp();
                out[2 * idx] += v;
                out[2 * idx + 1] += v;
            }
            t0 += beat;
            k += 1;
        }
        out
    }

    fn check(bpm: f64, offset: f64) {
        let sr = 44100;
        let s = click_track(bpm, offset, 60.0, sr);
        let g = detect_beat_grid(&s, sr).expect("grid");
        assert!((g.bpm - bpm).abs() < 0.02, "bpm {} vs {}", g.bpm, bpm);
        let bl = sr as f64 * 60.0 / bpm;
        let expect = (offset * sr as f64).rem_euclid(bl);
        let mut err = (g.first_beat - expect).abs();
        err = err.min(bl - err);
        assert!(err < 0.012 * sr as f64, "phase off by {:.1} ms", err / sr as f64 * 1000.0);
    }

    #[test]
    fn detects_common_tempos() {
        check(124.0, 0.31);
        check(128.0, 0.05);
        check(174.0, 0.2);
        check(100.0, 0.0);
        check(122.5, 0.44);
    }

    #[test]
    fn folds_into_range() {
        assert_eq!(fold_bpm(87.0, 88.0, 176.0), 174.0);
        assert_eq!(fold_bpm(174.0, 70.0, 140.0), 87.0);
        assert_eq!(fold_bpm(128.0, 70.0, 180.0), 128.0);
    }

    #[test]
    fn silence_has_no_grid() {
        let s = vec![0f32; 44100 * 2 * 20];
        assert!(detect_beat_grid(&s, 44100).is_none());
    }

    #[test]
    fn waveform_has_expected_length() {
        let s = click_track(120.0, 0.0, 10.0, 48000);
        let w = waveform(&s, 48000);
        let expect = (10.0 * WAVE_BINS_PER_SEC) as usize;
        assert!(w.len() == expect || w.len() == expect + 1, "{}", w.len());
        assert!(w.iter().any(|b| b.peak > 150));
    }
}

#[cfg(test)]
pub use tests::click_track;
