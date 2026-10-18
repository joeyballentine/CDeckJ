//! A full on-screen deck for playing by touch: header, readouts, overview, loop and deck
//! buttons, jog wheel, CUE / PLAY, hot cue pads and the tempo fader.

use crate::ui::prelude::*;
use crate::engine::deck::HOT_CUES;
use crate::track::GridSource;
use crate::ui::actions::preset_comment;
use crate::ui::{HOT_LETTERS, JOG_TICKS_PER_REV};
use crate::ui::App;

impl App {
    pub(crate) fn deck_panel(&mut self, ui: &Ui, r: Rect, d: usize, dv: &DeckView) {
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
        let key_col = key_colour(self, d);
        // Countdown to the next cue while playing (like a CDJ's beat countdown).
        let countdown = if dv.playing {
            next_cue(self, d, dv).filter(|(b, _)| *b <= 64.0).map(|(b, l)| (format!("{}›{}", bars_beats(b), if l == "MEMORY" { "M".to_string() } else if l == "CUE" { "C".to_string() } else { l }), if b <= 16.0 { t::CUE } else { t::INK_MUTED }))
        } else {
            None
        };
        let chips: Vec<(String, Color32)> = [
            countdown,
            key.map(|k| (k, key_col)),
            dv.slip.then(|| ("SLIP".to_string(), t::SYNC)),
            dv.reverse.then(|| ("REV".to_string(), t::DANGER)),
            dv.keylock.then(|| ("MT".to_string(), t::ACCENT)),
            dv.synced.then(|| ("SYNC".to_string(), t::SYNC)),
            dv.is_master.then(|| ("MASTER".to_string(), t::ACCENT)),
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
        let (jump_l, jump_r) = (format!("{} {jb}", icons::CARET_DOUBLE_LEFT), format!("{jb} {}", icons::CARET_DOUBLE_RIGHT));
        let (call_l, call_r) = (format!("{} CALL", icons::CARET_LEFT), format!("CALL {}", icons::CARET_RIGHT));
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
                ("MT", dv.keylock.then_some(t::ACCENT), 12),
                ("SLIP", dv.slip.then_some(t::SYNC), 13),
                ("REV", dv.reverse.then_some(t::DANGER), 14),
                (call_l.as_str(), None, 15),
                (call_r.as_str(), None, 16),
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
        let cc = round_button(ui, &mut self.touches, cue_r, Id::NULL.with(("cue", d)), "CUE", None, t::CUE, cue_lit);
        if cc.down != self.decks[d].cue_held {
            self.decks[d].cue_held = cc.down;
            self.send(Cmd::Cue(d, cc.down));
        } else if cc.key {
            self.send(Cmd::Cue(d, true));
            self.send(Cmd::Cue(d, false));
        }
        let pc = round_button(ui, &mut self.touches, play_r, Id::NULL.with(("play", d)), "PLAY", Some(icons::PLAY_PAUSE), t::PLAY, play_lit);
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

    pub(crate) fn jog(&mut self, ui: &Ui, p: &Painter, r: Rect, d: usize, dv: &DeckView) {
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
}
