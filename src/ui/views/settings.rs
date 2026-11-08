//! SETTINGS view (a CDJ's SHORTCUT/UTILITY screens): My Settings options, mixer curves,
//! audio output, a live controller test, and system options.

use crate::ui::prelude::*;
use crate::ui::App;

use crossbeam_channel::Receiver;

use crate::engine::{Cmd, EngineView, MixParam, DECKS};
use crate::midi::{Bound, Control, Msgs};
use crate::platform::install::{self, Offer};
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
    /// Where the installed copy lives (or would).
    install_target: Option<std::path::PathBuf>,
    update_rx: Option<Receiver<Result<(), String>>>,
    uninstall_rx: Option<Receiver<Result<String, String>>>,
    /// Progress or result of UPDATE or UNINSTALL, shown under those buttons.
    install_msg: Option<String>,
    /// First press of the two-press UNINSTALL.
    uninstall_confirm: Option<std::time::Instant>,
    /// MIDI learn is waiting for this control.
    pub learning: Option<Control>,
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
        self.settings.install_target = install::target();
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
            let br = Rect::from_min_size(pos2(side.min.x, side.min.y + 8.0 + i as f32 * 40.0), vec2(side.width(), 40.0));
            if nav_row(ui, &mut self.touches, br, Id::NULL.with(("spage", i)), icon, name, self.settings.page == i).clicked {
                self.settings.page = i;
            }
        }
        let main = Rect::from_min_max(pos2(side.max.x, r.min.y), r.max);
        card(&p, main);
        let inner = main.shrink2(vec2(18.0, 14.0));
        let page = self.settings.page;
        if matches!(page, 2 | 3) {
            // AUDIO scrolls its own device list; CONTROLLER sizes its log to the page.
            let mut rows = Rows { y: inner.min.y, x: inner.min.x, w: inner.width(), bottom: inner.max.y, label_w: 340.0 };
            if page == 2 {
                self.page_audio(ui, &mut rows, v);
            } else {
                self.page_controller(ui, &mut rows, inner);
            }
            return;
        }
        // The other pages scroll when they don't fit: glance mode, a banner, a short window.
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner));
        egui::ScrollArea::vertical().id_salt(("spage", page)).auto_shrink([false, false]).show(&mut child, |ui| {
            let top = ui.max_rect().min;
            let w = ui.max_rect().width();
            // Choices sit 2 pt above their row: start lower so the first row isn't clipped.
            let mut rows = Rows { y: top.y + 2.0, x: top.x, w, bottom: f32::INFINITY, label_w: 340.0 };
            match page {
                0 => self.page_player(ui, &mut rows, v),
                1 => self.page_mixer(ui, &mut rows, v),
                _ => self.page_system(ui, &mut rows),
            }
            ui.expand_to_include_rect(Rect::from_min_max(top, pos2(top.x + w, rows.y)));
        });
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
        if let Some(i) = rows.seg(self, ui, "Screen (GLANCE: bigger text, fewer words)  ( F6 )", "sgl", &["DETAILED", "GLANCE"], usize::from(c.glance)) {
            self.config.glance = i == 1;
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
        let (label, sel) = both_decks("Jog mode", v.decks.map(|d| usize::from(!d.vinyl)));
        if let Some(i) = rows.seg(self, ui, &label, "sjm", &["VINYL", "CDJ"], sel) {
            for d in 0..DECKS {
                self.send(Cmd::Vinyl(d, i == 0));
            }
        }
        let (label, sel) = both_decks("Tempo range", v.decks.map(|d| d.range_idx));
        if let Some(i) = rows.seg(self, ui, &label, "str", &["±6 %", "±10 %", "±16 %", "WIDE"], sel) {
            for d in 0..DECKS {
                self.send(Cmd::TempoRange(d, i));
            }
        }
        let (label, sel) = both_decks("Master Tempo: tempo without pitch", v.decks.map(|d| usize::from(d.keylock)));
        if let Some(i) = rows.seg(self, ui, &label, "smt", &["OFF", "ON"], sel) {
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
        let di = usize::from(self.audio_opts.direct_hints.is_some());
        if let Some(i) = rows.seg(self, ui, "Use the controller's sound card directly", "sdir", &["OFF", "ON"], di) {
            self.config.direct_audio = i == 1;
            self.audio_opts.direct_hints = (i == 1).then(|| crate::midi::device_hints(&crate::midi::builtin(), self.config.midi_port.as_deref()));
            crate::audio::DIRECT_FAILED.store(false, std::sync::atomic::Ordering::Relaxed);
            // Close the stream and hand the card back before reconnecting either way.
            self.audio = None;
            crate::audio::cardprofile::restore_saved();
            self.restart_audio();
        }
        let cp = usize::from(self.audio_opts.cue_profile_hints.is_some());
        if let Some(i) = rows.seg(self, ui, "Otherwise: 4-channel profile (headphone cue)", "scp", &["OFF", "ON"], cp) {
            self.config.cue_profile = i == 1;
            if i == 0 {
                self.audio = None;
                crate::audio::cardprofile::restore_saved();
            }
            self.audio_opts.cue_profile_hints = (i == 1).then(|| crate::midi::device_hints(&crate::midi::builtin(), self.config.midi_port.as_deref()));
            self.restart_audio();
        }
        if let Some(i) = rows.seg(self, ui, "Stereo output: cue in right ear", "ssc", &["OFF", "ON"], usize::from(self.audio_opts.split_cue)) {
            self.audio_opts.split_cue = i == 1;
            self.config.split_cue = i == 1;
            self.restart_audio();
        }
        // Output test: which socket does each output channel reach?
        let ch = self.audio.as_ref().map(|a| a.channels).unwrap_or(0);
        let labels: Vec<&str> = ["1 · MASTER L", "2 · MASTER R", "3 · PHONES L", "4 · PHONES R"].into_iter().take(ch.max(2) as usize).collect();
        if let Some(i) = rows.seg(self, ui, "Output test: beep on one channel", "stest", &labels, usize::MAX) {
            self.send(crate::engine::Cmd::TestTone(i));
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
                p.text(pos2(rows.x, rows.y + 4.0), Align2::LEFT_TOP, "Looking for devices…", t::small(), t::INK_FAINT);
                rows.y += 30.0;
            }
        }
        // The rest of the page, scrolling when there are more devices than fit.
        let list = Rect::from_min_max(pos2(rows.x, rows.y), pos2(rows.x + rows.w.min(720.0), rows.bottom));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(list));
        child.spacing_mut().item_spacing.y = 0.0;
        let mut picked = None;
        egui::ScrollArea::vertical().id_salt("sdevs").auto_shrink([false, false]).show(&mut child, |ui| {
            for (i, (label, dev)) in choices.into_iter().enumerate() {
                let (br, _) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::hover());
                let on = self.audio_opts.device == dev;
                if button(ui, &mut self.touches, br, Id::NULL.with(("sdev", i)), &label, on.then_some(t::ACCENT), t::small()).clicked {
                    picked = Some(dev);
                }
            }
        });
        if let Some(dev) = picked {
            self.audio_opts.device = dev.clone();
            self.config.audio_device = dev;
            self.restart_audio();
        }
    }

    /// Set (Some) or reset to the profile's (None) a control's binding, save it and hand it to
    /// the MIDI thread.
    fn bind_control(&mut self, c: Control, b: Option<Bound>) {
        let bind = &mut self.config.controller.bind;
        let name = c.name();
        match b {
            Some(b) => {
                // The message leaves any other control you bound it to.
                let s = b.to_string();
                for (k, v) in bind.iter_mut() {
                    if *k != name {
                        if let Msgs::Many(list) = v {
                            list.retain(|m| m.parse::<Bound>().map(|x| x.to_string()) != Ok(s.clone()));
                        }
                    }
                }
                bind.retain(|k, v| *k == name || !matches!(v, Msgs::One(m) if m.parse::<Bound>().map(|x| x.to_string()) == Ok(s.clone())));
                bind.retain(|_, v| !matches!(v, Msgs::Many(l) if l.is_empty()));
                bind.insert(name, Msgs::One(s));
            }
            None => {
                bind.remove(&name);
            }
        }
        let mut u = self.midi_user.lock();
        u.0 += 1;
        u.1 = self.config.controller.clone();
    }

    fn page_controller(&mut self, ui: &mut Ui, rows: &mut Rows, inner: Rect) {
        let p = ui.painter().clone();
        let conn = self.midi.lock().clone();
        let name = conn.as_ref().map(|c| c.port.clone()).unwrap_or_else(|| "No DJ controller connected".into());
        rows.text(&p, "Controller", &name);
        if let Some(c) = &conn {
            let mine = c.profile.user.len();
            let profile = match (c.known || self.config.controller.profile.is_some(), mine) {
                (true, 0) => c.profile.name.clone(),
                (true, n) => format!("{}, {n} changed", c.profile.name),
                (false, n) => format!("None: {n} controls bound"),
            };
            rows.text(&p, "Profile", &profile);
        }
        let pad = self.gamepad.as_ref().and_then(|g| g.lock().clone()).unwrap_or_else(|| "Not in use".into());
        rows.text(&p, "Steam Deck controls", &pad);

        // Left: what arrives. Right: what each control is bound to.
        let top = rows.y + 4.0;
        let split = rows.x + (rows.w * 0.42).round();
        heading(&p, pos2(rows.x, top + 4.0), "CONTROLLER TEST");
        let log = crate::midi::MIDI_LOG.lock().clone();
        let log_y = top + 30.0;
        let lines = ((inner.max.y - log_y - 44.0) / 21.0).max(1.0) as usize;
        if log.is_empty() {
            p.text(pos2(rows.x, log_y), Align2::LEFT_TOP, "Nothing received yet.", t::small(), t::INK_FAINT);
        }
        for (i, l) in log.iter().rev().take(lines).enumerate() {
            text_fit(&p, pos2(rows.x, log_y + i as f32 * 21.0), Align2::LEFT_TOP, l, t::mono_small(), if i == 0 { t::INK } else { t::INK_MUTED }, split - rows.x - 16.0);
        }
        let cr = Rect::from_min_size(pos2(rows.x, inner.max.y - 34.0), vec2(90.0, 32.0));
        if button(ui, &mut self.touches, cr, Id::NULL.with("sclr"), "CLEAR", None, t::button_font()).clicked {
            crate::midi::MIDI_LOG.lock().clear();
        }

        let Some(conn) = conn else {
            self.settings.learning = None;
            *crate::midi::LEARN.lock() = None;
            return;
        };
        // MIDI learn: once the control has been pressed or moved, bind it.
        if let Some(c) = self.settings.learning {
            let caught = crate::midi::LEARN.lock().clone().unwrap_or_default();
            if let Some(b) = crate::midi::learned(c, &caught, &conn.profile, std::time::Instant::now()) {
                *crate::midi::LEARN.lock() = None;
                self.settings.learning = None;
                self.bind_control(c, Some(b));
            } else {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
            }
        }
        heading(&p, pos2(split, top + 4.0), "BINDINGS: TAP ONE, THEN PRESS OR MOVE IT ON THE CONTROLLER");
        let list = Rect::from_min_max(pos2(split, top + 30.0), pos2(rows.x + rows.w, inner.max.y));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(list));
        child.spacing_mut().item_spacing.y = 0.0;
        let learning = self.settings.learning;
        let mut tapped = None;
        let mut reset = None;
        egui::ScrollArea::vertical().id_salt("sbind").auto_shrink([false, false]).show(&mut child, |ui| {
            let mut group = String::new();
            for c in Control::all() {
                if c.kind() == crate::midi::Kind::Output {
                    continue;
                }
                let p = ui.painter().clone();
                if c.group() != group {
                    group = c.group();
                    let (hr, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
                    heading(&p, pos2(hr.min.x, hr.min.y + 10.0), &group);
                }
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::hover());
                let mine = conn.profile.user.contains(&c);
                let row = Rect::from_min_max(r.min, pos2(r.max.x - if mine { 96.0 } else { 0.0 }, r.max.y - 2.0));
                let on = learning == Some(c);
                // The label is drawn beside the messages, so name the button for screen readers.
                let (ctl, resp) = button_resp(ui, &mut self.touches, row, Id::NULL.with(("sbind", c.name())), "", on.then_some(t::ACCENT), t::strong());
                let label = format!("{} {}", c.group(), c.label());
                resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, on, &label));
                if ctl.clicked {
                    tapped = Some(c);
                }
                text_fit(&p, pos2(row.min.x + 12.0, row.center().y), Align2::LEFT_CENTER, &c.label(), t::strong(), t::INK, row.width() * 0.5);
                let msgs: Vec<String> = conn.profile.messages(c).iter().map(|b| b.to_string()).collect();
                let (value, col) = if on {
                    ("PRESS OR MOVE IT NOW".to_string(), t::INK)
                } else if msgs.is_empty() {
                    ("—".to_string(), t::INK_FAINT)
                } else {
                    (msgs.join(", "), if mine { t::INK } else { t::INK_MUTED })
                };
                let font = if on { t::button_font() } else { t::mono_small() };
                text_fit(&p, pos2(row.max.x - 12.0, row.center().y), Align2::RIGHT_CENTER, &value, font, col, row.width() * 0.48);
                if mine {
                    let rr = Rect::from_min_max(pos2(row.max.x + 6.0, row.min.y), pos2(r.max.x, row.max.y));
                    if button(ui, &mut self.touches, rr, Id::NULL.with(("sreset", c.name())), "RESET", None, t::button_font()).clicked {
                        reset = Some(c);
                    }
                }
            }
        });
        if let Some(c) = tapped {
            if learning == Some(c) {
                self.settings.learning = None;
                *crate::midi::LEARN.lock() = None;
            } else {
                self.settings.learning = Some(c);
                *crate::midi::LEARN.lock() = Some(Vec::new());
            }
        }
        if let Some(c) = reset {
            self.bind_control(c, None);
        }
    }

    /// Installed copy: where it is, UPDATE from the latest release, UNINSTALL (press twice).
    fn install_rows(&mut self, ui: &mut Ui, rows: &mut Rows) {
        let p = ui.painter().clone();
        if let Some(rx) = &self.settings.update_rx {
            match rx.try_recv() {
                Ok(Ok(())) => {
                    self.settings.update_rx = None;
                    if let Some(t) = self.settings.install_target.clone() {
                        self.settings.install_msg = Some(self.restart_into(&t, Offer::Update));
                    }
                }
                Ok(Err(e)) => {
                    self.settings.update_rx = None;
                    self.settings.install_msg = Some(e);
                }
                Err(_) => ui.ctx().request_repaint_after(std::time::Duration::from_millis(200)),
            }
        }
        if let Some(rx) = &self.settings.uninstall_rx {
            if let Ok(res) = rx.try_recv() {
                self.settings.uninstall_rx = None;
                self.settings.install_msg = Some(match res {
                    Ok(m) | Err(m) => m,
                });
                self.settings.autolaunch = crate::platform::autolaunch::is_installed();
            }
        }
        let Some(target) = self.settings.install_target.clone().filter(|t| t.exists()) else {
            rows.text(&p, "Installed", "No");
            if let Some(m) = &self.settings.install_msg {
                rows.note(&p, m);
            }
            return;
        };
        rows.text(&p, "Installed", &target.display().to_string());
        let busy = self.settings.update_rx.is_some() || self.settings.uninstall_rx.is_some() || self.settings.job_rx.is_some();
        let mut button_row = |app: &mut App, label: &str, id: &str, text: &str, lit: bool| {
            rows.label(&p, label);
            let r = Rect::from_min_size(pos2(rows.x + rows.label_w, rows.y - 2.0), vec2(220.0, 38.0));
            let c = button(ui, &mut app.touches, r, Id::NULL.with(id), text, lit.then_some(t::DANGER), t::button_font()).clicked;
            rows.y += 44.0;
            c
        };
        let updating = self.settings.update_rx.is_some();
        if button_row(self, "Latest release from GitHub", "supd", if updating { "UPDATING…" } else { "UPDATE" }, false) && !busy {
            let (tx, rx) = crossbeam_channel::bounded(1);
            let t = target.clone();
            std::thread::spawn(move || {
                let _ = tx.send(install::update(&t));
            });
            self.settings.update_rx = Some(rx);
            self.settings.install_msg = Some("Downloading the latest release…".into());
        }
        let armed = self.settings.uninstall_confirm.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(3));
        if button_row(self, "Uninstall (settings, cues and recordings stay)", "suni", if armed { "PRESS AGAIN" } else { "UNINSTALL" }, armed) && !busy {
            if armed {
                self.settings.uninstall_confirm = None;
                if [Offer::Install, Offer::Update].iter().any(|&o| self.banner.as_deref() == Some(&install::note(o, &target))) {
                    self.banner = None;
                }
                let (tx, rx) = crossbeam_channel::bounded(1);
                std::thread::spawn(move || {
                    let _ = tx.send(install::uninstall(&target));
                });
                self.settings.uninstall_rx = Some(rx);
                self.settings.install_msg = Some("Working…".into());
            } else {
                self.settings.uninstall_confirm = Some(std::time::Instant::now());
            }
        }
        if armed {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
        }
        if let Some(m) = &self.settings.install_msg {
            rows.note(&p, m);
        }
    }

    fn page_system(&mut self, ui: &mut Ui, rows: &mut Rows) {
        self.install_rows(ui, rows);
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
            rows.note(&p, m);
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
        rows.text(&p, "Version", &install::version());
    }
}

/// A row that sets both decks: the shared choice, or nothing lit when the decks differ.
fn both_decks(name: &str, per_deck: [usize; DECKS]) -> (String, usize) {
    if per_deck.iter().all(|&s| s == per_deck[0]) {
        (format!("{name} (both decks)"), per_deck[0])
    } else {
        (format!("{name} (decks differ)"), usize::MAX)
    }
}

/// Lays out labelled settings rows top to bottom.
struct Rows {
    y: f32,
    x: f32,
    w: f32,
    bottom: f32,
    label_w: f32,
}

impl Rows {
    fn label(&self, p: &egui::Painter, s: &str) {
        text_fit(p, pos2(self.x, self.y + 17.0), Align2::LEFT_CENTER, s, t::body(), t::INK, self.label_w - 12.0);
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
        text_fit(p, pos2(self.x + self.label_w, self.y + 17.0), Align2::LEFT_CENTER, value, t::small(), t::INK_MUTED, self.w - self.label_w);
        self.y += 36.0;
    }

    /// A message under the row above it, in the value column.
    fn note(&mut self, p: &egui::Painter, m: &str) {
        let g = p.layout(m.to_string(), t::small(), t::INK_MUTED, self.w - self.label_w);
        let h = g.size().y;
        p.galley(pos2(self.x + self.label_w, self.y), g, t::INK_MUTED);
        self.y += h + 10.0;
    }
}
