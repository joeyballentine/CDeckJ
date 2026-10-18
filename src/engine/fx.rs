//! DJM-style beat FX: tempo-synced effects on one channel or the master, post-fader, so echo
//! and reverb tails keep ringing after the fader closes or the effect is switched off.

use super::dsp::Smooth;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FxKind {
    Delay,
    Echo,
    PingPong,
    Reverb,
    Flanger,
    Phaser,
    Trans,
    Roll,
}

pub const FX_KINDS: [FxKind; 8] =
    [FxKind::Delay, FxKind::Echo, FxKind::PingPong, FxKind::Reverb, FxKind::Flanger, FxKind::Phaser, FxKind::Trans, FxKind::Roll];

impl FxKind {
    pub fn name(self) -> &'static str {
        match self {
            FxKind::Delay => "DELAY",
            FxKind::Echo => "ECHO",
            FxKind::PingPong => "PING PONG",
            FxKind::Reverb => "REVERB",
            FxKind::Flanger => "FLANGER",
            FxKind::Phaser => "PHASER",
            FxKind::Trans => "TRANS",
            FxKind::Roll => "ROLL",
        }
    }

    /// Effects whose tail keeps sounding after they're switched off.
    fn has_tail(self) -> bool {
        matches!(self, FxKind::Delay | FxKind::Echo | FxKind::PingPong | FxKind::Reverb)
    }
}

/// Beat lengths the BEAT ◀ ▶ buttons step through.
pub const FX_BEATS: [f64; 9] = [0.125, 0.25, 0.5, 0.75, 1.0, 2.0, 4.0, 8.0, 16.0];

pub fn beats_label(b: f64) -> String {
    match b {
        x if (x - 0.75).abs() < 1e-9 => "3/4".into(),
        x if x < 1.0 => format!("1/{}", (1.0 / x).round()),
        x => format!("{x:.0}"),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FxTarget {
    Ch(usize),
    Master,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FxParams {
    pub kind: FxKind,
    /// Index into [`FX_BEATS`].
    pub beat_idx: usize,
    pub target: FxTarget,
    /// LEVEL/DEPTH, 0..1.
    pub level: f32,
    pub on: bool,
    /// Start ROLL and gate TRANS on the beat grid.
    pub quantize: bool,
}

impl Default for FxParams {
    fn default() -> Self {
        Self { kind: FxKind::Echo, beat_idx: 2, target: FxTarget::Master, level: 0.5, on: false, quantize: true }
    }
}

impl FxParams {
    pub fn beats(&self) -> f64 {
        FX_BEATS[self.beat_idx.min(FX_BEATS.len() - 1)]
    }
}

/// Tempo for the effects, from the deck that's leading.
#[derive(Clone, Copy, Debug)]
pub struct FxClock {
    pub bpm: f64,
    /// Beat position (in the leading track's grid) at the start of the block, when playing.
    pub beat: Option<f64>,
}

impl Default for FxClock {
    fn default() -> Self {
        Self { bpm: 120.0, beat: None }
    }
}

const MAX_DELAY_SECS: f32 = 6.0;
const MAX_ROLL_SECS: f32 = 8.0;
const ECHO_FEEDBACK: f32 = 0.55;

/// One Freeverb-style channel: parallel combs into series allpasses.
struct Verb {
    combs: Vec<(Vec<f32>, usize, f32)>,
    alls: Vec<(Vec<f32>, usize)>,
}

impl Verb {
    fn new(sr: f32, spread: usize) -> Verb {
        let s = sr / 44100.0;
        let len = |n: usize| (((n + spread) as f32) * s) as usize;
        Verb {
            combs: [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617].iter().map(|&n| (vec![0.0; len(n)], 0, 0.0)).collect(),
            alls: [556, 441, 341, 225].iter().map(|&n| (vec![0.0; len(n)], 0)).collect(),
        }
    }

    fn tick(&mut self, x: f32, feedback: f32, damp: f32) -> f32 {
        let mut out = 0.0;
        for (buf, i, store) in self.combs.iter_mut() {
            let y = buf[*i];
            *store = y * (1.0 - damp) + *store * damp;
            buf[*i] = x + *store * feedback;
            *i += 1;
            if *i == buf.len() {
                *i = 0;
            }
            out += y;
        }
        for (buf, i) in self.alls.iter_mut() {
            let b = buf[*i];
            buf[*i] = out + b * 0.5;
            out = b - out;
            *i += 1;
            if *i == buf.len() {
                *i = 0;
            }
        }
        out
    }

    fn clear(&mut self) {
        for (b, _, s) in self.combs.iter_mut() {
            b.fill(0.0);
            *s = 0.0;
        }
        for (b, _) in self.alls.iter_mut() {
            b.fill(0.0);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum RollState {
    Idle,
    /// Waiting for the next beat (quantize).
    Armed,
    Recording,
    Looping,
}

pub struct BeatFx {
    sr: f32,
    /// Stereo delay line for delay/echo/ping pong.
    line: Vec<f32>,
    w: usize,
    /// Frames written since the last reset: older parts of `line` are stale and read as
    /// silence (so switching effects never has to clear megabytes on the audio thread).
    fresh: usize,
    /// Short stereo line for the flanger.
    fline: Vec<f32>,
    fw: usize,
    fb_prev: [f32; 2],
    ap: [[f32; 4]; 2],
    ap_prev: [[f32; 4]; 2],
    verb: [Verb; 2],
    roll: Vec<f32>,
    roll_len: usize,
    roll_pos: usize,
    roll_state: RollState,
    lfo: f64,
    feed: Smooth,
    gate: Smooth,
    kind: FxKind,
    target: FxTarget,
    /// Frames since the effect last made sound (stops processing silence).
    quiet: usize,
}

impl BeatFx {
    pub fn new(sr: f32) -> BeatFx {
        let frames = (MAX_DELAY_SECS * sr) as usize;
        BeatFx {
            sr,
            line: vec![0.0; frames * 2],
            w: 0,
            fresh: 0,
            fline: vec![0.0; 2048 * 2],
            fw: 0,
            fb_prev: [0.0; 2],
            ap: [[0.0; 4]; 2],
            ap_prev: [[0.0; 4]; 2],
            verb: [Verb::new(sr, 0), Verb::new(sr, 23)],
            roll: vec![0.0; (MAX_ROLL_SECS * sr) as usize * 2],
            roll_len: 0,
            roll_pos: 0,
            roll_state: RollState::Idle,
            lfo: 0.0,
            feed: Smooth::new(0.0, 0.008, sr),
            gate: Smooth::new(1.0, 0.0015, sr),
            kind: FxKind::Echo,
            target: FxTarget::Master,
            quiet: usize::MAX,
        }
    }

    /// Cheap enough for the audio thread: only the small buffers are cleared.
    fn reset(&mut self) {
        self.fresh = 0;
        self.fline.fill(0.0);
        self.fb_prev = [0.0; 2];
        self.ap = [[0.0; 4]; 2];
        self.ap_prev = [[0.0; 4]; 2];
        for v in self.verb.iter_mut() {
            v.clear();
        }
        self.roll_state = RollState::Idle;
        self.quiet = usize::MAX;
    }

    /// Whether anything needs processing (on, fading, or a tail still ringing).
    pub fn active(&self, p: &FxParams) -> bool {
        p.on || self.feed.value > 1e-4 || (self.kind.has_tail() && self.quiet < (self.sr as usize) * 8)
    }

    /// Process interleaved stereo in place.
    pub fn process(&mut self, p: &FxParams, clk: &FxClock, io: &mut [f32]) {
        if p.kind != self.kind || p.target != self.target {
            self.reset();
            self.kind = p.kind;
            self.target = p.target;
        }
        if !self.active(p) {
            return;
        }
        let sr = self.sr;
        let n = io.len() / 2;
        let bpm = clk.bpm.clamp(40.0, 300.0);
        let beats = p.beats();
        let beat_secs = 60.0 / bpm;
        let period = (beats * beat_secs * sr as f64).max(1.0);
        let level = p.level.clamp(0.0, 1.0);
        let bpf = bpm / 60.0 / sr as f64; // beats per frame
        let target_feed = if p.on { 1.0 } else { 0.0 };
        let mut loud = 0f32;

        match p.kind {
            FxKind::Delay | FxKind::Echo | FxKind::PingPong => {
                let frames = self.line.len() / 2;
                let d = (period as usize).clamp(1, frames - 1);
                let fb = if p.kind == FxKind::Delay { 0.0 } else { ECHO_FEEDBACK };
                for i in 0..n {
                    let g = self.feed.tick(target_feed);
                    let r = (self.w + frames - d) % frames;
                    let (wl, wr) = if self.fresh >= d { (self.line[2 * r], self.line[2 * r + 1]) } else { (0.0, 0.0) };
                    let (xl, xr) = (io[2 * i], io[2 * i + 1]);
                    if p.kind == FxKind::PingPong {
                        let mono = 0.5 * (xl + xr) * g;
                        self.line[2 * self.w] = mono + wr * fb;
                        self.line[2 * self.w + 1] = wl * fb;
                    } else {
                        self.line[2 * self.w] = xl * g + wl * fb;
                        self.line[2 * self.w + 1] = xr * g + wr * fb;
                    }
                    self.w = (self.w + 1) % frames;
                    self.fresh = self.fresh.saturating_add(1);
                    io[2 * i] = xl + level * wl;
                    io[2 * i + 1] = xr + level * wr;
                    loud = loud.max(wl.abs()).max(wr.abs());
                }
            }
            FxKind::Reverb => {
                // The beat value sets the size of the room.
                let size = (p.beat_idx as f32 / (FX_BEATS.len() - 1) as f32).clamp(0.0, 1.0);
                let feedback = 0.7 + 0.28 * size;
                for i in 0..n {
                    let g = self.feed.tick(target_feed);
                    let (xl, xr) = (io[2 * i], io[2 * i + 1]);
                    let input = 0.5 * (xl + xr) * g * 0.03;
                    let (wl, wr) = (self.verb[0].tick(input, feedback, 0.3), self.verb[1].tick(input, feedback, 0.3));
                    io[2 * i] = xl + level * wl;
                    io[2 * i + 1] = xr + level * wr;
                    loud = loud.max(wl.abs()).max(wr.abs());
                }
            }
            FxKind::Flanger | FxKind::Phaser => {
                let inc = 1.0 / period;
                let flen = self.fline.len() / 2;
                for i in 0..n {
                    let g = self.feed.tick(target_feed);
                    self.lfo = (self.lfo + inc).fract();
                    let m = 0.5 - 0.5 * (self.lfo * std::f64::consts::TAU).cos() as f32;
                    for s in 0..2 {
                        let x = io[2 * i + s];
                        let wet = if p.kind == FxKind::Flanger {
                            let dly = (0.0005 + 0.0045 * m) * sr;
                            let rp = self.fw as f32 + flen as f32 - dly;
                            let (i0, fr) = (rp.floor() as usize % flen, rp.fract());
                            let a = self.fline[2 * i0 + s];
                            let b = self.fline[2 * ((i0 + 1) % flen) + s];
                            let y = a + (b - a) * fr;
                            self.fline[2 * self.fw + s] = x + y * 0.6;
                            0.5 * (x + y)
                        } else {
                            let f = 200.0 * (20.0f32).powf(m);
                            let t = (std::f32::consts::PI * f / sr).tan();
                            let c = (t - 1.0) / (t + 1.0);
                            let mut y = x + self.fb_prev[s] * 0.5;
                            for k in 0..4 {
                                let out = c * y + self.ap_prev[s][k] - c * self.ap[s][k];
                                self.ap_prev[s][k] = y;
                                self.ap[s][k] = out;
                                y = out;
                            }
                            self.fb_prev[s] = y;
                            0.5 * (x + y)
                        };
                        io[2 * i + s] = x + (wet - x) * g * level.max(0.05);
                    }
                    self.fw = (self.fw + 1) % flen;
                }
            }
            FxKind::Trans => {
                let mut beat = clk.beat;
                for i in 0..n {
                    let g = self.feed.tick(target_feed);
                    let ph = match beat {
                        Some(b) => (b / beats).rem_euclid(1.0),
                        None => {
                            self.lfo = (self.lfo + 1.0 / period).fract();
                            self.lfo
                        }
                    };
                    let gate = self.gate.tick(if ph < 0.5 { 1.0 } else { 1.0 - level.max(0.05) });
                    let k = 1.0 + (gate - 1.0) * g;
                    io[2 * i] *= k;
                    io[2 * i + 1] *= k;
                    if let Some(b) = beat.as_mut() {
                        *b += bpf;
                    }
                }
            }
            FxKind::Roll => {
                let max = self.roll.len() / 2;
                let len = (period as usize).clamp(64, max);
                let mut beat = clk.beat;
                // Quantized starts wait for the next beat (or the roll length if shorter).
                let step = beats.min(1.0);
                for i in 0..n {
                    let g = self.feed.tick(target_feed);
                    match (self.roll_state, p.on) {
                        (RollState::Idle, true) => {
                            self.roll_state = if p.quantize && beat.is_some() { RollState::Armed } else { RollState::Recording };
                            self.roll_pos = 0;
                            self.roll_len = len;
                        }
                        (RollState::Armed, true) => {
                            if let Some(b) = beat {
                                let next = b + bpf;
                                if (next / step).floor() > (b / step).floor() {
                                    self.roll_state = RollState::Recording;
                                }
                            }
                        }
                        (_, false) if g < 1e-3 => self.roll_state = RollState::Idle,
                        _ => {}
                    }
                    let (xl, xr) = (io[2 * i], io[2 * i + 1]);
                    match self.roll_state {
                        RollState::Recording => {
                            self.roll[2 * self.roll_pos] = xl;
                            self.roll[2 * self.roll_pos + 1] = xr;
                            self.roll_pos += 1;
                            if self.roll_pos >= self.roll_len {
                                self.roll_pos = 0;
                                self.roll_state = RollState::Looping;
                            }
                        }
                        RollState::Looping => {
                            // Short fade at the loop seam to avoid a click.
                            let fade = 96.min(self.roll_len / 4);
                            let pos = self.roll_pos;
                            let mut amp = 1.0;
                            if pos < fade {
                                amp = pos as f32 / fade as f32;
                            } else if pos + fade > self.roll_len {
                                amp = (self.roll_len - pos) as f32 / fade as f32;
                            }
                            let (rl, rr) = (self.roll[2 * pos] * amp, self.roll[2 * pos + 1] * amp);
                            let mix = g * level.max(0.05);
                            io[2 * i] = xl + (rl - xl) * mix;
                            io[2 * i + 1] = xr + (rr - xr) * mix;
                            self.roll_pos = (pos + 1) % self.roll_len;
                        }
                        _ => {}
                    }
                    if let Some(b) = beat.as_mut() {
                        *b += bpf;
                    }
                }
            }
        }
        if loud > 1e-4 || p.on {
            self.quiet = 0;
        } else {
            self.quiet = self.quiet.saturating_add(n);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(fx: &mut BeatFx, p: &FxParams, clk: &FxClock, input: &[f32]) -> Vec<f32> {
        let mut out = input.to_vec();
        for c in out.chunks_mut(512) {
            fx.process(p, clk, c);
        }
        out
    }

    fn impulse(frames: usize) -> Vec<f32> {
        let mut v = vec![0.0; frames * 2];
        v[0] = 1.0;
        v[1] = 1.0;
        v
    }

    #[test]
    fn echo_repeats_on_the_beat_and_keeps_its_tail() {
        let sr = 48000.0;
        let mut fx = BeatFx::new(sr);
        let mut p = FxParams { kind: FxKind::Echo, beat_idx: 4, level: 1.0, on: true, ..Default::default() };
        let clk = FxClock { bpm: 120.0, beat: None };
        // Let the on-fade settle, then send an impulse.
        run(&mut fx, &p, &clk, &vec![0.0; 4800 * 2]);
        let out = run(&mut fx, &p, &clk, &impulse(48000));
        // 1 beat at 120 BPM = 0.5 s = 24000 frames.
        assert!(out[2 * 24000].abs() > 0.9, "{}", out[2 * 24000]);
        assert!(out[2 * 12000].abs() < 1e-6);
        // Switched off: the tail keeps going but new input isn't echoed.
        p.on = false;
        // (The input fade takes a few ms, so the new impulse comes 40 ms after switching off.)
        let mut x = vec![0.0; 24000 * 2];
        x[2 * 2000] = 1.0;
        let out = run(&mut fx, &p, &clk, &x);
        assert!(out[0].abs() > 0.4, "tail of the first impulse rings on: {}", out[0]);
        let more = run(&mut fx, &p, &clk, &vec![0.0; 24200 * 2]);
        assert!(more[2 * 2000].abs() < 0.05, "new input not echoed after off: {}", more[2 * 2000]);
    }

    #[test]
    fn trans_gates_on_the_beat_grid() {
        let mut fx = BeatFx::new(48000.0);
        let p = FxParams { kind: FxKind::Trans, beat_idx: 4, level: 1.0, on: true, ..Default::default() };
        let clk = FxClock { bpm: 120.0, beat: Some(0.0) };
        let ones = vec![1.0f32; 48000 * 2];
        run(&mut fx, &p, &clk, &vec![0.0; 2]); // settle nothing
        let mut out = ones.clone();
        fx.process(&p, &clk, &mut out[..24000 * 2]);
        // First half-beat open, second half closed (at full depth: ~5 % left).
        assert!(out[2 * 6000] > 0.9, "{}", out[2 * 6000]);
        assert!(out[2 * 18000] < 0.1, "{}", out[2 * 18000]);
    }

    #[test]
    fn roll_repeats_what_it_captured() {
        let sr = 48000.0;
        let mut fx = BeatFx::new(sr);
        let p = FxParams { kind: FxKind::Roll, beat_idx: 2, level: 1.0, on: true, quantize: false, ..Default::default() };
        let clk = FxClock { bpm: 120.0, beat: None };
        // A ramp: after the half-beat capture (12000 frames), output should repeat the ramp.
        let x: Vec<f32> = (0..48000).flat_map(|i| [i as f32 / 48000.0; 2]).collect();
        let out = run(&mut fx, &p, &clk, &x);
        let at = 12000 + 3000; // looping, 3000 frames into the loop
        assert!((out[2 * at] - x[2 * 3000]).abs() < 0.02, "{} vs {}", out[2 * at], x[2 * 3000]);
    }

    #[test]
    fn switching_effects_never_replays_old_echoes() {
        let mut fx = BeatFx::new(48000.0);
        let mut p = FxParams { kind: FxKind::Echo, beat_idx: 4, level: 1.0, on: true, ..Default::default() };
        let clk = FxClock { bpm: 120.0, beat: None };
        run(&mut fx, &p, &clk, &vec![0.5; 24000 * 2]);
        p.kind = FxKind::Delay;
        let out = run(&mut fx, &p, &clk, &vec![0.0; 20000 * 2]);
        assert!(out.iter().all(|v| v.abs() < 1e-6), "stale echo heard after switching");
    }

    #[test]
    fn everything_stays_finite() {
        for kind in FX_KINDS {
            let mut fx = BeatFx::new(44100.0);
            let p = FxParams { kind, beat_idx: 8, level: 1.0, on: true, ..Default::default() };
            let noise: Vec<f32> = (0..44100 * 2).map(|i| ((i * 7919 % 2003) as f32 / 1001.0) - 1.0).collect();
            let out = run(&mut fx, &p, &FxClock { bpm: 174.0, beat: Some(3.5) }, &noise);
            assert!(out.iter().all(|v| v.is_finite() && v.abs() < 20.0), "{kind:?}");
        }
    }
}

#[cfg(test)]
mod bench {
    use super::*;

    /// `cargo test --release fx_cost -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn fx_cost() {
        let sr = 48000.0;
        let secs = 10.0;
        let noise: Vec<f32> = (0..(sr * secs) as usize * 2).map(|i| ((i * 7919 % 2003) as f32 / 1001.0) - 1.0).collect();
        for kind in FX_KINDS {
            let mut fx = BeatFx::new(sr);
            let p = FxParams { kind, beat_idx: 4, level: 0.7, on: true, ..Default::default() };
            let mut buf = noise.clone();
            let t0 = std::time::Instant::now();
            for c in buf.chunks_mut(512) {
                fx.process(&p, &FxClock { bpm: 128.0, beat: Some(0.0) }, c);
            }
            let ms_per_s = t0.elapsed().as_secs_f64() * 1000.0 / secs as f64;
            println!("{:>10}: {ms_per_s:.2} ms per second of audio ({:.2} % of a core)", kind.name(), ms_per_s / 10.0);
        }
    }
}
