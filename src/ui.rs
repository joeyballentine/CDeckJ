//! Steam Deck–sized (1280×800) touch UI: stacked waveforms, two CDJ-style decks, mixer, browser.

use std::any::Any;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, Sender};
use egui::text::{LayoutJob, TextWrapping};
use egui::{
    pos2, vec2, Align2, Color32, CornerRadius, Event, FontId, Id, Key, Mesh, Painter, Pos2, Rect,
    Sense, Shape, Stroke, StrokeKind, Ui, Vec2, ViewportCommand, WidgetInfo,
};
use parking_lot::Mutex;

use crate::audio::{self, AudioOptions, AudioOut};
use crate::config::{Config, Store};
use crate::engine::deck::{HotCue, HOT_CUES, SCRATCH_SECS_PER_TICK};
use crate::engine::{Cmd, DeckView, EngineView, MixParam, SharedEngine, SharedView, DECKS};
use crate::library::{self, LibEvent, LibTrack, Library, NavKind, Row, SortKey};
use crate::midi::{Ctl as MidiCtl, MidiStatus, PickupLocks, UiAction};
use crate::system::{GamepadStatus, KeepAwake};
use crate::theme::{self as t, caption, cond, mono, semi};
use crate::touch::Touches;
use crate::track::{self, GridSource, SavedCue, Track};

const JOG_TICKS_PER_REV: f64 = 720.0;
/// Idle redraw interval: everything that changes state (MIDI, gamepad, loaders) also
/// requests a repaint, so this only keeps clocks honest.
const IDLE_REPAINT: Duration = Duration::from_secs(1);
const HOT_LETTERS: [&str; 8] = ["A", "B", "C", "D", "E", "F", "G", "H"];
const WAVE_ZOOMS: [f64; 5] = [4.0, 6.0, 8.0, 12.0, 16.0];

enum LoadMsg {
    /// Playable (still decoding).
    Ready { deck: usize, generation: u64, track: Arc<Track> },
    /// Fully decoded and analysed.
    Done { deck: usize, generation: u64, track: Arc<Track> },
    Failed { deck: usize, generation: u64, error: String },
}

/// Cue state of a deck, compared frame to frame to know when to save.
#[derive(Clone, PartialEq)]
struct CueSnapshot {
    path: PathBuf,
    hot: [Option<HotCue>; HOT_CUES],
    memory: Vec<i64>,
    cue: i64,
}

#[derive(Default)]
struct DeckUi {
    track: Option<Arc<Track>>,
    loading: Option<String>,
    error: Option<String>,
    generation: u64,
    shift: bool,
    cue_held: bool,
    hot_held: [bool; HOT_CUES],
    jog_held: bool,
    wave_held: bool,
    play_held: bool,
    jog_angle: Option<f32>,
    saved: Option<CueSnapshot>,
    art: Option<crate::art::ArtKey>,
    /// Where the loaded track came from in the library (for history).
    lib: Option<LibTrack>,
}

pub struct AppInit {
    pub cmd: Sender<Cmd>,
    pub view: SharedView,
    pub engine: SharedEngine,
    pub ui_rx: Receiver<UiAction>,
    pub music_root: PathBuf,
    pub usb_paths: Vec<PathBuf>,
    pub audio: Result<AudioOut, String>,
    pub audio_opts: AudioOptions,
    pub midi: MidiStatus,
    pub gamepad: Option<GamepadStatus>,
    pub locks: PickupLocks,
    pub config: Config,
    pub config_warning: Option<String>,
    pub store: Arc<Mutex<Store>>,
    pub fullscreen: bool,
    pub preload: Vec<PathBuf>,
    pub keep_awake: Option<KeepAwake>,
    pub keep: Vec<Box<dyn Any>>,
    pub tap: Arc<crate::recorder::Tap>,
}

pub struct App {
    cmd: Sender<Cmd>,
    view: SharedView,
    engine: SharedEngine,
    ui_rx: Receiver<UiAction>,
    load_tx: Sender<LoadMsg>,
    load_rx: Receiver<LoadMsg>,
    lib_rx: Receiver<LibEvent>,
    lib: Library,
    scroll_to_selected: bool,
    browser_offset: f32,
    search: String,
    focus_search: bool,
    decks: [DeckUi; DECKS],
    next_gen: u64,
    audio: Option<AudioOut>,
    /// Keeps the engine running (silently) while there's no audio device.
    silent: Option<audio::SilentClock>,
    audio_opts: AudioOptions,
    audio_error: Option<String>,
    last_reconnect: Instant,
    /// A reconnect attempt running off the UI thread (device probing can block).
    reconnecting: Option<crossbeam_channel::Receiver<Result<AudioOut, String>>>,
    midi: MidiStatus,
    gamepad: Option<GamepadStatus>,
    locks: PickupLocks,
    config: Config,
    config_saved: Config,
    config_dirty_since: Option<Instant>,
    banner: Option<String>,
    store: Arc<Mutex<Store>>,
    fullscreen: bool,
    key_held: Vec<Key>,
    touches: Touches,
    ctx: egui::Context,
    _keep_awake: Option<KeepAwake>,
    _keep: Vec<Box<dyn Any>>,
    recorder: crate::recorder::Recorder,
    art: crate::art::ArtCache,
    /// What has gone on air this session (drives history and "played" marks).
    onair: crate::recorder::Tracklist,
    started: Instant,
    /// First tap of the two-tap stop, so a stray touch can't end a recording.
    rec_confirm: Option<Instant>,
    /// First press of the two-press EXIT.
    exit_confirm: Option<Instant>,
    rec_low_warned: bool,
}

impl App {
    pub fn new(ctx: &egui::Context, init: AppInit) -> Self {
        let (load_tx, load_rx) = unbounded();
        let (lib_tx, lib_rx) = unbounded();
        let c2 = ctx.clone();
        let cfg = init.config.clone();
        library::spawn_watchers(
            library::WatchOptions {
                local_root: init.music_root.clone(),
                extra_usb: init.usb_paths,
                store: init.store.clone(),
                analyze: cfg.background_analysis,
                bpm_range: (cfg.bpm_range[0], cfg.bpm_range[1]),
            },
            lib_tx,
            move || c2.request_repaint(),
        );
        let (audio, audio_error) = match init.audio {
            Ok(a) => (Some(a), None),
            Err(e) => (None, Some(e)),
        };
        let silent = audio.is_none().then(|| audio::SilentClock::start(&init.engine));
        let mut app = Self {
            cmd: init.cmd,
            view: init.view,
            engine: init.engine,
            ui_rx: init.ui_rx,
            load_tx,
            load_rx,
            lib_rx,
            lib: Library::new(init.music_root),
            scroll_to_selected: false,
            browser_offset: 0.0,
            search: String::new(),
            focus_search: false,
            decks: Default::default(),
            next_gen: 1,
            silent,
            audio,
            audio_opts: init.audio_opts,
            audio_error,
            last_reconnect: Instant::now(),
            reconnecting: None,
            midi: init.midi,
            gamepad: init.gamepad,
            locks: init.locks,
            config_saved: init.config.clone(),
            config: init.config,
            config_dirty_since: None,
            banner: init.config_warning,
            store: init.store,
            fullscreen: init.fullscreen,
            key_held: Vec::new(),
            touches: Touches::default(),
            ctx: ctx.clone(),
            _keep_awake: init.keep_awake,
            _keep: init.keep,
            recorder: crate::recorder::Recorder::new(init.tap),
            onair: Default::default(),
            started: Instant::now(),
            art: { let c = ctx.clone(); crate::art::ArtCache::new(move || c.request_repaint()) },
            rec_confirm: None,
            exit_confirm: None,
            rec_low_warned: false,
        };
        for (d, p) in init.preload.into_iter().take(DECKS).enumerate() {
            let (title, artist) = track::names_from_path(&p);
            let lt = LibTrack { path: p, title, artist, key: String::new(), bpm: None, duration: None, ext: String::new(), rb: None };
            app.load(d, &lt);
        }
        app
    }

    fn send(&self, c: Cmd) {
        let _ = self.cmd.send(c);
    }

    /// Send a mixer/tempo change made on screen, and hold the hardware control back until it
    /// catches up (soft takeover).
    fn send_touched(&self, c: Cmd, ctl: MidiCtl) {
        self.locks.lock().insert(ctl);
        self.send(c);
    }

    fn load(&mut self, deck: usize, lt: &LibTrack) {
        let generation = self.next_gen;
        self.next_gen += 1;
        let du = &mut self.decks[deck];
        du.generation = generation;
        du.error = None;
        du.loading = Some(lt.title.clone());
        du.art = None;
        du.lib = Some(lt.clone());
        let art = self.art_key(lt);
        self.decks[deck].art = art;
        let (path, mut meta, rb) = self.lib.load_request(lt);
        meta.auto_cue = match (self.config.auto_cue, self.config.auto_cue_db) {
            (false, _) => track::AutoCue::Off,
            (true, None) => track::AutoCue::Memory,
            (true, Some(db)) => track::AutoCue::Level(db),
        };
        meta.hotcue_autoload = self.config.hotcue_autoload;
        meta.bpm_range = (self.config.bpm_range[0], self.config.bpm_range[1]);
        meta.offset_ms = self.config.offset_for(&path);
        meta.duration_hint = lt.duration.map(|d| d as f64);
        if let Some(r) = self.store.lock().get(&path).cloned() {
            if r.grid_fixed || rb.is_none() {
                meta.saved_grid = r.grid;
            }
            meta.saved_cues = r.cues;
            meta.saved_cue_point = r.cue_point;
        }
        let tx = self.load_tx.clone();
        let ctx = self.ctx.clone();
        let store = self.store.clone();
        std::thread::Builder::new()
            .name("loader".into())
            .spawn(move || {
                if let Some((ex, id)) = rb {
                    meta.analysis = ex.track(id).and_then(|t| ex.analysis(t));
                }
                // Without rekordbox's waveforms, show a cached one of our own straight away.
                let has_rb_wave = meta.analysis.as_ref().is_some_and(|a| !a.waves.is_empty());
                if !has_rb_wave {
                    meta.cached_wave = crate::wave::load_cached(&path);
                }
                let had_cache = meta.cached_wave.is_some();
                let tx2 = tx.clone();
                let c2 = ctx.clone();
                let res = track::load_progressive(&path, meta, move |track| {
                    let _ = tx2.send(LoadMsg::Ready { deck, generation, track });
                    c2.request_repaint();
                });
                if let (Ok(t), false, false) = (&res, had_cache, has_rb_wave) {
                    if t.is_complete() {
                        crate::wave::save_cached(&path, &t.wave.read());
                        store.lock().update(&path, |r| r.wave = true);
                    }
                }
                let _ = tx.send(match res {
                    Ok(track) => LoadMsg::Done { deck, generation, track },
                    Err(error) => LoadMsg::Failed { deck, generation, error },
                });
                ctx.request_repaint();
            })
            .ok();
    }

    /// Where a library track's cover comes from: the stick's artwork, else the file's tags.
    fn art_key(&self, lt: &LibTrack) -> Option<crate::art::ArtKey> {
        match lt.rb {
            Some((u, id)) => {
                let ex = self.lib.usbs.get(u)?;
                ex.track(id).and_then(|t| ex.artwork_path(t)).map(crate::art::ArtKey::File)
            }
            None => Some(crate::art::ArtKey::Embedded(lt.path.clone())),
        }
    }

    fn load_selected(&mut self, deck: usize) {
        if let Some(Row::Track(t)) = self.lib.selected_row().cloned() {
            self.load(deck, &t);
        }
    }

    fn browse(&mut self, delta: i32) {
        self.lib.move_by(delta);
        self.scroll_to_selected = true;
    }

    fn browse_enter(&mut self) {
        if self.lib.enter() {
            self.search.clear();
            self.scroll_to_selected = true;
        }
    }

    fn browse_back(&mut self) {
        self.lib.back();
        self.search = self.lib.filter.clone();
        self.scroll_to_selected = true;
    }

    fn poll(&mut self) {
        while let Ok(ev) = self.lib_rx.try_recv() {
            if let (LibEvent::UsbAdded(ex), true) = (&ev, self.config.apply_usb_settings) {
                let s = crate::rekordbox::settings::MySettings::read(&ex.root);
                let name = ex.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "the USB".into());
                if !s.is_empty() {
                    self.apply_my_settings(&s, &name);
                }
            }
            self.lib.handle(ev);
        }
        while let Ok(m) = self.load_rx.try_recv() {
            match m {
                LoadMsg::Ready { deck, generation, track } => {
                    let du = &mut self.decks[deck];
                    if generation != du.generation {
                        continue; // superseded by a newer load
                    }
                    du.loading = None;
                    du.track = Some(track.clone());
                    du.cue_held = false;
                    du.hot_held = [false; HOT_CUES];
                    du.saved = None;
                    let _ = self.cmd.send(Cmd::Load { deck, track, generation });
                }
                LoadMsg::Done { deck, generation, track } => {
                    if generation != self.decks[deck].generation {
                        continue;
                    }
                    self.lib.learn(&track.path, track.bpm(), &track.key);
                    // Cache detected grids so the next load (and the browser) has them.
                    if let Some(g) = track.grid.load_full().filter(|g| g.source == GridSource::Detected && g.is_constant()) {
                        let sr = track.sample_rate as f64;
                        self.store.lock().update(&track.path, |r| r.grid = Some((g.bpm, g.first_beat() / sr)));
                    }
                }
                LoadMsg::Failed { deck, generation, error } => {
                    let du = &mut self.decks[deck];
                    if generation == du.generation {
                        du.loading = None;
                        du.error = Some(error);
                    }
                }
            }
        }
        while let Ok(a) = self.ui_rx.try_recv() {
            match a {
                UiAction::Browse(d) => self.browse(d),
                UiAction::BrowsePress => self.browse_enter(),
                UiAction::Back => self.browse_back(),
                UiAction::Load(d) => self.load_selected(d),
                UiAction::Record => self.record_toggle(),
            }
        }
    }

    /// Reconnect audio after the device drops (or keep trying if it never opened).
    fn maintain_audio(&mut self) {
        let lost = audio::STREAM_LOST.load(Ordering::Relaxed);
        if lost && self.audio.is_some() {
            self.audio = None;
            self.audio_error = Some("audio device disconnected".into());
            self.silent = Some(audio::SilentClock::start(&self.engine));
        }
        if let Some(rx) = &self.reconnecting {
            match rx.try_recv() {
                Ok(Ok(a)) => {
                    eprintln!("audio: connected to {}", a.description);
                    self.silent = None;
                    self.audio = Some(a);
                    self.audio_error = None;
                    self.reconnecting = None;
                }
                Ok(Err(e)) => {
                    self.audio_error = Some(e);
                    // A failed attempt may have left the engine set up for that device.
                    if self.silent.is_some() {
                        self.engine.lock().set_output(48000, 2, crate::engine::CueRouting::MasterOnly);
                    }
                    self.reconnecting = None;
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {}
                Err(crossbeam_channel::TryRecvError::Disconnected) => self.reconnecting = None,
            }
        } else if self.audio.is_none() && self.last_reconnect.elapsed() > Duration::from_secs(2) {
            self.last_reconnect = Instant::now();
            // Probing devices can take a while: do it on a thread while the silent clock
            // keeps time, and hand over only once a stream exists.
            let (tx, rx) = crossbeam_channel::bounded(1);
            let (opts, engine) = (self.audio_opts.clone(), self.engine.clone());
            std::thread::spawn(move || {
                let _ = tx.send(audio::start(&opts, &engine));
            });
            self.reconnecting = Some(rx);
        }
    }

    /// Save toggles/levels to config and cue edits to the track store, both debounced.
    fn persist(&mut self, v: &EngineView) {
        let c = &mut self.config;
        c.quantize = v.quantize;
        c.fullscreen = self.fullscreen;
        for d in 0..DECKS {
            let dv = &v.decks[d];
            c.decks[d].vinyl = dv.vinyl;
            c.decks[d].master_tempo = dv.keylock;
            c.decks[d].tempo_range = dv.range_idx;
        }
        c.fx = crate::config::FxPrefs { kind: v.mixer.fx.kind, beat_idx: v.mixer.fx.beat_idx, target: v.mixer.fx.target, level: v.mixer.fx.level };
        c.master_level = v.mixer.master;
        c.phones_mix = v.mixer.head_mix;
        c.phones_level = v.mixer.head_level;
        if self.config != self.config_saved {
            let since = *self.config_dirty_since.get_or_insert_with(Instant::now);
            if since.elapsed() > Duration::from_secs(2) {
                self.config.save_in_background();
                self.config_saved = self.config.clone();
                self.config_dirty_since = None;
            }
        }

        for d in 0..DECKS {
            let dv = &v.decks[d];
            let Some(track) = self.decks[d].track.clone() else { continue };
            if !dv.loaded || dv.track_gen != self.decks[d].generation {
                continue;
            }
            let mut memory: Vec<i64> = dv.memory.iter().flatten().map(|m| *m as i64).collect();
            memory.sort_unstable();
            let snap = CueSnapshot { path: track.path.clone(), hot: dv.hot, memory, cue: dv.cue as i64 };
            match &self.decks[d].saved {
                None => self.decks[d].saved = Some(snap), // state as loaded: nothing to save
                Some(prev) if *prev != snap && !dv.playing => {
                    let sr = track.sample_rate as f64;
                    let mut cues: Vec<SavedCue> = dv
                        .hot
                        .iter()
                        .enumerate()
                        .filter_map(|(i, h)| {
                            h.map(|h| SavedCue {
                                hot: Some(i),
                                at: h.pos / sr,
                                loop_end: h.loop_end.map(|e| e / sr),
                                color: h.color,
                                comment: preset_comment(&track, i, h.pos),
                            })
                        })
                        .collect();
                    cues.extend(dv.memory.iter().flatten().map(|m| SavedCue { hot: None, at: m / sr, loop_end: None, color: None, comment: String::new() }));
                    let cue = dv.cue / sr;
                    let mut st = self.store.lock();
                    st.update(&track.path, |r| {
                        r.cues = Some(cues);
                        r.cue_point = Some(cue);
                    });
                    // Cue edits are precious: write now (off the UI thread) rather than on the debounce.
                    let job = st.snapshot(true);
                    drop(st);
                    crate::config::write_in_background(job);
                    self.decks[d].saved = Some(snap);
                }
                _ => {}
            }
        }
        let job = self.store.lock().snapshot(false);
        crate::config::write_in_background(job);
    }

    /// Take on a stick's My Settings, as a CDJ does when the stick is loaded.
    fn apply_my_settings(&mut self, s: &crate::rekordbox::settings::MySettings, name: &str) {
        use crate::rekordbox::settings::AutoCueLevel;
        let c = &mut self.config;
        if let Some(v) = s.waveform_color {
            c.waveform_color = v;
        }
        if let Some(v) = s.overview_full {
            c.overview_full = v;
        }
        if let Some(v) = s.playhead_left {
            c.playhead_left = v;
        }
        if let Some(v) = s.time_remain {
            c.time_remain = v;
        }
        if let Some(v) = s.beat_jump {
            c.beat_jump = v;
        }
        if let Some(v) = s.auto_cue {
            c.auto_cue = v;
        }
        if let Some(v) = s.auto_cue_level {
            c.auto_cue_db = match v {
                AutoCueLevel::Memory => None,
                AutoCueLevel::Db(db) => Some(db),
            };
        }
        if let Some(v) = s.hotcue_autoload {
            c.hotcue_autoload = v;
        }
        if let Some(v) = s.beat_fx_quantize {
            c.beat_fx_quantize = v;
        }
        if let Some(v) = s.quantize_beats {
            c.quantize_beats = v;
            self.send(Cmd::QuantizeBeats(v));
        }
        if let Some(v) = s.beat_fx_quantize {
            self.send(Cmd::Mix(MixParam::FxQuantize(v)));
        }
        if let Some(v) = s.fader_curve {
            self.config.fader_curve = v;
            self.send(Cmd::Mix(MixParam::FaderCurve(v)));
        }
        if let Some(v) = s.crossfader_curve {
            self.config.crossfader_curve = v;
            self.send(Cmd::Mix(MixParam::CrossfaderCurve(v)));
        }
        if let Some(v) = s.quantize {
            self.send(Cmd::Quantize(v));
        }
        for d in 0..DECKS {
            if let Some(v) = s.jog_vinyl {
                self.send(Cmd::Vinyl(d, v));
            }
            if let Some(v) = s.master_tempo {
                self.send(Cmd::MasterTempo(d, v));
            }
            if let Some(v) = s.tempo_range {
                self.send(Cmd::TempoRange(d, v));
            }
        }
        self.banner = Some(format!("Using My Settings from {name}: {}", s.summary()));
    }

    fn recordings_dir(&self) -> PathBuf {
        self.config.recording_dir.clone().unwrap_or_else(|| library::default_root().join(crate::recorder::DIR_NAME))
    }

    /// Start recording, or stop on the second press within 3 s.
    fn record_toggle(&mut self) {
        if !self.recorder.is_recording() {
            self.rec_low_warned = false;
            self.rec_confirm = None;
            match self.recorder.start(&self.recordings_dir()) {
                Ok(base) => eprintln!("recording: {}", base.display()),
                Err(e) => self.banner = Some(format!("Couldn't start recording: {e}")),
            }
        } else if self.rec_confirm.is_some_and(|t| t.elapsed() < Duration::from_secs(3)) {
            self.rec_confirm = None;
            if let Some(f) = self.recorder.stop() {
                self.recording_finished(f);
            }
        } else {
            self.rec_confirm = Some(Instant::now());
        }
    }

    fn recording_finished(&mut self, f: crate::recorder::Finished) {
        let mut file = f.parts.first().map(|p| p.path.display().to_string()).unwrap_or_default();
        if let Some(home) = std::env::var_os("HOME").map(|h| h.to_string_lossy().into_owned()) {
            if !home.is_empty() && file.starts_with(&home) {
                file = format!("~{}", &file[home.len()..]);
            }
        }
        let len = crate::recorder::fmt_elapsed(f.secs);
        let tracks = match f.tracks {
            0 => String::new(),
            1 => ", 1 track".into(),
            n => format!(", {n} tracks"),
        };
        let parts = if f.parts.len() > 1 { format!(" (in {} files)", f.parts.len()) } else { String::new() };
        let lost = if f.dropped_secs > 0.05 { format!(" — {:.1} s lost because the disk was too slow", f.dropped_secs) } else { String::new() };
        self.banner = Some(match f.error {
            Some(e) => format!("Recording stopped: {e}. Saved {len}{tracks} to {file}"),
            None => format!("Recording saved: {len}{tracks}{parts} — {file}{lost}"),
        });
    }

    /// Add tracks to the session history once they've been on air for a few seconds.
    fn history_tick(&mut self, v: &EngineView) {
        let meta: [Option<(String, String)>; DECKS] =
            std::array::from_fn(|d| self.decks[d].track.as_ref().map(|t| (t.artist.clone(), t.title.clone())));
        let ms = self.started.elapsed().as_millis() as u64;
        let before = self.onair.entries.len();
        if !self.onair.observe(v, ms, 1000, meta) {
            return;
        }
        let new: Vec<usize> = self.onair.entries[before..].iter().map(|e| e.deck).collect();
        for d in new {
            let (Some(lt), Some(tr)) = (self.decks[d].lib.clone(), self.decks[d].track.clone()) else { continue };
            let rb = lt.rb.and_then(|(u, id)| {
                let ex = self.lib.usbs.get(u)?;
                Some((ex.track(id)?.file_path.clone(), ex.name.clone()))
            });
            self.lib.record_play(crate::history::HistEntry {
                time: crate::history::clock(),
                title: tr.title.clone(),
                artist: tr.artist.clone(),
                key: tr.key.clone(),
                bpm: tr.bpm().or(lt.bpm),
                duration: lt.duration.or(Some((tr.length() as f64 / tr.sample_rate.max(1) as f64).round() as u32)),
                path: lt.path.clone(),
                rb_path: rb.as_ref().map(|r| r.0.clone()),
                usb: rb.map(|r| r.1),
            });
        }
    }

    fn recording_tick(&mut self, v: &EngineView) {
        if !self.recorder.is_recording() {
            return;
        }
        let meta: [Option<(String, String)>; DECKS] =
            std::array::from_fn(|d| self.decks[d].track.as_ref().map(|t| (t.artist.clone(), t.title.clone())));
        if let Some(f) = self.recorder.tick(v, meta) {
            self.recording_finished(f);
            return;
        }
        if self.recorder.low_space() && !self.rec_low_warned {
            self.rec_low_warned = true;
            self.banner = Some("Less than 1 GB free for the recording (about an hour left). It will stop safely before the disk fills.".into());
        }
        if self.rec_confirm.is_some_and(|t| t.elapsed() >= Duration::from_secs(3)) {
            self.rec_confirm = None;
        }
    }

    fn set_zoom(&mut self, dir: i32) {
        let cur = WAVE_ZOOMS.iter().position(|z| (*z - self.config.waveform_seconds).abs() < 0.01).unwrap_or(2) as i32;
        let i = (cur + dir).clamp(0, WAVE_ZOOMS.len() as i32 - 1) as usize;
        self.config.waveform_seconds = WAVE_ZOOMS[i];
    }

    /// Fix a half/double tempo detection on a deck (×2 or ÷2) and remember it.
    fn scale_grid(&mut self, d: usize, factor: f64) {
        let Some(track) = self.decks[d].track.clone() else { return };
        let Some(g) = track.grid.load_full() else { return };
        let ng = g.scaled(factor);
        let sr = track.sample_rate as f64;
        self.store.lock().update(&track.path, |r| {
            r.grid = Some((ng.bpm, ng.first_beat() / sr));
            r.grid_fixed = true;
        });
        track.grid.store(Some(Arc::new(ng)));
        self.lib.learn(&track.path, track.bpm(), &track.key);
    }

    fn keyboard(&mut self, ui: &Ui) {
        if !ui.ctx().text_edit_focused() && ui.input(|i| i.key_pressed(Key::Slash) || (i.modifiers.command && i.key_pressed(Key::F))) {
            self.focus_search = true;
            return;
        }
        if ui.ctx().text_edit_focused() {
            return;
        }
        // A focused control owns the arrows, Enter and Space.
        let widget_focus = ui.ctx().memory(|m| m.focused().is_some());
        // Ctrl+Q quits (press twice); Ctrl-chords never reach the deck shortcuts.
        if ui.input(|i| i.modifiers.command && i.key_pressed(Key::Q)) {
            self.exit_press(ui.ctx());
        }
        let events: Vec<(Key, bool, bool)> = ui.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    Event::Key { key, pressed, repeat, modifiers, .. } if !modifiers.command => Some((*key, *pressed, *repeat)),
                    _ => None,
                })
                .collect()
        });
        for (key, pressed, repeat) in events {
            if widget_focus && matches!(key, Key::ArrowUp | Key::ArrowDown | Key::ArrowLeft | Key::ArrowRight | Key::Enter | Key::Space) {
                continue;
            }
            if pressed && repeat {
                match key {
                    Key::ArrowUp => self.browse(-1),
                    Key::ArrowDown => self.browse(1),
                    _ => {}
                }
                continue;
            }
            if pressed {
                if self.key_held.contains(&key) {
                    continue;
                }
                self.key_held.push(key);
            } else {
                self.key_held.retain(|k| *k != key);
            }
            let hot = |k: Key| -> Option<(usize, usize)> {
                Some(match k {
                    Key::Num1 => (0, 0),
                    Key::Num2 => (0, 1),
                    Key::Num3 => (0, 2),
                    Key::Num4 => (0, 3),
                    Key::Num7 => (1, 0),
                    Key::Num8 => (1, 1),
                    Key::Num9 => (1, 2),
                    Key::Num0 => (1, 3),
                    _ => return None,
                })
            };
            if let Some((d, i)) = hot(key) {
                self.send(Cmd::HotCue(d, i, pressed));
                continue;
            }
            match (key, pressed) {
                (Key::X, p) => self.send(Cmd::Cue(0, p)),
                (Key::N, p) => self.send(Cmd::Cue(1, p)),
                (Key::Z, true) => self.send(Cmd::Play(0)),
                (Key::M, true) => self.send(Cmd::Play(1)),
                (Key::A, true) => self.send(Cmd::SyncToggle(0)),
                (Key::K, true) => self.send(Cmd::SyncToggle(1)),
                (Key::S, true) => self.send(Cmd::MasterTempoToggle(0)),
                (Key::J, true) => self.send(Cmd::MasterTempoToggle(1)),
                (Key::Q, true) => self.load_selected(0),
                (Key::P, true) => self.load_selected(1),
                (Key::G, true) => self.send(Cmd::QuantizeToggle),
                (Key::R, true) => self.record_toggle(),
                (Key::F, true) => self.send(Cmd::Mix(MixParam::FxToggle)),
                (Key::W, true) => self.config.waveform_color = crate::wave::next_mode(self.config.waveform_color),
                // Same as the on-screen −/+: minus shows more time (zoom out).
                (Key::Minus, true) => self.set_zoom(1),
                (Key::Equals | Key::Plus, true) => self.set_zoom(-1),
                (Key::ArrowUp, true) => self.browse(-1),
                (Key::ArrowDown, true) => self.browse(1),
                (Key::PageUp, true) => self.browse(-10),
                (Key::PageDown, true) => self.browse(10),
                (Key::Enter | Key::ArrowRight, true) => self.browse_enter(),
                (Key::Backspace | Key::ArrowLeft | Key::Escape, true) => self.browse_back(),
                (Key::F11, true) => {
                    self.fullscreen = !self.fullscreen;
                    ui.ctx().send_viewport_cmd(ViewportCommand::Fullscreen(self.fullscreen));
                }
                _ => {}
            }
        }
    }
}

fn preset_comment(track: &Track, slot: usize, pos: f64) -> String {
    track.cues.iter().find(|c| c.hot == Some(slot) && (c.pos - pos).abs() < 1.0).map(|c| c.comment.clone()).unwrap_or_default()
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let (events, now) = ui.input(|i| (i.events.clone(), i.time));
        self.art.poll(ui.ctx());
        self.touches.begin_frame(&events, now);
        self.poll();
        self.maintain_audio();
        self.keyboard(ui);
        let v: EngineView = *self.view.lock();
        self.persist(&v);
        self.recording_tick(&v);
        self.history_tick(&v);

        let full = ui.max_rect();
        ui.painter().rect_filled(full, 0, t::CHROME);
        fit_to_window(ui.ctx(), full);

        let top_h = 34.0;
        let banner_h = if self.banner.is_some() { 30.0 } else { 0.0 };
        let wave_h = (full.height() * 0.15).clamp(92.0, 170.0);
        let fx_h = 44.0;
        let main_h = 396.0f32.min((full.height() - top_h - banner_h - wave_h - fx_h - 140.0).max(330.0));
        let mixer_w = 340.0;

        let top = Rect::from_min_size(full.min, vec2(full.width(), top_h));
        let banner = Rect::from_min_size(pos2(full.min.x, top.max.y), vec2(full.width(), banner_h));
        let waves = Rect::from_min_size(pos2(full.min.x, banner.max.y), vec2(full.width(), wave_h));
        let main = Rect::from_min_size(pos2(full.min.x, waves.max.y), vec2(full.width(), main_h));
        let fx_bar = Rect::from_min_size(pos2(full.min.x, main.max.y), vec2(full.width(), fx_h));
        let browser = Rect::from_min_max(pos2(full.min.x, fx_bar.max.y), full.max);
        let deck_w = (full.width() - mixer_w) / 2.0;
        let deck_rects = [
            Rect::from_min_size(main.min, vec2(deck_w, main_h)),
            Rect::from_min_size(pos2(main.min.x + deck_w + mixer_w, main.min.y), vec2(deck_w, main_h)),
        ];
        let mixer = Rect::from_min_size(pos2(main.min.x + deck_w, main.min.y), vec2(mixer_w, main_h));

        self.top_bar(ui, top, &v);
        if banner_h > 0.0 {
            self.banner_row(ui, banner);
        }
        for d in 0..DECKS {
            let h = wave_h / 2.0;
            let r = Rect::from_min_size(pos2(waves.min.x, waves.min.y + d as f32 * h), vec2(waves.width(), h));
            self.waveform(ui, r.shrink2(vec2(0.0, 1.0)), d, &v.decks[d]);
        }
        self.zoom_buttons(ui, waves);
        for d in 0..DECKS {
            self.deck_panel(ui, deck_rects[d].shrink(6.0), d, &v.decks[d]);
        }
        self.mixer_panel(ui, mixer.shrink2(vec2(0.0, 6.0)), &v);
        self.fx_bar(ui, fx_bar.shrink2(vec2(6.0, 2.0)), &v);
        self.browser(ui, browser.shrink(6.0));

        let busy = v.decks.iter().any(|d| d.playing || d.previewing || d.touched || !d.complete)
            || self.decks.iter().any(|d| d.loading.is_some())
            || v.peak_master > 0.001
            || self.touches.touch_mode();
        if busy {
            ui.ctx().request_repaint();
        } else if self.recorder.is_recording() {
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        } else if v.decks.iter().any(|d| d.loaded) {
            // Paused decks blink CUE/PLAY at 1 Hz (2 edges/s): wake exactly on the next edge.
            let now = ui.input(|i| i.time);
            let next_edge = ((now * 2.0).floor() + 1.0) / 2.0;
            ui.ctx().request_repaint_after(Duration::from_secs_f64((next_edge - now).max(0.01)));
        } else {
            ui.ctx().request_repaint_after(IDLE_REPAINT);
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(f) = self.recorder.stop() {
            eprintln!("recording: saved {}", f.parts.first().map(|p| p.path.display().to_string()).unwrap_or_default());
        }
        let _ = self.config.save();
        self.store.lock().flush(true);
    }
}

// ---------------------------------------------------------------------------------------------
// Drawing helpers
// ---------------------------------------------------------------------------------------------

/// The layout needs about 1100 × 700 points. On a screen that's smaller in points (e.g. a
/// 1280 × 800 panel at 150 % scaling) shrink everything to fit instead of clipping.
const DESIGN_MIN: Vec2 = vec2(1100.0, 700.0);

fn fit_to_window(ctx: &egui::Context, full: Rect) {
    let native = ctx.native_pixels_per_point().unwrap_or(1.0);
    let phys = full.size() * ctx.pixels_per_point();
    let fit = (phys.x / native / DESIGN_MIN.x).min(phys.y / native / DESIGN_MIN.y).min(1.0).max(0.5);
    if (ctx.zoom_factor() - fit).abs() > 0.01 {
        ctx.set_zoom_factor(fit);
    }
}

fn text_fit(p: &Painter, pos: Pos2, anchor: Align2, s: &str, font: FontId, color: Color32, max_w: f32) {
    let mut job = LayoutJob::simple_singleline(s.to_owned(), font, color);
    job.wrap = TextWrapping::truncate_at_width(max_w.max(1.0));
    let g = p.layout_job(job);
    let r = anchor.anchor_size(pos, g.size());
    p.galley(r.min, g, color);
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

fn card(p: &Painter, r: Rect) {
    p.rect_filled(r, CornerRadius::same(t::R_CARD), t::SURFACE);
    p.rect_stroke(r, CornerRadius::same(t::R_CARD), Stroke::new(1.0, t::BORDER), StrokeKind::Inside);
}

/// Shared input for every custom control: touch (per finger) or mouse/keyboard.
/// A focused control activated with Enter/Space reports `key = true`.
fn control(ui: &Ui, tc: &mut Touches, r: Rect, id: Id, info: impl Fn() -> WidgetInfo) -> (crate::touch::Ctl, egui::Response) {
    let resp = ui.interact(r, id, Sense::click_and_drag());
    resp.widget_info(info);
    let mut c = tc.ctl(r, id, &resp);
    c.key = resp.has_focus() && ui.input(|i| i.key_pressed(Key::Enter) || i.key_pressed(Key::Space));
    // A press and release inside one frame (a quick tap, or a synthetic click) never shows
    // as `down`: report it as a momentary press so hold-style controls still respond.
    if c.clicked && !c.released && !c.down {
        c.key = true;
        c.pressed = true;
    }
    // Keyboard activation counts as a click in touch mode too.
    c.clicked |= c.key;
    (c, resp)
}

/// A tap target over something already drawn (a readout), with a focus ring and tooltip.
fn invisible_button(ui: &Ui, tc: &mut Touches, r: Rect, id: Id, name: &str, tip: &str) -> crate::touch::Ctl {
    let n = name.to_string();
    let (c, resp) = control(ui, tc, r, id, || WidgetInfo::labeled(egui::Role::Button, true, &n));
    focus_ring(ui.painter(), r, &resp, t::R_CONTROL);
    if !tc.touch_mode() {
        resp.on_hover_text(tip);
    }
    c
}

/// −1 / +1 when ← / → is pressed while `resp` has keyboard focus.
fn focused_arrow(ui: &Ui, resp: &egui::Response) -> f64 {
    if !resp.has_focus() {
        return 0.0;
    }
    ui.input(|i| i.key_pressed(Key::ArrowRight) as i32 - i.key_pressed(Key::ArrowLeft) as i32) as f64
}

/// Like [`button`], with a tooltip (for terse labels and to show the keyboard shortcut).
#[allow(clippy::too_many_arguments)]
fn button_tip(ui: &Ui, tc: &mut Touches, r: Rect, id: Id, label: &str, lit: Option<Color32>, font: FontId, tip: &str) -> crate::touch::Ctl {
    let (c, resp) = button_resp(ui, tc, r, id, label, lit, font);
    if !tc.touch_mode() {
        resp.on_hover_text(tip);
    }
    c
}

fn focus_ring(p: &Painter, r: Rect, resp: &egui::Response, radius: u8) {
    if resp.has_focus() {
        p.rect_stroke(r.expand(2.0), CornerRadius::same(radius + 2), Stroke::new(2.0, t::ACCENT), StrokeKind::Outside);
    }
}

/// A flat hardware-style button. `lit` lights it in the given colour.
fn button(ui: &Ui, tc: &mut Touches, r: Rect, id: Id, label: &str, lit: Option<Color32>, font: FontId) -> crate::touch::Ctl {
    button_resp(ui, tc, r, id, label, lit, font).0
}

fn button_resp(ui: &Ui, tc: &mut Touches, r: Rect, id: Id, label: &str, lit: Option<Color32>, font: FontId) -> (crate::touch::Ctl, egui::Response) {
    let l = label.to_string();
    let selected = lit.is_some();
    let (c, resp) = control(ui, tc, r, id, || WidgetInfo::selected(egui::Role::Button, true, selected, &l));
    let p = ui.painter();
    let (fill, stroke, ink) = match lit {
        // Label mixed toward INK so it keeps ≥ 4.5:1 on its own tinted fill (REV, DELETE).
        Some(col) => (col.gamma_multiply(0.18), col, mix(col, t::INK, 0.4)),
        None if c.down => (t::HOVER, t::INK_FAINT, t::INK),
        None if c.hovered => (t::HOVER, t::BORDER, t::INK),
        None => (t::RAISED, t::BORDER, t::INK_MUTED),
    };
    let fill = if c.down && lit.is_some() { fill.gamma_multiply(1.4) } else { fill };
    p.rect_filled(r, CornerRadius::same(t::R_CONTROL), fill);
    p.rect_stroke(r, CornerRadius::same(t::R_CONTROL), Stroke::new(1.0, stroke), StrokeKind::Inside);
    p.text(r.center(), Align2::CENTER_CENTER, label, font, ink);
    focus_ring(p, r, &resp, t::R_CONTROL);
    (c, resp)
}

fn round_button(ui: &Ui, tc: &mut Touches, r: Rect, id: Id, label: &str, col: Color32, lit: bool) -> crate::touch::Ctl {
    let l = label.to_string();
    let (c, resp) = control(ui, tc, r, id, || WidgetInfo::labeled(egui::Role::Button, true, &l));
    let p = ui.painter();
    let center = r.center();
    let rad = r.width().min(r.height()) / 2.0;
    p.circle_filled(center, rad, if c.down { t::HOVER } else { t::RAISED });
    p.circle_stroke(center, rad - 1.5, Stroke::new(3.0, if lit { col } else { col.gamma_multiply(0.25) }));
    p.text(center, Align2::CENTER_CENTER, label, semi(15.0), if lit { col } else { t::INK_MUTED });
    if resp.has_focus() {
        p.circle_stroke(center, rad + 3.0, Stroke::new(2.0, t::ACCENT));
    }
    c
}

fn arc_points(c: Pos2, r: f32, a0: f32, a1: f32) -> Vec<Pos2> {
    let n = (((a1 - a0).abs() * r / 3.0) as usize).max(4);
    (0..=n)
        .map(|i| {
            let a = a0 + (a1 - a0) * i as f32 / n as f32;
            c + r * Vec2::angled(a)
        })
        .collect()
}

/// Rotary knob: drag to turn, double-tap to centre, arrow keys when focused.
#[allow(clippy::too_many_arguments)]
fn knob(ui: &Ui, tc: &mut Touches, r: Rect, id: Id, value: f32, label: &str, color: Color32, bipolar: bool) -> Option<f32> {
    let l = label.to_string();
    let (c, resp) = control(ui, tc, r, id, || WidgetInfo::slider(true, value as f64, &l));
    let p = ui.painter();
    let label_h = 14.0;
    let kr = ((r.height() - label_h).min(r.width()) * 0.5 - 3.0).max(6.0);
    let center = pos2(r.center().x, r.min.y + 2.0 + kr);
    use std::f32::consts::PI;
    let (start, sweep) = (0.75 * PI, 1.5 * PI);
    let ang = |v: f32| start + sweep * v.clamp(0.0, 1.0);
    p.circle_filled(center, kr, t::RAISED);
    p.add(Shape::line(arc_points(center, kr - 2.0, ang(0.0), ang(1.0)), Stroke::new(3.0, t::INSET)));
    let from = if bipolar { 0.5 } else { 0.0 };
    if (value - from).abs() > 0.004 {
        p.add(Shape::line(arc_points(center, kr - 2.0, ang(from.min(value)), ang(from.max(value))), Stroke::new(3.0, color)));
    }
    let tip = center + (kr - 6.0) * Vec2::angled(ang(value));
    p.line_segment([center + (kr * 0.25) * Vec2::angled(ang(value)), tip], Stroke::new(2.0, t::INK));
    if c.hovered || c.down {
        p.circle_stroke(center, kr, Stroke::new(1.0, t::INK_FAINT));
    }
    if resp.has_focus() {
        p.circle_stroke(center, kr + 3.0, Stroke::new(2.0, t::ACCENT));
    }
    p.text(pos2(r.center().x, r.max.y), Align2::CENTER_BOTTOM, label, caption(), t::INK_MUTED);
    if c.double {
        return Some(if bipolar { 0.5 } else { value });
    }
    if c.down && c.delta != Vec2::ZERO {
        let nv = (value + (c.delta.x - c.delta.y) / 160.0).clamp(0.0, 1.0);
        if nv != value {
            return Some(nv);
        }
    }
    if resp.has_focus() {
        let step = ui.input(|i| {
            (i.key_pressed(Key::ArrowUp) || i.key_pressed(Key::ArrowRight)) as i32 - (i.key_pressed(Key::ArrowDown) || i.key_pressed(Key::ArrowLeft)) as i32
        });
        if step != 0 {
            return Some((value + step as f32 * 0.05).clamp(0.0, 1.0));
        }
    }
    None
}

/// Vertical fader. `value` 1.0 = top.
#[allow(clippy::too_many_arguments)]
fn vfader(ui: &Ui, tc: &mut Touches, r: Rect, id: Id, value: f32, color: Color32, label: &str) -> Option<f32> {
    let l = label.to_string();
    let (c, resp) = control(ui, tc, r, id, || WidgetInfo::slider(true, value as f64, &l));
    let p = ui.painter();
    let track = Rect::from_center_size(r.center(), vec2(4.0, r.height() - 16.0));
    p.rect_filled(track, 2, t::INSET);
    let y = track.max.y - value.clamp(0.0, 1.0) * track.height();
    let cap = Rect::from_center_size(pos2(r.center().x, y), vec2(r.width() - 2.0, 16.0));
    p.rect_filled(cap, CornerRadius::same(3), if c.down { t::HOVER } else { t::RAISED });
    p.rect_stroke(cap, CornerRadius::same(3), Stroke::new(1.0, if resp.has_focus() { t::ACCENT } else { t::BORDER }), StrokeKind::Inside);
    p.hline(cap.x_range().shrink(3.0), y, Stroke::new(2.0, color));
    if c.down || c.clicked {
        if let Some(pos) = c.pos {
            return Some(((track.max.y - pos.y) / track.height()).clamp(0.0, 1.0));
        }
    }
    if resp.has_focus() {
        let step = ui.input(|i| i.key_pressed(Key::ArrowUp) as i32 - i.key_pressed(Key::ArrowDown) as i32);
        if step != 0 {
            return Some((value + step as f32 * 0.05).clamp(0.0, 1.0));
        }
    }
    None
}

/// Horizontal slider: drag or tap to set, ← → when focused. The label is drawn inside.
fn hslider(ui: &Ui, tc: &mut Touches, r: Rect, id: Id, value: f32, color: Color32, label: &str) -> Option<f32> {
    let l = label.to_string();
    let (c, resp) = control(ui, tc, r, id, || WidgetInfo::slider(true, value as f64, &l));
    let p = ui.painter();
    p.rect_filled(r, CornerRadius::same(t::R_CONTROL), t::INSET);
    let fill = Rect::from_min_size(r.min, vec2(r.width() * value.clamp(0.0, 1.0), r.height()));
    p.rect_filled(fill, CornerRadius::same(t::R_CONTROL), color.gamma_multiply(0.25));
    p.vline(fill.max.x, r.y_range().shrink(4.0), Stroke::new(2.0, color));
    p.text(r.left_center() + vec2(8.0, 0.0), Align2::LEFT_CENTER, "LEVEL", caption(), t::INK_MUTED);
    p.text(r.right_center() - vec2(8.0, 0.0), Align2::RIGHT_CENTER, format!("{:.0}", value * 100.0), mono(12.0), t::INK_MUTED);
    focus_ring(p, r, &resp, t::R_CONTROL);
    if c.down || c.clicked {
        if let Some(pos) = c.pos {
            return Some(((pos.x - r.min.x) / r.width()).clamp(0.0, 1.0));
        }
    }
    let step = focused_arrow(ui, &resp);
    if step != 0.0 {
        return Some((value + step as f32 * 0.05).clamp(0.0, 1.0));
    }
    None
}

fn meter(p: &Painter, r: Rect, level: f32) {
    p.rect_filled(r, 1, t::INSET);
    let segs = 14;
    let gap = 2.0;
    let seg_h = (r.height() - gap * (segs - 1) as f32) / segs as f32;
    let db = 20.0 * level.max(1e-6).log10();
    let lit = (((db + 42.0) / 42.0) * segs as f32).ceil() as i32;
    for i in 0..segs {
        let y1 = r.max.y - i as f32 * (seg_h + gap);
        let seg = Rect::from_min_max(pos2(r.min.x, y1 - seg_h), pos2(r.max.x, y1));
        let col = if i >= segs - 2 { t::DANGER } else if i >= segs - 5 { t::CUE } else { t::PLAY };
        p.rect_filled(seg, 1, if (i as i32) < lit { col } else { col.gamma_multiply(0.12) });
    }
}

/// rekordbox colour if the export assigned one, else the default pad colour.
fn hot_color(i: usize, rgb: Option<[u8; 3]>) -> Color32 {
    rgb.map(|[r, g, b]| Color32::from_rgb(r, g, b)).unwrap_or(t::HOT_CUE[i % 8])
}

fn fmt_time(secs: f64) -> String {
    // Round to tenths first so 119.97 s shows as 02:00.0, not 01:60.0.
    let tenths = (secs.max(0.0) * 10.0).round() as i64;
    format!("{:02}:{:02}.{}", tenths / 600, (tenths / 10) % 60, tenths % 10)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_controls_reach_the_accessibility_tree() {
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        ctx.enable_accesskit();
        let mut tc = Touches::default();
        let mut out = None;
        for _ in 0..2 {
            let mut o = ctx.run_ui(Default::default(), |ui| {
                let r = Rect::from_min_size(pos2(10.0, 10.0), vec2(60.0, 32.0));
                button_tip(ui, &mut tc, r, Id::NULL.with("t"), "PLAY", None, semi(13.0), "Play/pause ( Space )");
            });
            o.textures_delta.clear(); // no renderer in tests
            out = Some(o);
        }
        let o = out.unwrap();
        let update = o.platform_output.accesskit_update.expect("accesskit update");
        let labels: Vec<String> = update.nodes.iter().filter_map(|(_, n)| n.label().map(str::to_string)).collect();
        assert!(labels.iter().any(|l| l == "PLAY"), "{labels:?}");
    }

    #[test]
    fn time_format_rounds_cleanly() {
        assert_eq!(super::fmt_time(119.97), "02:00.0");
        assert_eq!(super::fmt_time(61.24), "01:01.2");
        assert_eq!(super::fmt_time(-3.0), "00:00.0");
    }
}

/// Small painted glyphs for browser rows (folder, drive, playlist, collection).
fn nav_icon(p: &Painter, c: Pos2, kind: NavKind) {
    let ink = t::INK_MUTED;
    let st = Stroke::new(1.5, ink);
    match kind {
        NavKind::Folder => {
            let body = Rect::from_center_size(c + vec2(0.0, 1.5), vec2(16.0, 11.0));
            p.rect_stroke(body, CornerRadius::same(2), st, StrokeKind::Middle);
            p.line_segment([pos2(body.min.x, body.min.y), pos2(body.min.x + 6.0, body.min.y - 3.0)], st);
            p.line_segment([pos2(body.min.x + 6.0, body.min.y - 3.0), pos2(body.min.x + 8.0, body.min.y)], st);
        }
        NavKind::Drive => {
            let body = Rect::from_center_size(c + vec2(0.0, 2.0), vec2(10.0, 13.0));
            p.rect_stroke(body, CornerRadius::same(2), Stroke::new(1.5, t::CUE), StrokeKind::Middle);
            let plug = Rect::from_min_max(pos2(c.x - 3.0, body.min.y - 5.0), pos2(c.x + 3.0, body.min.y));
            p.rect_stroke(plug, CornerRadius::ZERO, Stroke::new(1.5, t::CUE), StrokeKind::Middle);
        }
        NavKind::Playlist => {
            for k in 0..3 {
                let y = c.y - 5.0 + k as f32 * 5.0;
                p.circle_filled(pos2(c.x - 7.0, y), 1.5, ink);
                p.line_segment([pos2(c.x - 3.0, y), pos2(c.x + 8.0, y)], st);
            }
        }
        NavKind::Collection => {
            p.circle_stroke(c, 7.5, st);
            p.circle_filled(c, 2.0, ink);
        }
    }
}

/// Sort-direction caret drawn next to a column header.
fn caret(p: &Painter, at: Pos2, up: bool, col: Color32) {
    let s = if up { -1.0 } else { 1.0 };
    p.add(Shape::convex_polygon(vec![pos2(at.x - 4.0, at.y - 2.0 * s), pos2(at.x + 4.0, at.y - 2.0 * s), pos2(at.x, at.y + 3.0 * s)], col, Stroke::NONE));
}

// ---------------------------------------------------------------------------------------------
// Regions
// ---------------------------------------------------------------------------------------------

impl App {
    fn top_bar(&mut self, ui: &Ui, r: Rect, v: &EngineView) {
        let p = ui.painter().clone();
        p.text(pos2(r.min.x + 14.0, r.center().y), Align2::LEFT_CENTER, "PLATTER", semi(19.0), t::INK);
        let dot = |x: f32, ok: bool| p.circle_filled(pos2(x, r.center().y), 4.0, if ok { t::PLAY } else { t::DANGER });
        let mut x = r.min.x + 104.0;
        match (&self.audio, &self.audio_error) {
            (Some(a), _) => {
                dot(x, true);
                let routing = match v.routing {
                    crate::engine::CueRouting::Channels34 => "phones on 3/4",
                    crate::engine::CueRouting::SplitMono => "split cue L/R",
                    crate::engine::CueRouting::MasterOnly => "no cue output",
                };
                let s = format!("{}  ·  {} Hz  ·  {routing}", a.description, v.sample_rate);
                text_fit(&p, pos2(x + 10.0, r.center().y), Align2::LEFT_CENTER, &s, cond(15.0), t::INK_MUTED, 400.0);
            }
            (None, e) => {
                dot(x, false);
                let s = format!("No audio — {}. Retrying…", e.as_deref().unwrap_or("not started"));
                text_fit(&p, pos2(x + 10.0, r.center().y), Align2::LEFT_CENTER, &s, cond(15.0), t::DANGER, 400.0);
            }
        }
        x += 430.0;
        let midi = self.midi.lock().clone();
        let pad = self.gamepad.as_ref().and_then(|g| g.lock().clone());
        dot(x, midi.is_some() || pad.is_some());
        let ctl = match (&midi, &pad) {
            (Some(m), Some(g)) => format!("{m}  ·  {g}"),
            (Some(m), None) => m.clone(),
            (None, Some(g)) => format!("{g}  ·  no DJ controller"),
            (None, None) => "No controller — touch & keyboard".into(),
        };
        let right_w = 522.0;
        text_fit(&p, pos2(x + 10.0, r.center().y), Align2::LEFT_CENTER, &ctl, cond(15.0), t::INK_MUTED, r.max.x - right_w - x - 20.0);

        // Right: QUANTIZE, DSP load, master meter.
        // EXIT (press twice), at the far right like a window's close button.
        let er = Rect::from_min_size(pos2(r.max.x - 66.0, r.min.y + 5.0), vec2(58.0, 24.0));
        let confirm = self.exit_confirm.is_some_and(|t| t.elapsed() < Duration::from_secs(3));
        let busy = self.recorder.is_recording() || v.decks.iter().any(|d| d.playing);
        let tip = match (confirm, busy) {
            (true, true) => "Press again to quit: playback stops and any recording is saved",
            (true, false) => "Press again to quit",
            (false, _) => "Quit Platter. Press twice  ( Ctrl+Q )",
        };
        if button_tip(ui, &mut self.touches, er, Id::NULL.with("exit"), if confirm { "EXIT?" } else { "EXIT" }, confirm.then_some(t::DANGER), semi(13.0), tip).clicked {
            self.exit_press(ui.ctx());
        }
        let m = Rect::from_min_size(pos2(r.max.x - 186.0, r.center().y - 4.0), vec2(106.0, 8.0));
        p.rect_filled(m, 2, t::INSET);
        let db = 20.0 * v.peak_master.max(1e-6).log10();
        let f = ((db + 42.0) / 42.0).clamp(0.0, 1.0);
        let col = if db > -1.0 { t::DANGER } else if db > -6.0 { t::CUE } else { t::PLAY };
        p.rect_filled(Rect::from_min_size(m.min, vec2(m.width() * f, m.height())), 2, col);
        p.text(pos2(m.min.x - 8.0, r.center().y), Align2::RIGHT_CENTER, "MASTER", caption(), t::INK_FAINT);
        let load = (v.dsp_load * 100.0).round() as i32;
        let xr = audio::XRUNS.load(Ordering::Relaxed);
        let (dsp, dcol) = if xr > 0 {
            (format!("DSP {load}% · {xr} dropouts"), t::DANGER)
        } else {
            (format!("DSP {load}%"), if load > 70 { t::CUE } else { t::INK_FAINT })
        };
        p.text(pos2(m.min.x - 64.0, r.center().y), Align2::RIGHT_CENTER, dsp, mono(12.0), dcol);
        self.rec_button(ui, &p, Rect::from_min_size(pos2(r.max.x - right_w + 6.0, r.min.y + 5.0), vec2(90.0, 24.0)));
        let qr = Rect::from_min_size(pos2(r.max.x - right_w + 102.0, r.min.y + 5.0), vec2(84.0, 24.0));
        let qtip = if v.quantize { "Quantize on: cues, loops and jumps snap to the beat  ( G )" } else { "Quantize off  ( G )" };
        if button_tip(ui, &mut self.touches, qr, Id::NULL.with("quantize"), "QUANTIZE", v.quantize.then_some(t::LOOP), semi(13.0), qtip).clicked {
            self.send(Cmd::QuantizeToggle);
        }
    }

    /// BEAT FX: effect, beat value, channel, level and ON, laid out like a DJM's FX section.
    fn fx_bar(&mut self, ui: &Ui, r: Rect, v: &EngineView) {
        use crate::engine::fx::{beats_label, FxTarget, FX_KINDS};
        let p = ui.painter().clone();
        card(&p, r);
        let fx = v.mixer.fx;
        let h = 32.0;
        let y = r.center().y - h / 2.0;
        let mut x = r.min.x + 12.0;
        p.text(pos2(x, r.center().y), Align2::LEFT_CENTER, "BEAT FX", caption(), t::INK_FAINT);
        x += 58.0;
        // Right-hand block widths, so the effect chips get whatever is left.
        let (beat_w, ch_w, lvl_w, on_w) = (32.0 * 2.0 + 52.0, 3.0 * 38.0, 150.0, 64.0);
        let right = beat_w + ch_w + lvl_w + on_w + 4.0 * 14.0;
        let chip_w = ((r.max.x - 12.0 - x - right) / FX_KINDS.len() as f32 - 4.0).clamp(48.0, 92.0);
        for (i, k) in FX_KINDS.iter().enumerate() {
            let br = Rect::from_min_size(pos2(x, y), vec2(chip_w, h));
            let lit = (fx.kind == *k).then_some(if fx.on { t::DANGER } else { t::LOOP });
            if button(ui, &mut self.touches, br, Id::NULL.with(("fxk", i)), k.name(), lit, cond(14.0)).clicked {
                self.send(Cmd::Mix(MixParam::FxKind(*k)));
            }
            x += chip_w + 4.0;
        }
        x += 10.0;
        // Beat ◀ value ▶
        let l = Rect::from_min_size(pos2(x, y), vec2(32.0, h));
        if button_tip(ui, &mut self.touches, l, Id::NULL.with("fxb-"), "‹", None, semi(18.0), "Shorter beat value").clicked {
            self.send(Cmd::Mix(MixParam::FxBeat(-1)));
        }
        let label = if fx.kind == crate::engine::fx::FxKind::Reverb { format!("{}%", 10 + fx.beat_idx * 11) } else { beats_label(fx.beats()) };
        p.text(pos2(x + 32.0 + 26.0, r.center().y), Align2::CENTER_CENTER, label, mono(15.0), t::INK);
        let rr = Rect::from_min_size(pos2(x + 32.0 + 52.0, y), vec2(32.0, h));
        if button_tip(ui, &mut self.touches, rr, Id::NULL.with("fxb+"), "›", None, semi(18.0), "Longer beat value").clicked {
            self.send(Cmd::Mix(MixParam::FxBeat(1)));
        }
        x += beat_w + 14.0;
        // Channel: 1 / 2 / MASTER
        for (i, (lab, tg)) in [("1", FxTarget::Ch(0)), ("2", FxTarget::Ch(1)), ("M", FxTarget::Master)].into_iter().enumerate() {
            let br = Rect::from_min_size(pos2(x + i as f32 * 38.0, y), vec2(34.0, h));
            let col = match tg {
                FxTarget::Ch(c) => t::DECK_TINT[c],
                FxTarget::Master => t::INK,
            };
            let tip = match tg {
                FxTarget::Ch(c) => format!("Effect on channel {}", c + 1),
                FxTarget::Master => "Effect on the master".to_string(),
            };
            if button_tip(ui, &mut self.touches, br, Id::NULL.with(("fxc", i)), lab, (fx.target == tg).then_some(col), semi(15.0), &tip).clicked {
                self.send(Cmd::Mix(MixParam::FxTarget(tg)));
            }
        }
        x += ch_w + 14.0;
        // LEVEL/DEPTH
        let lr = Rect::from_min_size(pos2(x, y), vec2(lvl_w, h));
        if let Some(nv) = hslider(ui, &mut self.touches, lr, Id::NULL.with("fxlvl"), fx.level, if fx.on { t::DANGER } else { t::INK_MUTED }, "Beat FX level/depth") {
            self.send_touched(Cmd::Mix(MixParam::FxLevel(nv)), MidiCtl::FxLevel);
        }
        x += lvl_w + 14.0;
        // ON (blinks red while on, like the hardware)
        let or = Rect::from_min_size(pos2(x, y), vec2(on_w, h));
        let blink = (ui.input(|i| i.time) * 2.0) as i64 % 2 == 0;
        let lit = fx.on.then_some(if blink { t::DANGER } else { t::DANGER.gamma_multiply(0.7) });
        if button_tip(ui, &mut self.touches, or, Id::NULL.with("fxon"), "ON", lit, semi(15.0), "Beat FX on/off  ( F )").clicked {
            self.send(Cmd::Mix(MixParam::FxToggle));
        }
    }

    /// Quit on the second press within 3 s, so a stray tap can't end a set.
    fn exit_press(&mut self, ctx: &egui::Context) {
        if self.exit_confirm.is_some_and(|t| t.elapsed() < Duration::from_secs(3)) {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        } else {
            self.exit_confirm = Some(Instant::now());
            ctx.request_repaint_after(Duration::from_secs(3));
        }
    }

    fn rec_button(&mut self, ui: &Ui, p: &Painter, r: Rect) {
        let rec = self.recorder.is_recording();
        let confirm = rec && self.rec_confirm.is_some();
        let label = if confirm {
            "STOP?".to_string()
        } else if rec {
            format!("   {}", crate::recorder::fmt_elapsed(self.recorder.elapsed()))
        } else {
            "   REC".to_string()
        };
        let tip = if confirm {
            "Press again to stop recording"
        } else if rec {
            "Recording the mix. Press twice to stop  ( R R )"
        } else {
            "Record the mix to Music/Platter Recordings, with a tracklist  ( R )"
        };
        let font = if rec && !confirm { mono(13.0) } else { semi(13.0) };
        if button_tip(ui, &mut self.touches, r, Id::NULL.with("rec"), &label, rec.then_some(t::DANGER), font, tip).clicked {
            self.record_toggle();
        }
        if !confirm {
            let blink = (ui.input(|i| i.time) * 2.0) as i64 % 2 == 0;
            let lit = !rec || blink;
            let c = pos2(r.min.x + 16.0, r.center().y);
            if lit {
                p.circle_filled(c, 4.5, t::DANGER);
            } else {
                p.circle_stroke(c, 4.0, Stroke::new(1.2, t::DANGER));
            }
        }
    }

    fn banner_row(&mut self, ui: &Ui, r: Rect) {
        let p = ui.painter().clone();
        p.rect_filled(r, 0, t::CUE.gamma_multiply(0.14));
        if let Some(b) = &self.banner {
            text_fit(&p, pos2(r.min.x + 14.0, r.center().y), Align2::LEFT_CENTER, b, cond(15.0), t::INK, r.width() - 140.0);
        }
        let br = Rect::from_min_size(pos2(r.max.x - 104.0, r.min.y + 3.0), vec2(96.0, 24.0));
        if button(ui, &mut self.touches, br, Id::NULL.with("banner"), "DISMISS", None, semi(13.0)).clicked {
            self.banner = None;
        }
    }

    fn zoom_buttons(&mut self, ui: &Ui, waves: Rect) {
        let w = 30.0;
        // Waveform colour (BLUE / RGB / 3BAND), as WAVEFORM COLOR on a CDJ.
        let mode = self.config.waveform_color;
        let mr = Rect::from_min_size(pos2(waves.max.x - 2.0 * (w + 4.0) - 6.0 - 66.0, waves.min.y + 4.0), vec2(62.0, 26.0));
        if button_tip(ui, &mut self.touches, mr, Id::NULL.with("wavemode"), crate::wave::mode_name(mode), None, semi(13.0), "Waveform colour: Blue / RGB / 3-Band  ( W )").clicked {
            self.config.waveform_color = crate::wave::next_mode(mode);
        }
        for (i, (label, dir)) in [("−", 1), ("+", -1)].into_iter().enumerate() {
            let r = Rect::from_min_size(pos2(waves.max.x - 2.0 * (w + 4.0) - 6.0 + i as f32 * (w + 4.0), waves.min.y + 4.0), vec2(w, 26.0));
            let tip = if dir > 0 { "Zoom waveforms out  ( − )" } else { "Zoom waveforms in  ( = )" };
            if button_tip(ui, &mut self.touches, r, Id::NULL.with(("zoom", i)), label, None, semi(16.0), tip).clicked {
                self.set_zoom(dir);
            }
        }
    }

    fn waveform(&mut self, ui: &Ui, r: Rect, d: usize, dv: &DeckView) {
        let p = ui.painter_at(r);
        p.rect_filled(r, 0, t::INSET);
        let tint = t::DECK_TINT[d];
        let Some(track) = self.decks[d].track.clone().filter(|_| dv.loaded) else {
            p.text(r.left_center() + vec2(14.0, 0.0), Align2::LEFT_CENTER, format!("{}", d + 1), semi(20.0), tint.gamma_multiply(0.6));
            return;
        };
        let sr = track.sample_rate as f64;
        let pos_s = dv.pos / sr;
        let pps = r.width() as f64 / self.config.waveform_seconds; // pixels per second
        // Playhead centred, or toward the left (WAVEFORM CURRENT POSITION) to see more ahead.
        let cx = if self.config.playhead_left { r.min.x + r.width() * 0.3 } else { r.center().x };
        let mid_y = r.center().y;
        let half = r.height() * 0.46;
        let t_at = |x: f32| pos_s + (x - cx) as f64 / pps;
        let x_at = |s: f64| cx + ((s - pos_s) * pps) as f32;

        if let (Some(a), Some(b)) = (dv.loop_in, dv.loop_out) {
            let lr = Rect::from_x_y_ranges(x_at(a / sr)..=x_at(b / sr), r.y_range());
            p.rect_filled(lr, 0, t::LOOP.gamma_multiply(if dv.loop_active { 0.16 } else { 0.06 }));
        }
        if let Some(g) = track.grid.load_full() {
            let lo = g.beats.partition_point(|&b| b < t_at(r.min.x) * sr);
            let hi = g.beats.partition_point(|&b| b <= t_at(r.max.x) * sr);
            for k in lo..hi {
                let x = x_at(g.beats[k] / sr);
                let bar = g.is_downbeat(k as i64);
                p.vline(x, r.y_range(), Stroke::new(1.0, if bar { t::INK_FAINT.gamma_multiply(0.75) } else { t::BORDER }));
                if bar {
                    p.rect_filled(Rect::from_min_size(pos2(x - 1.0, r.max.y - 5.0), vec2(3.0, 5.0)), 0, t::INK_FAINT);
                }
            }
        }
        // Waveform in the chosen colour mode, one column per pixel.
        {
            let own = track.wave.read();
            let mode = self.config.waveform_color;
            let mut mesh = Mesh::default();
            let x0 = r.min.x.floor() as i32;
            let x1 = r.max.x.ceil() as i32;
            let px_s = 1.0 / pps;
            for x in x0..x1 {
                let ts = t_at(x as f32);
                let a = crate::wave::bin_at(&track, ts);
                if a < 0.0 {
                    continue;
                }
                let b = crate::wave::bin_at(&track, ts + px_s);
                let Some(col) = crate::wave::column(&track, &own, mode, a as usize, b as usize) else { break };
                let played = (x as f32) < cx;
                for &(h, c) in &col.layers[..col.n] {
                    let h = h * half;
                    let c = if played { c.gamma_multiply(0.62) } else { c };
                    mesh.add_colored_rect(Rect::from_min_max(pos2(x as f32, mid_y - h), pos2(x as f32 + 1.0, mid_y + h)), c);
                }
            }
            p.add(mesh);
        }
        let marker = |x: f32, col: Color32, top: bool| {
            let y = if top { r.min.y } else { r.max.y };
            let s = if top { 1.0 } else { -1.0 };
            p.add(Shape::convex_polygon(vec![pos2(x - 6.0, y), pos2(x + 6.0, y), pos2(x, y + 8.0 * s)], col, Stroke::NONE));
            p.vline(x, r.y_range(), Stroke::new(1.0, col.gamma_multiply(0.5)));
        };
        for m in dv.memory.iter().flatten() {
            marker(x_at(m / sr), t::MEMORY_CUE, false);
        }
        marker(x_at(dv.cue / sr), t::CUE, false);
        for (i, h) in dv.hot.iter().enumerate() {
            if let Some(h) = h {
                let col = hot_color(i, h.color);
                let x = x_at(h.pos / sr);
                if let Some(e) = h.loop_end {
                    p.rect_filled(Rect::from_x_y_ranges(x..=x_at(e / sr), r.min.y..=r.min.y + 4.0), 0, col.gamma_multiply(0.6));
                }
                marker(x, col, true);
                p.text(pos2(x + 4.0, r.min.y + 1.0), Align2::LEFT_TOP, HOT_LETTERS[i], semi(12.0), col);
            }
        }
        // Slip: where the track will come back in.
        if let Some(s) = dv.slip_pos {
            p.vline(x_at(s / sr), r.y_range(), Stroke::new(2.0, t::SYNC.gamma_multiply(0.8)));
        }
        p.vline(cx, r.y_range(), Stroke::new(2.0, t::INK));
        p.text(r.min + vec2(10.0, 4.0), Align2::LEFT_TOP, format!("{}", d + 1), semi(18.0), tint);

        // Drag the waveform to scratch, like pushing the record.
        let id = Id::NULL.with(("wave", d));
        let (c, resp) = control(ui, &mut self.touches, r, id, || WidgetInfo::labeled(egui::Role::Slider, true, format!("Deck {} waveform", d + 1)));
        focus_ring(&p, r.shrink(2.0), &resp, 2);
        // Focused: ← / → jump one beat.
        let k = focused_arrow(ui, &resp);
        if k != 0.0 {
            self.send(Cmd::BeatJump(d, k));
        }
        if c.down != self.decks[d].wave_held {
            self.decks[d].wave_held = c.down;
            self.send(Cmd::JogTouch(d, c.down));
        }
        if c.down && c.delta.x != 0.0 {
            let ticks = -(c.delta.x as f64) / pps / SCRATCH_SECS_PER_TICK;
            self.send(Cmd::JogScratch(d, ticks));
        }
    }

    fn deck_panel(&mut self, ui: &Ui, r: Rect, d: usize, dv: &DeckView) {
        let p = ui.painter().clone();
        card(&p, r);
        let inner = r.shrink(t::SP_M);
        let tint = t::DECK_TINT[d];
        let mut y = inner.min.y;

        // Screen readers: the painted header and readouts as one label (registered first, so
        // the controls on top of it keep their hover and clicks).
        {
            let title = self.decks[d].track.as_ref().map(|t| format!("{} by {}", t.title, t.artist)).unwrap_or_else(|| "empty".into());
            let bpm = dv.bpm.map(|b| format!(", {b:.2} BPM")).unwrap_or_default();
            let state = if dv.playing { "playing" } else if dv.loaded { "paused" } else { "" };
            let desc = format!("Deck {}: {title}, {state}, {} remaining, tempo {:+.2}%{bpm}", d + 1, fmt_time(((dv.length as f64 - dv.pos) / dv.sample_rate.max(1) as f64).max(0.0)), (dv.rate - 1.0) * 100.0);
            let hr = Rect::from_min_size(inner.min, vec2(inner.width(), 46.0 + 6.0 + 52.0));
            ui.interact(hr, Id::NULL.with(("deck_info", d)), Sense::hover()).widget_info(|| WidgetInfo::labeled(egui::Role::Label, true, &desc));
        }

        // Header: deck badge, title/artist, status chips.
        let badge_c = pos2(inner.min.x + 14.0, y + 20.0);
        p.circle_filled(badge_c, 14.0, tint.gamma_multiply(0.18));
        p.circle_stroke(badge_c, 14.0, Stroke::new(1.0, tint));
        p.text(badge_c, Align2::CENTER_CENTER, format!("{}", d + 1), semi(17.0), tint);
        let mut tx = inner.min.x + 38.0;
        let track = self.decks[d].track.clone().filter(|_| dv.loaded);
        // Cover art beside the title.
        if let (Some(key), true) = (self.decks[d].art.clone(), track.is_some()) {
            if let Some(tex) = self.art.get(&key) {
                let ar = Rect::from_min_size(pos2(tx, y + 1.0), vec2(42.0, 42.0));
                p.image(tex.id(), ar, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                p.rect_stroke(ar, CornerRadius::same(3), Stroke::new(1.0, t::BORDER), StrokeKind::Inside);
                tx += 50.0;
            }
        }
        let key = track.as_ref().map(|t| t.key.clone()).filter(|k| !k.is_empty());
        let chips: Vec<(String, Color32)> = [
            key.map(|k| (k, t::INK_MUTED)),
            dv.slip.then(|| ("SLIP".to_string(), t::SYNC)),
            dv.reverse.then(|| ("REV".to_string(), t::DANGER)),
            dv.keylock.then(|| ("MT".to_string(), t::LOOP)),
            dv.synced.then(|| ("SYNC".to_string(), t::SYNC)),
            dv.is_master.then(|| ("MASTER".to_string(), t::CUE)),
        ]
        .into_iter()
        .flatten()
        .collect();
        let shift = self.decks[d].shift;
        let grid_src = track.as_ref().and_then(|t| t.grid_source());
        let fix_bpm = shift && grid_src.is_some();
        let chips_w = if fix_bpm { 2.0 * 74.0 + 4.0 } else { chips.len() as f32 * 56.0 + 4.0 };
        let title_w = inner.max.x - tx - chips_w;
        let du = &self.decks[d];
        let sub_y = y + 26.0;
        if let Some(name) = &du.loading {
            text_fit(&p, pos2(tx, y + 2.0), Align2::LEFT_TOP, &format!("Loading {name}…"), t::title(), t::INK_MUTED, title_w);
            text_fit(&p, pos2(tx, sub_y), Align2::LEFT_TOP, "Opening the file", cond(15.0), t::INK_FAINT, title_w);
        } else if let Some(err) = &du.error {
            text_fit(&p, pos2(tx, y + 2.0), Align2::LEFT_TOP, "Couldn't load that file", t::title(), t::DANGER, title_w);
            text_fit(&p, pos2(tx, sub_y), Align2::LEFT_TOP, err, cond(15.0), t::INK_MUTED, title_w);
        } else if let Some(tr) = &track {
            text_fit(&p, pos2(tx, y + 2.0), Align2::LEFT_TOP, &tr.title, t::title(), t::INK, title_w);
            if let Some(e) = tr.error.lock().clone() {
                text_fit(&p, pos2(tx, sub_y), Align2::LEFT_TOP, &format!("Stopped decoding early: {e}"), cond(15.0), t::DANGER, title_w);
            } else {
                let artist = if tr.artist.is_empty() { "Unknown artist" } else { &tr.artist };
                text_fit(&p, pos2(tx, sub_y), Align2::LEFT_TOP, artist, cond(15.0), t::INK_MUTED, title_w);
            }
        } else {
            text_fit(&p, pos2(tx, y + 2.0), Align2::LEFT_TOP, "Empty deck", t::title(), t::INK_MUTED, title_w);
            text_fit(&p, pos2(tx, sub_y), Align2::LEFT_TOP, &format!("Pick a track below and press LOAD {}", d + 1), cond(15.0), t::INK_FAINT, title_w);
        }
        let mut chip_x = inner.max.x;
        if fix_bpm {
            // SHIFT: fix a half/double tempo detection.
            for (i, (label, f)) in [("BPM ÷2", 0.5), ("BPM ×2", 2.0)].into_iter().enumerate() {
                let br = Rect::from_min_size(pos2(inner.max.x - 148.0 + i as f32 * 76.0, y + 6.0), vec2(72.0, 26.0));
                if button(ui, &mut self.touches, br, Id::NULL.with(("bpmfix", d, i)), label, None, semi(13.5)).clicked {
                    self.scale_grid(d, f);
                }
            }
        }
        for (label, col) in chips.iter().filter(|_| !fix_bpm) {
            let w = 52.0;
            let cr = Rect::from_min_size(pos2(chip_x - w, y + 8.0), vec2(w, 22.0));
            p.rect_filled(cr, CornerRadius::same(t::R_CONTROL), col.gamma_multiply(0.18));
            text_fit(&p, cr.center(), Align2::CENTER_CENTER, label, caption(), mix(*col, t::INK, 0.4), w - 4.0);
            chip_x -= w + 4.0;
        }
        y += 46.0 + 6.0;

        // Readouts.
        let col_w = inner.width() / 3.0;
        let cap_y = y;
        let val_y = y + 16.0;
        let sr = dv.sample_rate.max(1) as f64;
        let elapsed = dv.pos / sr;
        let remain = (dv.length as f64 - dv.pos) / sr;
        let remain_mode = self.config.time_remain;
        let (cap, small) = if remain_mode { ("REMAIN", fmt_time(elapsed)) } else { ("ELAPSED", format!("-{}", fmt_time(remain))) };
        p.text(pos2(inner.min.x, cap_y), Align2::LEFT_TOP, cap, caption(), t::INK_FAINT);
        p.text(pos2(inner.min.x + 60.0, cap_y + 1.0), Align2::LEFT_TOP, small, mono(11.5), t::INK_FAINT);
        let low_time = dv.loaded && remain < 30.0 && dv.playing && dv.complete;
        let time_col = if low_time && (ui.input(|i| i.time) * 2.0) as i64 % 2 == 0 { t::DANGER } else { t::INK };
        let big = if remain_mode { format!("-{}", fmt_time(remain)) } else { fmt_time(elapsed) };
        p.text(pos2(inner.min.x, val_y), Align2::LEFT_TOP, big, t::readout(), if dv.loaded { time_col } else { t::INK_FAINT });
        // Tap the time to switch remaining/elapsed (TIME MODE).
        let time_r = Rect::from_min_size(pos2(inner.min.x, cap_y), vec2(col_w * 1.1, 50.0));
        let tip = if remain_mode { "Showing remaining time. Tap for elapsed" } else { "Showing elapsed time. Tap for remaining" };
        if invisible_button(ui, &mut self.touches, time_r, Id::NULL.with(("time", d)), "Time mode", tip).clicked {
            self.config.time_remain = !remain_mode;
        }

        let tc = inner.min.x + col_w * 1.15 + 4.0;
        p.text(pos2(tc, cap_y), Align2::LEFT_TOP, if dv.tempo_held { "TEMPO · HELD" } else { "TEMPO" }, caption(), t::INK_FAINT);
        let pct = (dv.rate - 1.0) * 100.0;
        let range = if dv.range >= 1.0 { "WIDE".to_string() } else { format!("±{:.0}", dv.range * 100.0) };
        // Measured, so "-100.00%" (WIDE range) never runs into the chip.
        let tr = p.text(pos2(tc, val_y + 3.0), Align2::LEFT_TOP, format!("{pct:+.2}%"), mono(19.0), if pct.abs() < 0.005 { t::INK_MUTED } else { t::INK });
        let rr = Rect::from_min_size(pos2(tr.max.x + 6.0, val_y + 1.0), vec2(46.0, 26.0));
        if button_tip(ui, &mut self.touches, rr, Id::NULL.with(("range", d)), &range, None, caption(), "Tempo range: ±6 / ±10 / ±16 % / WIDE").clicked {
            self.send(Cmd::TempoRangeCycle(d));
        }

        {
            let bpm_cap = match grid_src {
                Some(GridSource::Rekordbox) => "BPM · REKORDBOX GRID",
                Some(GridSource::Saved) => "BPM · SAVED GRID",
                _ => "BPM",
            };
            p.text(pos2(inner.max.x, cap_y), Align2::RIGHT_TOP, bpm_cap, caption(), t::INK_FAINT);
        }
        let bpm = dv.bpm.map(|b| format!("{b:.2}")).unwrap_or_else(|| if dv.loaded && !dv.complete { "…".into() } else { "--.--".into() });
        // Shrink the BPM readout rather than overlap the range chip on a narrow window.
        let room = inner.max.x - rr.max.x - 8.0;
        let bpm_size = (28.0f32).min(room / (bpm.chars().count().max(1) as f32 * 0.62)).max(16.0);
        p.text(pos2(inner.max.x, val_y - 3.0 + (28.0 - bpm_size) * 0.5), Align2::RIGHT_TOP, bpm, mono(bpm_size), if dv.synced { t::SYNC } else { t::INK });
        y += 52.0 + 6.0;

        // Overview (tap to seek) with decode progress.
        let ov = Rect::from_min_size(pos2(inner.min.x, y), vec2(inner.width(), 30.0));
        self.overview(ui, &p, ov, d, dv);
        y += 30.0 + 6.0;

        // Two button rows: loops & jumps, then deck functions.
        let jump = self.config.beat_jump * if shift { 4.0 } else { 1.0 };
        let jb = crate::rekordbox::settings::fmt_beats(jump);
        let (jump_l, jump_r) = (format!("« {jb}"), format!("{jb} »"));
        let rows: [[(&str, Option<Color32>, u8); 8]; 2] = [
            [
                ("IN", (dv.loop_in.is_some() && !dv.loop_active).then_some(t::LOOP), 0),
                ("OUT", dv.loop_active.then_some(t::LOOP), 1),
                (if dv.loop_active { "EXIT" } else { "RELOOP" }, dv.loop_active.then_some(t::LOOP), 2),
                ("4 BEAT", None, 3),
                ("½×", None, 4),
                ("2×", None, 5),
                (jump_l.as_str(), None, 6),
                (jump_r.as_str(), None, 7),
            ],
            [
                ("SHIFT", shift.then_some(t::INK), 10),
                ("SYNC", dv.synced.then_some(t::SYNC), 11),
                ("MT", dv.keylock.then_some(t::LOOP), 12),
                ("SLIP", dv.slip.then_some(t::SYNC), 13),
                ("REV", dv.reverse.then_some(t::DANGER), 14),
                ("« CALL", None, 15),
                ("CALL »", None, 16),
                (if shift { "DELETE" } else { "MEMORY" }, shift.then_some(t::MEMORY_CUE), 17),
            ],
        ];
        let bw = (inner.width() - 7.0 * 5.0) / 8.0;
        for row in rows {
            for (i, (label, lit, code)) in row.into_iter().enumerate() {
                let br = Rect::from_min_size(pos2(inner.min.x + i as f32 * (bw + 5.0), y), vec2(bw, 32.0));
                let c = button(ui, &mut self.touches, br, Id::NULL.with(("btn", d, code)), label, lit, semi(13.5));
                if !c.clicked {
                    continue;
                }
                let cmd = match code {
                    0 => Some(Cmd::LoopIn(d)),
                    1 => Some(Cmd::LoopOut(d)),
                    2 => Some(Cmd::ReloopExit(d)),
                    3 => Some(Cmd::AutoLoop(d, 4.0)),
                    4 => Some(Cmd::LoopScale(d, 0.5)),
                    5 => Some(Cmd::LoopScale(d, 2.0)),
                    6 => Some(Cmd::BeatJump(d, -jump)),
                    7 => Some(Cmd::BeatJump(d, jump)),
                    10 => {
                        self.decks[d].shift = !shift;
                        None
                    }
                    11 => Some(Cmd::SyncToggle(d)),
                    12 => Some(Cmd::MasterTempoToggle(d)),
                    13 => Some(Cmd::SlipToggle(d)),
                    14 => Some(Cmd::ReverseToggle(d)),
                    15 => Some(Cmd::Call(d, -1)),
                    16 => Some(Cmd::Call(d, 1)),
                    17 => Some(if shift { Cmd::MemoryDelete(d) } else { Cmd::MemoryAdd(d) }),
                    _ => None,
                };
                if let Some(c) = cmd {
                    self.send(c);
                }
            }
            y += 32.0 + 5.0;
        }
        y += 1.0;

        // Controls: jog | CUE/PLAY | pads | tempo fader.
        let ctl = Rect::from_min_max(pos2(inner.min.x, y), inner.max);
        // Pads keep a usable width (≥ 44 pt) on narrow windows: the jog gives way first.
        let fader_w = 26.0;
        let bsize = ((ctl.height() - 8.0) / 2.0).min(if ctl.width() < 400.0 { 54.0 } else { 60.0 });
        let fixed = 12.0 + bsize + 12.0 + 10.0 + fader_w;
        let pads_min = 4.0 * 44.0 + 3.0 * 6.0;
        let jog_max = ctl.height().min(132.0).max(1.0);
        // (not clamp: on a tiny first frame the max can fall below the min)
        let jog_d = (ctl.width() - fixed - pads_min).max(84.0).min(jog_max);
        let jog = Rect::from_min_size(pos2(ctl.min.x, ctl.center().y - jog_d / 2.0), vec2(jog_d, jog_d));
        self.jog(ui, &p, jog, d, dv);

        let fader = Rect::from_min_max(pos2(ctl.max.x - fader_w, ctl.min.y), ctl.max);
        let fv = (1.0 - (dv.tempo_fader as f32 + 1.0) / 2.0).clamp(0.0, 1.0); // top = −
        let fcol = if dv.synced { t::SYNC } else if dv.tempo_held { t::CUE } else { t::INK_MUTED };
        if let Some(nv) = vfader(ui, &mut self.touches, fader, Id::NULL.with(("tempo", d)), fv, fcol, &format!("Deck {} tempo", d + 1)) {
            let f = (1.0 - nv) * 2.0 - 1.0;
            let f = if f.abs() < 0.03 { 0.0 } else { f }; // centre détente
            self.send_touched(Cmd::TempoFader(d, f as f64), MidiCtl::Tempo(d));
        }
        p.text(pos2(fader.center().x, fader.min.y), Align2::CENTER_TOP, "−", semi(13.0), t::INK_FAINT);
        p.text(pos2(fader.center().x, fader.max.y), Align2::CENTER_BOTTOM, "+", semi(13.0), t::INK_FAINT);

        // CUE / PLAY (act on press, like the hardware; two fingers can hold CUE and hit PLAY).
        let bx = jog.max.x + 12.0;
        let cue_r = Rect::from_min_size(pos2(bx, ctl.center().y - bsize - 4.0), vec2(bsize, bsize));
        let play_r = Rect::from_min_size(pos2(bx, ctl.center().y + 4.0), vec2(bsize, bsize));
        let blink = (ui.input(|i| i.time) * 2.0) as i64 % 2 == 0;
        let cue_lit = dv.loaded && (dv.playing || dv.at_cue || blink);
        let play_lit = dv.playing || dv.previewing || (dv.loaded && blink);
        let cc = round_button(ui, &mut self.touches, cue_r, Id::NULL.with(("cue", d)), "CUE", t::CUE, cue_lit);
        if cc.down != self.decks[d].cue_held {
            self.decks[d].cue_held = cc.down;
            self.send(Cmd::Cue(d, cc.down));
        } else if cc.key {
            self.send(Cmd::Cue(d, true));
            self.send(Cmd::Cue(d, false));
        }
        let pc = round_button(ui, &mut self.touches, play_r, Id::NULL.with(("play", d)), "PLAY", t::PLAY, play_lit);
        if (pc.down && !self.decks[d].play_held) || pc.key {
            self.send(Cmd::Play(d));
        }
        self.decks[d].play_held = pc.down;

        // Hot cue pads.
        let px0 = bx + bsize + 12.0;
        let px1 = fader.min.x - 10.0;
        let gap = 6.0;
        let pw = ((px1 - px0 - 3.0 * gap) / 4.0).min((ctl.height() - gap) / 2.0);
        // Narrow pads grow taller instead, up to square-ish 52 pt.
        let ph = pw.max(52.0f32.min((ctl.height() - gap) / 2.0));
        let grid_h = ph * 2.0 + gap;
        let gy = ctl.center().y - grid_h / 2.0;
        let gx = px0 + ((px1 - px0) - (pw * 4.0 + gap * 3.0)) / 2.0;
        for i in 0..HOT_CUES {
            let (row, col) = (i / 4, i % 4);
            let pr = Rect::from_min_size(pos2(gx + col as f32 * (pw + gap), gy + row as f32 * (ph + gap)), vec2(pw, ph));
            let set = dv.hot[i].is_some();
            let colr = hot_color(i, dv.hot[i].and_then(|h| h.color));
            let (c, resp) = control(ui, &mut self.touches, pr, Id::NULL.with(("pad", d, i)), || {
                WidgetInfo::selected(egui::Role::Button, true, set, format!("Hot cue {}", HOT_LETTERS[i]))
            });
            let fill = if set { colr.gamma_multiply(if c.down { 0.65 } else { 0.4 }) } else if c.down { t::HOVER } else { t::RAISED };
            p.rect_filled(pr, CornerRadius::same(t::R_CONTROL), fill);
            p.rect_stroke(pr, CornerRadius::same(t::R_CONTROL), Stroke::new(1.0, if set { colr } else { t::BORDER }), StrokeKind::Inside);
            focus_ring(&p, pr, &resp, t::R_CONTROL);
            let comment = dv.hot[i].and_then(|h| self.decks[d].track.as_ref().map(|tr| preset_comment(tr, i, h.pos)));
            let sub = match (comment.filter(|c| !c.is_empty()), dv.hot[i].and_then(|h| h.loop_end)) {
                (Some(c), _) => Some(c),
                (None, Some(_)) => Some("LOOP".to_string()),
                _ => None,
            };
            let ink = if set { t::INK } else { t::INK_FAINT };
            if let Some(sub) = sub {
                p.text(pr.center() - vec2(0.0, 7.0), Align2::CENTER_CENTER, HOT_LETTERS[i], semi(16.0), ink);
                text_fit(&p, pr.center() + vec2(0.0, 10.0), Align2::CENTER_CENTER, &sub, cond(12.0), t::INK_MUTED, pw - 6.0);
            } else {
                p.text(pr.center(), Align2::CENTER_CENTER, HOT_LETTERS[i], semi(17.0), ink);
            }
            if shift || resp.secondary_clicked() {
                if c.pressed || resp.secondary_clicked() {
                    self.send(Cmd::HotCueDelete(d, i));
                }
            } else if c.down != self.decks[d].hot_held[i] {
                self.decks[d].hot_held[i] = c.down;
                self.send(Cmd::HotCue(d, i, c.down));
            } else if c.key {
                self.send(Cmd::HotCue(d, i, true));
                self.send(Cmd::HotCue(d, i, false));
            }
        }
        if shift {
            p.text(pos2(gx, gy - 2.0), Align2::LEFT_BOTTOM, "SHIFT: tap a pad to delete it", caption(), t::INK_MUTED);
        }
    }

    fn overview(&mut self, ui: &Ui, p: &Painter, r: Rect, d: usize, dv: &DeckView) {
        p.rect_filled(r, CornerRadius::same(4), t::INSET);
        let Some(track) = self.decks[d].track.clone().filter(|_| dv.loaded) else { return };
        let length = dv.length.max(1) as f64;
        let sr = track.sample_rate as f64;
        let len_s = length / sr;
        let w = r.width() as usize;
        let mut mesh = Mesh::default();
        let play_x = r.min.x + (dv.pos / length) as f32 * r.width();
        let full = self.config.overview_full;
        let (mid, half) = (r.center().y, r.height() * 0.45);
        let base = r.max.y - 2.0;
        {
            let own = track.wave.read();
            let mode = self.config.waveform_color;
            for x in 0..w {
                let a = crate::wave::bin_at(&track, x as f64 / w as f64 * len_s).max(0.0) as usize;
                let b = crate::wave::bin_at(&track, (x + 1) as f64 / w as f64 * len_s).max(0.0) as usize;
                let Some(col) = crate::wave::column(&track, &own, mode, a, b) else { break };
                let px = r.min.x + x as f32;
                for &(h, c) in &col.layers[..col.n] {
                    let c = if px < play_x { c.gamma_multiply(0.45) } else { c };
                    // Full: mirrored about the middle. Half: grows up from the bottom.
                    let rr = if full {
                        Rect::from_min_max(pos2(px, mid - h * half), pos2(px + 1.0, mid + h * half))
                    } else {
                        Rect::from_min_max(pos2(px, base - h * (r.height() - 4.0)), pos2(px + 1.0, base))
                    };
                    mesh.add_colored_rect(rr, c);
                }
            }
        }
        p.add(mesh);
        let x_of = |f: f64| r.min.x + (f / length) as f32 * r.width();
        if !dv.complete {
            // Still decoding: show how far.
            let done = x_of(dv.decoded as f64);
            p.rect_filled(Rect::from_min_max(pos2(done, r.max.y - 3.0), pos2(r.max.x, r.max.y)), 0, t::BORDER);
            p.text(pos2(r.max.x - 6.0, r.min.y + 2.0), Align2::RIGHT_TOP, format!("decoding {:.0}%", dv.decoded as f64 / length * 100.0), caption(), t::INK_FAINT);
        }
        if let (Some(a), Some(b)) = (dv.loop_in, dv.loop_out) {
            p.rect_filled(Rect::from_x_y_ranges(x_of(a)..=x_of(b).max(x_of(a) + 2.0), r.y_range()), 0, t::LOOP.gamma_multiply(0.3));
        }
        for m in dv.memory.iter().flatten() {
            p.vline(x_of(*m), r.y_range(), Stroke::new(2.0, t::MEMORY_CUE));
        }
        for (i, h) in dv.hot.iter().enumerate() {
            if let Some(h) = h {
                p.vline(x_of(h.pos), r.y_range(), Stroke::new(2.0, hot_color(i, h.color)));
            }
        }
        p.vline(x_of(dv.cue), r.y_range(), Stroke::new(2.0, t::CUE));
        if let Some(s) = dv.slip_pos {
            p.vline(x_of(s), r.y_range(), Stroke::new(2.0, t::SYNC));
        }
        p.vline(play_x, r.y_range(), Stroke::new(2.0, t::INK));
        let (c, resp) = control(ui, &mut self.touches, r, Id::NULL.with(("overview", d)), || WidgetInfo::slider(true, dv.pos / length, format!("Deck {} position", d + 1)));
        focus_ring(p, r, &resp, t::R_CONTROL);
        // Focused: ← / → jump 16 beats.
        let k = focused_arrow(ui, &resp);
        if k != 0.0 {
            self.send(Cmd::BeatJump(d, 16.0 * k));
        }
        if c.down || c.clicked {
            if let Some(pos) = c.pos {
                let f = ((pos.x - r.min.x) / r.width()).clamp(0.0, 1.0);
                self.send(Cmd::Seek(d, f as f64));
            }
        }
    }

    fn jog(&mut self, ui: &Ui, p: &Painter, r: Rect, d: usize, dv: &DeckView) {
        let c = r.center();
        let rad = r.width() / 2.0;
        let tint = t::DECK_TINT[d];
        p.circle_filled(c, rad, t::INSET);
        p.circle_stroke(c, rad - 1.0, Stroke::new(if dv.touched { 2.0 } else { 1.0 }, if dv.touched { tint } else { t::BORDER }));
        p.circle_filled(c, rad * 0.62, t::RAISED);
        if dv.length > 0 {
            let f = (dv.pos / dv.length as f64) as f32;
            use std::f32::consts::{PI, TAU};
            p.add(Shape::line(arc_points(c, rad - 6.0, -PI / 2.0, -PI / 2.0 + TAU * f.max(0.002)), Stroke::new(3.0, tint.gamma_multiply(0.75))));
        }
        let dir = if dv.reverse { -1.0 } else { 1.0 };
        let ang = (dir * dv.platter_rev.fract() as f32) * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
        p.line_segment([c + Vec2::angled(ang) * rad * 0.66, c + Vec2::angled(ang) * (rad - 12.0)], Stroke::new(3.0, t::INK));
        let mode = if dv.vinyl { "VINYL" } else { "CDJ" };
        p.text(c, Align2::CENTER_CENTER, mode, caption(), if dv.vinyl { t::INK_MUTED } else { t::INK_FAINT });

        let (ctl, resp) = control(ui, &mut self.touches, r, Id::NULL.with(("jog", d)), || WidgetInfo::labeled(egui::Role::Slider, true, format!("Deck {} jog wheel", d + 1)));
        if resp.has_focus() {
            p.circle_stroke(r.center(), r.width() / 2.0 + 3.0, Stroke::new(2.0, t::ACCENT));
        }
        // Focused: ← / → search about a third of a second.
        let k = focused_arrow(ui, &resp);
        if k != 0.0 {
            self.send(Cmd::JogSearch(d, 3.0 * k));
        }
        let du = &mut self.decks[d];
        if ctl.down != du.jog_held {
            du.jog_held = ctl.down;
            du.jog_angle = None;
            let _ = self.cmd.send(Cmd::JogTouch(d, ctl.down));
        }
        if let (true, Some(pos)) = (ctl.down, ctl.pos) {
            let v = pos - c;
            if v.length() > 4.0 {
                let a = v.y.atan2(v.x);
                if let Some(prev) = du.jog_angle {
                    let mut da = a - prev;
                    if da > std::f32::consts::PI {
                        da -= std::f32::consts::TAU;
                    } else if da < -std::f32::consts::PI {
                        da += std::f32::consts::TAU;
                    }
                    let ticks = da as f64 / std::f64::consts::TAU * JOG_TICKS_PER_REV;
                    if ticks != 0.0 {
                        let _ = self.cmd.send(Cmd::JogScratch(d, ticks));
                    }
                }
                du.jog_angle = Some(a);
            }
        }
        // Tap the centre label to switch vinyl/CDJ mode.
        let centre = Rect::from_center_size(c, vec2(rad * 0.8, 22.0));
        if ctl.clicked && ctl.pos.is_some_and(|pp| centre.contains(pp)) {
            let _ = self.cmd.send(Cmd::VinylToggle(d));
        }
    }

    fn mixer_panel(&mut self, ui: &Ui, r: Rect, v: &EngineView) {
        let p = ui.painter().clone();
        card(&p, r);
        let inner = r.shrink(10.0);
        let strip_w = 118.0;
        let center = Rect::from_min_max(pos2(inner.min.x + strip_w, inner.min.y), pos2(inner.max.x - strip_w, inner.max.y));
        let xf_h = 40.0;
        for c in 0..2 {
            let x0 = if c == 0 { inner.min.x } else { inner.max.x - strip_w };
            let sr = Rect::from_min_max(pos2(x0, inner.min.y), pos2(x0 + strip_w, inner.max.y - xf_h - 6.0));
            self.strip(ui, sr, c, v);
        }
        let kh = 54.0;
        let kw = center.width();
        let m = v.mixer;
        let mut y = center.min.y;
        let knobs = [
            ("MASTER", m.master, t::INK_MUTED, None),
            ("PH MIX", m.head_mix, t::CUE, Some(MidiCtl::HeadMix)),
            ("PH LVL", m.head_level, t::INK_MUTED, Some(MidiCtl::HeadLevel)),
        ];
        for (i, (label, val, col, ctl)) in knobs.into_iter().enumerate() {
            let kr = Rect::from_min_size(pos2(center.min.x, y), vec2(kw, kh));
            if let Some(nv) = knob(ui, &mut self.touches, kr, Id::NULL.with(("mk", i)), val, label, col, false) {
                let mp = match i {
                    0 => MixParam::Master(nv),
                    1 => MixParam::HeadMix(nv),
                    _ => MixParam::HeadLevel(nv),
                };
                match ctl {
                    Some(c) => self.send_touched(Cmd::Mix(mp), c),
                    None => self.send(Cmd::Mix(mp)),
                }
            }
            y += kh + 4.0;
        }
        let mr = Rect::from_min_max(pos2(center.center().x - 7.0, y + 6.0), pos2(center.center().x + 7.0, inner.max.y - xf_h - 8.0));
        if mr.height() > 30.0 {
            meter(&p, mr, v.peak_master);
        }
        // Crossfader.
        let xr = Rect::from_center_size(pos2(inner.center().x, inner.max.y - xf_h / 2.0), vec2(200.0, xf_h - 6.0));
        let (c, resp) = control(ui, &mut self.touches, xr, Id::NULL.with("xfader"), || WidgetInfo::slider(true, m.crossfader as f64, "Crossfader"));
        let track = Rect::from_center_size(xr.center(), vec2(xr.width() - 24.0, 4.0));
        p.rect_filled(track, 2, t::INSET);
        p.vline(track.center().x, Rect::from_center_size(xr.center(), vec2(1.0, 14.0)).y_range(), Stroke::new(1.0, t::INK_FAINT));
        let x = track.min.x + m.crossfader * track.width();
        let cap = Rect::from_center_size(pos2(x, xr.center().y), vec2(18.0, xr.height() - 4.0));
        p.rect_filled(cap, CornerRadius::same(3), if c.down { t::HOVER } else { t::RAISED });
        p.rect_stroke(cap, CornerRadius::same(3), Stroke::new(1.0, if resp.has_focus() { t::ACCENT } else { t::BORDER }), StrokeKind::Inside);
        p.vline(x, cap.y_range().shrink(3.0), Stroke::new(2.0, t::INK));
        p.text(pos2(xr.min.x - 6.0, xr.center().y), Align2::RIGHT_CENTER, "1", semi(15.0), t::DECK_TINT[0]);
        p.text(pos2(xr.max.x + 6.0, xr.center().y), Align2::LEFT_CENTER, "2", semi(15.0), t::DECK_TINT[1]);
        let target = if c.double {
            Some(0.5)
        } else if c.down || c.clicked {
            c.pos.map(|pos| (pos.x - track.min.x) / track.width())
        } else if resp.has_focus() {
            let step = ui.input(|i| i.key_pressed(Key::ArrowRight) as i32 - i.key_pressed(Key::ArrowLeft) as i32);
            (step != 0).then(|| m.crossfader + step as f32 * 0.05)
        } else {
            None
        };
        if let Some(v) = target {
            self.send_touched(Cmd::Mix(MixParam::Crossfader(v.clamp(0.0, 1.0))), MidiCtl::Crossfader);
        }
    }

    fn strip(&mut self, ui: &Ui, r: Rect, c: usize, v: &EngineView) {
        let p = ui.painter().clone();
        let cp = v.mixer.ch[c];
        let kw = r.width() / 2.0;
        let kh = 50.0;
        let cell = |row: usize, col: usize| Rect::from_min_size(pos2(r.min.x + col as f32 * kw, r.min.y + row as f32 * kh), vec2(kw, kh));
        let tint = t::DECK_TINT[c];
        type Mk = fn(usize, f32) -> MixParam;
        let knobs: [(usize, usize, &str, f32, Mk, MidiCtl); 5] = [
            (0, 0, "TRIM", cp.trim, MixParam::Trim, MidiCtl::Trim(c)),
            (0, 1, "HI", cp.hi, MixParam::Hi, MidiCtl::Hi(c)),
            (1, 0, "MID", cp.mid, MixParam::Mid, MidiCtl::Mid(c)),
            (1, 1, "LOW", cp.low, MixParam::Low, MidiCtl::Low(c)),
            (2, if c == 0 { 1 } else { 0 }, "FILTER", cp.filter, MixParam::Filter, MidiCtl::Filter(c)),
        ];
        for (i, (row, col, label, val, mk, ctl)) in knobs.into_iter().enumerate() {
            let killed = matches!(label, "HI" | "MID" | "LOW") && val < 0.02;
            let colr = if killed { t::DANGER } else { tint };
            if let Some(nv) = knob(ui, &mut self.touches, cell(row, col), Id::NULL.with(("k", c, i)), val, label, colr, true) {
                self.send_touched(Cmd::Mix(mk(c, nv)), ctl);
            }
        }
        let free = cell(2, if c == 0 { 0 } else { 1 });
        p.text(free.center(), Align2::CENTER_CENTER, format!("CH {}", c + 1), semi(15.0), tint);
        let mut y = r.min.y + 3.0 * kh + 6.0;
        let pfl = Rect::from_min_size(pos2(r.min.x + 6.0, y), vec2(r.width() - 12.0, 28.0));
        if button(ui, &mut self.touches, pfl, Id::NULL.with(("pfl", c)), "CUE", cp.pfl.then_some(t::CUE), semi(14.0)).clicked {
            self.send(Cmd::Mix(MixParam::PflToggle(c)));
        }
        y += 28.0 + 6.0;
        let fr = Rect::from_min_max(pos2(r.center().x - 18.0, y), pos2(r.center().x + 18.0, r.max.y));
        if let Some(nv) = vfader(ui, &mut self.touches, fr, Id::NULL.with(("fader", c)), cp.fader, tint, &format!("Channel {} fader", c + 1)) {
            self.send_touched(Cmd::Mix(MixParam::Fader(c, nv)), MidiCtl::Fader(c));
        }
        let mx = if c == 0 { fr.max.x + 10.0 } else { fr.min.x - 18.0 };
        meter(&p, Rect::from_min_max(pos2(mx, y + 4.0), pos2(mx + 8.0, r.max.y - 4.0)), v.decks[c].peak);
    }

    fn browser(&mut self, ui: &mut Ui, r: Rect) {
        let p = ui.painter().clone();
        card(&p, r);
        let inner = r.shrink2(vec2(t::SP_M, 8.0));
        let head = Rect::from_min_size(inner.min, vec2(inner.width(), 32.0));

        // Back + breadcrumb.
        let mut x = head.min.x;
        if self.lib.depth() > 0 || !self.lib.filter.is_empty() {
            let br = Rect::from_min_size(pos2(x, head.min.y + 1.0), vec2(74.0, 30.0));
            if button_tip(ui, &mut self.touches, br, Id::NULL.with("back"), "‹ BACK", None, semi(14.0), "Up one level, or clear the search  ( Backspace )").clicked {
                self.browse_back();
            }
            x += 84.0;
        }
        let right_w = 2.0 * 104.0 + 230.0;
        let crumbs = self.lib.crumbs();
        let last = crumbs.len() - 1;
        let mut job = LayoutJob::default();
        for (i, c) in crumbs.iter().enumerate() {
            let (font, col) = if i == last { (semi(16.0), t::INK) } else { (cond(16.0), t::INK_FAINT) };
            job.append(c, 0.0, egui::TextFormat::simple(font, col));
            if i != last {
                job.append("  ›  ", 0.0, egui::TextFormat::simple(cond(16.0), t::INK_FAINT));
            }
        }
        job.wrap = TextWrapping::truncate_at_width((head.max.x - x - right_w - 10.0).max(40.0));
        let g = p.layout_job(job);
        p.galley(pos2(x, head.center().y - g.size().y / 2.0), g, t::INK);

        // Search.
        let sr = Rect::from_min_size(pos2(head.max.x - right_w, head.min.y + 2.0), vec2(216.0, 28.0));
        let te = egui::TextEdit::singleline(&mut self.search).hint_text("Search  ( / )").font(cond(16.0)).desired_width(sr.width());
        let resp = ui.put(sr, te);
        if std::mem::take(&mut self.focus_search) {
            resp.request_focus();
        }
        if resp.changed() {
            let s = self.search.clone();
            self.lib.set_filter(&s);
            self.scroll_to_selected = true;
        }
        if resp.has_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
            self.search.clear();
            self.lib.set_filter("");
            resp.surrender_focus();
        }

        let can_load = matches!(self.lib.selected_row(), Some(Row::Track(_)));
        for d in 0..DECKS {
            let br = Rect::from_min_size(pos2(head.max.x - (DECKS - d) as f32 * 104.0 + 8.0, head.min.y + 1.0), vec2(96.0, 30.0));
            let tip = if d == 0 { "Load the selected track on deck 1  ( Q )" } else { "Load the selected track on deck 2  ( P )" };
            if button_tip(ui, &mut self.touches, br, Id::NULL.with(("load", d)), &format!("LOAD {}", d + 1), can_load.then_some(t::DECK_TINT[d]), semi(15.0), tip).clicked {
                self.load_selected(d);
            }
        }

        let list = Rect::from_min_max(pos2(inner.min.x, head.max.y + 6.0), inner.max);
        if let Some((done, total)) = self.lib.analysis_progress {
            text_fit(&p, pos2(list.max.x, r.max.y - 2.0), Align2::RIGHT_BOTTOM, &format!("Analysing BPM {done}/{total}"), caption(), t::INK_FAINT, 200.0);
        }
        if self.lib.rows.is_empty() {
            let (title, line) = match self.lib.node() {
                _ if !self.lib.filter.is_empty() => ("No matches".to_string(), format!("Nothing here matches \u{201c}{}\u{201d}. BACK clears the search.", self.lib.filter)),
                library::Node::Folder if self.lib.local.is_none() => ("Scanning your music folder…".to_string(), String::new()),
                library::Node::Folder => (
                    "No tracks in this folder".to_string(),
                    format!("Add MP3, FLAC, WAV, OGG or M4A files to {} or start with --music <folder>", self.lib.local_root.display()),
                ),
                _ => ("Nothing here".to_string(), "This list is empty on the drive.".to_string()),
            };
            p.text(list.center() - vec2(0.0, 12.0), Align2::CENTER_CENTER, title, t::title(), t::INK);
            p.text(list.center() + vec2(0.0, 14.0), Align2::CENTER_CENTER, line, t::body(), t::INK_MUTED);
            return;
        }

        let rows = self.lib.rows.clone();
        let tracks_view = rows.iter().any(|r| matches!(r, Row::Track(_)));
        let cols = |w: f32| {
            let fixed = 40.0 + 70.0 + 54.0 + 64.0;
            let flex = (w - fixed).max(100.0);
            [0.0, 40.0, 40.0 + flex * 0.58, 40.0 + flex, 40.0 + flex + 70.0, 40.0 + flex + 124.0]
        };
        let mut top = list.min.y;
        if tracks_view {
            let cx = cols(list.width());
            let heads = [(1, "TITLE", SortKey::Title), (2, "ARTIST", SortKey::Artist), (3, "BPM", SortKey::Bpm), (4, "KEY", SortKey::Key), (5, "TIME", SortKey::Time)];
            for (i, name, key) in heads {
                let x = list.min.x + cx[i] + 8.0;
                let hr = Rect::from_min_size(pos2(x - 6.0, top - 3.0), vec2(62.0, 24.0));
                let (c, resp) = control(ui, &mut self.touches, hr, Id::NULL.with(("sort", i)), || WidgetInfo::labeled(egui::Role::Button, true, format!("Sort by {name}")));
                focus_ring(&p, hr, &resp, t::R_CONTROL);
                let active = self.lib.sort.filter(|(k, _)| *k == key);
                p.text(pos2(x, top + 8.0), Align2::LEFT_CENTER, name, caption(), if active.is_some() || c.hovered { t::INK_MUTED } else { t::INK_FAINT });
                if let Some((_, asc)) = active {
                    let w = p.layout_no_wrap(name.to_string(), caption(), t::INK).size().x;
                    caret(&p, pos2(x + w + 8.0, top + 8.0), asc, t::INK_MUTED);
                }
                if c.clicked {
                    self.lib.sort_by(key);
                }
            }
            top += 18.0;
        }
        let rows_rect = Rect::from_min_max(pos2(list.min.x, top), list.max);
        let row_h = 30.0;
        let loaded: Vec<Option<PathBuf>> = self.decks.iter().map(|d| d.track.as_ref().map(|t| t.path.clone())).collect();
        let played = &self.lib.played;
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rows_rect));
        child.spacing_mut().item_spacing.y = 0.0;
        let selected = self.lib.selected();
        let scroll_to = std::mem::take(&mut self.scroll_to_selected);
        let mut clicked: Option<usize> = None;
        let mut entered: Option<usize> = None;
        let mut load_req: Option<usize> = None;
        let id_salt = format!("rows{:?}{}", self.lib.node(), self.lib.depth());
        let mut area = egui::ScrollArea::vertical().id_salt(id_salt).auto_shrink([false, false]);
        if scroll_to {
            // Rows outside the viewport aren't laid out, so scroll by arithmetic.
            let top = selected as f32 * row_h;
            let vis = rows_rect.height();
            let mut off = self.browser_offset;
            if top < off {
                off = top;
            } else if top + row_h > off + vis {
                off = top + row_h - vis;
            }
            area = area.vertical_scroll_offset(off.max(0.0));
        }
        // Covers for the rows that might be visible (only rekordbox tracks list artwork up
        // front; local files' embedded art would mean opening every file).
        let first = (self.browser_offset / row_h).max(0.0) as usize;
        let vis_rows = (rows_rect.height() / row_h) as usize + 2;
        let arts: HashMap<usize, Option<crate::art::ArtKey>> = (first..(first + vis_rows).min(rows.len()))
            .filter_map(|i| match &rows[i] {
                Row::Track(e) if e.rb.is_some() => Some((i, self.art_key(e))),
                _ => None,
            })
            .collect();
        let art = &mut self.art;
        let out = area.show_rows(&mut child, row_h, rows.len(), |ui, range| {
            for i in range {
                let (rr, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                let p = ui.painter();
                if i == selected {
                    p.rect_filled(rr, CornerRadius::same(t::R_CONTROL), t::ACCENT.gamma_multiply(0.2));
                    p.rect_stroke(rr, CornerRadius::same(t::R_CONTROL), Stroke::new(1.0, t::ACCENT.gamma_multiply(0.6)), StrokeKind::Inside);
                } else if resp.hovered() {
                    p.rect_filled(rr, CornerRadius::same(t::R_CONTROL), t::RAISED);
                } else if i % 2 == 1 {
                    p.rect_filled(rr, 0, t::CHROME.gamma_multiply(0.6));
                }
                let y = rr.center().y;
                match &rows[i] {
                    Row::Nav { label, detail, kind, .. } => {
                        resp.widget_info(|| WidgetInfo::labeled(egui::Role::Button, true, label));
                        nav_icon(p, pos2(rr.min.x + 20.0, y), *kind);
                        text_fit(p, pos2(rr.min.x + 42.0, y), Align2::LEFT_CENTER, label, semi(16.0), t::INK, rr.width() * 0.55);
                        text_fit(p, pos2(rr.max.x - 30.0, y), Align2::RIGHT_CENTER, detail, cond(15.0), t::INK_FAINT, rr.width() * 0.4);
                        p.text(pos2(rr.max.x - 12.0, y), Align2::CENTER_CENTER, "›", semi(18.0), t::INK_FAINT);
                        if resp.clicked() {
                            entered = Some(i);
                        }
                    }
                    Row::Track(e) => {
                        resp.widget_info(|| WidgetInfo::selected(egui::Role::Button, true, i == selected, format!("{} — {}", e.title, e.artist)));
                        let was_played = played.contains(&e.path);
                        let mut bx = rr.min.x + 10.0;
                        if was_played && !loaded.iter().any(|lp| lp.as_ref() == Some(&e.path)) {
                            // Played this session: a tick, and the row in muted ink.
                            let c = pos2(bx + 4.0, y);
                            let st = Stroke::new(2.0, t::PLAY);
                            p.line_segment([c + vec2(-5.0, 0.0), c + vec2(-1.5, 3.5)], st);
                            p.line_segment([c + vec2(-1.5, 3.5), c + vec2(5.0, -4.0)], st);
                        }
                        for (d, lp) in loaded.iter().enumerate() {
                            if lp.as_ref() == Some(&e.path) {
                                p.circle_filled(pos2(bx, y), 8.0, t::DECK_TINT[d].gamma_multiply(0.25));
                                p.text(pos2(bx, y), Align2::CENTER_CENTER, format!("{}", d + 1), semi(12.0), t::DECK_TINT[d]);
                                bx += 18.0;
                            }
                        }
                        let cx = cols(rr.width());
                        let at = |c: usize| rr.min.x + cx[c] + 8.0;
                        let mut title_x = at(1);
                        if let Some(key) = arts.get(&i).cloned().flatten() {
                            if let Some(tex) = art.get(&key) {
                                let ar = Rect::from_center_size(pos2(title_x + 12.0, y), vec2(24.0, 24.0));
                                p.image(tex.id(), ar, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                            }
                            title_x += 32.0;
                        }
                        let title_ink = if was_played { t::INK_FAINT } else { t::INK };
                        text_fit(p, pos2(title_x, y), Align2::LEFT_CENTER, &e.title, semi(16.0), title_ink, cx[2] - cx[1] - 16.0 - (title_x - at(1)));
                        text_fit(p, pos2(at(2), y), Align2::LEFT_CENTER, &e.artist, cond(16.0), t::INK_MUTED, cx[3] - cx[2] - 16.0);
                        let bpm = e.bpm.map(|b| format!("{b:.1}")).unwrap_or_else(|| "–".into());
                        p.text(pos2(at(3), y), Align2::LEFT_CENTER, bpm, mono(13.0), if e.bpm.is_some() { t::INK } else { t::INK_FAINT });
                        text_fit(p, pos2(at(4), y), Align2::LEFT_CENTER, &e.key, mono(13.0), t::INK_MUTED, 50.0);
                        let time = e.duration.map(|d| format!("{}:{:02}", d / 60, d % 60)).unwrap_or_else(|| e.ext.clone());
                        p.text(pos2(at(5), y), Align2::LEFT_CENTER, time, mono(13.0), t::INK_FAINT);
                        if resp.clicked() {
                            clicked = Some(i);
                        }
                        if resp.double_clicked() {
                            load_req = Some(i);
                        }
                    }
                }
            }
        });
        self.browser_offset = out.state.clamped_offset().y;
        if let Some(i) = clicked {
            self.lib.select(i);
        }
        if let Some(i) = entered {
            if self.lib.enter_row(i) {
                self.search.clear();
                self.scroll_to_selected = true;
            }
        }
        if let Some(i) = load_req {
            self.lib.select(i);
            // Double-tap loads to whichever deck isn't playing.
            let d = {
                let v = self.view.lock();
                if v.decks[0].playing && !v.decks[1].playing { 1 } else { 0 }
            };
            self.load_selected(d);
        }
    }
}

/// Startup commands that restore saved toggles and levels.
pub fn restore_commands(cfg: &Config) -> Vec<Cmd> {
    let mut v = vec![Cmd::Quantize(cfg.quantize)];
    for d in 0..DECKS {
        let p = &cfg.decks[d];
        v.push(Cmd::Vinyl(d, p.vinyl));
        v.push(Cmd::MasterTempo(d, p.master_tempo));
        v.push(Cmd::TempoRange(d, p.tempo_range));
    }
    v.push(Cmd::Mix(MixParam::Master(cfg.master_level)));
    v.push(Cmd::Mix(MixParam::HeadMix(cfg.phones_mix)));
    v.push(Cmd::Mix(MixParam::HeadLevel(cfg.phones_level)));
    v.push(Cmd::QuantizeBeats(cfg.quantize_beats));
    v.push(Cmd::Mix(MixParam::FaderCurve(cfg.fader_curve)));
    v.push(Cmd::Mix(MixParam::CrossfaderCurve(cfg.crossfader_curve)));
    v.push(Cmd::Mix(MixParam::FxKind(cfg.fx.kind)));
    v.push(Cmd::Mix(MixParam::FxBeatIdx(cfg.fx.beat_idx)));
    v.push(Cmd::Mix(MixParam::FxTarget(cfg.fx.target)));
    v.push(Cmd::Mix(MixParam::FxLevel(cfg.fx.level)));
    v.push(Cmd::Mix(MixParam::FxQuantize(cfg.beat_fx_quantize)));
    v
}
