//! The realtime engine. Owned by the audio callback; controlled through [`Cmd`]s and observed
//! through a published [`EngineView`] snapshot.

pub mod deck;
pub mod dsp;
pub mod mixer;
pub mod stretch;

use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};
use parking_lot::Mutex;

use crate::track::Track;
use deck::{Deck, HotCue, Preview, HOT_CUES, TEMPO_RANGES};
use mixer::{Mixer, MixerParams};

pub const DECKS: usize = 2;
const MAX_BLOCK: usize = 1024;

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
}

/// Everything the UI, MIDI and keyboard can ask the engine to do.
#[derive(Debug)]
pub enum Cmd {
    Load { deck: usize, track: Arc<Track>, generation: u64 },
    Play(usize),
    Cue(usize, bool),
    ToStart(usize),
    HotCue(usize, usize, bool),
    HotCueDelete(usize, usize),
    JogTouch(usize, bool),
    JogScratch(usize, f64),
    JogBend(usize, f64),
    JogSearch(usize, f64),
    /// -1 (top) ..= +1 (bottom).
    TempoFader(usize, f64),
    TempoRangeCycle(usize),
    SyncToggle(usize),
    VinylToggle(usize),
    MasterTempoToggle(usize),
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
    pub frames: usize,
    pub sample_rate: u32,
    pub cue: f64,
    pub at_cue: bool,
    pub hot: [Option<HotCue>; HOT_CUES],
    pub loop_in: Option<f64>,
    pub loop_out: Option<f64>,
    pub loop_active: bool,
    pub tempo_fader: f64,
    pub range: f64,
    pub rate: f64,
    pub bpm: Option<f64>,
    pub synced: bool,
    pub is_master: bool,
    pub vinyl: bool,
    pub keylock: bool,
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
}

impl EngineView {
    pub fn new(sample_rate: u32, channels: u16, routing: CueRouting) -> Self {
        Self {
            decks: [DeckView::default(); DECKS],
            mixer: MixerParams::default(),
            peak_master: 0.0,
            sample_rate,
            channels,
            routing,
        }
    }
}

pub type SharedView = Arc<Mutex<EngineView>>;

pub struct Engine {
    decks: [Deck; DECKS],
    mixer: Mixer,
    params: MixerParams,
    cmd_rx: Receiver<Cmd>,
    garbage_tx: Sender<Arc<Track>>,
    view: SharedView,
    channels: usize,
    routing: CueRouting,
    sample_rate: u32,
    deck_buf: [Vec<f32>; DECKS],
    master: Vec<f32>,
    cue: Vec<f32>,
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
            cmd_rx,
            garbage_tx,
            view,
            channels: channels as usize,
            routing,
            sample_rate,
            deck_buf: [vec![0.0; MAX_BLOCK * 2], vec![0.0; MAX_BLOCK * 2]],
            master: vec![0.0; MAX_BLOCK * 2],
            cue: vec![0.0; MAX_BLOCK * 2],
        }
    }

    fn other(d: usize) -> usize {
        (d + 1) % DECKS
    }

    /// Effective BPM of a deck as currently playing (fader or sync applied).
    fn effective_bpm(&self, d: usize) -> Option<f64> {
        self.decks[d].bpm().map(|b| b * self.rate_for(d))
    }

    /// Which deck is the tempo master: the un-synced one, else deck 1.
    fn master_deck(&self) -> usize {
        (0..DECKS).find(|&d| !self.decks[d].synced).unwrap_or(0)
    }

    fn rate_for(&self, d: usize) -> f64 {
        let deck = &self.decks[d];
        if deck.synced && self.master_deck() != d {
            let m = self.master_deck();
            if let (Some(own), Some(mb)) = (deck.bpm(), self.decks[m].bpm()) {
                return mb * self.decks[m].fader_rate() / own;
            }
        }
        deck.fader_rate()
    }

    fn handle(&mut self, cmd: Cmd) {
        use Cmd::*;
        match cmd {
            Load { deck, track, generation } => {
                if let Some(old) = self.decks[deck].load(track, generation) {
                    // Free the old buffer off the audio thread when possible.
                    if let Err(e) = self.garbage_tx.try_send(old) {
                        drop(e);
                    }
                }
            }
            Play(d) => self.decks[d].play_pressed(),
            Cue(d, p) => self.decks[d].cue_button(p),
            ToStart(d) => self.decks[d].to_start(),
            HotCue(d, i, p) => self.decks[d].hot_cue(i, p),
            HotCueDelete(d, i) => self.decks[d].hot_cue_delete(i),
            JogTouch(d, t) => self.decks[d].jog_touch(t),
            JogScratch(d, t) => self.decks[d].jog_scratch(t),
            JogBend(d, t) => self.decks[d].jog_bend(t),
            JogSearch(d, t) => self.decks[d].jog_search(t),
            TempoFader(d, v) => self.decks[d].tempo_fader = v.clamp(-1.0, 1.0),
            TempoRangeCycle(d) => {
                let deck = &mut self.decks[d];
                deck.range_idx = (deck.range_idx + 1) % TEMPO_RANGES.len();
            }
            SyncToggle(d) => {
                if self.decks[d].synced {
                    self.decks[d].synced = false;
                } else {
                    self.enable_sync(d);
                }
            }
            VinylToggle(d) => self.decks[d].vinyl = !self.decks[d].vinyl,
            MasterTempoToggle(d) => self.decks[d].keylock = !self.decks[d].keylock,
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

    fn enable_sync(&mut self, d: usize) {
        let m = Self::other(d);
        if self.decks[d].bpm().is_none() || self.decks[m].bpm().is_none() {
            return;
        }
        // If the other deck is also synced, it becomes the master.
        self.decks[m].synced = false;
        self.decks[d].synced = true;
        // Beat-align: match our position within the beat to the master's.
        let (Some(tm), Some(td)) = (self.decks[m].track.clone(), self.decks[d].track.clone()) else {
            return;
        };
        if let (Some(bm), Some(bd)) = (tm.beat_at(self.decks[m].pos), td.beat_at(self.decks[d].pos)) {
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
        }
    }

    /// Fill an interleaved f32 output buffer with `self.channels` channels.
    pub fn process(&mut self, out: &mut [f32]) {
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
        self.publish();
    }

    fn render_block(&mut self, out: &mut [f32], n: usize) {
        let rates = [self.rate_for(0), self.rate_for(1)];
        for d in 0..DECKS {
            self.decks[d].render(&mut self.deck_buf[d][..n * 2], rates[d]);
        }
        self.mixer.process(
            &self.params,
            [&self.deck_buf[0][..n * 2], &self.deck_buf[1][..n * 2]],
            &mut self.master[..n * 2],
            &mut self.cue[..n * 2],
        );
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
                frames: t.map(|t| t.frames).unwrap_or(0),
                sample_rate: t.map(|t| t.sample_rate).unwrap_or(0),
                cue: deck.cue,
                at_cue: deck.at_cue(),
                hot: deck.hot,
                loop_in: deck.loop_in,
                loop_out: deck.loop_out,
                loop_active: deck.loop_active,
                tempo_fader: deck.tempo_fader,
                range: TEMPO_RANGES[deck.range_idx],
                rate: self.rate_for(d),
                bpm: self.effective_bpm(d),
                synced: deck.synced,
                is_master: master == d && self.decks[Self::other(d)].synced,
                vinyl: deck.vinyl,
                keylock: deck.keylock,
                touched: deck.touched,
                platter_rev: deck.platter_rev,
                peak: self.mixer.peak_ch[d].max(v.decks[d].peak * 0.9),
            };
        }
        v.mixer = self.params;
        v.peak_master = self.mixer.peak_master.max(v.peak_master * 0.9);
        v.sample_rate = self.sample_rate;
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

    #[test]
    fn master_and_cue_routing() {
        let (mut e, tx, _v) = engine();
        tx.send(Cmd::Load { deck: 0, track: track(124.0, 0.1), generation: 1 }).unwrap();
        tx.send(Cmd::Play(0)).unwrap();
        let out = run(&mut e, 1.0);
        let master: f32 = out.chunks(4).map(|f| f[0].abs()).sum();
        let cue: f32 = out.chunks(4).map(|f| f[2].abs()).sum();
        assert!(master > 10.0);
        // No PFL: phones follow master.
        assert!(cue > 1.0);

        // PFL deck 1, fader down: phones still hear it, master doesn't.
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
    fn sync_matches_tempo_and_phase() {
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
        let ta = e.decks[0].track.clone().unwrap();
        let tb = e.decks[1].track.clone().unwrap();
        let pa = ta.beat_at(e.decks[0].pos).unwrap().rem_euclid(1.0);
        let pb = tb.beat_at(e.decks[1].pos).unwrap().rem_euclid(1.0);
        let mut d = (pa - pb).abs();
        d = d.min(1.0 - d);
        assert!(d < 0.03, "phase diff {d} beats");
    }
}
