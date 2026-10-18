//! The touch UI, sized for the Steam Deck (1280 × 800). This module holds the app state and the
//! frame loop; the rest is split by job:
//!
//! - `state`, `actions`, `keyboard`: per-frame work, and what input asks the app to do
//! - `widgets`, `theme`, `icons`, `touch`: the building blocks and the look
//! - `chrome`: the top bar and the rail of screen buttons
//! - `panels`: pieces several screens share (decks, mixer, waveforms, track list, …)
//! - `views`: one file per screen (PERFORM and its display layout, WAVE, BROWSE / TAG, SETTINGS)

use std::any::Any;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, Sender};
use egui::{pos2, vec2, Key, Rect, Ui, Vec2};
use parking_lot::Mutex;

use crate::audio::{self, AudioOptions, AudioOut};
use crate::config::{Config, Store};
use crate::engine::deck::{HotCue, HOT_CUES};
use crate::engine::{Cmd, EngineView, SharedEngine, SharedView, DECKS};
use crate::library::{self, LibEvent, LibTrack, Library};
use crate::midi::{MidiStatus, PickupLocks, UiAction};
use crate::platform::{GamepadStatus, KeepAwake};
use crate::track::{self, Track};
use theme::{self as t, cond};
use touch::Touches;

mod actions;
pub(crate) mod cache;
mod chrome;
mod deckinfo;
pub mod icons;
mod keyboard;
mod panels;
mod prelude;
mod state;
pub mod theme;
pub mod touch;
mod views;
mod widgets;

pub use state::restore_commands;
use chrome::rail;
use views::{browse, settings, View};

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
    art: Option<crate::ui::cache::art::ArtKey>,
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
    art: crate::ui::cache::art::ArtCache,
    /// The app icon for the top bar, uploaded on first use.
    logo: Option<egui::TextureHandle>,
    /// What has gone on air this session (drives history and "played" marks).
    onair: crate::recorder::Tracklist,
    started: Instant,
    /// First tap of the two-tap stop, so a stray touch can't end a recording.
    rec_confirm: Option<Instant>,
    view_mode: View,
    /// The play view (PERFORM or WAVE) to return to after loading from BROWSE.
    last_play_view: View,
    keyboard_open: bool,
    previews: crate::ui::cache::preview::PreviewCache,
    browse_popup: Option<browse::Popup>,
    clear_tags_confirm: Option<Instant>,
    settings: settings::SettingsState,
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
            art: { let c = ctx.clone(); crate::ui::cache::art::ArtCache::new(move || c.request_repaint()) },
            logo: None,
            rec_confirm: None,
            view_mode: View::Perform,
            last_play_view: View::Perform,
            keyboard_open: false,
            previews: { let c = ctx.clone(); crate::ui::cache::preview::PreviewCache::new(move || c.request_repaint()) },
            browse_popup: None,
            clear_tags_confirm: None,
            settings: Default::default(),
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
        // The controller test (SETTINGS) is the only reader of the MIDI log.
        crate::midi::MIDI_LOG_ON.store(self.view_mode == View::Settings && self.settings.page == 3, Ordering::Relaxed);
        self.persist(&v);
        self.recording_tick(&v);
        self.history_tick(&v);

        let full = ui.max_rect();
        ui.painter().rect_filled(full, 0, t::CHROME);
        fit_to_window(ui.ctx(), full);

        let top_h = 40.0;
        // Banners wrap (error messages can be long) and grow to fit, up to three lines.
        let banner_h = match &self.banner {
            Some(b) => {
                let g = ui.painter().layout(b.clone(), cond(15.0), t::INK, full.width() - 140.0);
                (g.size().y + 12.0).clamp(30.0, 66.0)
            }
            None => 0.0,
        };
        let top = Rect::from_min_size(full.min, vec2(full.width(), top_h));
        let banner = Rect::from_min_size(pos2(full.min.x, top.max.y), vec2(full.width(), banner_h));
        // The rail of big screen buttons down the left; views get the rest.
        let rail_w = if full.width() >= rail::WIDE { rail::RAIL_W } else { rail::RAIL_W_NARROW };
        let rail_r = Rect::from_min_max(pos2(full.min.x + 6.0, banner.max.y + 6.0), pos2(full.min.x + 6.0 + rail_w, full.max.y - 6.0));
        let body = Rect::from_min_max(pos2(rail_r.max.x, banner.max.y), full.max);
        self.top_bar(ui, top, &v, body.min.x);
        if banner_h > 0.0 {
            self.banner_row(ui, banner);
        }
        self.rail(ui, rail_r, &v);
        match self.view_mode {
            View::Perform if self.display_layout() => self.perform_display(ui, body, &v),
            View::Perform => self.perform_view(ui, body, &v),
            View::Wave => self.wave_view(ui, body.shrink2(vec2(6.0, 4.0)), &v),
            View::Browse | View::Tag | View::Settings => {
                let strip = Rect::from_min_size(body.min + vec2(6.0, 4.0), vec2(body.width() - 12.0, 58.0));
                self.now_playing(ui, strip, &v);
                let rest = Rect::from_min_max(pos2(body.min.x + 6.0, strip.max.y + 6.0), body.max - vec2(6.0, 6.0));
                if self.view_mode == View::Settings {
                    self.settings_view(ui, rest, &v);
                } else {
                    self.browse_view(ui, rest, &v);
                }
            }
        }

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
        // Close the stream before handing the card back its old profile.
        self.audio = None;
        crate::audio::cardprofile::restore_saved();
        if let Some(f) = self.recorder.stop() {
            eprintln!("recording: saved {}", f.parts.first().map(|p| p.path.display().to_string()).unwrap_or_default());
        }
        let _ = self.config.save();
        self.store.lock().flush(true);
    }
}

/// The layout needs about 1200 × 720 points (the full PERFORM layout beside the rail). On a
/// screen or window that's smaller in points (e.g. a 1280 × 800 panel at 150 % scaling) shrink
/// everything to fit instead of squeezing it. The Steam Deck (1280 × 800) runs at 100 %.
const DESIGN_MIN: Vec2 = vec2(1200.0, 720.0);

fn fit_to_window(ctx: &egui::Context, full: Rect) {
    let native = ctx.native_pixels_per_point().unwrap_or(1.0);
    let phys = full.size() * ctx.pixels_per_point();
    let fit = (phys.x / native / DESIGN_MIN.x).min(phys.y / native / DESIGN_MIN.y).min(1.0).max(0.5);
    if (ctx.zoom_factor() - fit).abs() > 0.01 {
        ctx.set_zoom_factor(fit);
    }
}
