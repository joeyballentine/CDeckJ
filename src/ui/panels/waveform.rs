//! Waveforms: the scrolling three-band view, the whole-track overview (tap to seek) and the
//! zoom and colour buttons over them.

use crate::ui::prelude::*;
use egui::Mesh;
use crate::engine::deck::SCRATCH_SECS_PER_TICK;
use crate::ui::HOT_LETTERS;
use crate::ui::App;

impl App {
    /// Waveform colour and zoom over the top-right of the waveforms; `big` for touch.
    pub(crate) fn zoom_buttons(&mut self, ui: &Ui, waves: Rect, big: bool) {
        let (w, h, mw) = if big { (44.0, 36.0, 76.0) } else { (30.0, 26.0, 62.0) };
        // Waveform colour (BLUE / RGB / 3BAND), as WAVEFORM COLOR on a CDJ.
        let mode = self.config.waveform_color;
        let mr = Rect::from_min_size(pos2(waves.max.x - 2.0 * (w + 4.0) - 6.0 - mw - 4.0, waves.min.y + 4.0), vec2(mw, h));
        if button_tip(ui, &mut self.touches, mr, Id::NULL.with("wavemode"), crate::track::wave::mode_name(mode), None, semi(if big { 14.0 } else { 13.0 }), "Waveform colour: Blue / RGB / 3-Band  ( W )").clicked {
            self.config.waveform_color = crate::track::wave::next_mode(mode);
        }
        for (i, (label, dir)) in [(icons::ZOOM_OUT, 1), (icons::ZOOM_IN, -1)].into_iter().enumerate() {
            let r = Rect::from_min_size(pos2(waves.max.x - 2.0 * (w + 4.0) - 6.0 + i as f32 * (w + 4.0), waves.min.y + 4.0), vec2(w, h));
            let tip = if dir > 0 { "Zoom waveforms out  ( − )" } else { "Zoom waveforms in  ( = )" };
            if button_tip(ui, &mut self.touches, r, Id::NULL.with(("zoom", i)), label, None, semi(if big { 20.0 } else { 16.0 }), tip).clicked {
                self.set_zoom(dir);
            }
        }
    }

    pub(crate) fn waveform(&mut self, ui: &Ui, r: Rect, d: usize, dv: &DeckView) {
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
                let a = crate::track::wave::bin_at(&track, ts);
                if a < 0.0 {
                    continue;
                }
                let b = crate::track::wave::bin_at(&track, ts + px_s);
                let Some(col) = crate::track::wave::column(&track, &own, mode, a as usize, b as usize) else { break };
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

    pub(crate) fn overview(&mut self, ui: &Ui, p: &Painter, r: Rect, d: usize, dv: &DeckView) {
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
                let a = crate::track::wave::bin_at(&track, x as f64 / w as f64 * len_s).max(0.0) as usize;
                let b = crate::track::wave::bin_at(&track, (x + 1) as f64 / w as f64 * len_s).max(0.0) as usize;
                let Some(col) = crate::track::wave::column(&track, &own, mode, a, b) else { break };
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
}
