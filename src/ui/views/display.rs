//! PERFORM as a display, for playing from a DJ controller. The controller has the deck
//! buttons, jogs, mixer and Beat FX, so the screen does what a CDJ's does: big waveforms,
//! big readouts, deck status and the track list, with large targets for the few things only
//! the screen can do (load, tag, waveform zoom and colour, time mode). Without a controller
//! PERFORM shows the full touch layout instead.

use crate::ui::prelude::*;
use crate::ui::HOT_LETTERS;
use crate::ui::App;

use crate::config::PerformLayout;
use crate::engine::{DeckView, EngineView, DECKS};

impl App {
    /// Whether PERFORM shows the display layout now.
    pub(crate) fn display_layout(&self) -> bool {
        match self.config.perform_layout {
            PerformLayout::Auto => self.midi.lock().is_some(),
            PerformLayout::Full => false,
            PerformLayout::Display => true,
        }
    }

    pub(crate) fn perform_display(&mut self, ui: &mut Ui, full: Rect, v: &EngineView) {
        let info_h = sz(206.0, 226.0);
        let wave_h = (full.height() * 0.29).clamp(140.0, 260.0);
        let waves = Rect::from_min_size(full.min, vec2(full.width(), wave_h));
        for d in 0..DECKS {
            let h = wave_h / 2.0;
            let r = Rect::from_min_size(pos2(waves.min.x, waves.min.y + d as f32 * h), vec2(waves.width(), h));
            self.waveform(ui, r.shrink2(vec2(0.0, 0.5)), d, &v.decks[d]);
        }
        self.zoom_buttons(ui, waves, true);

        let row = Rect::from_min_size(pos2(full.min.x, waves.max.y), vec2(full.width(), info_h));
        let centre_w = 132.0;
        let deck_w = (row.width() - centre_w) / 2.0;
        let d0 = Rect::from_min_size(row.min, vec2(deck_w, info_h));
        let c = Rect::from_min_size(pos2(d0.max.x, row.min.y), vec2(centre_w, info_h));
        let d1 = Rect::from_min_size(pos2(c.max.x, row.min.y), vec2(deck_w, info_h));
        self.display_deck(ui, d0, 0, &v.decks[0]);
        self.display_centre(ui, c, v);
        self.display_deck(ui, d1, 1, &v.decks[1]);

        let list = Rect::from_min_max(pos2(full.min.x, row.max.y), full.max);
        self.browser(ui, list, true);
    }

    /// One deck's status: title, time, tempo, BPM, overview, state and hot cues.
    fn display_deck(&mut self, ui: &Ui, r: Rect, d: usize, dv: &DeckView) {
        let p = ui.painter().clone();
        card(&p, r);
        let inner = r.shrink2(vec2(14.0, 10.0));
        let tint = t::DECK_TINT[d];
        let track = self.decks[d].track.clone().filter(|_| dv.loaded);
        let sr = dv.sample_rate.max(1) as f64;
        let (elapsed, remain) = (dv.pos / sr, ((dv.length as f64 - dv.pos) / sr).max(0.0));

        // Screen readers get the whole readout as one label.
        {
            let title = track.as_ref().map(|t| format!("{} by {}", t.title, t.artist)).unwrap_or_else(|| "empty".into());
            let bpm = dv.bpm.map(|b| format!(", {b:.2} BPM")).unwrap_or_default();
            let state = if dv.playing { "playing" } else if dv.loaded { "paused" } else { "" };
            let desc = format!("Deck {}: {title}, {state}, {} remaining, tempo {:+.2}%{bpm}", d + 1, fmt_time(remain), (dv.rate - 1.0) * 100.0);
            ui.interact(r, Id::NULL.with(("ddeck", d)), Sense::hover()).widget_info(|| WidgetInfo::labeled(egui::Role::Label, true, &desc));
        }

        // Row 1: badge, artwork, title and artist.
        let y = inner.min.y;
        let badge = pos2(inner.min.x + 16.0, y + 21.0);
        p.rect_filled(Rect::from_center_size(badge, vec2(32.0, 32.0)), CornerRadius::ZERO, tint.gamma_multiply(0.25));
        p.text(badge, Align2::CENTER_CENTER, format!("{}", d + 1), semi(19.0), tint);
        let mut tx = inner.min.x + 42.0;
        if let (Some(key), true) = (self.decks[d].art.clone(), track.is_some()) {
            if let Some(tex) = self.art.get(&key) {
                let ar = Rect::from_min_size(pos2(tx, y), vec2(42.0, 42.0));
                p.image(tex.id(), ar, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                tx += 50.0;
            }
        }
        let title_w = inner.max.x - tx;
        let du = &self.decks[d];
        let (line1, line2, c1, c2) = if let Some(name) = &du.loading {
            (format!("Loading {name}…"), "Opening the file".to_string(), t::INK_MUTED, t::INK_FAINT)
        } else if let Some(err) = &du.error {
            ("Couldn't load that file".to_string(), err.clone(), t::DANGER, t::INK_MUTED)
        } else if let Some(tr) = &track {
            let artist = if tr.artist.is_empty() { "Unknown artist".to_string() } else { tr.artist.clone() };
            (tr.title.clone(), artist, t::INK, t::INK_MUTED)
        } else {
            ("Empty deck".to_string(), format!("Pick a track below and press LOAD {} (or LOAD on the controller)", d + 1), t::INK_MUTED, t::INK_FAINT)
        };
        text_fit(&p, pos2(tx, y - 1.0), Align2::LEFT_TOP, &line1, semi(sz(21.0, 25.0)), c1, title_w);
        text_fit(&p, pos2(tx, y + sz(24.0, 29.0)), Align2::LEFT_TOP, &line2, t::body(), c2, title_w);

        // Row 2: time | tempo | BPM, sized to the card.
        let y2 = y + sz(52.0, 56.0);
        let big = (inner.width() / 480.0 * sz(34.0, 42.0)).clamp(26.0, sz(34.0, 42.0));
        let remain_mode = self.config.time_remain;
        p.text(pos2(inner.min.x, y2), Align2::LEFT_TOP, if remain_mode { "REMAIN" } else { "ELAPSED" }, caption(), t::INK_FAINT);
        let low_time = dv.loaded && remain < 30.0 && dv.playing && dv.complete;
        let time_col = if !dv.loaded {
            t::INK_FAINT
        } else if low_time && (ui.input(|i| i.time) * 2.0) as i64 % 2 == 0 {
            t::DANGER
        } else {
            t::INK
        };
        let time = if remain_mode { format!("-{}", fmt_time(remain)) } else { fmt_time(elapsed) };
        let tr = p.text(pos2(inner.min.x - 1.0, y2 + 14.0), Align2::LEFT_TOP, time, mono(big), time_col);
        let time_r = Rect::from_min_max(pos2(inner.min.x, y2 - 2.0), pos2(tr.max.x + 8.0, tr.max.y + 2.0));
        let tip = if remain_mode { "Showing remaining time. Tap for elapsed" } else { "Showing elapsed time. Tap for remaining" };
        if invisible_button(ui, &mut self.touches, time_r, Id::NULL.with(("dtime", d)), "Time mode", tip).clicked {
            self.config.time_remain = !remain_mode;
        }
        let tx2 = tr.max.x + (inner.width() * 0.05).max(14.0);
        let range = if dv.range >= 1.0 { "WIDE".to_string() } else { format!("±{:.0}%", dv.range * 100.0) };
        p.text(pos2(tx2, y2), Align2::LEFT_TOP, format!("TEMPO {range}"), caption(), t::INK_FAINT);
        let pct = (dv.rate - 1.0) * 100.0;
        p.text(pos2(tx2, y2 + 20.0), Align2::LEFT_TOP, format!("{pct:+.2}%"), mono(big * 0.62), if pct.abs() < 0.005 { t::INK_MUTED } else { t::INK });
        let (bpm_cap, bpm_cap_col) = if dv.is_master { ("BPM · MASTER", t::ACCENT) } else { ("BPM", t::INK_FAINT) };
        p.text(pos2(inner.max.x, y2), Align2::RIGHT_TOP, bpm_cap, caption(), bpm_cap_col);
        let bpm = dv.bpm.map(|b| format!("{b:.2}")).unwrap_or_else(|| if dv.loaded && !dv.complete { "…".into() } else { "--.--".into() });
        p.text(pos2(inner.max.x + 1.0, y2 + 14.0), Align2::RIGHT_TOP, bpm, mono(big), if dv.synced { t::SYNC } else if dv.loaded { t::INK } else { t::INK_FAINT });

        // Row 3: overview (tap to seek).
        let y3 = y2 + sz(58.0, 70.0);
        let ov = Rect::from_min_size(pos2(inner.min.x, y3), vec2(inner.width(), 34.0));
        self.overview(ui, &p, ov, d, dv);

        // Row 4: state chips, then the hot cues A–H (set ones in their colour).
        let y4 = ov.max.y + 10.0;
        let chip_h = sz(24.0, 28.0);
        let pad_s = sz(22.0, 26.0);
        let pads_w = 8.0 * pad_s;
        let pads_x = inner.max.x - pads_w;
        let mut x = inner.min.x;
        let key = track.as_ref().map(|t| t.key.clone()).filter(|k| !k.is_empty());
        let countdown = dv
            .playing
            .then(|| next_cue(self, d, dv))
            .flatten()
            .filter(|(b, _)| *b <= 64.0)
            .map(|(b, l)| (format!("{} › {l}", bars_beats(b)), if b <= 16.0 { t::CUE } else { t::INK_MUTED }));
        let state = if dv.playing {
            Some(("PLAYING".to_string(), t::PLAY))
        } else if dv.loaded && dv.at_cue {
            Some(("AT CUE".to_string(), t::CUE))
        } else if dv.loaded {
            Some(("PAUSED".to_string(), t::CUE))
        } else {
            None
        };
        let chips: Vec<(String, Color32)> = [
            state,
            key.map(|k| key_label(self, d, &k)),
            dv.loop_active.then(|| ("LOOP".to_string(), t::LOOP)),
            dv.synced.then(|| ("SYNC".to_string(), t::SYNC)),
            dv.keylock.then(|| ("MT".to_string(), t::ACCENT)),
            dv.slip.then(|| ("SLIP".to_string(), t::SYNC)),
            dv.reverse.then(|| ("REV".to_string(), t::DANGER)),
            countdown,
        ]
        .into_iter()
        .flatten()
        .collect();
        for (label, col) in chips {
            let w = p.layout_no_wrap(label.clone(), caption(), t::INK).size().x + 16.0;
            if x + w > pads_x - 8.0 {
                break;
            }
            let cr = Rect::from_min_size(pos2(x, y4), vec2(w, chip_h));
            tile(&p, cr, col.gamma_multiply(0.18));
            p.text(cr.center(), Align2::CENTER_CENTER, label, caption(), mix(col, t::INK, 0.4));
            x += w;
        }
        for i in 0..crate::engine::deck::HOT_CUES {
            let pr = Rect::from_min_size(pos2(pads_x + i as f32 * pad_s, y4 + (chip_h - pad_s) / 2.0), vec2(pad_s, pad_s));
            let set = dv.hot[i].is_some();
            let col = hot_color(i, dv.hot[i].and_then(|h| h.color));
            tile(&p, pr, if set { col.gamma_multiply(0.35) } else { t::INSET });
            p.text(pr.center(), Align2::CENTER_CENTER, HOT_LETTERS[i], semi(sz(12.0, 14.0)), if set { t::INK } else { t::INK_FAINT });
        }
    }

    /// Between the decks: phase, channel and master meters, crossfader and Beat FX.
    fn display_centre(&mut self, ui: &Ui, r: Rect, v: &EngineView) {
        let p = ui.painter().clone();
        card(&p, r);
        let inner = r.shrink2(vec2(10.0, 10.0));
        // Phase: where each deck is in the bar, and the beat offset between them.
        p.text(pos2(inner.center().x, inner.min.y), Align2::CENTER_TOP, "PHASE", caption(), t::INK_FAINT);
        let seg_w = (inner.width() - 3.0 * 3.0) / 4.0;
        let mut fr: [Option<f64>; DECKS] = [None; DECKS];
        for d in 0..DECKS {
            let y = inner.min.y + 18.0 + d as f32 * 12.0;
            let pos = bar_position(self, d, &v.decks[d]).filter(|_| v.decks[d].loaded);
            for b in 0..4 {
                let sr = Rect::from_min_size(pos2(inner.min.x + b as f32 * (seg_w + 3.0), y), vec2(seg_w, 9.0));
                let lit = pos.is_some_and(|(k, _)| k == b);
                let col = if b == 0 { t::DECK_TINT[d] } else { mix(t::DECK_TINT[d], t::INK, 0.4) };
                p.rect_filled(sr, CornerRadius::ZERO, if lit { col.gamma_multiply(0.85) } else { t::INSET });
            }
            fr[d] = pos.map(|(k, f)| k as f64 + f);
        }
        let (txt, col) = match (fr[0], fr[1]) {
            (Some(a), Some(b)) => {
                let mut diff = (b - a).rem_euclid(1.0);
                if diff > 0.5 {
                    diff -= 1.0;
                }
                if diff.abs() < 0.02 {
                    ("IN PHASE".to_string(), t::PLAY)
                } else {
                    (format!("{diff:+.2} beat"), if diff.abs() < 0.08 { t::CUE } else { t::DANGER })
                }
            }
            _ => ("–".to_string(), t::INK_FAINT),
        };
        p.text(pos2(inner.center().x, inner.min.y + 46.0), Align2::CENTER_TOP, txt, t::body(), col);

        // Meters: channel 1, master, channel 2.
        let my0 = inner.min.y + 74.0;
        let my1 = inner.max.y - 50.0;
        let mw = 12.0;
        let cx = inner.center().x;
        for (i, (x, level, label, col)) in [
            (cx - 34.0, v.decks[0].peak * v.mixer.ch[0].fader, "1", t::DECK_TINT[0]),
            (cx, v.peak_master, "M", t::INK_MUTED),
            (cx + 34.0, v.decks[1].peak * v.mixer.ch[1].fader, "2", t::DECK_TINT[1]),
        ]
        .into_iter()
        .enumerate()
        {
            let w = if i == 1 { mw + 4.0 } else { mw };
            meter(&p, Rect::from_min_max(pos2(x - w / 2.0, my0), pos2(x + w / 2.0, my1)), level);
            p.text(pos2(x, my1 + 4.0), Align2::CENTER_TOP, label, semi(13.0), col);
        }
        // Crossfader position.
        let xr = Rect::from_min_size(pos2(inner.min.x + 6.0, inner.max.y - 26.0), vec2(inner.width() - 12.0, 4.0));
        p.rect_filled(xr, 0, t::INSET);
        let kx = xr.min.x + v.mixer.crossfader * xr.width();
        p.rect_filled(Rect::from_center_size(pos2(kx, xr.center().y), vec2(6.0, 12.0)), 0, t::INK_MUTED);
        // Beat FX in one line: effect, beat, where, lit red while on.
        let fx = v.mixer.fx;
        let target = match fx.target {
            crate::engine::fx::FxTarget::Ch(c) => format!("CH{}", c + 1),
            crate::engine::fx::FxTarget::Master => "MST".into(),
        };
        let beat = if fx.kind == crate::engine::fx::FxKind::Reverb { format!("{}%", 10 + fx.beat_idx * 11) } else { crate::engine::fx::beats_label(fx.beats()) };
        let line = format!("{} {beat} {target}", fx.kind.name());
        text_fit(&p, pos2(inner.center().x, inner.max.y), Align2::CENTER_BOTTOM, &line, caption(), if fx.on { t::DANGER } else { t::INK_FAINT }, inner.width());
    }
}
