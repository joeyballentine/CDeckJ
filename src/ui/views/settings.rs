//! SETTINGS view (a CDJ's SHORTCUT/UTILITY screens): My Settings options, mixer curves,
//! audio output, a live controller test, and system options.

use crate::ui::prelude::*;
use crate::ui::App;

use crossbeam_channel::Receiver;

use crate::engine::{Cmd, EngineView, MixParam, DECKS};
use crate::rekordbox::settings::{CrossfaderCurve, FaderCurve, WaveColor};

const PAGES: [(&str, &str); 5] = [
    (crate::ui::icons::PLAYER, "PLAYER"),
    (crate::ui::icons::MIXER, "MIXER & FX"),
    (crate::ui::icons::AUDIO, "AUDIO"),
    (crate::ui::icons::CONTROLLER, "CONTROLLER"),
    (crate::ui::icons::SYSTEM, "SYSTEM"),
];

#[derive(Default)]
pub struct SettingsState {
    pub page: usize,
    devices: Option<Vec<(String, String, String)>>,
    devices_rx: Option<Receiver<Vec<(String, String, String)>>>,
    autolaunch: bool,
    job_rx: Option<Receiver<Result<String, String>>>,
    msg: Option<String>,
}

impl App {
    /// Refresh things that are slow to find out, off the UI thread.
    pub(crate) fn settings_opened(&mut self) {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let alsa = self.audio_opts.force_alsa;
        std::thread::spawn(move || {
            let _ = tx.send(crate::audio::output_device_list(alsa));
        });
        self.settings.devices_rx = Some(rx);
        self.settings.autolaunch = crate::platform::autolaunch::is_installed();
    }

    /// Drop the audio stream and reconnect with the current options.
    fn restart_audio(&mut self) {
        self.audio = None;
        self.reconnecting = None;
        if self.silent.is_none() {
            self.silent = Some(crate::audio::SilentClock::start(&self.engine));
        }
        self.last_reconnect = std::time::Instant::now() - std::time::Duration::from_secs(10);
    }

    pub(crate) fn settings_view(&mut self, ui: &mut Ui, r: Rect, v: &EngineView) {
        if let Some(rx) = &self.settings.devices_rx {
            if let Ok(d) = rx.try_recv() {
                self.settings.devices = Some(d);
                self.settings.devices_rx = None;
            }
        }
        if let Some(rx) = &self.settings.job_rx {
            if let Ok(res) = rx.try_recv() {
                self.settings.msg = Some(match res {
                    Ok(m) | Err(m) => m,
                });
                self.settings.job_rx = None;
                self.settings.autolaunch = crate::platform::autolaunch::is_installed();
            }
        }
        let p = ui.painter().clone();
        // Page tabs down the left.
        let side = Rect::from_min_size(r.min, vec2(190.0, r.height()));
        card(&p, side);
        for (i, (icon, name)) in PAGES.iter().enumerate() {
            let br = Rect::from_min_size(pos2(side.min.x + 6.0, side.min.y + 8.0 + i as f32 * 44.0), vec2(side.width() - 12.0, 40.0));
            if nav_row(ui, &mut self.touches, br, Id::NULL.with(("spage", i)), icon, name, self.settings.page == i).clicked {
                self.settings.page = i;
            }
        }
        let main = Rect::from_min_max(pos2(side.max.x + 6.0, r.min.y), r.max);
        card(&p, main);
        let inner = main.shrink2(vec2(18.0, 14.0));
        let mut rows = Rows { y: inner.min.y, x: inner.min.x, w: inner.width(), label_w: 340.0 };
        match self.settings.page {
            0 => self.page_player(ui, &mut rows, v),
            1 => self.page_mixer(ui, &mut rows, v),
            2 => self.page_audio(ui, &mut rows, v),
            3 => self.page_controller(ui, &mut rows, inner),
            _ => self.page_system(ui, &mut rows),
        }
    }

    fn page_player(&mut self, ui: &mut Ui, rows: &mut Rows, v: &EngineView) {
        let c = self.config.clone();
        let w = match c.waveform_color {
            WaveColor::Blue => 0,
            WaveColor::Rgb => 1,
            WaveColor::ThreeBand => 2,
        };
        let pl = match c.perform_layout {
            crate::config::PerformLayout::Auto => 0,
            crate::config::PerformLayout::Full => 1,
            crate::config::PerformLayout::Display => 2,
        };
        if let Some(i) = rows.seg(self, ui, "PERFORM screen (AUTO follows the controller)", "spl", &["AUTO", "FULL", "DISPLAY"], pl) {
            use crate::config::PerformLayout as L;
            self.config.perform_layout = [L::Auto, L::Full, L::Display][i];
        }
        if let Some(i) = rows.seg(self, ui, "Waveform colour", "swc", &["BLUE", "RGB", "3-BAND"], w) {
            self.config.waveform_color = [WaveColor::Blue, WaveColor::Rgb, WaveColor::ThreeBand][i];
        }
        if let Some(i) = rows.seg(self, ui, "Overview waveform", "sov", &["HALF", "FULL"], usize::from(c.overview_full)) {
            self.config.overview_full = i == 1;
        }
        if let Some(i) = rows.seg(self, ui, "Playhead position", "sph", &["CENTRE", "LEFT"], usize::from(c.playhead_left)) {
            self.config.playhead_left = i == 1;
        }
        if let Some(i) = rows.seg(self, ui, "Time display", "stm", &["REMAINING", "ELAPSED"], usize::from(!c.time_remain)) {
            self.config.time_remain = i == 0;
        }
        if let Some(i) = rows.seg(self, ui, "Quantize", "sq", &["OFF", "ON"], usize::from(v.quantize)) {
            self.send(Cmd::Quantize(i == 1));
        }
        let qb = [0.125, 0.25, 0.5, 1.0];
        let qi = qb.iter().position(|&x| (x - c.quantize_beats).abs() < 1e-6).unwrap_or(3);
        if let Some(i) = rows.seg(self, ui, "Quantize value", "sqb", &["1/8", "1/4", "1/2", "1"], qi) {
            self.config.quantize_beats = qb[i];
            self.send(Cmd::QuantizeBeats(qb[i]));
        }
        let bj = [0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0];
        let bi = bj.iter().position(|&x| (x - c.beat_jump).abs() < 1e-6).unwrap_or(3);
        if let Some(i) = rows.seg(self, ui, "Beat jump (SHIFT ×4)", "sbj", &["1/2", "1", "2", "4", "8", "16", "32"], bi) {
            self.config.beat_jump = bj[i];
        }
        let levels = [-36.0, -48.0, -60.0, -72.0];
        let ai = if !c.auto_cue { 0 } else { c.auto_cue_db.map(|db| 2 + levels.iter().position(|&l| (l - db).abs() < 0.5).unwrap_or(2)).unwrap_or(1) };
        if let Some(i) = rows.seg(self, ui, "Auto cue (next load)", "sac", &["OFF", "MEMORY", "−36 dB", "−48 dB", "−60 dB", "−72 dB"], ai) {
            self.config.auto_cue = i > 0;
            self.config.auto_cue_db = (i >= 2).then(|| levels[i - 2]);
        }
        if let Some(i) = rows.seg(self, ui, "Hot cue auto load (next load)", "sha", &["OFF", "ON"], usize::from(c.hotcue_autoload)) {
            self.config.hotcue_autoload = i == 1;
        }
        if let Some(i) = rows.seg(self, ui, "Jog mode (both decks)", "sjm", &["VINYL", "CDJ"], usize::from(!v.decks[0].vinyl)) {
            for d in 0..DECKS {
                self.send(Cmd::Vinyl(d, i == 0));
            }
        }
        if let Some(i) = rows.seg(self, ui, "Tempo range (both decks)", "str", &["±6 %", "±10 %", "±16 %", "WIDE"], v.decks[0].range_idx) {
            for d in 0..DECKS {
                self.send(Cmd::TempoRange(d, i));
            }
        }
        let mt = usize::from(v.decks.iter().all(|d| d.keylock));
        if let Some(i) = rows.seg(self, ui, "Master Tempo: tempo without pitch (both decks)", "smt", &["OFF", "ON"], mt) {
            for d in 0..DECKS {
                self.send(Cmd::MasterTempo(d, i == 1));
            }
        }
        if let Some(i) = rows.seg(self, ui, "Use a stick's My Settings", "smy", &["OFF", "ON"], usize::from(c.apply_usb_settings)) {
            self.config.apply_usb_settings = i == 1;
        }
    }

    fn page_mixer(&mut self, ui: &mut Ui, rows: &mut Rows, v: &EngineView) {
        let fc = match v.mixer.fader_curve {
            FaderCurve::SteepTop => 0,
            FaderCurve::Linear => 1,
            FaderCurve::SteepBottom => 2,
        };
        if let Some(i) = rows.seg(self, ui, "Channel fader curve", "sfc", &["STEEP TOP", "LINEAR", "STEEP BOTTOM"], fc) {
            let c = [FaderCurve::SteepTop, FaderCurve::Linear, FaderCurve::SteepBottom][i];
            self.config.fader_curve = c;
            self.send(Cmd::Mix(MixParam::FaderCurve(c)));
        }
        let xc = match v.mixer.crossfader_curve {
            CrossfaderCurve::ConstantPower => 0,
            CrossfaderCurve::SlowCut => 1,
            CrossfaderCurve::FastCut => 2,
        };
        if let Some(i) = rows.seg(self, ui, "Crossfader curve", "sxc", &["SMOOTH", "SLOW CUT", "FAST CUT"], xc) {
            let c = [CrossfaderCurve::ConstantPower, CrossfaderCurve::SlowCut, CrossfaderCurve::FastCut][i];
            self.config.crossfader_curve = c;
            self.send(Cmd::Mix(MixParam::CrossfaderCurve(c)));
        }
        if let Some(i) = rows.seg(self, ui, "Beat FX quantize", "sfq", &["OFF", "ON"], usize::from(v.mixer.fx.quantize)) {
            self.config.beat_fx_quantize = i == 1;
            self.send(Cmd::Mix(MixParam::FxQuantize(i == 1)));
        }
        let zooms = [4.0, 6.0, 8.0, 12.0, 16.0];
        let zi = zooms.iter().position(|&z| (z - self.config.waveform_seconds).abs() < 0.01).unwrap_or(2);
        if let Some(i) = rows.seg(self, ui, "Scrolling waveform shows", "szm", &["4 s", "6 s", "8 s", "12 s", "16 s"], zi) {
            self.config.waveform_seconds = zooms[i];
        }
    }

    fn page_audio(&mut self, ui: &mut Ui, rows: &mut Rows, v: &EngineView) {
        let p = ui.painter().clone();
        let now = match &self.audio {
            Some(a) => format!("{} · {} ch · {} Hz", a.description, a.channels, a.sample_rate),
            None => format!("Not connected — {}", self.audio_error.clone().unwrap_or_default()),
        };
        rows.text(&p, "Output now", &now);
        let bufs = [128u32, 256, 512, 1024];
        let bi = bufs.iter().position(|&b| b == self.audio_opts.buffer_frames).unwrap_or(1);
        if let Some(i) = rows.seg(self, ui, "Buffer (lower = less delay)", "sbuf", &["128", "256", "512", "1024"], bi) {
            self.audio_opts.buffer_frames = bufs[i];
            self.config.buffer_frames = bufs[i];
            self.restart_audio();
        }
        let cp = usize::from(self.audio_opts.cue_profile_hints.is_some());
        if let Some(i) = rows.seg(self, ui, "Set the controller to 4 channels (headphone cue)", "scp", &["OFF", "ON"], cp) {
            self.config.cue_profile = i == 1;
            if i == 0 {
                self.audio = None;
                crate::audio::cardprofile::restore_saved();
            }
            self.audio_opts.cue_profile_hints = (i == 1).then(|| self.config.midi.port_hints.clone());
            self.restart_audio();
        }
        if let Some(i) = rows.seg(self, ui, "Stereo output: cue in right ear", "ssc", &["OFF", "ON"], usize::from(self.audio_opts.split_cue)) {
            self.audio_opts.split_cue = i == 1;
            self.config.split_cue = i == 1;
            self.restart_audio();
        }
        let _ = v;
        heading(&p, pos2(rows.x, rows.y + 6.0), "CHOOSE THE OUTPUT (AUTO PICKS THE CONTROLLER'S SOUND CARD)");
        rows.y += 28.0;
        let mut choices: Vec<(String, Option<String>)> = vec![("AUTO".into(), None)];
        match &self.settings.devices {
            Some(list) => {
                for (host, desc, _) in list {
                    choices.push((format!("{desc}   ({host})"), Some(desc.clone())));
                }
            }
            None => {
                p.text(pos2(rows.x, rows.y + 4.0), Align2::LEFT_TOP, "Looking for devices…", cond(15.0), t::INK_FAINT);
                rows.y += 30.0;
            }
        }
        for (i, (label, dev)) in choices.into_iter().enumerate() {
            if rows.y > rows.limit() {
                break;
            }
            let br = Rect::from_min_size(pos2(rows.x, rows.y), vec2(rows.w.min(720.0), 36.0));
            let on = self.audio_opts.device == dev;
            if button(ui, &mut self.touches, br, Id::NULL.with(("sdev", i)), &label, on.then_some(t::ACCENT), cond(15.0)).clicked {
                self.audio_opts.device = dev.clone();
                self.config.audio_device = dev;
                self.restart_audio();
            }
            rows.y += 40.0;
        }
    }

    fn page_controller(&mut self, ui: &mut Ui, rows: &mut Rows, inner: Rect) {
        let p = ui.painter().clone();
        let name = self.midi.lock().clone().unwrap_or_else(|| "No DJ controller connected".into());
        rows.text(&p, "Controller", &name);
        let pad = self.gamepad.as_ref().and_then(|g| g.lock().clone()).unwrap_or_else(|| "Not in use".into());
        rows.text(&p, "Steam Deck controls", &pad);
        heading(&p, pos2(rows.x, rows.y + 8.0), "CONTROLLER TEST: MOVE OR PRESS ANYTHING; EACH MESSAGE AND WHAT IT DOES APPEARS HERE");
        rows.y += 30.0;
        let log = crate::midi::MIDI_LOG.lock().clone();
        let lines = ((inner.max.y - rows.y - 40.0) / 21.0).max(1.0) as usize;
        if log.is_empty() {
            p.text(pos2(rows.x, rows.y), Align2::LEFT_TOP, "Nothing received yet.", cond(15.0), t::INK_FAINT);
        }
        for (i, l) in log.iter().rev().take(lines).enumerate() {
            text_fit(&p, pos2(rows.x, rows.y + i as f32 * 21.0), Align2::LEFT_TOP, l, mono(13.0), if i == 0 { t::INK } else { t::INK_MUTED }, rows.w);
        }
        let cr = Rect::from_min_size(pos2(inner.max.x - 90.0, inner.max.y - 34.0), vec2(90.0, 32.0));
        if button(ui, &mut self.touches, cr, Id::NULL.with("sclr"), "CLEAR", None, semi(14.0)).clicked {
            crate::midi::MIDI_LOG.lock().clear();
        }
        p.text(
            pos2(rows.x, inner.max.y - 18.0),
            Align2::LEFT_CENTER,
            "Wrong control? Note the bytes and set the number in [midi] in ~/.config/cdeckj/config.toml.",
            cond(14.0),
            t::INK_FAINT,
        );
    }

    fn page_system(&mut self, ui: &mut Ui, rows: &mut Rows) {
        let p = ui.painter().clone();
        let busy = self.settings.job_rx.is_some();
        if let Some(i) = rows.seg(self, ui, "Open CDeckJ when the controller is plugged in", "sal", &["OFF", "ON"], usize::from(self.settings.autolaunch)) {
            if !busy && (i == 1) != self.settings.autolaunch {
                let (tx, rx) = crossbeam_channel::bounded(1);
                std::thread::spawn(move || {
                    let _ = tx.send(if i == 1 { crate::platform::autolaunch::install() } else { crate::platform::autolaunch::uninstall() });
                });
                self.settings.job_rx = Some(rx);
                self.settings.msg = Some("Working…".into());
            }
        }
        if let Some(m) = &self.settings.msg {
            let g = p.layout(m.clone(), cond(14.0), t::INK_MUTED, rows.w - rows.label_w);
            let h = g.size().y;
            p.galley(pos2(rows.x + rows.label_w, rows.y), g, t::INK_MUTED);
            rows.y += h + 10.0;
        }
        if let Some(i) = rows.seg(self, ui, "Keep the screen awake (next start)", "ska", &["OFF", "ON"], usize::from(self.config.keep_awake)) {
            self.config.keep_awake = i == 1;
        }
        if let Some(i) = rows.seg(self, ui, "Steam Deck controls (next start)", "sgp", &["OFF", "ON"], usize::from(self.config.gamepad)) {
            self.config.gamepad = i == 1;
        }
        if let Some(i) = rows.seg(self, ui, "Analyse the music folder (next start)", "sba", &["OFF", "ON"], usize::from(self.config.background_analysis)) {
            self.config.background_analysis = i == 1;
        }
        rows.y += 8.0;
        let rec = self.recordings_dir();
        rows.text(&p, "Recordings", &rec.display().to_string());
        rows.text(&p, "Music folder", &self.lib.local_root.display().to_string());
        rows.text(&p, "Settings file", &crate::config::Config::path().display().to_string());
        rows.text(&p, "Saved data", &crate::config::data_dir().display().to_string());
        rows.text(&p, "Version", env!("CARGO_PKG_VERSION"));
    }
}

/// Lays out labelled settings rows top to bottom.
struct Rows {
    y: f32,
    x: f32,
    w: f32,
    label_w: f32,
}

impl Rows {
    fn limit(&self) -> f32 {
        f32::MAX
    }

    fn label(&self, p: &egui::Painter, s: &str) {
        text_fit(p, pos2(self.x, self.y + 17.0), Align2::LEFT_CENTER, s, cond(16.0), t::INK, self.label_w - 12.0);
    }

    fn seg(&mut self, app: &mut App, ui: &Ui, label: &str, id: &str, opts: &[&str], sel: usize) -> Option<usize> {
        let p = ui.painter().clone();
        self.label(&p, label);
        let w = (opts.len() as f32 * 96.0).min(self.w - self.label_w);
        let r = Rect::from_min_size(pos2(self.x + self.label_w, self.y - 2.0), vec2(w, 38.0));
        let out = segmented(app, ui, r, id, opts, sel, label);
        self.y += 44.0;
        out
    }

    fn text(&mut self, p: &egui::Painter, label: &str, value: &str) {
        self.label(p, label);
        text_fit(p, pos2(self.x + self.label_w, self.y + 17.0), Align2::LEFT_CENTER, value, cond(15.0), t::INK_MUTED, self.w - self.label_w);
        self.y += 36.0;
    }
}
