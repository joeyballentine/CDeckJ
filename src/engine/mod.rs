//! The realtime engine. Driven by the audio callback; controlled through [`Cmd`]s and observed
//! through a published [`EngineView`] snapshot.

pub mod deck;
pub mod dsp;
pub mod fx;
pub mod mixer;
pub mod stretch;

use std::sync::Arc;
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender};
use parking_lot::Mutex;

use crate::track::Track;
use deck::{Deck, HotCue, Preview, HOT_CUES, MEMORY_CUES, TEMPO_RANGES};
use mixer::{Mixer, MixerParams};

pub const DECKS: usize = 2;
const MAX_BLOCK: usize = 1024;
/// Sync's phase lock: seconds to pull a phase error back in, and the speed limit for it.
const PHASE_LOCK_SECS: f64 = 1.5;
const PHASE_LOCK_MAX: f64 = 0.01;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MixParam {
    Trim(usize, f32),
    Hi(usize, f32),
    Mid(usize, f32),
    Low(usize, f32),
    Filter(usize, f32),
    Fader(usize, f32),
    PflToggle(usize),
    Crossfader(f32),
    HeadMix(f32),
    HeadLevel(f32),
    Master(f32),
    FaderCurve(mixer::FaderCurve),
    CrossfaderCurve(mixer::CrossfaderCurve),
    FxKind(fx::FxKind),
    /// Step through the effects (FX SELECT).
    FxNext(i32),
    /// Step the beat value (BEAT ◀ ▶).
    FxBeat(i32),
    FxBeatIdx(usize),
    FxTarget(fx::FxTarget),
    FxLevel(f32),
    FxToggle,
    FxQuantize(bool),
}

/// Everything the UI, MIDI, gamepad and keyboard can ask the engine to do.
#[derive(Debug)]
pub enum Cmd {
    Load { deck: usize, track: Arc<Track>, generation: u64 },
    Play(usize),
    Cue(usize, bool),
    ToStart(usize),
    HotCue(usize, usize, bool),
    HotCueDelete(usize, usize),
    MemoryAdd(usize),
    MemoryDelete(usize),
    Call(usize, i32),
    JogTouch(usize, bool),
    JogScratch(usize, f64),
    JogBend(usize, f64),
    JogSearch(usize, f64),
    /// -1 (top) ..= +1 (bottom).
    TempoFader(usize, f64),
    TempoRangeCycle(usize),
    TempoRange(usize, usize),
    SyncToggle(usize),
    VinylToggle(usize),
    Vinyl(usize, bool),
    MasterTempoToggle(usize),
    MasterTempo(usize, bool),
    SlipToggle(usize),
    ReverseToggle(usize),
    Reverse(usize, bool),
    QuantizeToggle,
    Quantize(bool),
    /// Quantize resolution in beats (1, ½, ¼, ⅛).
    QuantizeBeats(f64),
    LoopIn(usize),
    LoopOut(usize),
    ReloopExit(usize),
    AutoLoop(usize, f64),
    LoopScale(usize, f64),
    BeatJump(usize, f64),
    Seek(usize, f64),
    Mix(MixParam),
}

/// How the headphone (cue) bus reaches the speakers/phones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CueRouting {
    /// 4+ channel device (e.g. the controller's own sound card): master 1/2, phones 3/4.
    Channels34,
    /// Stereo device: left = master (mono), right = cue (mono). Use with a splitter cable.
    SplitMono,
    /// Stereo device, master only.
    MasterOnly,
}

#[derive(Clone, Copy, Debug, Default)]
#[allow(dead_code)]
pub struct DeckView {
    pub track_gen: u64,
    pub loaded: bool,
    pub playing: bool,
    pub previewing: bool,
    pub pos: f64,
    /// Seek length (expected length while still decoding).
    pub length: usize,
    pub decoded: usize,
    pub complete: bool,
    pub sample_rate: u32,
    pub cue: f64,
    pub at_cue: bool,
    pub hot: [Option<HotCue>; HOT_CUES],
    pub memory: [Option<f64>; MEMORY_CUES],
    pub loop_in: Option<f64>,
    pub loop_out: Option<f64>,
    pub loop_active: bool,
    pub tempo_fader: f64,
    pub tempo_held: bool,
    pub range: f64,
    pub range_idx: usize,
    pub rate: f64,
    pub bpm: Option<f64>,
    pub synced: bool,
    pub is_master: bool,
    pub vinyl: bool,
    pub keylock: bool,
    pub slip: bool,
    pub slip_pos: Option<f64>,
    pub reverse: bool,
    pub touched: bool,
    pub platter_rev: f64,
    pub peak: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct EngineView {
    pub decks: [DeckView; DECKS],
    pub mixer: MixerParams,
    pub peak_master: f32,
    pub sample_rate: u32,
    pub channels: u16,
    pub routing: CueRouting,
    pub quantize: bool,
    /// Share of each audio period spent rendering (0..1, decaying peak).
    pub dsp_load: f32,
}

impl EngineView {
    pub fn new(sample_rate: u32, channels: u16, routing: CueRouting) -> Self {
        Self {
            decks: [DeckView { rate: 1.0, range: 0.10, range_idx: deck::DEFAULT_RANGE, vinyl: true, ..DeckView::default() }; DECKS],
            mixer: MixerParams::default(),
            peak_master: 0.0,
            sample_rate,
            channels,
            routing,
            quantize: true,
            dsp_load: 0.0,
        }
    }
}

pub type SharedView = Arc<Mutex<EngineView>>;
/// The engine lives behind a mutex so a new audio stream can take it over after a device
/// drop. Only the audio callback locks it in normal operation, so the lock is uncontended.
pub type SharedEngine = Arc<Mutex<Engine>>;

pub struct Engine {
    decks: [Deck; DECKS],
    mixer: Mixer,
    params: MixerParams,
    quantize: bool,
    cmd_rx: Receiver<Cmd>,
    garbage_tx: Sender<Arc<Track>>,
    view: SharedView,
    channels: usize,
    routing: CueRouting,
    sample_rate: u32,
    deck_buf: [Vec<f32>; DECKS],
    master: Vec<f32>,
    cue: Vec<f32>,
    dsp_load: f32,
    tap: Option<Arc<crate::recorder::Tap>>,
}

impl Engine {
    pub fn new(
        sample_rate: u32,
        channels: u16,
        routing: CueRouting,
        cmd_rx: Receiver<Cmd>,
        garbage_tx: Sender<Arc<Track>>,
        view: SharedView,
    ) -> Self {
        let sr = sample_rate as f64;
        Self {
            decks: [Deck::new(sr), Deck::new(sr)],
            mixer: Mixer::new(sample_rate as f32),
            params: MixerParams::default(),
            quantize: true,
            cmd_rx,
            garbage_tx,
            view,
            channels: channels as usize,
            routing,
            sample_rate,
            deck_buf: [vec![0.0; MAX_BLOCK * 2], vec![0.0; MAX_BLOCK * 2]],
            master: vec![0.0; MAX_BLOCK * 2],
            cue: vec![0.0; MAX_BLOCK * 2],
            dsp_load: 0.0,
            tap: None,
        }
    }

    /// Send the mix to a recorder.
    pub fn set_tap(&mut self, tap: Arc<crate::recorder::Tap>) {
        tap.set_rate(self.sample_rate);
        self.tap = Some(tap);
    }

    /// Re-target the engine at a new output (after reconnecting audio). Deck state is kept.
    pub fn set_output(&mut self, sample_rate: u32, channels: u16, routing: CueRouting) {
        if sample_rate != self.sample_rate {
            self.mixer = Mixer::new(sample_rate as f32);
            for d in self.decks.iter_mut() {
                d.set_output_rate(sample_rate as f64);
            }
        }
        self.sample_rate = sample_rate;
        if let Some(t) = &self.tap {
            t.set_rate(sample_rate);
        }
        self.channels = channels as usize;
        self.routing = routing;
        let mut v = self.view.lock();
        v.sample_rate = sample_rate;
        v.channels = channels;
        v.routing = routing;
    }

    fn other(d: usize) -> usize {
        (d + 1) % DECKS
    }

    /// Which deck is the tempo master: the un-synced one, else deck 1.
    /// Tempo for the beat FX: the sync master if it's playing, else a playing deck that's
    /// audible, else the master deck's tempo with a free-running clock.
    fn fx_clock(&self) -> fx::FxClock {
        let m = self.master_deck();
        let audible = |d: usize| self.decks[d].playing && self.params.ch[d].fader > 0.05;
        let d = if self.decks[m].playing { m } else { (0..DECKS).find(|&d| audible(d)).unwrap_or(m) };
        let deck = &self.decks[d];
        let rate = self.rate_for(d);
        match deck.bpm() {
            Some(b) => fx::FxClock { bpm: b * rate, beat: deck.playing.then(|| deck.beat_at(deck.pos)).flatten() },
            None => fx::FxClock::default(),
        }
    }

    fn master_deck(&self) -> usize {
        (0..DECKS).find(|&d| !self.decks[d].synced).unwrap_or(0)
    }

    /// Tempo multiplier without sync's phase correction.
    fn base_rate(&self, d: usize) -> f64 {
        let deck = &self.decks[d];
        let m = self.master_deck();
        if deck.synced && m != d {
            if let (Some(own), Some(mb)) = (deck.bpm(), self.decks[m].bpm()) {
                return mb * self.decks[m].fader_rate() / own;
            }
        }
        deck.fader_rate()
    }

    /// Tempo multiplier including sync's continuous phase lock.
    fn rate_for(&self, d: usize) -> f64 {
        let base = self.base_rate(d);
        let m = self.master_deck();
        let (deck, master) = (&self.decks[d], &self.decks[m]);
        if !deck.synced || m == d || !deck.playing || !master.playing || deck.bending || master.bending {
            return base;
        }
        let (Some(bm), Some(bd), Some(own)) = (master.beat_at(master.pos), deck.beat_at(deck.pos), deck.bpm()) else {
            return base;
        };
        let mut e = bm.rem_euclid(1.0) - bd.rem_euclid(1.0);
        if e > 0.5 {
            e -= 1.0;
        } else if e < -0.5 {
            e += 1.0;
        }
        if e.abs() < 0.004 {
            return base;
        }
        let corr = (e * 60.0 / own / PHASE_LOCK_SECS).clamp(-PHASE_LOCK_MAX, PHASE_LOCK_MAX);
        base + corr
    }

    fn effective_bpm(&self, d: usize) -> Option<f64> {
        self.decks[d].bpm().map(|b| b * self.base_rate(d))
    }

    fn handle(&mut self, cmd: Cmd) {
        use Cmd::*;
        match cmd {
            Load { deck, track, generation } => {
                if let Some(old) = self.decks[deck].load(track, generation) {
                    // Free the old buffers off the audio thread when possible.
                    if let Err(e) = self.garbage_tx.try_send(old) {
                        drop(e);
                    }
                }
                self.decks[deck].quantize = self.quantize;
            }
            Play(d) => self.decks[d].play_pressed(),
            Cue(d, p) => self.decks[d].cue_button(p),
            ToStart(d) => self.decks[d].to_start(),
            HotCue(d, i, p) => self.decks[d].hot_cue(i, p),
            HotCueDelete(d, i) => self.decks[d].hot_cue_delete(i),
            MemoryAdd(d) => self.decks[d].memory_add(),
            MemoryDelete(d) => self.decks[d].memory_delete(),
            Call(d, dir) => self.decks[d].call(dir),
            JogTouch(d, t) => self.decks[d].jog_touch(t),
            JogScratch(d, t) => self.decks[d].jog_scratch(t),
            JogBend(d, t) => self.decks[d].jog_bend(t),
            JogSearch(d, t) => self.decks[d].jog_search(t),
            TempoFader(d, v) => self.decks[d].set_tempo_fader(v),
            TempoRangeCycle(d) => {
                let deck = &mut self.decks[d];
                deck.range_idx = (deck.range_idx + 1) % TEMPO_RANGES.len();
            }
            TempoRange(d, i) => self.decks[d].range_idx = i.min(TEMPO_RANGES.len() - 1),
            SyncToggle(d) => {
                if self.decks[d].synced {
                    // Keep the synced tempo until the fader is moved through it.
                    let r = self.base_rate(d);
                    self.decks[d].synced = false;
                    if (r - self.decks[d].fader_rate()).abs() > 1e-6 {
                        self.decks[d].tempo_hold = Some(r);
                    }
                } else {
                    self.enable_sync(d);
                }
            }
            VinylToggle(d) => self.decks[d].vinyl = !self.decks[d].vinyl,
            Vinyl(d, on) => self.decks[d].vinyl = on,
            MasterTempoToggle(d) => self.decks[d].keylock = !self.decks[d].keylock,
            MasterTempo(d, on) => self.decks[d].keylock = on,
            SlipToggle(d) => {
                let on = !self.decks[d].slip;
                self.decks[d].set_slip(on);
            }
            ReverseToggle(d) => {
                let on = !self.decks[d].reverse;
                self.decks[d].set_reverse(on);
            }
            Reverse(d, on) => self.decks[d].set_reverse(on),
            QuantizeToggle => self.set_quantize(!self.quantize),
            Quantize(on) => self.set_quantize(on),
            QuantizeBeats(b) => {
                for d in self.decks.iter_mut() {
                    d.quantize_beats = b.clamp(0.125, 1.0);
                }
            }
            LoopIn(d) => self.decks[d].loop_in_pressed(),
            LoopOut(d) => self.decks[d].loop_out_pressed(),
            ReloopExit(d) => self.decks[d].reloop_exit(),
            AutoLoop(d, b) => self.decks[d].auto_loop(b),
            LoopScale(d, k) => self.decks[d].loop_scale(k),
            BeatJump(d, b) => self.decks[d].beat_jump(b),
            Seek(d, f) => self.decks[d].seek_fraction(f),
            Mix(p) => self.set_mix(p),
        }
    }

    fn set_quantize(&mut self, on: bool) {
        self.quantize = on;
        for d in self.decks.iter_mut() {
            d.quantize = on;
        }
    }

    fn enable_sync(&mut self, d: usize) {
        let m = Self::other(d);
        if self.decks[d].bpm().is_none() || self.decks[m].bpm().is_none() {
            return;
        }
        // If the other deck is also synced, it becomes the master.
        self.decks[m].synced = false;
        self.decks[d].synced = true;
        self.decks[d].tempo_hold = None;
        // Beat-align now; the phase lock keeps it there.
        if let (Some(bm), Some(bd)) = (self.decks[m].beat_at(self.decks[m].pos), self.decks[d].beat_at(self.decks[d].pos)) {
            let mut diff = bm.rem_euclid(1.0) - bd.rem_euclid(1.0);
            if diff > 0.5 {
                diff -= 1.0;
            } else if diff < -0.5 {
                diff += 1.0;
            }
            if diff.abs() > 0.005 {
                self.decks[d].nudge_beats(diff);
            }
        }
    }

    fn set_mix(&mut self, p: MixParam) {
        use MixParam::*;
        let m = &mut self.params;
        match p {
            Trim(c, v) => m.ch[c].trim = v,
            Hi(c, v) => m.ch[c].hi = v,
            Mid(c, v) => m.ch[c].mid = v,
            Low(c, v) => m.ch[c].low = v,
            Filter(c, v) => m.ch[c].filter = v,
            Fader(c, v) => m.ch[c].fader = v,
            PflToggle(c) => m.ch[c].pfl = !m.ch[c].pfl,
            Crossfader(v) => m.crossfader = v,
            HeadMix(v) => m.head_mix = v,
            HeadLevel(v) => m.head_level = v,
            Master(v) => m.master = v,
            FaderCurve(c) => m.fader_curve = c,
            CrossfaderCurve(c) => m.crossfader_curve = c,
            FxKind(k) => m.fx.kind = k,
            FxNext(dir) => {
                let n = fx::FX_KINDS.len() as i32;
                let i = fx::FX_KINDS.iter().position(|&k| k == m.fx.kind).unwrap_or(0) as i32;
                m.fx.kind = fx::FX_KINDS[(i + dir).rem_euclid(n) as usize];
            }
            FxBeat(dir) => m.fx.beat_idx = (m.fx.beat_idx as i32 + dir).clamp(0, fx::FX_BEATS.len() as i32 - 1) as usize,
            FxBeatIdx(i) => m.fx.beat_idx = i.min(fx::FX_BEATS.len() - 1),
            FxTarget(t) => m.fx.target = t,
            FxLevel(v) => m.fx.level = v.clamp(0.0, 1.0),
            FxToggle => m.fx.on = !m.fx.on,
            FxQuantize(q) => m.fx.quantize = q,
        }
    }

    /// Fill an interleaved f32 output buffer with `self.channels` channels.
    pub fn process(&mut self, out: &mut [f32]) {
        dsp::no_denormals();
        let t0 = Instant::now();
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            self.handle(cmd);
        }
        let ch = self.channels.max(1);
        let total = out.len() / ch;
        let mut done = 0;
        while done < total {
            let n = (total - done).min(MAX_BLOCK);
            self.render_block(&mut out[done * ch..(done + n) * ch], n);
            done += n;
        }
        let period = total as f64 / self.sample_rate.max(1) as f64;
        if period > 0.0 {
            let load = (t0.elapsed().as_secs_f64() / period) as f32;
            self.dsp_load = load.max(self.dsp_load * 0.97);
        }
        self.publish();
    }

    fn render_block(&mut self, out: &mut [f32], n: usize) {
        let rates = [self.rate_for(0), self.rate_for(1)];
        for d in 0..DECKS {
            self.decks[d].render(&mut self.deck_buf[d][..n * 2], rates[d]);
        }
        let clk = self.fx_clock();
        self.mixer.process_fx(
            &self.params,
            &clk,
            [&self.deck_buf[0][..n * 2], &self.deck_buf[1][..n * 2]],
            &mut self.master[..n * 2],
            &mut self.cue[..n * 2],
        );
        if let Some(t) = &self.tap {
            t.push(self.mixer.rec(n));
        }
        let ch = self.channels.max(1);
        let (m, c) = (&self.master, &self.cue);
        for i in 0..n {
            let frame = &mut out[i * ch..(i + 1) * ch];
            frame.fill(0.0);
            let (ml, mr, cl, cr) = (m[2 * i], m[2 * i + 1], c[2 * i], c[2 * i + 1]);
            match (ch, self.routing) {
                (1, _) => frame[0] = 0.5 * (ml + mr),
                (_, CueRouting::SplitMono) => {
                    frame[0] = 0.5 * (ml + mr);
                    frame[1] = 0.5 * (cl + cr);
                }
                (c, CueRouting::Channels34) if c >= 4 => {
                    frame[0] = ml;
                    frame[1] = mr;
                    frame[2] = cl;
                    frame[3] = cr;
                }
                _ => {
                    frame[0] = ml;
                    frame[1] = mr;
                }
            }
        }
    }

    fn publish(&mut self) {
        let Some(mut v) = self.view.try_lock() else { return };
        let master = self.master_deck();
        for d in 0..DECKS {
            let deck = &self.decks[d];
            let t = deck.track.as_ref();
            v.decks[d] = DeckView {
                track_gen: deck.track_gen,
                loaded: t.is_some(),
                playing: deck.playing,
                previewing: deck.preview != Preview::None,
                pos: deck.pos,
                length: t.map(|t| t.length()).unwrap_or(0),
                decoded: t.map(|t| t.frames()).unwrap_or(0),
                complete: t.map(|t| t.is_complete()).unwrap_or(true),
                sample_rate: t.map(|t| t.sample_rate).unwrap_or(0),
                cue: deck.cue,
                at_cue: deck.at_cue(),
                hot: deck.hot,
                memory: deck.memory,
                loop_in: deck.loop_in,
                loop_out: deck.loop_out,
                loop_active: deck.loop_active,
                tempo_fader: deck.tempo_fader,
                tempo_held: deck.tempo_hold.is_some(),
                range: TEMPO_RANGES[deck.range_idx],
                range_idx: deck.range_idx,
                rate: self.base_rate(d),
                bpm: self.effective_bpm(d),
                synced: deck.synced,
                is_master: master == d && self.decks[Self::other(d)].synced,
                vinyl: deck.vinyl,
                keylock: deck.keylock,
                slip: deck.slip,
                slip_pos: deck.slip_pos,
                reverse: deck.reverse,
                touched: deck.touched,
                platter_rev: deck.platter_rev,
                peak: self.mixer.peak_ch[d].max(v.decks[d].peak * 0.9),
            };
        }
        v.mixer = self.params;
        v.quantize = self.quantize;
        v.peak_master = self.mixer.peak_master.max(v.peak_master * 0.9);
        v.sample_rate = self.sample_rate;
        v.dsp_load = self.dsp_load;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::click_track;
    use crossbeam_channel::{bounded, unbounded};
    use std::path::PathBuf;

    fn engine() -> (Engine, Sender<Cmd>, SharedView) {
        let (tx, rx) = unbounded();
        let (gtx, _grx) = bounded(4);
        let view = Arc::new(Mutex::new(EngineView::new(48000, 4, CueRouting::Channels34)));
        (Engine::new(48000, 4, CueRouting::Channels34, rx, gtx, view.clone()), tx, view)
    }

    fn track(bpm: f64, offset: f64) -> Arc<Track> {
        Arc::new(Track::from_samples(PathBuf::from("t.wav"), 44100, click_track(bpm, offset, 30.0, 44100)))
    }

    fn run(e: &mut Engine, secs: f64) -> Vec<f32> {
        let mut out = vec![0f32; 480 * 4];
        let mut all = Vec::new();
        for _ in 0..(secs * 100.0) as usize {
            e.process(&mut out);
            all.extend_from_slice(&out);
        }
        all
    }

    fn phase_diff(e: &Engine) -> f64 {
        let pa = e.decks[0].beat_at(e.decks[0].pos).unwrap().rem_euclid(1.0);
        let pb = e.decks[1].beat_at(e.decks[1].pos).unwrap().rem_euclid(1.0);
        let d = (pa - pb).abs();
        d.min(1.0 - d)
    }

    #[test]
    fn master_and_cue_routing() {
        let (mut e, tx, _v) = engine();
        tx.send(Cmd::Load { deck: 0, track: track(124.0, 0.1), generation: 1 }).unwrap();
        tx.send(Cmd::Play(0)).unwrap();
        let out = run(&mut e, 1.0);
        let master: f32 = out.chunks(4).map(|f| f[0].abs()).sum();
        let cue: f32 = out.chunks(4).map(|f| f[2].abs()).sum();
        assert!(master > 10.0);
        assert!(cue > 1.0, "no PFL: phones follow master");
        tx.send(Cmd::Mix(MixParam::PflToggle(0))).unwrap();
        tx.send(Cmd::Mix(MixParam::Fader(0, 0.0))).unwrap();
        run(&mut e, 0.1);
        let out = run(&mut e, 1.0);
        let master: f32 = out.chunks(4).map(|f| f[0].abs()).sum();
        let cue: f32 = out.chunks(4).map(|f| f[2].abs()).sum();
        assert!(master < 0.01, "master {master}");
        assert!(cue > 10.0);
    }

    #[test]
    fn sync_matches_tempo_and_holds_phase() {
        let (mut e, tx, view) = engine();
        tx.send(Cmd::Load { deck: 0, track: track(124.0, 0.1), generation: 1 }).unwrap();
        tx.send(Cmd::Load { deck: 1, track: track(128.0, 0.3), generation: 2 }).unwrap();
        tx.send(Cmd::Play(0)).unwrap();
        tx.send(Cmd::Play(1)).unwrap();
        run(&mut e, 2.0);
        tx.send(Cmd::SyncToggle(1)).unwrap();
        run(&mut e, 3.0);
        let v = *view.lock();
        let (a, b) = (v.decks[0].bpm.unwrap(), v.decks[1].bpm.unwrap());
        assert!((a - b).abs() < 0.01, "{a} vs {b}");
        assert!(phase_diff(&e) < 0.02);
        // Knock deck 2 off the beat with a jog nudge: the phase lock pulls it back.
        for _ in 0..40 {
            tx.send(Cmd::JogBend(1, 4.0)).unwrap();
        }
        run(&mut e, 0.3);
        let knocked = phase_diff(&e);
        run(&mut e, 10.0);
        assert!(phase_diff(&e) < 0.01, "phase lock: {knocked} -> {}", phase_diff(&e));
    }

    #[test]
    fn sync_off_keeps_tempo_until_fader_moves_through_it() {
        let (mut e, tx, view) = engine();
        tx.send(Cmd::Load { deck: 0, track: track(124.0, 0.1), generation: 1 }).unwrap();
        tx.send(Cmd::Load { deck: 1, track: track(128.0, 0.3), generation: 2 }).unwrap();
        tx.send(Cmd::SyncToggle(1)).unwrap();
        run(&mut e, 0.1);
        tx.send(Cmd::SyncToggle(1)).unwrap();
        run(&mut e, 0.1);
        let v = *view.lock();
        assert!(!v.decks[1].synced && v.decks[1].tempo_held);
        assert!((v.decks[1].bpm.unwrap() - 124.0).abs() < 0.01, "tempo kept after sync off");
        tx.send(Cmd::TempoFader(1, -0.6)).unwrap(); // passes the held −3.1 %
        run(&mut e, 0.1);
        let v = *view.lock();
        assert!(!v.decks[1].tempo_held);
    }

    #[test]
    fn reconnect_keeps_deck_state() {
        let (mut e, tx, view) = engine();
        tx.send(Cmd::Load { deck: 0, track: track(124.0, 0.1), generation: 1 }).unwrap();
        tx.send(Cmd::Play(0)).unwrap();
        run(&mut e, 1.0);
        e.set_output(44100, 2, CueRouting::MasterOnly);
        let mut out = vec![0f32; 441 * 2];
        for _ in 0..100 {
            e.process(&mut out);
        }
        let v = *view.lock();
        assert!(v.decks[0].playing);
        // Started from the auto-cue at 0.1 s.
        assert!((v.decks[0].pos / 44100.0 - 2.1).abs() < 0.02, "{}", v.decks[0].pos / 44100.0);
        assert_eq!(v.sample_rate, 44100);
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    use crate::analysis::click_track;
    use crossbeam_channel::{bounded, unbounded};
    use std::path::PathBuf;

    /// `cargo test --release bench -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn dsp_cost() {
        for keylock in [false, true] {
            let (tx, rx) = unbounded();
            let (gtx, _g) = bounded(4);
            let view = Arc::new(Mutex::new(EngineView::new(48000, 4, CueRouting::Channels34)));
            let mut e = Engine::new(48000, 4, CueRouting::Channels34, rx, gtx, view);
            for d in 0..2 {
                let t = Arc::new(Track::from_samples(PathBuf::from("t.wav"), 44100, click_track(124.0, 0.1, 60.0, 44100)));
                tx.send(Cmd::Load { deck: d, track: t, generation: d as u64 + 1 }).unwrap();
                tx.send(Cmd::TempoFader(d, 0.3)).unwrap();
                tx.send(Cmd::MasterTempo(d, keylock)).unwrap();
                tx.send(Cmd::Play(d)).unwrap();
            }
            tx.send(Cmd::Mix(MixParam::Filter(0, 0.3))).unwrap();
            let mut out = vec![0f32; 256 * 4];
            let secs = 20.0;
            let t0 = Instant::now();
            for _ in 0..(secs * 48000.0 / 256.0) as usize {
                e.process(&mut out);
            }
            let el = t0.elapsed().as_secs_f64();
            println!("keylock={keylock}: {:.1} ms per second of audio ({:.2}% of one core)", el / secs * 1000.0, el / secs * 100.0);
        }
    }
}

#[cfg(test)]
mod bench2 {
    use super::*;
    use crate::analysis::click_track;
    use std::path::PathBuf;

    #[test]
    #[ignore]
    fn parts_cost() {
        let t = Arc::new(Track::from_samples(PathBuf::from("t.wav"), 44100, click_track(124.0, 0.1, 60.0, 44100)));
        let mut d = Deck::new(48000.0);
        d.load(t, 1);
        d.play_pressed();
        let mut buf = vec![0f32; 512];
        let n = (20.0 * 48000.0 / 256.0) as usize;
        let t0 = Instant::now();
        for _ in 0..n {
            d.render(&mut buf, 1.03);
        }
        println!("deck render: {:.1} ms/s", t0.elapsed().as_secs_f64() / 20.0 * 1000.0);
        dsp::no_denormals();
        let mut m = Mixer::new(48000.0);
        let p = MixerParams::default();
        let a = vec![0.1f32; 512];
        let (mut o1, mut o2) = (vec![0f32; 512], vec![0f32; 512]);
        let t0 = Instant::now();
        for _ in 0..n {
            m.process(&p, [&a, &a], &mut o1, &mut o2);
        }
        println!("mixer: {:.1} ms/s", t0.elapsed().as_secs_f64() / 20.0 * 1000.0);
    }
}
