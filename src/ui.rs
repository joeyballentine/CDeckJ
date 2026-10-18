//! Steam Deck–sized (1280×800) touch UI: stacked waveforms, two CDJ-style decks, mixer, browser.

use std::any::Any;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{unbounded, Receiver, Sender};
use egui::text::{LayoutJob, TextWrapping};
use egui::{
    pos2, vec2, Align2, Color32, CornerRadius, Event, FontId, Id, Key, Mesh, Painter, Pos2, Rect,
    Response, Sense, Shape, Stroke, StrokeKind, Ui, Vec2, ViewportCommand,
};

use crate::engine::deck::{HOT_CUES, SCRATCH_SECS_PER_TICK};
use crate::engine::{Cmd, DeckView, EngineView, MixParam, SharedView, DECKS};
use crate::library::{self, Entry};
use crate::midi::UiAction;
use crate::theme::{self as t, caption, cond, mono, semi};
use crate::track::{self, Track, WAVE_BINS_PER_SEC};

/// Seconds visible in the scrolling waveforms.
const WAVE_SECONDS: f64 = 8.0;
const JOG_TICKS_PER_REV: f64 = 720.0;

pub struct LoadResult {
    deck: usize,
    generation: u64,
    result: Result<Arc<Track>, String>,
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
    touch_held: bool,
    wave_touch: bool,
    play_held: bool,
    jog_angle: Option<f32>,
}

pub struct App {
    cmd: Sender<Cmd>,
    view: SharedView,
    ui_rx: Receiver<UiAction>,
    load_tx: Sender<LoadResult>,
    load_rx: Receiver<LoadResult>,
    lib_rx: Receiver<Vec<Entry>>,
    library: Option<Vec<Entry>>,
    music_root: PathBuf,
    selected: usize,
    scroll_to_selected: bool,
    decks: [DeckUi; DECKS],
    next_gen: u64,
    audio_status: Result<String, String>,
    midi_status: Option<String>,
    fullscreen: bool,
    key_held: Vec<Key>,
    ctx: egui::Context,
    /// Audio stream and MIDI connections: kept alive for the app's lifetime.
    _keep: Vec<Box<dyn Any>>,
}

pub struct AppInit {
    pub cmd: Sender<Cmd>,
    pub view: SharedView,
    pub ui_rx: Receiver<UiAction>,
    pub music_root: PathBuf,
    pub audio_status: Result<String, String>,
    pub midi_status: Option<String>,
    pub fullscreen: bool,
    pub preload: Vec<PathBuf>,
    pub keep: Vec<Box<dyn Any>>,
}

impl App {
    pub fn new(ctx: &egui::Context, init: AppInit) -> Self {
        let (load_tx, load_rx) = unbounded();
        let (lib_tx, lib_rx) = unbounded();
        let root = init.music_root.clone();
        let c2 = ctx.clone();
        std::thread::spawn(move || {
            let _ = lib_tx.send(library::scan(&root));
            c2.request_repaint();
        });
        let mut app = Self {
            cmd: init.cmd,
            view: init.view,
            ui_rx: init.ui_rx,
            load_tx,
            load_rx,
            lib_rx,
            library: None,
            music_root: init.music_root,
            selected: 0,
            scroll_to_selected: false,
            decks: Default::default(),
            next_gen: 1,
            audio_status: init.audio_status,
            midi_status: init.midi_status,
            fullscreen: init.fullscreen,
            key_held: Vec::new(),
            ctx: ctx.clone(),
            _keep: init.keep,
        };
        for (d, p) in init.preload.into_iter().take(DECKS).enumerate() {
            app.load(d, p);
        }
        app
    }

    fn send(&self, c: Cmd) {
        let _ = self.cmd.send(c);
    }

    fn load(&mut self, deck: usize, path: PathBuf) {
        let generation = self.next_gen;
        self.next_gen += 1;
        let du = &mut self.decks[deck];
        du.generation = generation;
        du.error = None;
        du.loading = Some(track::names_from_path(&path).0);
        let tx = self.load_tx.clone();
        let ctx = self.ctx.clone();
        std::thread::Builder::new()
            .name("loader".into())
            .spawn(move || {
                let result = track::load(&path).map(Arc::new);
                let _ = tx.send(LoadResult { deck, generation, result });
                ctx.request_repaint();
            })
            .ok();
    }

    fn load_selected(&mut self, deck: usize) {
        let Some(e) = self.library.as_ref().and_then(|l| l.get(self.selected)) else { return };
        let path = e.path.clone();
        self.load(deck, path);
    }

    fn browse(&mut self, delta: i32) {
        let n = self.library.as_ref().map(|l| l.len()).unwrap_or(0);
        if n == 0 {
            return;
        }
        self.selected = (self.selected as i64 + delta as i64).clamp(0, n as i64 - 1) as usize;
        self.scroll_to_selected = true;
    }

    fn poll(&mut self) {
        while let Ok(lib) = self.lib_rx.try_recv() {
            self.library = Some(lib);
        }
        while let Ok(r) = self.load_rx.try_recv() {
            let du = &mut self.decks[r.deck];
            if r.generation != du.generation {
                continue; // superseded by a newer load
            }
            du.loading = None;
            match r.result {
                Ok(track) => {
                    du.track = Some(track.clone());
                    du.cue_held = false;
                    du.hot_held = [false; HOT_CUES];
                    let _ = self.cmd.send(Cmd::Load { deck: r.deck, track, generation: r.generation });
                }
                Err(e) => du.error = Some(e),
            }
        }
        while let Ok(a) = self.ui_rx.try_recv() {
            match a {
                UiAction::Browse(d) => self.browse(d),
                UiAction::BrowsePress => self.scroll_to_selected = true,
                UiAction::Load(d) => self.load_selected(d),
            }
        }
    }

    fn keyboard(&mut self, ui: &Ui) {
        if ui.ctx().text_edit_focused() {
            return;
        }
        let events: Vec<(Key, bool, bool)> = ui.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    Event::Key { key, pressed, repeat, .. } => Some((*key, *pressed, *repeat)),
                    _ => None,
                })
                .collect()
        });
        for (key, pressed, repeat) in events {
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
                (Key::Q, true) => self.load_selected(0),
                (Key::P, true) => self.load_selected(1),
                (Key::ArrowUp, true) => self.browse(-1),
                (Key::ArrowDown, true) => self.browse(1),
                (Key::PageUp, true) => self.browse(-10),
                (Key::PageDown, true) => self.browse(10),
                (Key::F11, true) => {
                    self.fullscreen = !self.fullscreen;
                    ui.ctx().send_viewport_cmd(ViewportCommand::Fullscreen(self.fullscreen));
                }
                _ => {}
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.poll();
        self.keyboard(ui);
        let v: EngineView = *self.view.lock();

        let full = ui.max_rect();
        ui.painter().rect_filled(full, 0, t::CHROME);

        let top_h = 34.0;
        let wave_h = (full.height() * 0.2).clamp(110.0, 200.0);
        let main_h = 350.0f32.min((full.height() - top_h - wave_h - 140.0).max(300.0));
        let mixer_w = 340.0;

        let top = Rect::from_min_size(full.min, vec2(full.width(), top_h));
        let waves = Rect::from_min_size(pos2(full.min.x, top.max.y), vec2(full.width(), wave_h));
        let main = Rect::from_min_size(pos2(full.min.x, waves.max.y), vec2(full.width(), main_h));
        let browser = Rect::from_min_max(pos2(full.min.x, main.max.y), full.max);
        let deck_w = (full.width() - mixer_w) / 2.0;
        let deck_rects = [
            Rect::from_min_size(main.min, vec2(deck_w, main_h)),
            Rect::from_min_size(pos2(main.min.x + deck_w + mixer_w, main.min.y), vec2(deck_w, main_h)),
        ];
        let mixer = Rect::from_min_size(pos2(main.min.x + deck_w, main.min.y), vec2(mixer_w, main_h));

        self.top_bar(ui, top, &v);
        for d in 0..DECKS {
            let h = wave_h / 2.0;
            let r = Rect::from_min_size(pos2(waves.min.x, waves.min.y + d as f32 * h), vec2(waves.width(), h));
            self.waveform(ui, r.shrink2(vec2(0.0, 1.0)), d, &v.decks[d]);
        }
        for d in 0..DECKS {
            self.deck_panel(ui, deck_rects[d].shrink(6.0), d, &v.decks[d]);
        }
        self.mixer_panel(ui, mixer.shrink2(vec2(0.0, 6.0)), &v);
        self.browser(ui, browser.shrink(6.0));

        let busy = v.decks.iter().any(|d| d.playing || d.previewing || d.touched)
            || self.decks.iter().any(|d| d.loading.is_some())
            || v.peak_master > 0.001;
        if busy {
            ui.ctx().request_repaint();
        } else {
            ui.ctx().request_repaint_after(Duration::from_millis(60));
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Drawing helpers
// ---------------------------------------------------------------------------------------------

fn text_fit(p: &Painter, pos: Pos2, anchor: Align2, s: &str, font: FontId, color: Color32, max_w: f32) {
    let mut job = LayoutJob::simple_singleline(s.to_owned(), font, color);
    job.wrap = TextWrapping::truncate_at_width(max_w.max(1.0));
    let g = p.layout_job(job);
    let r = anchor.anchor_size(pos, g.size());
    p.galley(r.min, g, color);
}

fn card(p: &Painter, r: Rect) {
    p.rect_filled(r, CornerRadius::same(t::R_CARD), t::SURFACE);
    p.rect_stroke(r, CornerRadius::same(t::R_CARD), Stroke::new(1.0, t::BORDER), StrokeKind::Inside);
}

/// A flat hardware-style button. `lit` lights it in the given colour.
fn button(ui: &Ui, r: Rect, id: Id, label: &str, lit: Option<Color32>, font: FontId) -> Response {
    let resp = ui.interact(r, id, Sense::click_and_drag());
    let p = ui.painter();
    let down = resp.is_pointer_button_down_on();
    let (fill, stroke, ink) = match lit {
        Some(c) => (c.gamma_multiply(0.22), c, c),
        None if down => (t::HOVER, t::INK_FAINT, t::INK),
        None if resp.hovered() => (t::HOVER, t::BORDER, t::INK),
        None => (t::RAISED, t::BORDER, t::INK_MUTED),
    };
    let fill = if down && lit.is_some() { fill.gamma_multiply(1.6) } else { fill };
    p.rect_filled(r, CornerRadius::same(t::R_CONTROL), fill);
    p.rect_stroke(r, CornerRadius::same(t::R_CONTROL), Stroke::new(1.0, stroke), StrokeKind::Inside);
    p.text(r.center(), Align2::CENTER_CENTER, label, font, ink);
    resp
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

/// Rotary knob: vertical drag to turn, double-tap to centre. Returns the new value when moved.
fn knob(ui: &Ui, r: Rect, id: Id, value: f32, label: &str, color: Color32, bipolar: bool) -> Option<f32> {
    let resp = ui.interact(r, id, Sense::click_and_drag());
    let p = ui.painter();
    let label_h = 14.0;
    let kr = ((r.height() - label_h).min(r.width()) * 0.5 - 3.0).max(6.0);
    let c = pos2(r.center().x, r.min.y + 2.0 + kr);
    use std::f32::consts::PI;
    let (start, sweep) = (0.75 * PI, 1.5 * PI);
    let ang = |v: f32| start + sweep * v.clamp(0.0, 1.0);
    p.circle_filled(c, kr, t::RAISED);
    p.add(Shape::line(arc_points(c, kr - 2.0, ang(0.0), ang(1.0)), Stroke::new(3.0, t::INSET)));
    let from = if bipolar { 0.5 } else { 0.0 };
    if (value - from).abs() > 0.004 {
        p.add(Shape::line(arc_points(c, kr - 2.0, ang(from.min(value)), ang(from.max(value))), Stroke::new(3.0, color)));
    }
    let tip = c + (kr - 6.0) * Vec2::angled(ang(value));
    p.line_segment([c + (kr * 0.25) * Vec2::angled(ang(value)), tip], Stroke::new(2.0, t::INK));
    if resp.hovered() || resp.dragged() {
        p.circle_stroke(c, kr, Stroke::new(1.0, t::INK_FAINT));
    }
    p.text(pos2(r.center().x, r.max.y), Align2::CENTER_BOTTOM, label, caption(), t::INK_MUTED);
    if resp.double_clicked() {
        return Some(if bipolar { 0.5 } else { value });
    }
    if resp.dragged() {
        let d = resp.drag_delta();
        let nv = (value + (d.x - d.y) / 160.0).clamp(0.0, 1.0);
        if nv != value {
            return Some(nv);
        }
    }
    None
}

/// Vertical fader. `value` 1.0 = top.
fn vfader(ui: &Ui, r: Rect, id: Id, value: f32, color: Color32) -> Option<f32> {
    let resp = ui.interact(r, id, Sense::click_and_drag());
    let p = ui.painter();
    let track = Rect::from_center_size(r.center(), vec2(4.0, r.height() - 16.0));
    p.rect_filled(track, 2, t::INSET);
    let cap_h = 16.0;
    let y = track.max.y - value.clamp(0.0, 1.0) * track.height();
    let cap = Rect::from_center_size(pos2(r.center().x, y), vec2(r.width() - 2.0, cap_h));
    p.rect_filled(cap, CornerRadius::same(3), if resp.dragged() { t::HOVER } else { t::RAISED });
    p.rect_stroke(cap, CornerRadius::same(3), Stroke::new(1.0, t::BORDER), StrokeKind::Inside);
    p.hline(cap.x_range().shrink(3.0), y, Stroke::new(2.0, color));
    if (resp.dragged() || resp.clicked()) && let Some(pos) = resp.interact_pointer_pos() {
        let nv = ((track.max.y - pos.y) / track.height()).clamp(0.0, 1.0);
        return Some(nv);
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
        let on = (i as i32) < lit;
        p.rect_filled(seg, 1, if on { col } else { col.gamma_multiply(0.12) });
    }
}

fn fmt_time(secs: f64) -> String {
    let s = secs.max(0.0);
    let m = (s / 60.0).floor();
    format!("{:02}:{:04.1}", m as i64, s - m * 60.0)
}

// ---------------------------------------------------------------------------------------------
// Regions
// ---------------------------------------------------------------------------------------------

impl App {
    fn top_bar(&self, ui: &Ui, r: Rect, v: &EngineView) {
        let p = ui.painter();
        p.text(pos2(r.min.x + 14.0, r.center().y), Align2::LEFT_CENTER, "PLATTER", semi(19.0), t::INK);
        let mut x = r.min.x + 104.0;
        let dot = |x: f32, ok: bool| p.circle_filled(pos2(x, r.center().y), 4.0, if ok { t::PLAY } else { t::DANGER });
        match &self.audio_status {
            Ok(desc) => {
                dot(x, true);
                let routing = match v.routing {
                    crate::engine::CueRouting::Channels34 => "phones on 3/4",
                    crate::engine::CueRouting::SplitMono => "split cue L/R",
                    crate::engine::CueRouting::MasterOnly => "no cue output",
                };
                let s = format!("{desc}  ·  {} Hz  ·  {routing}", v.sample_rate);
                text_fit(p, pos2(x + 10.0, r.center().y), Align2::LEFT_CENTER, &s, cond(15.0), t::INK_MUTED, 540.0);
            }
            Err(e) => {
                dot(x, false);
                text_fit(p, pos2(x + 10.0, r.center().y), Align2::LEFT_CENTER, &format!("No audio output — {e}"), cond(15.0), t::DANGER, 520.0);
            }
        }
        x += 560.0;
        dot(x, self.midi_status.is_some());
        let midi = match &self.midi_status {
            Some(n) => n.clone(),
            None => "No controller — touch & keyboard only".into(),
        };
        text_fit(p, pos2(x + 10.0, r.center().y), Align2::LEFT_CENTER, &midi, cond(15.0), t::INK_MUTED, r.max.x - x - 140.0);
        // Master level, horizontal.
        let m = Rect::from_min_size(pos2(r.max.x - 120.0, r.center().y - 4.0), vec2(106.0, 8.0));
        p.rect_filled(m, 2, t::INSET);
        let db = 20.0 * v.peak_master.max(1e-6).log10();
        let f = ((db + 42.0) / 42.0).clamp(0.0, 1.0);
        let col = if db > -1.0 { t::DANGER } else if db > -6.0 { t::CUE } else { t::PLAY };
        p.rect_filled(Rect::from_min_size(m.min, vec2(m.width() * f, m.height())), 2, col);
        p.text(pos2(m.min.x - 8.0, r.center().y), Align2::RIGHT_CENTER, "MASTER", caption(), t::INK_FAINT);
    }

    fn waveform(&mut self, ui: &Ui, r: Rect, d: usize, dv: &DeckView) {
        let p = ui.painter_at(r);
        p.rect_filled(r, 0, t::INSET);
        let tint = t::DECK_TINT[d];
        let Some(track) = self.decks[d].track.as_ref().filter(|_| dv.loaded) else {
            p.text(r.left_center() + vec2(14.0, 0.0), Align2::LEFT_CENTER, format!("{}", d + 1), semi(20.0), tint.gamma_multiply(0.6));
            return;
        };
        let sr = track.sample_rate as f64;
        let pos_s = dv.pos / sr;
        let pps = r.width() as f64 / WAVE_SECONDS; // pixels per second
        let cx = r.center().x;
        let mid_y = r.center().y;
        let half = r.height() * 0.46;
        let t_at = |x: f32| pos_s + (x - cx) as f64 / pps;
        let x_at = |s: f64| cx + ((s - pos_s) * pps) as f32;

        // Loop region.
        if let (Some(a), Some(b)) = (dv.loop_in, dv.loop_out) {
            let lr = Rect::from_x_y_ranges(x_at(a / sr)..=x_at(b / sr), r.y_range());
            p.rect_filled(lr, 0, t::LOOP.gamma_multiply(if dv.loop_active { 0.16 } else { 0.06 }));
        }
        // Beat grid.
        if let Some(bl) = track.beat_len_frames() {
            let bl_s = bl / sr;
            let first = track.first_beat / sr;
            let k0 = ((t_at(r.min.x) - first) / bl_s).floor() as i64;
            let k1 = ((t_at(r.max.x) - first) / bl_s).ceil() as i64;
            let dur = track.frames as f64 / sr;
            for k in k0..=k1 {
                let ts = first + k as f64 * bl_s;
                if ts < 0.0 || ts > dur {
                    continue;
                }
                let x = x_at(ts);
                let bar = k.rem_euclid(4) == 0;
                p.vline(x, r.y_range(), Stroke::new(1.0, if bar { t::INK_FAINT.gamma_multiply(0.7) } else { t::BORDER }));
            }
        }
        // Three-band waveform, one column per pixel.
        let mut mesh = Mesh::default();
        let wave = &track.wave;
        let x0 = r.min.x.floor() as i32;
        let x1 = r.max.x.ceil() as i32;
        for x in x0..x1 {
            let ts = t_at(x as f32 + 0.5);
            if ts < 0.0 {
                continue;
            }
            let bi = (ts * WAVE_BINS_PER_SEC) as usize;
            let Some(b) = wave.get(bi) else { break };
            let pk = b.peak as f32 / 255.0;
            let col_r = |h: f32| Rect::from_min_max(pos2(x as f32, mid_y - h), pos2(x as f32 + 1.0, mid_y + h));
            let played = (x as f32) < cx;
            let dim = |c: Color32| if played { c.gamma_multiply(0.7) } else { c };
            let lo = pk * half * (0.35 + 0.65 * b.low as f32 / 255.0);
            mesh.add_colored_rect(col_r(lo.max(pk * half * 0.6)), dim(t::WAVE_LOW));
            mesh.add_colored_rect(col_r(pk * half * 0.75 * b.mid as f32 / 255.0), dim(t::WAVE_MID));
            mesh.add_colored_rect(col_r(pk * half * 0.5 * b.high as f32 / 255.0), dim(t::WAVE_HIGH));
        }
        p.add(mesh);
        // Cue and hot cue markers.
        let marker = |x: f32, col: Color32, top: bool| {
            let y = if top { r.min.y } else { r.max.y };
            let s = if top { 1.0 } else { -1.0 };
            p.add(Shape::convex_polygon(
                vec![pos2(x - 6.0, y), pos2(x + 6.0, y), pos2(x, y + 8.0 * s)],
                col,
                Stroke::NONE,
            ));
            p.vline(x, r.y_range(), Stroke::new(1.0, col.gamma_multiply(0.5)));
        };
        marker(x_at(dv.cue / sr), t::CUE, false);
        for (i, h) in dv.hot.iter().enumerate() {
            if let Some(h) = h {
                let x = x_at(h / sr);
                marker(x, t::HOT_CUE[i], true);
                p.text(pos2(x + 4.0, r.min.y + 1.0), Align2::LEFT_TOP, ["A", "B", "C", "D", "E", "F", "G", "H"][i], semi(12.0), t::HOT_CUE[i]);
            }
        }
        // Playhead.
        p.vline(cx, r.y_range(), Stroke::new(2.0, t::INK));
        p.text(r.min + vec2(10.0, 4.0), Align2::LEFT_TOP, format!("{}", d + 1), semi(18.0), tint);

        // Drag the waveform to scratch, like pushing the record.
        let resp = ui.interact(r, Id::NULL.with(("wave", d)), Sense::click_and_drag());
        let down = resp.is_pointer_button_down_on();
        if down != self.decks[d].wave_touch {
            self.decks[d].wave_touch = down;
            let _ = self.cmd.send(Cmd::JogTouch(d, down));
        }
        if resp.dragged() {
            let dx = resp.drag_delta().x as f64;
            let ticks = -dx / pps / SCRATCH_SECS_PER_TICK;
            let _ = self.cmd.send(Cmd::JogScratch(d, ticks));
        }
    }

    fn deck_panel(&mut self, ui: &Ui, r: Rect, d: usize, dv: &DeckView) {
        let p = ui.painter().clone();
        card(&p, r);
        let inner = r.shrink(t::SP_M);
        let tint = t::DECK_TINT[d];
        let mut y = inner.min.y;

        // Header: deck badge, title/artist, MASTER/SYNC chips.
        let badge_c = pos2(inner.min.x + 14.0, y + 20.0);
        p.circle_filled(badge_c, 14.0, tint.gamma_multiply(0.18));
        p.circle_stroke(badge_c, 14.0, Stroke::new(1.0, tint));
        p.text(badge_c, Align2::CENTER_CENTER, format!("{}", d + 1), semi(17.0), tint);
        let tx = inner.min.x + 38.0;
        let du = &self.decks[d];
        let chips_w = 120.0;
        let title_w = inner.max.x - tx - chips_w;
        if let Some(name) = &du.loading {
            text_fit(&p, pos2(tx, y + 2.0), Align2::LEFT_TOP, &format!("Loading {name}…"), t::title(), t::INK_MUTED, title_w);
            text_fit(&p, pos2(tx, y + 26.0), Align2::LEFT_TOP, "Decoding and finding the beat grid", cond(15.0), t::INK_FAINT, title_w);
        } else if let Some(err) = &du.error {
            text_fit(&p, pos2(tx, y + 2.0), Align2::LEFT_TOP, "Couldn't load that file", t::title(), t::DANGER, title_w);
            text_fit(&p, pos2(tx, y + 26.0), Align2::LEFT_TOP, err, cond(15.0), t::INK_MUTED, title_w);
        } else if let Some(tr) = du.track.as_ref().filter(|_| dv.loaded) {
            text_fit(&p, pos2(tx, y + 2.0), Align2::LEFT_TOP, &tr.title, t::title(), t::INK, title_w);
            let artist = if tr.artist.is_empty() { "Unknown artist" } else { &tr.artist };
            text_fit(&p, pos2(tx, y + 26.0), Align2::LEFT_TOP, artist, cond(15.0), t::INK_MUTED, title_w);
        } else {
            text_fit(&p, pos2(tx, y + 2.0), Align2::LEFT_TOP, "Empty deck", t::title(), t::INK_MUTED, title_w);
            text_fit(&p, pos2(tx, y + 26.0), Align2::LEFT_TOP, &format!("Pick a track below and press LOAD {}", d + 1), cond(15.0), t::INK_FAINT, title_w);
        }
        let mut chip_x = inner.max.x;
        let mut chip = |label: &str, col: Color32| {
            let w = 58.0;
            let cr = Rect::from_min_size(pos2(chip_x - w, y + 8.0), vec2(w, 22.0));
            p.rect_filled(cr, CornerRadius::same(t::R_CONTROL), col.gamma_multiply(0.18));
            p.text(cr.center(), Align2::CENTER_CENTER, label, caption(), col);
            chip_x -= w + 6.0;
        };
        if dv.synced {
            chip("SYNC", t::SYNC);
        }
        if dv.is_master {
            chip("MASTER", t::CUE);
        }
        y += 46.0 + 6.0;

        // Readouts.
        let col_w = inner.width() / 3.0;
        let cap_y = y;
        let val_y = y + 16.0;
        let elapsed = if dv.sample_rate > 0 { dv.pos / dv.sample_rate as f64 } else { 0.0 };
        let remain = if dv.sample_rate > 0 { (dv.frames as f64 - dv.pos) / dv.sample_rate as f64 } else { 0.0 };
        p.text(pos2(inner.min.x, cap_y), Align2::LEFT_TOP, "REMAIN", caption(), t::INK_FAINT);
        let low_time = dv.loaded && remain < 30.0 && dv.playing;
        let time_col = if low_time && (ui.input(|i| i.time) * 2.0) as i64 % 2 == 0 { t::DANGER } else { t::INK };
        p.text(pos2(inner.min.x, val_y), Align2::LEFT_TOP, format!("-{}", fmt_time(remain)), t::readout(), if dv.loaded { time_col } else { t::INK_FAINT });
        p.text(pos2(inner.min.x + 56.0, cap_y + 1.0), Align2::LEFT_TOP, fmt_time(elapsed), mono(11.5), t::INK_FAINT);

        let tc = inner.min.x + col_w * 1.22 + 6.0;
        p.text(pos2(tc, cap_y), Align2::LEFT_TOP, "TEMPO", caption(), t::INK_FAINT);
        let pct = (dv.rate - 1.0) * 100.0;
        let range = if dv.range >= 1.0 { "WIDE".to_string() } else { format!("±{:.0}", dv.range * 100.0) };
        p.text(pos2(tc, val_y + 3.0), Align2::LEFT_TOP, format!("{pct:+.2}%"), mono(19.0), if pct.abs() < 0.005 { t::INK_MUTED } else { t::INK });
        p.text(pos2(tc + 92.0, val_y + 8.0), Align2::LEFT_TOP, range, caption(), t::INK_FAINT);

        p.text(pos2(inner.max.x, cap_y), Align2::RIGHT_TOP, "BPM", caption(), t::INK_FAINT);
        let bpm = dv.bpm.map(|b| format!("{b:.2}")).unwrap_or_else(|| "--.--".into());
        p.text(pos2(inner.max.x, val_y - 3.0), Align2::RIGHT_TOP, bpm, mono(28.0), if dv.synced { t::SYNC } else { t::INK });
        y += 52.0 + 6.0;

        // Overview (tap to seek).
        let ov = Rect::from_min_size(pos2(inner.min.x, y), vec2(inner.width(), 34.0));
        self.overview(ui, &p, ov, d, dv);
        y += 34.0 + 8.0;

        // Button row.
        let labels: [(&str, Option<Color32>); 9] = [
            ("SHIFT", self.decks[d].shift.then_some(t::INK)),
            ("IN", (dv.loop_in.is_some() && !dv.loop_active).then_some(t::LOOP)),
            ("OUT", dv.loop_active.then_some(t::LOOP)),
            (if dv.loop_active { "EXIT" } else { "RELOOP" }, dv.loop_active.then_some(t::LOOP)),
            ("4 BEAT", None),
            ("« 4", None),
            ("4 »", None),
            ("SYNC", dv.synced.then_some(t::SYNC)),
            ("RANGE", None),
        ];
        let bw = (inner.width() - 8.0 * 5.0) / 9.0;
        for (i, (label, lit)) in labels.iter().enumerate() {
            let br = Rect::from_min_size(pos2(inner.min.x + i as f32 * (bw + 5.0), y), vec2(bw, 30.0));
            let resp = button(ui, br, Id::NULL.with(("btn", d, i)), label, *lit, semi(13.5));
            if resp.clicked() {
                let shift = self.decks[d].shift;
                let cmd = match (i, shift) {
                    (0, _) => {
                        self.decks[d].shift = !shift;
                        None
                    }
                    (1, false) => Some(Cmd::LoopIn(d)),
                    (1, true) => Some(Cmd::LoopScale(d, 0.5)),
                    (2, false) => Some(Cmd::LoopOut(d)),
                    (2, true) => Some(Cmd::LoopScale(d, 2.0)),
                    (3, _) => Some(Cmd::ReloopExit(d)),
                    (4, _) => Some(Cmd::AutoLoop(d, 4.0)),
                    (5, s) => Some(Cmd::BeatJump(d, if s { -16.0 } else { -4.0 })),
                    (6, s) => Some(Cmd::BeatJump(d, if s { 16.0 } else { 4.0 })),
                    (7, _) => Some(Cmd::SyncToggle(d)),
                    (8, _) => Some(Cmd::TempoRangeCycle(d)),
                    _ => None,
                };
                if let Some(c) = cmd {
                    self.send(c);
                }
            }
        }
        y += 30.0 + 8.0;

        // Controls: jog | CUE/PLAY | pads | tempo fader.
        let ctl = Rect::from_min_max(pos2(inner.min.x, y), inner.max);
        let jog_d = ctl.height().min(132.0);
        let jog = Rect::from_min_size(pos2(ctl.min.x, ctl.center().y - jog_d / 2.0), vec2(jog_d, jog_d));
        self.jog(ui, &p, jog, d, dv);

        let fader_w = 26.0;
        let fader = Rect::from_min_max(pos2(ctl.max.x - fader_w, ctl.min.y), ctl.max);
        let fv = (1.0 - (dv.tempo_fader as f32 + 1.0) / 2.0).clamp(0.0, 1.0); // top = −
        if let Some(nv) = vfader(ui, fader, Id::NULL.with(("tempo", d)), fv, if dv.synced { t::SYNC } else { t::INK_MUTED }) {
            let f = (1.0 - nv) * 2.0 - 1.0;
            let f = if f.abs() < 0.03 { 0.0 } else { f }; // centre détente
            self.send(Cmd::TempoFader(d, f as f64));
        }
        p.text(pos2(fader.center().x, fader.min.y), Align2::CENTER_TOP, "−", semi(13.0), t::INK_FAINT);
        p.text(pos2(fader.center().x, fader.max.y), Align2::CENTER_BOTTOM, "+", semi(13.0), t::INK_FAINT);

        // CUE / PLAY.
        let bx = jog.max.x + 12.0;
        let bsize = ((ctl.height() - 8.0) / 2.0).min(60.0);
        let cue_r = Rect::from_min_size(pos2(bx, ctl.center().y - bsize - 4.0), vec2(bsize, bsize));
        let play_r = Rect::from_min_size(pos2(bx, ctl.center().y + 4.0), vec2(bsize, bsize));
        let blink = (ui.input(|i| i.time) * 2.0) as i64 % 2 == 0;
        let cue_lit = dv.loaded && (dv.playing || dv.at_cue || (!dv.playing && blink));
        let play_lit = dv.playing || (dv.loaded && !dv.previewing && blink && !dv.playing);
        let cue_resp = round_button(ui, cue_r, Id::NULL.with(("cue", d)), "CUE", t::CUE, cue_lit);
        let play_resp = round_button(ui, play_r, Id::NULL.with(("play", d)), "PLAY", t::PLAY, play_lit || dv.previewing);
        let down = cue_resp.is_pointer_button_down_on();
        if down != self.decks[d].cue_held {
            self.decks[d].cue_held = down;
            self.send(Cmd::Cue(d, down));
        }
        // PLAY acts on press, like the hardware.
        let play_down = play_resp.is_pointer_button_down_on();
        if play_down && !self.decks[d].play_held {
            self.send(Cmd::Play(d));
        }
        self.decks[d].play_held = play_down;

        // Hot cue pads.
        let px0 = bx + bsize + 12.0;
        let px1 = fader.min.x - 10.0;
        let gap = 6.0;
        let pw = ((px1 - px0 - 3.0 * gap) / 4.0).min((ctl.height() - gap) / 2.0);
        let grid_h = pw * 2.0 + gap;
        let gy = ctl.center().y - grid_h / 2.0;
        let gx = px0 + ((px1 - px0) - (pw * 4.0 + gap * 3.0)) / 2.0;
        for i in 0..HOT_CUES {
            let (row, col) = (i / 4, i % 4);
            let pr = Rect::from_min_size(pos2(gx + col as f32 * (pw + gap), gy + row as f32 * (pw + gap)), vec2(pw, pw));
            let set = dv.hot[i].is_some();
            let colr = t::HOT_CUE[i];
            let resp = ui.interact(pr, Id::NULL.with(("pad", d, i)), Sense::click_and_drag());
            let pdown = resp.is_pointer_button_down_on();
            let fill = if set { colr.gamma_multiply(if pdown { 0.65 } else { 0.4 }) } else if pdown { t::HOVER } else { t::RAISED };
            p.rect_filled(pr, CornerRadius::same(t::R_CONTROL), fill);
            p.rect_stroke(pr, CornerRadius::same(t::R_CONTROL), Stroke::new(1.0, if set { colr } else { t::BORDER }), StrokeKind::Inside);
            let letter = ["A", "B", "C", "D", "E", "F", "G", "H"][i];
            p.text(pr.center(), Align2::CENTER_CENTER, letter, semi(17.0), if set { t::INK } else { t::INK_FAINT });
            let shift = self.decks[d].shift;
            if shift || resp.secondary_clicked() {
                if resp.clicked() || resp.secondary_clicked() {
                    self.send(Cmd::HotCueDelete(d, i));
                }
            } else if pdown != self.decks[d].hot_held[i] {
                self.decks[d].hot_held[i] = pdown;
                self.send(Cmd::HotCue(d, i, pdown));
            }
        }
        if self.decks[d].shift {
            p.text(pos2(gx, gy - 2.0), Align2::LEFT_BOTTOM, "SHIFT: tap a pad to delete it", caption(), t::INK_MUTED);
        }
    }

    fn overview(&self, ui: &Ui, p: &Painter, r: Rect, d: usize, dv: &DeckView) {
        p.rect_filled(r, CornerRadius::same(4), t::INSET);
        let Some(track) = self.decks[d].track.as_ref().filter(|_| dv.loaded) else { return };
        let frames = track.frames.max(1) as f64;
        let n = track.wave.len().max(1);
        let w = r.width() as usize;
        let mut mesh = Mesh::default();
        let mid = r.center().y;
        let half = r.height() * 0.45;
        let play_x = r.min.x + (dv.pos / frames) as f32 * r.width();
        for x in 0..w {
            let a = x * n / w;
            let b = ((x + 1) * n / w).max(a + 1).min(n);
            let (mut pk, mut lo, mut hi) = (0u8, 0u32, 0u32);
            for bin in &track.wave[a..b] {
                pk = pk.max(bin.peak);
                lo += bin.low as u32;
                hi += bin.high as u32;
            }
            let cnt = (b - a) as u32;
            let h = pk as f32 / 255.0 * half;
            let px = r.min.x + x as f32;
            let mix = (hi as f32 / (lo + hi + 1) as f32 * 1.4).clamp(0.0, 1.0);
            let _ = cnt;
            let col = t::WAVE_LOW.lerp_to_gamma(t::WAVE_HIGH, mix * 0.6);
            let col = if px < play_x { col.gamma_multiply(0.45) } else { col };
            mesh.add_colored_rect(Rect::from_min_max(pos2(px, mid - h), pos2(px + 1.0, mid + h)), col);
        }
        p.add(mesh);
        let x_of = |f: f64| r.min.x + (f / frames) as f32 * r.width();
        if let (Some(a), Some(b)) = (dv.loop_in, dv.loop_out) {
            p.rect_filled(Rect::from_x_y_ranges(x_of(a)..=x_of(b).max(x_of(a) + 2.0), r.y_range()), 0, t::LOOP.gamma_multiply(0.3));
        }
        for (i, h) in dv.hot.iter().enumerate() {
            if let Some(h) = h {
                p.vline(x_of(*h), r.y_range(), Stroke::new(2.0, t::HOT_CUE[i]));
            }
        }
        p.vline(x_of(dv.cue), r.y_range(), Stroke::new(2.0, t::CUE));
        p.vline(play_x, r.y_range(), Stroke::new(2.0, t::INK));
        let resp = ui.interact(r, Id::NULL.with(("overview", d)), Sense::click_and_drag());
        if (resp.clicked() || resp.dragged()) && let Some(pos) = resp.interact_pointer_pos() {
            let f = ((pos.x - r.min.x) / r.width()).clamp(0.0, 1.0);
            let _ = self.cmd.send(Cmd::Seek(d, f as f64));
        }
    }

    fn jog(&mut self, ui: &Ui, p: &Painter, r: Rect, d: usize, dv: &DeckView) {
        let c = r.center();
        let rad = r.width() / 2.0;
        let tint = t::DECK_TINT[d];
        p.circle_filled(c, rad, t::INSET);
        p.circle_stroke(c, rad - 1.0, Stroke::new(if dv.touched { 2.0 } else { 1.0 }, if dv.touched { tint } else { t::BORDER }));
        p.circle_filled(c, rad * 0.62, t::RAISED);
        // Position ring: elapsed fraction of the track.
        if dv.frames > 0 {
            let f = (dv.pos / dv.frames as f64) as f32;
            use std::f32::consts::{PI, TAU};
            p.add(Shape::line(arc_points(c, rad - 6.0, -PI / 2.0, -PI / 2.0 + TAU * f.max(0.002)), Stroke::new(3.0, tint.gamma_multiply(0.75))));
        }
        let ang = (dv.platter_rev.fract() as f32) * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
        p.line_segment([c + Vec2::angled(ang) * rad * 0.66, c + Vec2::angled(ang) * (rad - 12.0)], Stroke::new(3.0, t::INK));
        let mode = if dv.vinyl { "VINYL" } else { "CDJ" };
        p.text(c, Align2::CENTER_CENTER, mode, caption(), if dv.vinyl { t::INK_MUTED } else { t::INK_FAINT });

        let resp = ui.interact(r, Id::NULL.with(("jog", d)), Sense::click_and_drag());
        let down = resp.is_pointer_button_down_on();
        let du = &mut self.decks[d];
        if down != du.touch_held {
            du.touch_held = down;
            du.jog_angle = None;
            let _ = self.cmd.send(Cmd::JogTouch(d, down));
        }
        if let (true, Some(pos)) = (down, resp.interact_pointer_pos()) {
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
                    if ticks.abs() > 0.0 {
                        let _ = self.cmd.send(Cmd::JogScratch(d, ticks));
                    }
                }
                du.jog_angle = Some(a);
            }
        }
        // Tap the centre label to switch vinyl/CDJ mode.
        let centre = Rect::from_center_size(c, vec2(rad * 0.8, 22.0));
        if resp.clicked() && resp.interact_pointer_pos().is_some_and(|pp| centre.contains(pp)) {
            let _ = self.cmd.send(Cmd::VinylToggle(d));
        }
    }

    fn mixer_panel(&self, ui: &Ui, r: Rect, v: &EngineView) {
        let p = ui.painter();
        card(p, r);
        let inner = r.shrink(10.0);
        let strip_w = 118.0;
        let center = Rect::from_min_max(pos2(inner.min.x + strip_w, inner.min.y), pos2(inner.max.x - strip_w, inner.max.y));
        let xf_h = 40.0;
        for c in 0..2 {
            let x0 = if c == 0 { inner.min.x } else { inner.max.x - strip_w };
            let sr = Rect::from_min_max(pos2(x0, inner.min.y), pos2(x0 + strip_w, inner.max.y - xf_h - 6.0));
            self.strip(ui, sr, c, v);
        }
        // Centre: master / phones.
        let kh = 52.0;
        let kw = center.width();
        let m = &v.mixer;
        let send = |mp: MixParam| {
            let _ = self.cmd.send(Cmd::Mix(mp));
        };
        let mut y = center.min.y;
        for (i, (label, val, col)) in [("MASTER", m.master, t::INK_MUTED), ("PH MIX", m.head_mix, t::CUE), ("PH LVL", m.head_level, t::INK_MUTED)].into_iter().enumerate() {
            let kr = Rect::from_min_size(pos2(center.min.x, y), vec2(kw, kh));
            if let Some(nv) = knob(ui, kr, Id::NULL.with(("mk", i)), val, label, col, false) {
                send(match i {
                    0 => MixParam::Master(nv),
                    1 => MixParam::HeadMix(nv),
                    _ => MixParam::HeadLevel(nv),
                });
            }
            y += kh + 4.0;
        }
        let mr = Rect::from_min_max(pos2(center.center().x - 7.0, y + 6.0), pos2(center.center().x + 7.0, inner.max.y - xf_h - 8.0));
        if mr.height() > 30.0 {
            meter(p, mr, v.peak_master);
        }
        // Crossfader.
        let xr = Rect::from_center_size(pos2(inner.center().x, inner.max.y - xf_h / 2.0), vec2(200.0, xf_h - 6.0));
        let resp = ui.interact(xr, Id::NULL.with("xfader"), Sense::click_and_drag());
        let track = Rect::from_center_size(xr.center(), vec2(xr.width() - 24.0, 4.0));
        p.rect_filled(track, 2, t::INSET);
        p.vline(track.center().x, Rect::from_center_size(xr.center(), vec2(1.0, 14.0)).y_range(), Stroke::new(1.0, t::INK_FAINT));
        let x = track.min.x + m.crossfader * track.width();
        let cap = Rect::from_center_size(pos2(x, xr.center().y), vec2(18.0, xr.height() - 4.0));
        p.rect_filled(cap, CornerRadius::same(3), if resp.dragged() { t::HOVER } else { t::RAISED });
        p.rect_stroke(cap, CornerRadius::same(3), Stroke::new(1.0, t::BORDER), StrokeKind::Inside);
        p.vline(x, cap.y_range().shrink(3.0), Stroke::new(2.0, t::INK));
        p.text(pos2(xr.min.x - 6.0, xr.center().y), Align2::RIGHT_CENTER, "1", semi(15.0), t::DECK_TINT[0]);
        p.text(pos2(xr.max.x + 6.0, xr.center().y), Align2::LEFT_CENTER, "2", semi(15.0), t::DECK_TINT[1]);
        if resp.double_clicked() {
            send(MixParam::Crossfader(0.5));
        } else if (resp.dragged() || resp.clicked()) && let Some(pos) = resp.interact_pointer_pos() {
            send(MixParam::Crossfader(((pos.x - track.min.x) / track.width()).clamp(0.0, 1.0)));
        }
    }

    fn strip(&self, ui: &Ui, r: Rect, c: usize, v: &EngineView) {
        let p = ui.painter();
        let cp = v.mixer.ch[c];
        let send = |mp: MixParam| {
            let _ = self.cmd.send(Cmd::Mix(mp));
        };
        let kw = r.width() / 2.0;
        let kh = 50.0;
        let cell = |row: usize, col: usize| Rect::from_min_size(pos2(r.min.x + col as f32 * kw, r.min.y + row as f32 * kh), vec2(kw, kh));
        let tint = t::DECK_TINT[c];
        let knobs: [(usize, usize, &str, f32, fn(usize, f32) -> MixParam, bool); 5] = [
            (0, 0, "TRIM", cp.trim, MixParam::Trim, true),
            (0, 1, "HI", cp.hi, MixParam::Hi, true),
            (1, 0, "MID", cp.mid, MixParam::Mid, true),
            (1, 1, "LOW", cp.low, MixParam::Low, true),
            (2, if c == 0 { 1 } else { 0 }, "FILTER", cp.filter, MixParam::Filter, true),
        ];
        for (i, (row, col, label, val, mk, bip)) in knobs.into_iter().enumerate() {
            let killed = matches!(label, "HI" | "MID" | "LOW") && val < 0.02;
            let colr = if killed { t::DANGER } else { tint };
            if let Some(nv) = knob(ui, cell(row, col), Id::NULL.with(("k", c, i)), val, label, colr, bip) {
                send(mk(c, nv));
            }
        }
        // Channel number in the free cell.
        let free = cell(2, if c == 0 { 0 } else { 1 });
        p.text(free.center(), Align2::CENTER_CENTER, format!("CH {}", c + 1), semi(15.0), tint);
        let mut y = r.min.y + 3.0 * kh + 6.0;
        let pfl = Rect::from_min_size(pos2(r.min.x + 6.0, y), vec2(r.width() - 12.0, 28.0));
        if button(ui, pfl, Id::NULL.with(("pfl", c)), "CUE", cp.pfl.then_some(t::CUE), semi(14.0)).clicked() {
            send(MixParam::PflToggle(c));
        }
        y += 28.0 + 6.0;
        let fr = Rect::from_min_max(pos2(r.center().x - 18.0, y), pos2(r.center().x + 18.0, r.max.y));
        if let Some(nv) = vfader(ui, fr, Id::NULL.with(("fader", c)), cp.fader, tint) {
            send(MixParam::Fader(c, nv));
        }
        let mx = if c == 0 { fr.max.x + 10.0 } else { fr.min.x - 18.0 };
        meter(p, Rect::from_min_max(pos2(mx, y + 4.0), pos2(mx + 8.0, r.max.y - 4.0)), v.decks[c].peak);
    }

    fn browser(&mut self, ui: &mut Ui, r: Rect) {
        let p = ui.painter().clone();
        card(&p, r);
        let inner = r.shrink2(vec2(t::SP_M, 8.0));
        let head = Rect::from_min_size(inner.min, vec2(inner.width(), 32.0));
        p.text(pos2(head.min.x, head.center().y), Align2::LEFT_CENTER, "LIBRARY", caption(), t::INK_FAINT);
        let count = match &self.library {
            Some(l) => format!("{}  ·  {} tracks", self.music_root.display(), l.len()),
            None => format!("{}  ·  scanning…", self.music_root.display()),
        };
        text_fit(&p, pos2(head.min.x + 66.0, head.center().y), Align2::LEFT_CENTER, &count, cond(15.0), t::INK_MUTED, head.width() - 300.0);
        for d in 0..DECKS {
            let br = Rect::from_min_size(pos2(head.max.x - (DECKS - d) as f32 * 104.0, head.min.y + 1.0), vec2(96.0, 30.0));
            let has = self.library.as_ref().is_some_and(|l| !l.is_empty());
            let resp = button(ui, br, Id::NULL.with(("load", d)), &format!("LOAD {}", d + 1), has.then_some(t::DECK_TINT[d]), semi(15.0));
            if resp.clicked() {
                self.load_selected(d);
            }
        }
        let list = Rect::from_min_max(pos2(inner.min.x, head.max.y + 6.0), inner.max);
        let lib = match &self.library {
            None => {
                p.text(list.center(), Align2::CENTER_CENTER, "Scanning your music folder…", t::body(), t::INK_MUTED);
                return;
            }
            Some(l) if l.is_empty() => {
                p.text(list.center() - vec2(0.0, 12.0), Align2::CENTER_CENTER, "No tracks found", t::title(), t::INK);
                p.text(
                    list.center() + vec2(0.0, 14.0),
                    Align2::CENTER_CENTER,
                    format!("Put MP3, FLAC, WAV, OGG or M4A files in {} or start with --music <folder>", self.music_root.display()),
                    t::body(),
                    t::INK_MUTED,
                );
                return;
            }
            Some(l) => l.clone(),
        };
        // Column header.
        let cols = |w: f32| [0.0, 40.0, 40.0 + (w - 140.0) * 0.48, 40.0 + (w - 140.0) * 0.78, w - 56.0];
        let cx = cols(list.width());
        let hy = list.min.y;
        for (i, name) in ["", "TITLE", "ARTIST", "FOLDER", "TYPE"].iter().enumerate() {
            p.text(pos2(list.min.x + cx[i] + 8.0, hy + 8.0), Align2::LEFT_CENTER, *name, caption(), t::INK_FAINT);
        }
        let rows = Rect::from_min_max(pos2(list.min.x, hy + 18.0), list.max);
        let row_h = 30.0;
        let loaded: Vec<Option<PathBuf>> = self.decks.iter().map(|d| d.track.as_ref().map(|t| t.path.clone())).collect();
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rows));
        child.spacing_mut().item_spacing.y = 0.0;
        let selected = self.selected;
        let scroll_to = std::mem::take(&mut self.scroll_to_selected);
        let mut clicked: Option<usize> = None;
        let mut load_req: Option<(usize, usize)> = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(&mut child, row_h, lib.len(), |ui, range| {
            for i in range {
                let e = &lib[i];
                let (rr, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                let p = ui.painter();
                if i == selected {
                    p.rect_filled(rr, CornerRadius::same(t::R_CONTROL), t::ACCENT.gamma_multiply(0.2));
                    p.rect_stroke(rr, CornerRadius::same(t::R_CONTROL), Stroke::new(1.0, t::ACCENT.gamma_multiply(0.6)), StrokeKind::Inside);
                    if scroll_to {
                        ui.scroll_to_rect(rr, None);
                    }
                } else if resp.hovered() {
                    p.rect_filled(rr, CornerRadius::same(t::R_CONTROL), t::RAISED);
                } else if i % 2 == 1 {
                    p.rect_filled(rr, 0, t::CHROME.gamma_multiply(0.6));
                }
                // Deck badges for tracks currently loaded.
                let mut bx = rr.min.x + 10.0;
                for (d, lp) in loaded.iter().enumerate() {
                    if lp.as_ref() == Some(&e.path) {
                        p.circle_filled(pos2(bx, rr.center().y), 8.0, t::DECK_TINT[d].gamma_multiply(0.25));
                        p.text(pos2(bx, rr.center().y), Align2::CENTER_CENTER, format!("{}", d + 1), semi(12.0), t::DECK_TINT[d]);
                        bx += 18.0;
                    }
                }
                let y = rr.center().y;
                let w = rr.width();
                let cx = cols(w);
                let ink = if i == selected { t::INK } else { t::INK };
                text_fit(p, pos2(rr.min.x + cx[1] + 8.0, y), Align2::LEFT_CENTER, &e.title, semi(16.0), ink, cx[2] - cx[1] - 16.0);
                text_fit(p, pos2(rr.min.x + cx[2] + 8.0, y), Align2::LEFT_CENTER, &e.artist, cond(16.0), t::INK_MUTED, cx[3] - cx[2] - 16.0);
                text_fit(p, pos2(rr.min.x + cx[3] + 8.0, y), Align2::LEFT_CENTER, &e.folder, cond(15.0), t::INK_FAINT, cx[4] - cx[3] - 16.0);
                p.text(pos2(rr.min.x + cx[4] + 8.0, y), Align2::LEFT_CENTER, &e.ext, mono(12.0), t::INK_FAINT);
                if resp.clicked() {
                    clicked = Some(i);
                }
                if resp.double_clicked() {
                    // Double-tap loads to whichever deck isn't playing.
                    let v = self.view.lock();
                    let d = if v.decks[0].playing && !v.decks[1].playing { 1 } else { 0 };
                    load_req = Some((d, i));
                }
            }
        });
        if let Some(i) = clicked {
            self.selected = i;
        }
        if let Some((d, i)) = load_req {
            self.selected = i;
            self.load_selected(d);
        }
    }
}

fn round_button(ui: &Ui, r: Rect, id: Id, label: &str, col: Color32, lit: bool) -> Response {
    let resp = ui.interact(r, id, Sense::click_and_drag());
    let p = ui.painter();
    let c = r.center();
    let rad = r.width().min(r.height()) / 2.0;
    let down = resp.is_pointer_button_down_on();
    p.circle_filled(c, rad, if down { t::HOVER } else { t::RAISED });
    p.circle_stroke(c, rad - 1.5, Stroke::new(3.0, if lit { col } else { col.gamma_multiply(0.25) }));
    p.text(c, Align2::CENTER_CENTER, label, semi(15.0), if lit { col } else { t::INK_MUTED });
    resp
}
