//! One CDJ-style player: transport, cue/hot-cue/memory-cue behaviour, loops, quantize,
//! slip, reverse, jog and tempo.

use std::sync::Arc;

use super::dsp::hermite;
use super::stretch::Stretcher;
use crate::track::Track;

pub const HOT_CUES: usize = 8;
pub const MEMORY_CUES: usize = 32;
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
    pub memory: [Option<f64>; MEMORY_CUES],
    pub preview: Preview,
    pub loop_in: Option<f64>,
    pub loop_out: Option<f64>,
    pub loop_active: bool,
    /// Tempo fader, -1 (top, slower) ..= +1 (bottom, faster), like a CDJ.
    pub tempo_fader: f64,
    /// Tempo kept after SYNC is switched off, until the fader is moved through it.
    pub tempo_hold: Option<f64>,
    pub range_idx: usize,
    pub synced: bool,
    pub vinyl: bool,
    /// Master Tempo: keep the key when the tempo changes.
    pub keylock: bool,
    pub quantize: bool,
    pub slip: bool,
    /// Where the track would be without the current slip action (scratch, loop, reverse…).
    pub slip_pos: Option<f64>,
    pub reverse: bool,
    pub touched: bool,
    /// Rate actually applied last block (1.0 = original speed), for display.
    pub applied_rate: f64,
    /// Platter angle in revolutions, for the on-screen jog display.
    pub platter_rev: f64,
    /// Seconds of the pitch-bend/scratch currently applied (for sync's phase lock to back off).
    pub bending: bool,

    slip_hot: Option<usize>,
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
            memory: [None; MEMORY_CUES],
            preview: Preview::None,
            loop_in: None,
            loop_out: None,
            loop_active: false,
            tempo_fader: 0.0,
            tempo_hold: None,
            range_idx: DEFAULT_RANGE,
            synced: false,
            vinyl: true,
            keylock: false,
            quantize: true,
            slip: false,
            slip_pos: None,
            reverse: false,
            touched: false,
            applied_rate: 1.0,
            platter_rev: 0.0,
            bending: false,
            slip_hot: None,
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

    pub fn set_output_rate(&mut self, out_sr: f64) {
        self.out_sr = out_sr;
        self.stretch_active = false;
    }

    fn sr(&self) -> f64 {
        self.track.as_ref().map(|t| t.sample_rate as f64).unwrap_or(44100.0)
    }

    /// Length used for seeking (expected length while the track is still decoding).
    fn length(&self) -> f64 {
        self.track.as_ref().map(|t| t.length() as f64).unwrap_or(0.0)
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

    /// Snap to the nearest beat when quantize is on.
    fn q(&self, pos: f64) -> f64 {
        match (&self.track, self.quantize) {
            (Some(t), true) => t.snap(pos),
            _ => pos,
        }
    }

    /// Where to land when jumping to `target` while playing: with quantize, keep the current
    /// position within the beat so the jump stays in time.
    fn in_time(&self, target: f64) -> f64 {
        if !(self.quantize && self.playing) {
            return target;
        }
        match (self.beat_at(self.pos), self.beat_at(target)) {
            (Some(now), Some(t)) => self.pos_of_beat(t.round() + (now - now.round())).unwrap_or(target),
            _ => target,
        }
    }

    /// Tempo multiplier from the fader alone (or the held tempo after SYNC off).
    pub fn fader_rate(&self) -> f64 {
        self.tempo_hold.unwrap_or(1.0 + self.tempo_fader * TEMPO_RANGES[self.range_idx])
    }

    /// Move the tempo fader. A held tempo is released once the fader reaches it.
    pub fn set_tempo_fader(&mut self, v: f64) {
        let v = v.clamp(-1.0, 1.0);
        if let Some(h) = self.tempo_hold {
            let range = TEMPO_RANGES[self.range_idx];
            let before = 1.0 + self.tempo_fader * range - h;
            let after = 1.0 + v * range - h;
            if before.signum() != after.signum() || after.abs() < 0.0015 {
                self.tempo_hold = None;
            }
        }
        self.tempo_fader = v;
    }

    pub fn audible(&self) -> bool {
        self.playing || self.preview != Preview::None
    }

    pub fn at_cue(&self) -> bool {
        (self.pos - self.cue).abs() < AT_CUE_SECS * self.sr()
    }

    /// Install a new track; returns the old one so it can be freed off the audio thread.
    pub fn load(&mut self, track: Arc<Track>, generation: u64) -> Option<Arc<Track>> {
        // Auto-cue to the first audible sample (within what has been decoded), like a CDJ.
        let decoded = track.frames();
        let first = (0..decoded)
            .find(|&i| {
                let (l, r) = track.audio.frame(i);
                l.abs().max(r.abs()) > 0.003
            })
            .unwrap_or(0) as f64;
        let mut hot = [None; HOT_CUES];
        let mut memory = [None; MEMORY_CUES];
        let mut m = 0;
        for c in &track.cues {
            match c.hot {
                Some(i) if i < HOT_CUES => hot[i] = Some(HotCue { pos: c.pos, loop_end: c.loop_end, color: c.color }),
                Some(_) => {}
                None if m < MEMORY_CUES => {
                    memory[m] = Some(c.pos);
                    m += 1;
                }
                None => {}
            }
        }
        let cue = track.initial_cue.unwrap_or(first);
        let old = self.track.replace(track);
        self.track_gen = generation;
        self.playing = false;
        self.preview = Preview::None;
        self.hot = hot;
        self.memory = memory;
        self.loop_in = None;
        self.loop_out = None;
        self.loop_active = false;
        self.synced = false;
        self.reverse = false;
        self.slip_pos = None;
        self.slip_hot = None;
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
        let to = to.clamp(0.0, self.length().max(0.0));
        if self.amp > 0.001 {
            self.fade_from = self.pos;
            self.fade_left = FADE_FRAMES;
        }
        self.pos = to;
        self.scratch_target = to;
    }

    /// Remember where the track "really" is before a slip action starts.
    fn slip_begin(&mut self) {
        if self.slip && self.playing && self.slip_pos.is_none() {
            self.slip_pos = Some(self.pos);
        }
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
        if !self.playing {
            self.slip_pos = None;
        }
    }

    /// CDJ cue: while playing, return to cue and pause; while paused away from the cue point,
    /// set a new cue (snapped with quantize); while paused on it, play while held.
    pub fn cue_button(&mut self, pressed: bool) {
        if self.track.is_none() {
            return;
        }
        if pressed {
            if self.playing {
                self.playing = false;
                self.slip_pos = None;
                self.stop_at(self.cue);
            } else if !self.at_cue() {
                self.cue = self.q(self.pos);
                self.stop_at(self.cue);
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
        self.slip_pos = None;
        self.stop_at(0.0);
    }

    pub fn hot_cue(&mut self, i: usize, pressed: bool) {
        if self.track.is_none() || i >= HOT_CUES {
            return;
        }
        if pressed {
            match self.hot[i] {
                None => {
                    let p = self.q(self.pos);
                    self.hot[i] = Some(HotCue { pos: p, loop_end: None, color: None });
                }
                Some(h) => {
                    if self.slip && self.playing {
                        // Slip hot cue: momentary, the track carries on underneath.
                        self.slip_begin();
                        self.slip_hot = Some(i);
                    }
                    if let Some(e) = h.loop_end {
                        self.loop_in = Some(h.pos);
                        self.loop_out = Some(e);
                        self.loop_active = true;
                    } else if self.loop_active
                        && !matches!((self.loop_in, self.loop_out), (Some(a), Some(b)) if h.pos >= a && h.pos < b)
                    {
                        self.loop_active = false;
                    }
                    let to = if h.loop_end.is_some() { h.pos } else { self.in_time(h.pos) };
                    self.jump(to);
                    if !self.playing {
                        self.preview = Preview::Hot(i);
                    }
                }
            }
        } else {
            if self.slip_hot == Some(i) {
                self.slip_hot = None;
            }
            if self.preview == Preview::Hot(i) {
                self.preview = Preview::None;
                if let Some(h) = self.hot[i] {
                    self.stop_at(h.pos);
                }
            }
        }
    }

    pub fn hot_cue_delete(&mut self, i: usize) {
        if i < HOT_CUES {
            self.hot[i] = None;
        }
    }

    pub fn seek_fraction(&mut self, frac: f64) {
        let to = frac.clamp(0.0, 1.0) * self.length();
        self.slip_pos = None;
        self.jump(to);
        if !self.playing {
            self.cue = self.pos;
        }
    }

    // ---- memory cues -------------------------------------------------------------------------

    /// MEMORY: store the current cue point (or playhead while playing).
    pub fn memory_add(&mut self) {
        if self.track.is_none() {
            return;
        }
        let at = if self.playing { self.q(self.pos) } else { self.cue };
        let tol = 0.01 * self.sr();
        if self.memory.iter().flatten().any(|m| (m - at).abs() < tol) {
            return;
        }
        if let Some(slot) = self.memory.iter_mut().find(|m| m.is_none()) {
            *slot = Some(at);
        }
    }

    /// SHIFT + MEMORY: delete the memory cue at (or nearest before) the playhead.
    pub fn memory_delete(&mut self) {
        let pos = self.pos;
        let tol = 0.05 * self.sr();
        let idx = self
            .memory
            .iter()
            .enumerate()
            .filter_map(|(i, m)| m.map(|m| (i, m)))
            .filter(|&(_, m)| m <= pos + tol)
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i);
        if let Some(i) = idx {
            self.memory[i] = None;
        }
    }

    /// CUE/LOOP CALL: go to the next (+1) or previous (−1) memory cue and stand by on it.
    pub fn call(&mut self, dir: i32) {
        let tol = 0.02 * self.sr();
        let pos = self.pos;
        let next = self.memory.iter().flatten().copied().filter(|&m| if dir > 0 { m > pos + tol } else { m < pos - tol });
        let target = if dir > 0 { next.min_by(|a, b| a.total_cmp(b)) } else { next.max_by(|a, b| a.total_cmp(b)) };
        if let Some(m) = target {
            self.playing = false;
            self.preview = Preview::None;
            self.slip_pos = None;
            self.cue = m;
            self.stop_at(m);
        }
    }

    // ---- loops -------------------------------------------------------------------------------

    pub fn loop_in_pressed(&mut self) {
        if self.track.is_none() {
            return;
        }
        self.loop_in = Some(self.q(self.pos));
        self.loop_out = None;
        self.loop_active = false;
    }

    pub fn loop_out_pressed(&mut self) {
        if let Some(a) = self.loop_in {
            let out = self.q(self.pos);
            if out > a + 0.02 * self.sr() {
                self.slip_begin();
                self.loop_out = Some(out);
                self.loop_active = true;
                if self.pos >= out {
                    let to = a + (self.pos - out);
                    self.jump(to);
                }
            }
        }
    }

    pub fn reloop_exit(&mut self) {
        if self.loop_active {
            self.loop_active = false;
        } else if let (Some(a), Some(_)) = (self.loop_in, self.loop_out) {
            self.slip_begin();
            self.loop_active = true;
            let to = self.in_time(a);
            self.jump(to);
        }
    }

    pub fn auto_loop(&mut self, beats: f64) {
        let Some(b) = self.beat_at(self.pos) else { return };
        let start_beat = if self.quantize || (b - b.round()).abs() < 0.25 { b.round() } else { b };
        let (Some(start), Some(end)) = (self.pos_of_beat(start_beat), self.pos_of_beat(start_beat + beats)) else {
            return;
        };
        self.slip_begin();
        self.loop_in = Some(start);
        self.loop_out = Some(end);
        self.loop_active = true;
        if self.pos >= end || self.pos < start {
            let to = start + (self.pos - start).rem_euclid(end - start);
            self.jump(to);
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
        if let Some(s) = self.slip_pos.as_mut() {
            *s += d;
        }
        self.jump(self.pos + d);
    }

    // ---- slip / reverse ----------------------------------------------------------------------

    pub fn set_slip(&mut self, on: bool) {
        self.slip = on;
        if !on {
            self.slip_pos = None;
            self.slip_hot = None;
        }
    }

    pub fn set_reverse(&mut self, on: bool) {
        if on && !self.reverse {
            self.slip_begin();
        }
        self.reverse = on;
    }

    // ---- jog ---------------------------------------------------------------------------------

    pub fn jog_touch(&mut self, touched: bool) {
        if touched && self.vinyl {
            self.slip_begin();
        }
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
            let to = (self.pos + ticks * SCRATCH_SECS_PER_TICK * 0.5 * self.sr()).clamp(0.0, self.length());
            self.pos = to;
            self.scratch_target = to;
            if !self.playing {
                self.cue = self.pos;
            }
        }
    }

    pub fn jog_search(&mut self, ticks: f64) {
        if self.track.is_some() {
            let to = (self.pos + ticks * SEARCH_SECS_PER_TICK * self.sr()).clamp(0.0, self.length());
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
    pub(crate) fn read(t: &Track, pos: f64) -> (f32, f32) {
        let i = pos.floor();
        let f = (pos - i) as f32;
        let i = i as i64;
        let at = |k: i64| if k < 0 { (0.0, 0.0) } else { t.audio.frame(k as usize) };
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
        let dir = if self.reverse { -1.0 } else { 1.0 };
        let scratching = self.touched && self.vinyl;

        let v_target = if scratching {
            // Chase the platter position: smooth, inertia-free scratching.
            let lag = (0.012 * self.out_sr).max(n as f64);
            ((self.scratch_target - self.pos) / lag).clamp(-12.0 * ratio, 12.0 * ratio)
        } else if self.audible() {
            dir * (rate + self.bend) * ratio
        } else {
            0.0
        };
        self.bending = scratching || self.bend.abs() > 1e-3;
        self.applied_rate = if self.audible() { rate + self.bend } else { 0.0 };
        // Bend decays like a released platter.
        self.bend *= (-(n as f64) / (0.08 * self.out_sr)).exp();
        if self.bend.abs() < 1e-4 {
            self.bend = 0.0;
        }

        // Slip: the shadow playhead keeps moving at normal speed; jump back when the
        // slip action (scratch, reverse, loop, momentary hot cue) ends.
        if let Some(s) = self.slip_pos.as_mut() {
            if self.playing {
                *s += rate * ratio * n as f64;
            }
            let active = scratching || self.reverse || self.loop_active || self.slip_hot.is_some();
            if !active {
                let to = *s;
                self.slip_pos = None;
                self.jump(to);
            }
        }

        let stretch = self.keylock && !scratching && v_target > 0.0;
        if stretch && !self.stretch_active {
            self.stretcher.reset();
        }
        self.stretch_active = stretch;

        let complete = track.is_complete();
        let frames = track.frames() as f64;
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
            if self.loop_active {
                if let (Some(a), Some(b)) = (self.loop_in, self.loop_out) {
                    let len = b - a;
                    if v > 0.0 && self.pos >= b {
                        self.fade_from = self.pos;
                        self.fade_left = FADE_FRAMES;
                        self.pos = a + (self.pos - b).min(len);
                        self.scratch_target -= len;
                    } else if v < 0.0 && self.pos < a && self.pos > a - len {
                        self.fade_from = self.pos;
                        self.fade_left = FADE_FRAMES;
                        self.pos += len;
                        self.scratch_target += len;
                    }
                }
            }
            if complete && self.pos >= frames {
                self.pos = frames;
                self.playing = false;
                self.preview = Preview::None;
                self.slip_pos = None;
            } else if self.pos < 0.0 {
                self.pos = 0.0;
                if self.reverse {
                    self.playing = false;
                }
            }
        }
        self.v_prev = v_target;
        if !scratching {
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
        d.quantize = false;
        d.load(Arc::new(t), 1);
        d
    }

    /// 120 BPM grid from 0, so a beat is exactly 24000 frames.
    fn deck_with_grid(secs: f64) -> Deck {
        let mut d = deck_with_ramp(secs);
        let t = d.track.clone().unwrap();
        t.grid.store(Some(Arc::new(crate::track::Grid::constant(
            120.0,
            0.0,
            48000,
            t.frames(),
            crate::track::GridSource::Saved,
        ))));
        d.quantize = true;
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
        d.set_tempo_fader(1.0); // +10 %
        d.play_pressed();
        run(&mut d, 1.0);
        assert!((d.pos / 48000.0 - 1.1).abs() < 0.02);
    }

    #[test]
    fn held_tempo_released_when_fader_crosses_it() {
        let mut d = deck_with_ramp(10.0);
        d.tempo_hold = Some(1.05);
        d.set_tempo_fader(0.2); // fader says +2 %: still held at +5 %
        assert_eq!(d.fader_rate(), 1.05);
        d.set_tempo_fader(0.6); // passes +5 %: fader takes over
        assert!(d.tempo_hold.is_none());
        assert!((d.fader_rate() - 1.06).abs() < 1e-9);
    }

    #[test]
    fn cdj_cue_behaviour() {
        let mut d = deck_with_ramp(10.0);
        let start = d.cue;
        d.play_pressed();
        run(&mut d, 0.5);
        d.cue_button(true);
        d.cue_button(false);
        run(&mut d, 0.05);
        assert!(!d.playing);
        assert!((d.pos - start).abs() < 1.0);
        d.cue_button(true);
        run(&mut d, 0.3);
        assert!(d.pos > start + 0.2 * 48000.0);
        d.cue_button(false);
        run(&mut d, 0.05);
        assert!((d.pos - start).abs() < 1.0);
        d.cue_button(true);
        d.play_pressed();
        d.cue_button(false);
        run(&mut d, 0.2);
        assert!(d.playing && d.pos > start + 0.1 * 48000.0);
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
    fn quantize_snaps_cues_loops_and_keeps_phase() {
        let mut d = deck_with_grid(20.0);
        d.play_pressed();
        run(&mut d, 1.1); // ~52800 frames: between beats 2 and 3
        d.hot_cue(0, true);
        d.hot_cue(0, false);
        assert_eq!(d.hot[0].unwrap().pos, 48000.0, "hot cue snapped to beat");
        run(&mut d, 2.3);
        let phase_before = (d.pos / 24000.0).fract();
        d.hot_cue(0, true);
        d.hot_cue(0, false);
        let phase_after = (d.pos / 24000.0).fract();
        assert!((phase_before - phase_after).abs() < 1e-6, "jump kept beat phase");
        d.loop_in_pressed();
        assert_eq!(d.loop_in.unwrap() % 24000.0, 0.0);
    }

    #[test]
    fn memory_cues_add_call_delete() {
        let mut d = deck_with_grid(30.0);
        for secs in [2.0, 6.0] {
            d.seek_fraction(secs / 30.0);
            d.cue_button(true);
            d.cue_button(false);
            d.memory_add();
        }
        assert_eq!(d.memory.iter().flatten().count(), 2);
        d.seek_fraction(0.0);
        d.call(1);
        assert!((d.pos - 2.0 * 48000.0).abs() < 1.0 && !d.playing);
        d.call(1);
        assert!((d.pos - 6.0 * 48000.0).abs() < 1.0);
        d.call(-1);
        assert!((d.pos - 2.0 * 48000.0).abs() < 1.0);
        d.memory_delete();
        assert_eq!(d.memory.iter().flatten().count(), 1);
    }

    #[test]
    fn slip_returns_to_where_the_track_would_be() {
        let mut d = deck_with_ramp(20.0);
        d.set_slip(true);
        d.play_pressed();
        run(&mut d, 1.0);
        d.set_reverse(true);
        run(&mut d, 1.0);
        assert!(d.pos < 1.0 * 48000.0, "played backwards");
        d.set_reverse(false);
        run(&mut d, 0.02);
        // 93 + 93 + 1 blocks of 512 frames have elapsed in "track time".
        let expect = (93.0 + 93.0 + 1.0) * 512.0 / 48000.0;
        assert!((d.pos / 48000.0 - expect).abs() < 0.01, "{} vs {expect}", d.pos / 48000.0);
    }

    #[test]
    fn reverse_plays_backwards_and_stops_at_start() {
        let mut d = deck_with_ramp(10.0);
        d.play_pressed();
        run(&mut d, 1.0);
        d.set_reverse(true);
        run(&mut d, 2.0);
        assert_eq!(d.pos, 0.0);
        assert!(!d.playing);
    }

    #[test]
    fn master_tempo_keeps_time_moving() {
        let mut d = deck_with_ramp(10.0);
        d.keylock = true;
        d.set_tempo_fader(-1.0); // -10 %
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
        a.beats = (0..36).map(|k| Beat { number: ((k + 1) % 4 + 1) as u16, tempo: 12000, time_ms: 250 + k * 500 }).collect();
        a.memory_cues = vec![Cue { hot: 0, time_ms: 750, loop_end_ms: None, color: None, comment: String::new() }];
        a.hot_cues = vec![Cue { hot: 2, time_ms: 2250, loop_end_ms: Some(4250), color: Some([1, 2, 3]), comment: String::new() }];
        let meta = TrackMeta { analysis: Some(a), ..Default::default() };
        let t = Track::from_parts(PathBuf::from("rb.wav"), sr, s, meta);
        assert_eq!(t.bpm(), Some(120.0));
        assert!(t.grid.load().as_ref().unwrap().is_downbeat(3));
        let mut d = Deck::new(sr as f64);
        d.load(Arc::new(t), 1);
        assert!((d.cue - 0.75 * 48000.0).abs() < 1.0, "cue at first memory cue");
        assert_eq!(d.memory.iter().flatten().count(), 1);
        let h = d.hot[1].unwrap();
        assert_eq!(h.color, Some([1, 2, 3]));
        d.play_pressed();
        d.hot_cue(1, true);
        d.hot_cue(1, false);
        run(&mut d, 5.0);
        assert!(d.loop_active);
        assert!(d.pos >= 2.25 * 48000.0 - 1.0 && d.pos <= 4.25 * 48000.0 + 1.0, "{}", d.pos);
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
    fn keeps_playing_while_the_track_is_still_decoding() {
        let sr = 48000u32;
        let t = Arc::new(Track::partial_for_test(sr, sr as usize * 10));
        t.audio.push(&vec![0.2f32; sr as usize * 2]); // 1 s decoded of 10
        let mut d = Deck::new(sr as f64);
        d.quantize = false;
        d.load(t.clone(), 1);
        d.play_pressed();
        run(&mut d, 2.0);
        assert!(d.playing, "must not stop at the decode frontier");
        assert!((d.pos / 48000.0 - 2.0).abs() < 0.05);
        t.audio.push(&vec![0.2f32; sr as usize * 2 * 2]);
        t.audio.finish(); // ends at 3 s
        run(&mut d, 2.0);
        assert!(!d.playing);
        assert_eq!(d.pos, 3.0 * 48000.0);
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
