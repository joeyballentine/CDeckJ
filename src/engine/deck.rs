//! One CDJ-style player: transport, cue/hot-cue behaviour, loops, jog and tempo.

use std::sync::Arc;

use super::dsp::hermite;
use super::stretch::Stretcher;
use crate::track::Track;

pub const HOT_CUES: usize = 8;
/// Tempo ranges a CDJ offers: ±6, ±10, ±16 %, and WIDE.
pub const TEMPO_RANGES: [f64; 4] = [0.06, 0.10, 0.16, 1.00];
pub const DEFAULT_RANGE: usize = 1;

/// Seconds of audio one jog tick moves while scratching (720 ticks/rev at 33⅓ rpm).
pub const SCRATCH_SECS_PER_TICK: f64 = 1.8 / 720.0;
/// Rate offset added per jog tick when nudging the side of the platter.
const BEND_PER_TICK: f64 = 0.004;
/// Seconds moved per tick for SHIFT + jog track search.
const SEARCH_SECS_PER_TICK: f64 = 0.12;
/// Crossfade length used to hide clicks on jumps and loop wraps.
const FADE_FRAMES: u32 = 192;
/// How close (seconds) the playhead must be to the cue point to count as "at cue".
const AT_CUE_SECS: f64 = 0.01;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Preview {
    #[default]
    None,
    Cue,
    Hot(usize),
}

/// A hot cue: a point, or a saved loop when `loop_end` is set.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HotCue {
    pub pos: f64,
    pub loop_end: Option<f64>,
    pub color: Option<[u8; 3]>,
}

pub struct Deck {
    pub track: Option<Arc<Track>>,
    pub track_gen: u64,
    /// Playhead in source frames.
    pub pos: f64,
    pub playing: bool,
    pub cue: f64,
    pub hot: [Option<HotCue>; HOT_CUES],
    pub preview: Preview,
    pub loop_in: Option<f64>,
    pub loop_out: Option<f64>,
    pub loop_active: bool,
    /// Tempo fader, -1 (top, slower) ..= +1 (bottom, faster), like a CDJ.
    pub tempo_fader: f64,
    pub range_idx: usize,
    pub synced: bool,
    pub vinyl: bool,
    /// Master Tempo: keep the key when the tempo changes.
    pub keylock: bool,
    pub touched: bool,
    /// Rate actually applied last block (1.0 = original speed), for display.
    pub applied_rate: f64,
    /// Platter angle in revolutions, for the on-screen jog display.
    pub platter_rev: f64,

    scratch_target: f64,
    bend: f64,
    v_prev: f64,
    amp: f32,
    fade_from: f64,
    fade_left: u32,
    out_sr: f64,
    stretcher: Box<Stretcher>,
    stretch_active: bool,
}

impl Deck {
    pub fn new(out_sr: f64) -> Self {
        Self {
            track: None,
            track_gen: 0,
            pos: 0.0,
            playing: false,
            cue: 0.0,
            hot: [None; HOT_CUES],
            preview: Preview::None,
            loop_in: None,
            loop_out: None,
            loop_active: false,
            tempo_fader: 0.0,
            range_idx: DEFAULT_RANGE,
            synced: false,
            vinyl: true,
            keylock: false,
            touched: false,
            applied_rate: 1.0,
            platter_rev: 0.0,
            scratch_target: 0.0,
            bend: 0.0,
            v_prev: 0.0,
            amp: 0.0,
            fade_from: 0.0,
            fade_left: 0,
            out_sr,
            stretcher: Box::new(Stretcher::new()),
            stretch_active: false,
        }
    }

    fn sr(&self) -> f64 {
        self.track.as_ref().map(|t| t.sample_rate as f64).unwrap_or(44100.0)
    }

    fn frames(&self) -> f64 {
        self.track.as_ref().map(|t| t.frames as f64).unwrap_or(0.0)
    }

    /// Original tempo of the track at the playhead.
    pub fn bpm(&self) -> Option<f64> {
        self.track.as_ref().and_then(|t| t.bpm_at(self.pos))
    }

    pub fn beat_at(&self, pos: f64) -> Option<f64> {
        self.track.as_ref().and_then(|t| t.beat_at(pos))
    }

    pub fn pos_of_beat(&self, beat: f64) -> Option<f64> {
        self.track.as_ref().and_then(|t| t.pos_of_beat(beat))
    }

    /// Tempo multiplier from the fader alone.
    pub fn fader_rate(&self) -> f64 {
        1.0 + self.tempo_fader * TEMPO_RANGES[self.range_idx]
    }

    pub fn audible(&self) -> bool {
        self.playing || self.preview != Preview::None
    }

    pub fn at_cue(&self) -> bool {
        (self.pos - self.cue).abs() < AT_CUE_SECS * self.sr()
    }

    /// Install a new track; returns the old one so it can be freed off the audio thread.
    pub fn load(&mut self, track: Arc<Track>, generation: u64) -> Option<Arc<Track>> {
        // Auto-cue to the first audible sample, like a CDJ.
        let first = track
            .samples
            .chunks_exact(2)
            .position(|f| f[0].abs().max(f[1].abs()) > 0.003)
            .unwrap_or(0) as f64;
        // rekordbox exports: hot cues go on the pads, the first memory cue becomes the cue point.
        let mut hot = [None; HOT_CUES];
        let mut cue = first;
        for c in &track.cues {
            match c.hot {
                Some(i) if i < HOT_CUES => {
                    hot[i] = Some(HotCue { pos: c.pos, loop_end: c.loop_end, color: c.color });
                }
                Some(_) => {}
                None if cue == first => cue = c.pos,
                None => {}
            }
        }
        let old = self.track.replace(track);
        self.track_gen = generation;
        self.playing = false;
        self.preview = Preview::None;
        self.hot = hot;
        self.loop_in = None;
        self.loop_out = None;
        self.loop_active = false;
        self.synced = false;
        self.bend = 0.0;
        self.cue = cue;
        self.stop_at(cue);
        old
    }

    /// Jump and stop dead (CUE, preview release): no run-out after the jump.
    fn stop_at(&mut self, to: f64) {
        self.jump(to);
        self.v_prev = 0.0;
    }

    fn jump(&mut self, to: f64) {
        if self.track.is_none() {
            return;
        }
        let to = to.clamp(0.0, self.frames());
        if self.amp > 0.001 {
            self.fade_from = self.pos;
            self.fade_left = FADE_FRAMES;
        }
        self.pos = to;
        self.scratch_target = to;
    }

    // ---- transport ---------------------------------------------------------------------------

    pub fn play_pressed(&mut self) {
        if self.track.is_none() {
            return;
        }
        if self.preview != Preview::None {
            // Pressing PLAY while holding CUE / a hot cue keeps it playing on release.
            self.preview = Preview::None;
            self.playing = true;
            return;
        }
        self.playing = !self.playing;
    }

    /// CDJ cue: while playing, return to cue and pause; while paused away from the cue point,
    /// set a new cue; while paused on it, play for as long as the button is held.
    pub fn cue_button(&mut self, pressed: bool) {
        if self.track.is_none() {
            return;
        }
        if pressed {
            if self.playing {
                self.playing = false;
                self.stop_at(self.cue);
            } else if !self.at_cue() {
                self.cue = self.pos;
            } else {
                self.preview = Preview::Cue;
            }
        } else if self.preview == Preview::Cue {
            self.preview = Preview::None;
            self.stop_at(self.cue);
        }
    }

    /// SHIFT + CUE: back to the start of the track.
    pub fn to_start(&mut self) {
        self.playing = false;
        self.preview = Preview::None;
        self.stop_at(0.0);
    }

    pub fn hot_cue(&mut self, i: usize, pressed: bool) {
        if self.track.is_none() || i >= HOT_CUES {
            return;
        }
        if pressed {
            match self.hot[i] {
                None => self.hot[i] = Some(HotCue { pos: self.pos, loop_end: None, color: None }),
                Some(HotCue { pos: p, loop_end: Some(e), .. }) => {
                    // Saved loop: engage it from its start.
                    self.loop_in = Some(p);
                    self.loop_out = Some(e);
                    self.loop_active = true;
                    self.jump(p);
                    if !self.playing {
                        self.preview = Preview::Hot(i);
                    }
                }
                Some(HotCue { pos: p, .. }) => {
                    if self.loop_active
                        && !matches!((self.loop_in, self.loop_out), (Some(a), Some(b)) if p >= a && p < b)
                    {
                        self.loop_active = false;
                    }
                    self.jump(p);
                    if !self.playing {
                        self.preview = Preview::Hot(i);
                    }
                }
            }
        } else if self.preview == Preview::Hot(i) {
            self.preview = Preview::None;
            if let Some(h) = self.hot[i] {
                self.stop_at(h.pos);
            }
        }
    }

    pub fn hot_cue_delete(&mut self, i: usize) {
        if i < HOT_CUES {
            self.hot[i] = None;
        }
    }

    pub fn seek_fraction(&mut self, frac: f64) {
        let to = frac.clamp(0.0, 1.0) * self.frames();
        self.jump(to);
        if !self.playing {
            self.cue = self.pos;
        }
    }

    // ---- loops -------------------------------------------------------------------------------

    pub fn loop_in_pressed(&mut self) {
        if self.track.is_none() {
            return;
        }
        self.loop_in = Some(self.pos);
        self.loop_out = None;
        self.loop_active = false;
    }

    pub fn loop_out_pressed(&mut self) {
        if let Some(a) = self.loop_in {
            if self.pos > a + 0.02 * self.sr() {
                self.loop_out = Some(self.pos);
                self.loop_active = true;
            }
        }
    }

    pub fn reloop_exit(&mut self) {
        if self.loop_active {
            self.loop_active = false;
        } else if let (Some(a), Some(_)) = (self.loop_in, self.loop_out) {
            self.loop_active = true;
            self.jump(a);
        }
    }

    pub fn auto_loop(&mut self, beats: f64) {
        let Some(b) = self.beat_at(self.pos) else { return };
        // Snap to the nearest grid line if we're close to one.
        let start_beat = if (b - b.round()).abs() < 0.25 { b.round() } else { b };
        let (Some(start), Some(end)) = (self.pos_of_beat(start_beat), self.pos_of_beat(start_beat + beats)) else {
            return;
        };
        self.loop_in = Some(start);
        self.loop_out = Some(end);
        self.loop_active = true;
        if self.pos >= end {
            self.jump(start);
        }
    }

    pub fn loop_scale(&mut self, k: f64) {
        if let (Some(a), Some(b)) = (self.loop_in, self.loop_out) {
            let len = ((b - a) * k).max(0.005 * self.sr());
            self.loop_out = Some(a + len);
        }
    }

    pub fn beat_jump(&mut self, beats: f64) {
        let Some(b) = self.beat_at(self.pos) else { return };
        let Some(to) = self.pos_of_beat(b + beats) else { return };
        let d = to - self.pos;
        if self.loop_active {
            self.loop_in = self.loop_in.map(|v| v + d);
            self.loop_out = self.loop_out.map(|v| v + d);
        }
        self.jump(self.pos + d);
    }

    // ---- jog ---------------------------------------------------------------------------------

    pub fn jog_touch(&mut self, touched: bool) {
        self.touched = touched;
        self.scratch_target = self.pos;
    }

    /// Top-plate movement in vinyl mode: scratch. Falls back to a nudge if not touched.
    pub fn jog_scratch(&mut self, ticks: f64) {
        if self.touched && self.vinyl {
            self.scratch_target += ticks * SCRATCH_SECS_PER_TICK * self.sr();
        } else {
            self.jog_bend(ticks);
        }
    }

    /// Side of the platter (or top with vinyl off): pitch bend while playing,
    /// frame-accurate search while paused.
    pub fn jog_bend(&mut self, ticks: f64) {
        if self.audible() {
            self.bend = (self.bend + ticks * BEND_PER_TICK).clamp(-0.8, 0.8);
        } else if self.track.is_some() {
            let to = (self.pos + ticks * SCRATCH_SECS_PER_TICK * 0.5 * self.sr())
                .clamp(0.0, self.frames());
            self.pos = to;
            self.scratch_target = to;
            if !self.playing {
                self.cue = self.pos;
            }
        }
    }

    pub fn jog_search(&mut self, ticks: f64) {
        if self.track.is_some() {
            let to = (self.pos + ticks * SEARCH_SECS_PER_TICK * self.sr()).clamp(0.0, self.frames());
            self.pos = to;
            self.scratch_target = to;
        }
    }

    /// Shift the playhead by a fraction of a beat (used by sync's phase alignment).
    pub fn nudge_beats(&mut self, beats: f64) {
        if let Some(to) = self.beat_at(self.pos).and_then(|b| self.pos_of_beat(b + beats)) {
            self.jump(to);
        }
    }

    // ---- rendering ---------------------------------------------------------------------------

    #[inline]
    fn read(t: &Track, pos: f64) -> (f32, f32) {
        let i = pos.floor();
        let f = (pos - i) as f32;
        let i = i as isize;
        let n = t.frames as isize;
        let s = &t.samples;
        let at = |k: isize| -> (f32, f32) {
            if k < 0 || k >= n {
                (0.0, 0.0)
            } else {
                let k = k as usize * 2;
                (s[k], s[k + 1])
            }
        };
        let (a, b, c, d) = (at(i - 1), at(i), at(i + 1), at(i + 2));
        (hermite(a.0, b.0, c.0, d.0, f), hermite(a.1, b.1, c.1, d.1, f))
    }

    /// Render `out.len() / 2` stereo frames at the given tempo multiplier.
    pub fn render(&mut self, out: &mut [f32], rate: f64) {
        let n = out.len() / 2;
        let Some(track) = self.track.clone() else {
            out.fill(0.0);
            return;
        };
        let ratio = track.sample_rate as f64 / self.out_sr;
        let frames = track.frames as f64;

        let v_target = if self.touched && self.vinyl {
            // Chase the platter position: smooth, inertia-free scratching.
            let lag = (0.012 * self.out_sr).max(n as f64);
            ((self.scratch_target - self.pos) / lag).clamp(-12.0 * ratio, 12.0 * ratio)
        } else if self.audible() {
            (rate + self.bend) * ratio
        } else {
            0.0
        };
        self.applied_rate = if self.audible() { rate + self.bend } else { 0.0 };
        // Bend decays like a released platter.
        self.bend *= (-(n as f64) / (0.08 * self.out_sr)).exp();
        if self.bend.abs() < 1e-4 {
            self.bend = 0.0;
        }

        let stretch = self.keylock && !(self.touched && self.vinyl) && v_target > 0.0;
        if stretch && !self.stretch_active {
            self.stretcher.reset();
        }
        self.stretch_active = stretch;

        let amp_coef = 1.0 - (-1.0 / (0.004 * self.out_sr as f32)).exp();
        let v0 = self.v_prev;
        for i in 0..n {
            let t = (i + 1) as f64 / n as f64;
            let v = v0 + (v_target - v0) * t;

            let (mut l, mut r) = if stretch {
                // The stretcher's overlap-add already crossfades jumps and loop wraps.
                self.fade_left = 0;
                self.stretcher.next(&track, self.pos, ratio, Self::read)
            } else {
                Self::read(&track, self.pos)
            };
            if self.fade_left > 0 {
                let (fl, fr) = Self::read(&track, self.fade_from);
                let w = self.fade_left as f32 / FADE_FRAMES as f32;
                l = l * (1.0 - w) + fl * w;
                r = r * (1.0 - w) + fr * w;
                self.fade_from += v;
                self.fade_left -= 1;
            }
            let moving = if v.abs() > 1e-4 { 1.0 } else { 0.0 };
            self.amp += amp_coef * (moving - self.amp);
            out[2 * i] = l * self.amp;
            out[2 * i + 1] = r * self.amp;

            self.pos += v;
            if self.loop_active && v > 0.0 {
                if let (Some(a), Some(b)) = (self.loop_in, self.loop_out) {
                    if self.pos >= b {
                        self.fade_from = self.pos;
                        self.fade_left = FADE_FRAMES;
                        self.pos = a + (self.pos - b).min(b - a);
                        self.scratch_target -= b - a;
                    }
                }
            }
            if self.pos >= frames {
                self.pos = frames;
                self.playing = false;
                self.preview = Preview::None;
            } else if self.pos < 0.0 {
                self.pos = 0.0;
            }
        }
        self.v_prev = v_target;
        if !(self.touched && self.vinyl) {
            self.scratch_target = self.pos;
        }
        // 33⅓ rpm platter display.
        self.platter_rev = (self.pos / track.sample_rate as f64) / 1.8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn deck_with_ramp(secs: f64) -> Deck {
        let sr = 48000u32;
        let n = (secs * sr as f64) as usize;
        let mut s = Vec::with_capacity(n * 2);
        for i in 0..n {
            let v = 0.5 * ((i as f32) * 0.05).sin() + 0.01;
            s.push(v);
            s.push(v);
        }
        let t = Track::from_samples(PathBuf::from("ramp.wav"), sr, s);
        let mut d = Deck::new(sr as f64);
        d.load(Arc::new(t), 1);
        d
    }

    fn run(d: &mut Deck, secs: f64) {
        let mut buf = vec![0f32; 512 * 2];
        let blocks = (secs * 48000.0 / 512.0) as usize;
        for _ in 0..blocks {
            let r = d.fader_rate();
            d.render(&mut buf, r);
        }
    }

    #[test]
    fn plays_in_real_time() {
        let mut d = deck_with_ramp(10.0);
        d.play_pressed();
        run(&mut d, 1.0);
        let secs = d.pos / 48000.0;
        assert!((secs - 1.0).abs() < 0.02, "{secs}");
    }

    #[test]
    fn tempo_fader_changes_speed() {
        let mut d = deck_with_ramp(10.0);
        d.tempo_fader = 1.0; // +10 %
        d.play_pressed();
        run(&mut d, 1.0);
        assert!((d.pos / 48000.0 - 1.1).abs() < 0.02);
    }

    #[test]
    fn cdj_cue_behaviour() {
        let mut d = deck_with_ramp(10.0);
        let start = d.cue;
        d.play_pressed();
        run(&mut d, 0.5);
        // CUE while playing: back to cue point, paused.
        d.cue_button(true);
        d.cue_button(false);
        run(&mut d, 0.05);
        assert!(!d.playing);
        assert!((d.pos - start).abs() < 1.0);
        // Hold CUE at the cue point: previews, release returns.
        d.cue_button(true);
        run(&mut d, 0.3);
        assert!(d.pos > start + 0.2 * 48000.0);
        d.cue_button(false);
        run(&mut d, 0.05);
        assert!((d.pos - start).abs() < 1.0);
        // Hold CUE, press PLAY, release CUE: keeps playing.
        d.cue_button(true);
        d.play_pressed();
        d.cue_button(false);
        run(&mut d, 0.2);
        assert!(d.playing && d.pos > start + 0.1 * 48000.0);
        // Pause away from the cue and press CUE: sets a new cue point.
        d.play_pressed();
        let here = d.pos;
        d.cue_button(true);
        d.cue_button(false);
        assert!((d.cue - here).abs() < 1.0);
    }

    #[test]
    fn hot_cue_set_and_recall() {
        let mut d = deck_with_ramp(10.0);
        d.play_pressed();
        run(&mut d, 1.0);
        d.hot_cue(0, true);
        d.hot_cue(0, false);
        let h = d.hot[0].unwrap().pos;
        run(&mut d, 1.0);
        d.hot_cue(0, true);
        d.hot_cue(0, false);
        assert!(d.playing);
        assert!((d.pos - h).abs() < 1.0);
    }

    #[test]
    fn master_tempo_keeps_time_moving() {
        let mut d = deck_with_ramp(10.0);
        d.keylock = true;
        d.tempo_fader = -1.0; // -10 %
        d.play_pressed();
        run(&mut d, 1.0);
        assert!((d.pos / 48000.0 - 0.9).abs() < 0.02, "{}", d.pos / 48000.0);
    }

    #[test]
    fn rekordbox_cues_and_grid_are_applied() {
        use crate::rekordbox::anlz::{Analysis, Beat, Cue};
        use crate::track::TrackMeta;
        let sr = 48000u32;
        let s = vec![0.1f32; sr as usize * 2 * 20];
        let mut a = Analysis::default();
        // 120 BPM grid starting on beat 2 of the bar, at 250 ms.
        a.beats = (0..36).map(|k| Beat { number: ((k + 1) % 4 + 1) as u16, tempo: 12000, time_ms: 250 + k * 500 }).collect();
        a.memory_cues = vec![Cue { hot: 0, time_ms: 750, loop_end_ms: None, color: None, comment: String::new() }];
        a.hot_cues = vec![Cue { hot: 2, time_ms: 2250, loop_end_ms: Some(4250), color: Some([1, 2, 3]), comment: String::new() }];
        let meta = TrackMeta { analysis: Some(a), ..Default::default() };
        let t = Track::from_parts(PathBuf::from("rb.wav"), sr, s, meta);
        assert_eq!(t.bpm, Some(120.0));
        assert_eq!(t.bar_phase, 1);
        assert!(t.is_downbeat(3));
        let mut d = Deck::new(sr as f64);
        d.load(Arc::new(t), 1);
        assert!((d.cue - 0.75 * 48000.0).abs() < 1.0, "cue at first memory cue");
        assert!((d.pos - d.cue).abs() < 1.0);
        let h = d.hot[1].unwrap();
        assert_eq!(h.color, Some([1, 2, 3]));
        // Pressing the loop hot cue engages the saved loop.
        d.play_pressed();
        d.hot_cue(1, true);
        d.hot_cue(1, false);
        run(&mut d, 5.0);
        assert!(d.loop_active);
        assert!(d.pos >= 2.25 * 48000.0 - 1.0 && d.pos <= 4.25 * 48000.0 + 1.0, "{}", d.pos);
        // Beat jump follows the grid: 4 beats = 2 s.
        d.reloop_exit();
        let before = d.pos;
        d.beat_jump(4.0);
        assert!((d.pos - before - 2.0 * 48000.0).abs() < 2.0);
    }

    #[test]
    fn loop_wraps() {
        let mut d = deck_with_ramp(10.0);
        d.play_pressed();
        run(&mut d, 1.0);
        d.loop_in_pressed();
        let a = d.pos;
        run(&mut d, 0.5);
        d.loop_out_pressed();
        let b = d.loop_out.unwrap();
        run(&mut d, 3.0);
        assert!(d.pos >= a && d.pos <= b, "pos {} not in [{a}, {b}]", d.pos);
        d.reloop_exit();
        run(&mut d, 1.0);
        assert!(d.pos > b);
    }

    #[test]
    fn touching_platter_stops_and_scratch_moves() {
        let mut d = deck_with_ramp(10.0);
        d.play_pressed();
        run(&mut d, 1.0);
        d.jog_touch(true);
        run(&mut d, 0.1);
        let held = d.pos;
        run(&mut d, 0.2);
        assert!((d.pos - held).abs() < 50.0, "platter held should stop");
        // forward one revolution = 1.8 s
        for _ in 0..720 {
            d.jog_scratch(1.0);
        }
        run(&mut d, 0.5);
        assert!(((d.pos - held) / 48000.0 - 1.8).abs() < 0.05);
        d.jog_touch(false);
        run(&mut d, 0.5);
        assert!(d.pos > held + 1.8 * 48000.0 + 0.4 * 48000.0);
    }
}
