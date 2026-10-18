//! What crosses between the UI and the audio thread: commands in ([`Cmd`], [`MixParam`]) and a
//! snapshot of the decks and mixer out ([`EngineView`]), published every audio block.

use std::sync::Arc;

use parking_lot::Mutex;

use super::deck::{HotCue, HOT_CUES, MEMORY_CUES};
use super::mixer::{self, MixerParams};
use super::{deck, fx};
use super::DECKS;
use crate::track::Track;

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
