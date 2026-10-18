//! The top bar (icon, name, audio and controller status, REC, QUANTIZE, DSP load, master
//! level) and the banner under it.

use crate::ui::prelude::*;
use std::sync::atomic::Ordering;
use crate::audio;
use crate::ui::App;

impl App {
    /// The top bar: name, audio and controller status, REC, QUANTIZE, DSP load, master level.
    /// `x0` is where the body starts (right of the rail), so the status lines up with it.
    pub(crate) fn top_bar(&mut self, ui: &Ui, r: Rect, v: &EngineView, x0: f32) {
        let p = ui.painter().clone();
        // The app icon, then the name.
        let logo = self.logo.get_or_insert_with(|| {
            let icon = crate::platform::desktop::window_icon();
            let img = egui::ColorImage::from_rgba_unmultiplied([icon.width as usize, icon.height as usize], &icon.rgba);
            ui.ctx().load_texture("cdeckj-logo", img, egui::TextureOptions::LINEAR)
        });
        let ls = 26.0;
        let lr = Rect::from_min_size(pos2(r.min.x + 10.0, r.center().y - ls / 2.0), vec2(ls, ls));
        p.image(logo.id(), lr, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        let name = p.text(pos2(lr.max.x + 7.0, r.center().y), Align2::LEFT_CENTER, "CDeckJ", semi(18.0), t::INK);
        let right_w = 536.0;
        let x = (x0 + 8.0).max(name.max.x + 16.0);
        // Compact audio and controller status; tap for the full story.
        let status_icon = |x: f32, icon: &str, ok: bool| {
            p.text(pos2(x + 5.0, r.center().y), Align2::CENTER_CENTER, icon, cond(16.0), if ok { t::PLAY } else { t::DANGER });
        };
        let midi = self.midi.lock().clone();
        let pad = self.gamepad.as_ref().and_then(|g| g.lock().clone());
        let (audio_ok, audio_short, audio_long) = match (&self.audio, &self.audio_error) {
            (Some(a), _) => {
                let routing = match v.routing {
                    crate::engine::CueRouting::Channels34 => "phones on 3/4",
                    crate::engine::CueRouting::SplitMono => "split cue L/R",
                    crate::engine::CueRouting::MasterOnly => "no cue output",
                };
                let dev = a.description.split(" · ").last().unwrap_or(&a.description).to_string();
                (true, format!("{dev} · {routing}"), format!("Audio: {}  ·  {} Hz  ·  {routing}", a.description, v.sample_rate))
            }
            (None, e) => {
                let s = format!("No audio — {}. Retrying…", e.as_deref().unwrap_or("not started").trim_end_matches('.'));
                (false, "No audio".to_string(), s)
            }
        };
        let ctl_long = match (&midi, &pad) {
            (Some(m), Some(g)) => format!("Controller: {m}  ·  {g}"),
            (Some(m), None) => format!("Controller: {m}"),
            (None, Some(g)) => format!("Controller: {g}  ·  no DJ controller"),
            (None, None) => "No controller connected: touch & keyboard".into(),
        };
        let ctl_short = match &midi {
            Some(m) => m.split(':').next().unwrap_or(m).trim().to_string(),
            None if pad.is_some() => "Steam Deck controls".into(),
            None => "No controller".into(),
        };
        let avail = (r.max.x - right_w - x - 12.0).max(60.0);
        let half = avail / 2.0;
        status_icon(x, if audio_ok { icons::SPEAKER } else { icons::SPEAKER_OFF }, audio_ok);
        text_fit(&p, pos2(x + 18.0, r.center().y), Align2::LEFT_CENTER, &audio_short, cond(14.0), if audio_ok { t::INK_MUTED } else { t::DANGER }, half - 26.0);
        let ctl_icon = if midi.is_none() && pad.is_some() { icons::GAMEPAD } else { icons::CONTROLLER };
        status_icon(x + half, ctl_icon, midi.is_some() || pad.is_some());
        text_fit(&p, pos2(x + half + 18.0, r.center().y), Align2::LEFT_CENTER, &ctl_short, cond(14.0), t::INK_MUTED, half - 26.0);
        let sr = Rect::from_min_size(pos2(x - 4.0, r.min.y + 2.0), vec2(avail + 4.0, r.height() - 4.0));
        if invisible_button(ui, &mut self.touches, sr, Id::NULL.with("status"), "Audio and controller status", "Tap for the full audio and controller details").clicked {
            self.banner = Some(format!("{audio_long}\n{ctl_long}"));
        }

        // Right: REC, QUANTIZE, DSP load, master meter. (EXIT is at the foot of the rail.)
        let m = Rect::from_min_size(pos2(r.max.x - 118.0, r.center().y - 4.0), vec2(106.0, 8.0));
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
        let bh = r.height() - 8.0;
        self.rec_button(ui, &p, Rect::from_min_size(pos2(r.max.x - right_w + 6.0, r.min.y + 4.0), vec2(104.0, bh)));
        let qr = Rect::from_min_size(pos2(r.max.x - right_w + 118.0, r.min.y + 4.0), vec2(118.0, bh));
        let qtip = if v.quantize { "Quantize on: cues, loops and jumps snap to the beat  ( G )" } else { "Quantize off  ( G )" };
        if button_tip(ui, &mut self.touches, qr, Id::NULL.with("quantize"), &format!("{}  QUANTIZE", icons::MAGNET), v.quantize.then_some(t::ACCENT), semi(14.0), qtip).clicked {
            self.send(Cmd::QuantizeToggle);
        }
    }

    pub(crate) fn rec_button(&mut self, ui: &Ui, p: &Painter, r: Rect) {
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
            "Record the mix to Music/CDeckJ Recordings, with a tracklist  ( R )"
        };
        let font = if rec && !confirm { mono(14.0) } else { semi(14.0) };
        if button_tip(ui, &mut self.touches, r, Id::NULL.with("rec"), &label, rec.then_some(t::DANGER), font, tip).clicked {
            self.record_toggle();
        }
        if !confirm {
            // Solid while armed or on the blink, outline in between.
            let blink = (ui.input(|i| i.time) * 2.0) as i64 % 2 == 0;
            let c = pos2(r.min.x + 19.0, r.center().y);
            let font = if !rec || blink { icons::fill(16.0) } else { cond(16.0) };
            p.text(c, Align2::CENTER_CENTER, icons::RECORD, font, t::DANGER);
        }
    }

    pub(crate) fn banner_row(&mut self, ui: &Ui, r: Rect) {
        let p = ui.painter().clone();
        p.rect_filled(r, 0, t::ACCENT.gamma_multiply(0.16));
        if let Some(b) = &self.banner {
            let g = p.layout(b.clone(), cond(15.0), t::INK, r.width() - 140.0);
            p.galley(pos2(r.min.x + 14.0, r.center().y - g.size().y / 2.0), g, t::INK);
        }
        let br = Rect::from_min_size(pos2(r.max.x - 104.0, r.min.y + 3.0), vec2(96.0, 24.0));
        if button(ui, &mut self.touches, br, Id::NULL.with("banner"), &format!("{}  DISMISS", icons::X), None, semi(13.0)).clicked {
            self.banner = None;
        }
    }
}
