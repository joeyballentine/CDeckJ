//! Two-channel DJ mixer: trim, 3-band isolator EQ, colour filter, fader, crossfader, PFL.

use super::dsp::{db_to_gain, Isolator, Smooth, Svf};

/// Knob/fader positions as the hardware reports them (all 0..=1, knobs centred at 0.5).
#[derive(Clone, Copy, Debug)]
pub struct ChannelParams {
    pub trim: f32,
    pub hi: f32,
    pub mid: f32,
    pub low: f32,
    pub filter: f32,
    pub fader: f32,
    pub pfl: bool,
}

impl Default for ChannelParams {
    fn default() -> Self {
        Self {
            trim: 0.5,
            hi: 0.5,
            mid: 0.5,
            low: 0.5,
            filter: 0.5,
            fader: 1.0,
            pfl: false,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct MixerParams {
    pub ch: [ChannelParams; 2],
    /// 0 = full A (deck 1), 1 = full B (deck 2).
    pub crossfader: f32,
    /// 0 = cue only, 1 = master only.
    pub head_mix: f32,
    pub head_level: f32,
    pub master: f32,
}

impl Default for MixerParams {
    fn default() -> Self {
        Self {
            ch: [ChannelParams::default(); 2],
            crossfader: 0.5,
            head_mix: 0.0,
            head_level: 0.8,
            master: 0.8,
        }
    }
}

/// Trim: -24 dB … 0 dB (centre) … +9 dB.
pub fn trim_gain(v: f32) -> f32 {
    if v < 0.01 {
        return 0.0;
    }
    let db = if v >= 0.5 { (v - 0.5) * 2.0 * 9.0 } else { (v - 0.5) * 2.0 * 24.0 };
    db_to_gain(db)
}

/// EQ band: full kill … 0 dB (centre) … +6 dB.
pub fn eq_gain(v: f32) -> f32 {
    if v < 0.02 {
        return 0.0;
    }
    let db = if v >= 0.5 { (v - 0.5) * 2.0 * 6.0 } else { (0.5 - v) * 2.0 * -26.0 };
    db_to_gain(db)
}

/// Channel fader curve (roughly a DJM "long" curve).
pub fn fader_gain(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    v * v * (1.5 - 0.5 * v)
}

/// Crossfader: both channels at full level in the centre, fast-ish cut at the edges.
pub fn xfade_gains(x: f32) -> (f32, f32) {
    let x = x.clamp(0.0, 1.0);
    ((2.0 * (1.0 - x)).min(1.0), (2.0 * x).min(1.0))
}

/// Filter knob → (low-pass cutoff, high-pass cutoff). Centre = transparent.
pub fn filter_cutoffs(v: f32) -> (f32, f32) {
    const DEAD: f32 = 0.03;
    if v < 0.5 - DEAD {
        let t = (0.5 - DEAD - v) / (0.5 - DEAD); // 0..1
        (20000.0 * (100.0f32 / 20000.0).powf(t), 10.0)
    } else if v > 0.5 + DEAD {
        let t = (v - 0.5 - DEAD) / (0.5 - DEAD);
        (22000.0, 15.0 * (6000.0f32 / 15.0).powf(t))
    } else {
        (22000.0, 10.0)
    }
}

const FILTER_Q: f32 = 1.1;

struct Strip {
    iso: [Isolator; 2],
    lp: [Svf; 2],
    hp: [Svf; 2],
    g_trim: Smooth,
    g_lo: Smooth,
    g_mid: Smooth,
    g_hi: Smooth,
    g_out: Smooth,
    g_pfl: Smooth,
    cut: (f32, f32),
}

impl Strip {
    fn new(sr: f32) -> Self {
        let mut s = Self {
            iso: [Isolator::new(sr); 2],
            lp: [Svf::default(); 2],
            hp: [Svf::default(); 2],
            g_trim: Smooth::new(1.0, 0.01, sr),
            g_lo: Smooth::new(1.0, 0.01, sr),
            g_mid: Smooth::new(1.0, 0.01, sr),
            g_hi: Smooth::new(1.0, 0.01, sr),
            g_out: Smooth::new(1.0, 0.005, sr),
            g_pfl: Smooth::new(0.0, 0.005, sr),
            cut: (22000.0, 10.0),
        };
        for f in s.lp.iter_mut() {
            f.set(22000.0, FILTER_Q, sr);
        }
        for f in s.hp.iter_mut() {
            f.set(10.0, FILTER_Q, sr);
        }
        s
    }
}

pub struct Mixer {
    sr: f32,
    strips: [Strip; 2],
    g_master: Smooth,
    g_head_cue: Smooth,
    g_head_master: Smooth,
    pub peak_ch: [f32; 2],
    pub peak_master: f32,
}

impl Mixer {
    pub fn new(sr: f32) -> Self {
        Self {
            sr,
            strips: [Strip::new(sr), Strip::new(sr)],
            g_master: Smooth::new(0.8, 0.01, sr),
            g_head_cue: Smooth::new(1.0, 0.01, sr),
            g_head_master: Smooth::new(0.0, 0.01, sr),
            peak_ch: [0.0; 2],
            peak_master: 0.0,
        }
    }

    /// Mix the two deck buffers (interleaved stereo) into master and cue buses.
    pub fn process(
        &mut self,
        p: &MixerParams,
        decks: [&[f32]; 2],
        master: &mut [f32],
        cue: &mut [f32],
    ) {
        let n = master.len() / 2;
        master.fill(0.0);
        cue.fill(0.0);
        let (xa, xb) = xfade_gains(p.crossfader);
        let mut any_pfl = false;

        for (c, strip) in self.strips.iter_mut().enumerate() {
            let cp = &p.ch[c];
            any_pfl |= cp.pfl;
            let cut = filter_cutoffs(cp.filter);
            // Glide the cutoff per block to avoid zipper noise.
            let lp = strip.cut.0 + (cut.0 - strip.cut.0) * 0.35;
            let hp = strip.cut.1 + (cut.1 - strip.cut.1) * 0.35;
            strip.cut = (lp, hp);
            for f in strip.lp.iter_mut() {
                f.set(lp, FILTER_Q, self.sr);
            }
            for f in strip.hp.iter_mut() {
                f.set(hp, FILTER_Q, self.sr);
            }
            let filter_on = lp < 21000.0 || hp > 12.0;

            let t_trim = trim_gain(cp.trim);
            let (t_lo, t_mid, t_hi) = (eq_gain(cp.low), eq_gain(cp.mid), eq_gain(cp.hi));
            let t_out = fader_gain(cp.fader) * if c == 0 { xa } else { xb };
            let t_pfl = if cp.pfl { 1.0 } else { 0.0 };
            let input = decks[c];
            let mut peak = 0f32;

            for i in 0..n {
                let gt = strip.g_trim.tick(t_trim);
                let gl = strip.g_lo.tick(t_lo);
                let gm = strip.g_mid.tick(t_mid);
                let gh = strip.g_hi.tick(t_hi);
                let go = strip.g_out.tick(t_out);
                let gp = strip.g_pfl.tick(t_pfl);
                for s in 0..2 {
                    let mut x = input[2 * i + s] * gt;
                    x = strip.iso[s].tick(x, gl, gm, gh);
                    if filter_on {
                        x = strip.lp[s].tick(x).low;
                        x = strip.hp[s].tick(x).high;
                    }
                    peak = peak.max(x.abs());
                    master[2 * i + s] += x * go;
                    cue[2 * i + s] += x * gp;
                }
            }
            self.peak_ch[c] = peak;
        }

        let t_master = p.master * p.master * 1.25;
        let hm = p.head_mix.clamp(0.0, 1.0);
        let lvl = p.head_level * p.head_level;
        // With no PFL pressed the headphones follow master, like a DJM.
        let (t_hc, t_hm) = if any_pfl {
            ((2.0 * (1.0 - hm)).min(1.0) * lvl, (2.0 * hm).min(1.0) * lvl)
        } else {
            (0.0, lvl)
        };
        let mut mpeak = 0f32;
        for i in 0..n {
            let g = self.g_master.tick(t_master);
            let hc = self.g_head_cue.tick(t_hc);
            let hmg = self.g_head_master.tick(t_hm);
            for s in 0..2 {
                let m = master[2 * i + s] * g;
                mpeak = mpeak.max(m.abs());
                let m = super::dsp::soft_clip(m);
                master[2 * i + s] = m;
                cue[2 * i + s] = super::dsp::soft_clip(cue[2 * i + s] * hc + m * hmg);
            }
        }
        self.peak_master = mpeak;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves() {
        assert!((trim_gain(0.5) - 1.0).abs() < 1e-6);
        assert!((eq_gain(0.5) - 1.0).abs() < 1e-6);
        assert_eq!(eq_gain(0.0), 0.0);
        assert_eq!(xfade_gains(0.5), (1.0, 1.0));
        assert_eq!(xfade_gains(0.0), (1.0, 0.0));
        assert!((fader_gain(1.0) - 1.0).abs() < 1e-6);
        assert_eq!(filter_cutoffs(0.5), (22000.0, 10.0));
        assert!(filter_cutoffs(0.0).0 < 150.0);
        assert!(filter_cutoffs(1.0).1 > 5000.0);
    }
}
