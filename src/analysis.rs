//! Offline analysis run once per loaded track: colour waveform summary and BPM / beat grid.

use crate::track::{WaveBin, WAVE_BINS_PER_SEC};

/// One-pole low-pass coefficient for a cutoff frequency.
fn lp_coef(fc: f32, sr: f32) -> f32 {
    1.0 - (-2.0 * std::f32::consts::PI * fc / sr).exp()
}

/// Rekordbox-style 3-band waveform summary.
pub fn waveform(samples: &[f32], sample_rate: u32) -> Vec<WaveBin> {
    let sr = sample_rate as f32;
    let frames = samples.len() / 2;
    let bin_len = (sample_rate as f64 / WAVE_BINS_PER_SEC).max(1.0);
    let nbins = (frames as f64 / bin_len).ceil() as usize;

    let a_lo = lp_coef(200.0, sr);
    let a_hi = lp_coef(2500.0, sr);
    let (mut lo1, mut lo2, mut hi1) = (0f32, 0f32, 0f32);

    // (peak, low², mid², high², count)
    let mut raw: Vec<[f32; 5]> = vec![[0.0; 5]; nbins];
    for i in 0..frames {
        let x = 0.5 * (samples[2 * i] + samples[2 * i + 1]);
        lo1 += a_lo * (x - lo1);
        lo2 += a_lo * (lo1 - lo2);
        hi1 += a_hi * (x - hi1);
        let low = lo2;
        let high = x - hi1;
        let mid = hi1 - lo2;
        let b = ((i as f64 / bin_len) as usize).min(nbins - 1);
        let r = &mut raw[b];
        r[0] = r[0].max(samples[2 * i].abs().max(samples[2 * i + 1].abs()));
        r[1] += low * low;
        r[2] += mid * mid;
        r[3] += high * high;
        r[4] += 1.0;
    }

    let mut max_peak = 1e-6f32;
    let mut max_band = [1e-9f32; 3];
    for r in raw.iter_mut() {
        let n = r[4].max(1.0);
        for k in 0..3 {
            r[k + 1] = (r[k + 1] / n).sqrt();
            max_band[k] = max_band[k].max(r[k + 1]);
        }
        max_peak = max_peak.max(r[0]);
    }
    // Boost highs/mids visually so hats and vocals show up next to the kick.
    raw.iter()
        .map(|r| {
            let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0) as u8;
            WaveBin {
                peak: q(r[0] / max_peak),
                low: q(r[1] / max_band[0]),
                mid: q((r[2] / max_band[1]).sqrt()),
                high: q((r[3] / max_band[2]).sqrt()),
            }
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BeatGrid {
    pub bpm: f64,
    /// First beat position in frames, in `[0, beat_len)`.
    pub first_beat: f64,
}

const ENV_RATE: f64 = 200.0; // onset envelope samples per second

/// Onset-strength envelope: rectified log-energy rise of a kick band and a hat band.
fn onset_envelope(samples: &[f32], sample_rate: u32) -> (Vec<f32>, f64) {
    let sr = sample_rate as f32;
    let hop = sample_rate as f64 / ENV_RATE;
    let frames = samples.len() / 2;
    let n = (frames as f64 / hop) as usize;
    let a_lo = lp_coef(150.0, sr);
    let a_hi = lp_coef(4000.0, sr);
    let (mut l1, mut l2, mut h1) = (0f32, 0f32, 0f32);
    let mut e_lo = vec![0f32; n];
    let mut e_hi = vec![0f32; n];
    for i in 0..frames {
        let x = 0.5 * (samples[2 * i] + samples[2 * i + 1]);
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

/// Estimate a constant-tempo beat grid. Returns `None` for beatless material.
pub fn detect_beat_grid(samples: &[f32], sample_rate: u32) -> Option<BeatGrid> {
    let (env, hop) = onset_envelope(samples, sample_rate);
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
    fn silence_has_no_grid() {
        let s = vec![0f32; 44100 * 2 * 20];
        assert!(detect_beat_grid(&s, 44100).is_none());
    }

    #[test]
    fn waveform_has_expected_length() {
        let s = click_track(120.0, 0.0, 10.0, 48000);
        let w = waveform(&s, 48000);
        assert_eq!(w.len(), (10.0 * WAVE_BINS_PER_SEC) as usize);
        assert!(w.iter().any(|b| b.peak > 200));
    }
}

#[cfg(test)]
pub use tests::click_track;
