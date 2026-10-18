//! WAVE view: both decks' waveforms large, with the overview, a phase meter between the
//! decks, a countdown to the next cue and big readouts — for beatmatching and phrasing on
//! a small screen. The controller does the rest; only transport and jumps are on screen.

use crate::ui::prelude::*;
use crate::ui::App;

use crate::engine::{Cmd, DeckView, EngineView, DECKS};

impl App {
    pub(crate) fn wave_view(&mut self, ui: &mut Ui, r: Rect, v: &EngineView) {
        let head_h = 50.0;
        let ov_h = 40.0;
        let ctl_h = 42.0;
        let phase_h = 44.0;
        let gap = 6.0;
        let wave_h = ((r.height() - phase_h - 2.0 * (head_h + ov_h + ctl_h + 4.0 * gap)) / 2.0).max(80.0);
        let deck_h = head_h + wave_h + ov_h + ctl_h + 3.0 * gap;
        for d in 0..DECKS {
            let y0 = if d == 0 { r.min.y } else { r.min.y + deck_h + phase_h + 2.0 * gap };
            let dr = Rect::from_min_size(pos2(r.min.x, y0), vec2(r.width(), deck_h));
            self.wave_deck(ui, dr, d, &v.decks[d], head_h, wave_h, ov_h, ctl_h, gap);
        }
        let pr = Rect::from_min_size(pos2(r.min.x, r.min.y + deck_h + gap), vec2(r.width(), phase_h));
        self.phase_meter(ui, pr, v);
    }

    #[allow(clippy::too_many_arguments)]
    fn wave_deck(&mut self, ui: &mut Ui, r: Rect, d: usize, dv: &DeckView, head_h: f32, wave_h: f32, ov_h: f32, ctl_h: f32, gap: f32) {
        let p = ui.painter().clone();
        let tint = t::DECK_TINT[d];
        // Header: badge, artwork, title | key, next cue, tempo, time, BPM.
        let hr = Rect::from_min_size(r.min, vec2(r.width(), head_h));
        card(&p, hr);
        let inner = hr.shrink2(vec2(10.0, 5.0));
        let badge = pos2(inner.min.x + 13.0, inner.center().y);
        p.circle_filled(badge, 13.0, tint.gamma_multiply(0.18));
        p.circle_stroke(badge, 13.0, Stroke::new(1.0, tint));
        p.text(badge, Align2::CENTER_CENTER, format!("{}", d + 1), semi(16.0), tint);
        let mut x = inner.min.x + 34.0;
        let track = self.decks[d].track.clone().filter(|_| dv.loaded);
        if let (Some(key), true) = (self.decks[d].art.clone(), track.is_some()) {
            if let Some(tex) = self.art.get(&key) {
                let ar = Rect::from_min_size(pos2(x, inner.min.y), vec2(inner.height(), inner.height()));
                p.image(tex.id(), ar, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                x += inner.height() + 8.0;
            }
        }
        let right_w = 640.0;
        let title_w = (inner.max.x - right_w - x).max(80.0);
        match &track {
            Some(tr) => {
                text_fit(&p, pos2(x, inner.min.y - 1.0), Align2::LEFT_TOP, &tr.title, semi(19.0), t::INK, title_w);
                text_fit(&p, pos2(x, inner.min.y + 22.0), Align2::LEFT_TOP, &tr.artist, cond(15.0), t::INK_MUTED, title_w);
            }
            None => {
                p.text(pos2(x, inner.center().y), Align2::LEFT_CENTER, "Empty deck", semi(19.0), t::INK_FAINT);
            }
        }
        // Readouts from the right edge inwards.
        let cap_y = inner.min.y;
        let val_y = inner.max.y;
        let mut rx = inner.max.x;
        let bpm = dv.bpm.map(|b| format!("{b:.2}")).unwrap_or_else(|| "--.--".into());
        p.text(pos2(rx, val_y + 2.0), Align2::RIGHT_BOTTOM, bpm, mono(26.0), if dv.synced { t::SYNC } else { t::INK });
        p.text(pos2(rx, cap_y), Align2::RIGHT_TOP, if dv.is_master { "BPM · MASTER" } else { "BPM" }, caption(), if dv.is_master { t::ACCENT } else { t::INK_FAINT });
        rx -= 130.0;
        let sr = dv.sample_rate.max(1) as f64;
        let (elapsed, remain) = (dv.pos / sr, (dv.length as f64 - dv.pos) / sr);
        let big = if self.config.time_remain { format!("-{}", fmt_time(remain.max(0.0))) } else { fmt_time(elapsed) };
        p.text(pos2(rx, val_y + 2.0), Align2::RIGHT_BOTTOM, big, mono(26.0), if dv.loaded { t::INK } else { t::INK_FAINT });
        p.text(pos2(rx, cap_y), Align2::RIGHT_TOP, if self.config.time_remain { "REMAIN" } else { "ELAPSED" }, caption(), t::INK_FAINT);
        rx -= 170.0;
        let pct = (dv.rate - 1.0) * 100.0;
        p.text(pos2(rx, val_y), Align2::RIGHT_BOTTOM, format!("{pct:+.2}%"), mono(18.0), if pct.abs() < 0.005 { t::INK_MUTED } else { t::INK });
        p.text(pos2(rx, cap_y), Align2::RIGHT_TOP, "TEMPO", caption(), t::INK_FAINT);
        rx -= 112.0;
        match next_cue(self, d, dv) {
            Some((beats, label)) => {
                let warn = beats <= 16.0 && dv.playing;
                p.text(pos2(rx, val_y), Align2::RIGHT_BOTTOM, bars_beats(beats), mono(18.0), if warn { t::CUE } else { t::INK });
                p.text(pos2(rx, cap_y), Align2::RIGHT_TOP, format!("BARS TO {label}"), caption(), t::INK_FAINT);
            }
            None => {
                p.text(pos2(rx, cap_y), Align2::RIGHT_TOP, "NEXT CUE", caption(), t::INK_FAINT);
                p.text(pos2(rx, val_y), Align2::RIGHT_BOTTOM, "–", mono(18.0), t::INK_FAINT);
            }
        }
        rx -= 104.0;
        if let Some(tr) = &track {
            if !tr.key.is_empty() {
                p.text(pos2(rx, val_y), Align2::RIGHT_BOTTOM, &tr.key, mono(18.0), key_colour(self, d));
                p.text(pos2(rx, cap_y), Align2::RIGHT_TOP, "KEY", caption(), t::INK_FAINT);
            }
        }

        // Big scrolling waveform and the overview.
        let wr = Rect::from_min_size(pos2(r.min.x, hr.max.y + gap), vec2(r.width(), wave_h));
        p.rect_filled(wr, CornerRadius::same(t::R_CARD), t::INSET);
        self.waveform(ui, wr.shrink2(vec2(0.0, 1.0)), d, dv);
        let or = Rect::from_min_size(pos2(r.min.x, wr.max.y + gap), vec2(r.width(), ov_h));
        self.overview(ui, &p, or, d, dv);

        // Controls: CUE, PLAY, beat jump, loops, SYNC.
        let cr = Rect::from_min_size(pos2(r.min.x, or.max.y + gap), vec2(r.width(), ctl_h));
        let h = ctl_h;
        let mut x = cr.min.x;
        let cue_r = Rect::from_min_size(pos2(x, cr.min.y), vec2(h, h));
        let blink = (ui.input(|i| i.time) * 2.0) as i64 % 2 == 0;
        let cc = round_button(ui, &mut self.touches, cue_r, Id::NULL.with(("wcue", d)), "CUE", None, t::CUE, dv.loaded && (dv.playing || dv.at_cue || blink));
        if cc.down != self.decks[d].cue_held {
            self.decks[d].cue_held = cc.down;
            self.send(Cmd::Cue(d, cc.down));
        } else if cc.key {
            self.send(Cmd::Cue(d, true));
            self.send(Cmd::Cue(d, false));
        }
        x += h + 8.0;
        let play_r = Rect::from_min_size(pos2(x, cr.min.y), vec2(h, h));
        let pc = round_button(ui, &mut self.touches, play_r, Id::NULL.with(("wplay", d)), "PLAY", Some(crate::ui::icons::PLAY_PAUSE), t::PLAY, dv.playing || (dv.loaded && blink));
        if (pc.down && !self.decks[d].play_held) || pc.key {
            self.send(Cmd::Play(d));
        }
        self.decks[d].play_held = pc.down;
        x += h + 16.0;
        let jump = self.config.beat_jump;
        let jb = crate::rekordbox::settings::fmt_beats(jump);
        let btns: [(String, Option<Color32>, Cmd, &str); 7] = [
            (format!("{} {jb}", crate::ui::icons::CARET_DOUBLE_LEFT), None, Cmd::BeatJump(d, -jump), "Jump back"),
            (format!("{jb} {}", crate::ui::icons::CARET_DOUBLE_RIGHT), None, Cmd::BeatJump(d, jump), "Jump forward"),
            ("4 BEAT".into(), None, Cmd::AutoLoop(d, 4.0), "4-beat loop"),
            ("½×".into(), None, Cmd::LoopScale(d, 0.5), "Halve the loop"),
            ("2×".into(), None, Cmd::LoopScale(d, 2.0), "Double the loop"),
            (if dv.loop_active { "EXIT".into() } else { "RELOOP".into() }, dv.loop_active.then_some(t::LOOP), Cmd::ReloopExit(d), "Reloop / exit loop"),
            ("SYNC".into(), dv.synced.then_some(t::SYNC), Cmd::SyncToggle(d), "Sync to the other deck"),
        ];
        let bw = 92.0;
        for (i, (label, lit, cmd, tip)) in btns.into_iter().enumerate() {
            let br = Rect::from_min_size(pos2(x, cr.min.y + 2.0), vec2(bw, h - 4.0));
            if button_tip(ui, &mut self.touches, br, Id::NULL.with(("wbtn", d, i)), &label, lit, semi(14.0), tip).clicked {
                self.send(cmd);
            }
            x += bw + 6.0;
        }
        // Hot cues A–H, compact (tap to jump or set; SHIFT is on the PERFORM view).
        let pads_x = x + 10.0;
        let pw = ((cr.max.x - pads_x) / 8.0 - 4.0).clamp(30.0, 56.0);
        for i in 0..crate::engine::deck::HOT_CUES {
            let pr = Rect::from_min_size(pos2(pads_x + i as f32 * (pw + 4.0), cr.min.y + 2.0), vec2(pw, h - 4.0));
            if pr.max.x > cr.max.x + 1.0 {
                break;
            }
            let set = dv.hot[i].is_some();
            let colr = hot_color(i, dv.hot[i].and_then(|h| h.color));
            let (c, _) = control(ui, &mut self.touches, pr, Id::NULL.with(("wpad", d, i)), || {
                egui::WidgetInfo::selected(egui::Role::Button, true, set, format!("Hot cue {}", crate::ui::HOT_LETTERS[i]))
            });
            p.rect_filled(pr, CornerRadius::same(t::R_CONTROL), if set { colr.gamma_multiply(if c.down { 0.6 } else { 0.35 }) } else { t::RAISED });
            p.rect_stroke(pr, CornerRadius::same(t::R_CONTROL), Stroke::new(1.0, if set { colr } else { t::BORDER }), egui::StrokeKind::Inside);
            p.text(pr.center(), Align2::CENTER_CENTER, crate::ui::HOT_LETTERS[i], semi(15.0), if set { t::INK } else { t::INK_FAINT });
            if c.down != self.decks[d].hot_held[i] {
                self.decks[d].hot_held[i] = c.down;
                self.send(Cmd::HotCue(d, i, c.down));
            } else if c.key {
                self.send(Cmd::HotCue(d, i, true));
                self.send(Cmd::HotCue(d, i, false));
            }
        }
    }

    /// Where both decks are in the bar, and how far apart their beats are.
    fn phase_meter(&mut self, ui: &mut Ui, r: Rect, v: &EngineView) {
        let p = ui.painter().clone();
        card(&p, r);
        let inner = r.shrink2(vec2(12.0, 6.0));
        p.text(pos2(inner.min.x, inner.center().y), Align2::LEFT_CENTER, "PHASE", caption(), t::INK_FAINT);
        let seg_w = 52.0;
        let x0 = inner.min.x + 60.0;
        let row_h = (inner.height() - 4.0) / 2.0;
        let mut fracs: [Option<f64>; DECKS] = [None; DECKS];
        for d in 0..DECKS {
            let y = inner.min.y + d as f32 * (row_h + 4.0);
            let pos = bar_position(self, d, &v.decks[d]);
            for b in 0..4 {
                let sr = Rect::from_min_size(pos2(x0 + b as f32 * (seg_w + 4.0), y), vec2(seg_w, row_h));
                let lit = pos.is_some_and(|(k, _)| k == b);
                let col = if b == 0 { t::DECK_TINT[d] } else { mix(t::DECK_TINT[d], t::INK, 0.4) };
                p.rect_filled(sr, CornerRadius::same(2), if lit { col.gamma_multiply(0.8) } else { t::INSET });
                if lit {
                    if let Some((_, f)) = pos {
                        p.vline(sr.min.x + f as f32 * sr.width(), sr.y_range(), Stroke::new(2.0, t::INK));
                    }
                }
            }
            fracs[d] = pos.filter(|_| v.decks[d].loaded).map(|(k, f)| k as f64 + f);
        }
        // Offset between the decks in beats (wrapped to the nearest beat).
        let ox = x0 + 4.0 * (seg_w + 4.0) + 24.0;
        if let (Some(a), Some(b)) = (fracs[0], fracs[1]) {
            let mut diff = (b - a).rem_euclid(1.0);
            if diff > 0.5 {
                diff -= 1.0;
            }
            let bar_diff = ((b.floor() - a.floor()).rem_euclid(4.0)) as i64;
            let ok = diff.abs() < 0.02;
            let col = if ok { t::PLAY } else if diff.abs() < 0.08 { t::CUE } else { t::DANGER };
            let scale = Rect::from_min_size(pos2(ox, inner.center().y - 6.0), vec2(220.0, 12.0));
            p.rect_filled(scale, CornerRadius::same(3), t::INSET);
            p.vline(scale.center().x, scale.y_range().expand(3.0), Stroke::new(1.0, t::INK_FAINT));
            let nx = scale.center().x + (diff as f32 * 2.0).clamp(-1.0, 1.0) * scale.width() / 2.0;
            p.rect_filled(Rect::from_center_size(pos2(nx, scale.center().y), vec2(6.0, 18.0)), CornerRadius::same(2), col);
            let txt = if ok { "IN PHASE".to_string() } else { format!("deck 2 {:+.2} beat", diff) };
            p.text(pos2(scale.max.x + 12.0, inner.center().y), Align2::LEFT_CENTER, txt, cond(15.0), col);
            if bar_diff != 0 && ok {
                p.text(pos2(scale.max.x + 100.0, inner.center().y), Align2::LEFT_CENTER, format!("· bars off by {bar_diff} beat(s)"), cond(15.0), t::CUE);
            }
        } else {
            p.text(pos2(ox, inner.center().y), Align2::LEFT_CENTER, "Load both decks to compare their beats", cond(15.0), t::INK_FAINT);
        }
        // Zoom and colour at the right.
        let w = 34.0;
        for (i, (label, dir)) in [(crate::ui::icons::ZOOM_OUT, 1), (crate::ui::icons::ZOOM_IN, -1)].into_iter().enumerate() {
            let br = Rect::from_min_size(pos2(inner.max.x - 2.0 * (w + 4.0) + i as f32 * (w + 4.0), r.center().y - 15.0), vec2(w, 30.0));
            let tip = if dir > 0 { "Zoom waveforms out  ( − or the browse knob )" } else { "Zoom waveforms in  ( = or the browse knob )" };
            if button_tip(ui, &mut self.touches, br, Id::NULL.with(("wzoom", i)), label, None, semi(16.0), tip).clicked {
                self.set_zoom(dir);
            }
        }
        let mode = self.config.waveform_color;
        let mr = Rect::from_min_size(pos2(inner.max.x - 2.0 * (w + 4.0) - 72.0, r.center().y - 15.0), vec2(66.0, 30.0));
        if button(ui, &mut self.touches, mr, Id::NULL.with("wmode"), crate::track::wave::mode_name(mode), None, semi(13.0)).clicked {
            self.config.waveform_color = crate::track::wave::next_mode(mode);
        }
    }
}
