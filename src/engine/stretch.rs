//! Master Tempo (key lock): a WSOLA time-stretcher reading straight from the decoded track.
//!
//! Grains of `2·HOP` output frames are overlap-added with a Hann window at 50 % overlap. Each new
//! grain starts near the deck's playhead, nudged (±SEARCH frames) to the offset whose waveform
//! best matches the natural continuation of the previous grain, so pitch stays put while the
//! playhead moves at any tempo. Nothing here allocates after construction.

use crate::track::Track;

const HOP: usize = 1024;
const SEARCH: i64 = 480;
/// Correlation is evaluated on every DECIM-th sample, then refined.
const DECIM: usize = 4;

pub struct Stretcher {
    win: Vec<f32>,
    /// Second half of the previous grain (windowed), interleaved stereo.
    tail: Vec<f32>,
    /// Output ready to be consumed, interleaved stereo.
    out: Vec<f32>,
    out_pos: usize,
    prev_start: f64,
    have_prev: bool,
    reference: Vec<f32>,
}

#[inline]
fn mono(t: &Track, pos: f64) -> f32 {
    if pos < 0.0 {
        return 0.0;
    }
    let i = pos as usize;
    let f = (pos - i as f64) as f32;
    let (al, ar) = t.audio.frame(i);
    let (bl, br) = t.audio.frame(i + 1);
    let (a, b) = (al + ar, bl + br);
    0.5 * (a + (b - a) * f)
}

impl Default for Stretcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Stretcher {
    pub fn new() -> Self {
        let n = 2 * HOP;
        let win = (0..n)
            .map(|j| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * j as f32 / n as f32).cos())
            .collect();
        Self {
            win,
            tail: vec![0.0; HOP * 2],
            out: vec![0.0; HOP * 2],
            out_pos: HOP,
            prev_start: 0.0,
            have_prev: false,
            reference: vec![0.0; HOP],
        }
    }

    /// Forget history (call when engaging, so stale audio isn't faded in).
    pub fn reset(&mut self) {
        self.tail.fill(0.0);
        self.out_pos = HOP;
        self.have_prev = false;
    }

    /// Next output frame. `pos` is the deck's playhead, `ratio` source frames per output frame
    /// at original speed (track rate / output rate). `read` interpolates the track.
    #[inline]
    pub fn next(&mut self, t: &Track, pos: f64, ratio: f64, read: impl Fn(&Track, f64) -> (f32, f32)) -> (f32, f32) {
        if self.out_pos >= HOP {
            self.grain(t, pos, ratio, &read);
            self.out_pos = 0;
        }
        let i = self.out_pos;
        self.out_pos += 1;
        (self.out[2 * i], self.out[2 * i + 1])
    }

    fn grain(&mut self, t: &Track, ideal: f64, ratio: f64, read: &impl Fn(&Track, f64) -> (f32, f32)) {
        let start = if self.have_prev {
            let natural = self.prev_start + HOP as f64 * ratio;
            if (natural - ideal).abs() < 0.5 {
                natural // already where we want to be (normal speed)
            } else {
                self.best_offset(t, natural, ideal, ratio)
            }
        } else {
            ideal
        };
        for j in 0..2 * HOP {
            let w = self.win[j];
            let (l, r) = read(t, start + j as f64 * ratio);
            if j < HOP {
                self.out[2 * j] = self.tail[2 * j] + w * l;
                self.out[2 * j + 1] = self.tail[2 * j + 1] + w * r;
            } else {
                let k = j - HOP;
                self.tail[2 * k] = w * l;
                self.tail[2 * k + 1] = w * r;
            }
        }
        self.prev_start = start;
        self.have_prev = true;
    }

    fn best_offset(&mut self, t: &Track, natural: f64, ideal: f64, ratio: f64) -> f64 {
        for j in 0..HOP {
            self.reference[j] = mono(t, natural + j as f64 * ratio);
        }
        let score = |off: i64, step: usize, refr: &[f32]| -> f32 {
            let base = ideal + off as f64;
            let (mut xy, mut yy) = (0f32, 1e-9f32);
            let mut j = 0;
            while j < HOP {
                let y = mono(t, base + j as f64 * ratio);
                xy += refr[j] * y;
                yy += y * y;
                j += step;
            }
            xy / yy.sqrt()
        };
        let mut best = (f32::MIN, 0i64);
        let mut off = -SEARCH;
        while off <= SEARCH {
            let s = score(off, DECIM, &self.reference);
            if s > best.0 {
                best = (s, off);
            }
            off += 2;
        }
        let coarse = best.1;
        best = (f32::MIN, coarse);
        for off in coarse - 2..=coarse + 2 {
            let s = score(off, 1, &self.reference);
            if s > best.0 {
                best = (s, off);
            }
        }
        ideal + best.1 as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tone(freq: f32, secs: f32, sr: u32) -> Track {
        let n = (secs * sr as f32) as usize;
        let mut s = Vec::with_capacity(n * 2);
        for i in 0..n {
            let v = (2.0 * std::f32::consts::PI * freq * i as f32 / sr as f32).sin() * 0.5;
            s.push(v);
            s.push(v);
        }
        Track::from_samples(PathBuf::from("tone.wav"), sr, s)
    }

    fn lin(t: &Track, p: f64) -> (f32, f32) {
        let i = p.floor();
        if i < 0.0 {
            return (0.0, 0.0);
        }
        let i = i as usize;
        let f = (p - i as f64) as f32;
        let (a, b) = (t.audio.frame(i), t.audio.frame(i + 1));
        (a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f)
    }

    /// Estimate frequency by counting zero crossings.
    fn freq_of(x: &[f32], sr: f32) -> f32 {
        let zc = x.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        zc as f32 / (x.len() as f32 / sr)
    }

    #[test]
    fn keeps_pitch_when_speeding_up() {
        let sr = 48000;
        let t = tone(440.0, 6.0, sr);
        let mut st = Stretcher::new();
        let tempo = 1.08;
        let mut pos = 0.0;
        let mut out = Vec::new();
        for _ in 0..(sr * 3) {
            let (l, _) = st.next(&t, pos, 1.0, lin);
            out.push(l);
            pos += tempo;
        }
        let f = freq_of(&out[sr as usize..], sr as f32);
        assert!((f - 440.0).abs() < 4.0, "pitch drifted to {f} Hz");
        // Level stays steady (no OLA dips).
        let rms = (out[sr as usize..].iter().map(|v| v * v).sum::<f32>() / (2 * sr) as f32).sqrt();
        assert!((rms - 0.5 / 2f32.sqrt()).abs() < 0.04, "rms {rms}");
    }

    #[test]
    fn transparent_at_normal_speed() {
        let sr = 48000;
        let t = tone(220.0, 3.0, sr);
        let mut st = Stretcher::new();
        let mut pos = 0.0;
        let mut max_err = 0f32;
        for i in 0..(sr * 2) as usize {
            let (l, _) = st.next(&t, pos, 1.0, lin);
            // Output lags input by nothing: after the first grain, sample i == source i.
            if i > 2 * HOP {
                max_err = max_err.max((l - t.audio.frame(i).0).abs());
            }
            pos += 1.0;
        }
        assert!(max_err < 1e-3, "error {max_err}");
    }
}
