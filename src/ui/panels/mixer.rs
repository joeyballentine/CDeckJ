//! The on-screen mixer (channel strips, master and headphone knobs, crossfader) and the
//! BEAT FX bar.

use crate::ui::prelude::*;
use crate::ui::App;

impl App {
    pub(crate) fn mixer_panel(&mut self, ui: &Ui, r: Rect, v: &EngineView) {
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
            ("PH MIX", m.head_mix, t::SYNC, Some(MidiCtl::HeadMix)),
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

    pub(crate) fn strip(&mut self, ui: &Ui, r: Rect, c: usize, v: &EngineView) {
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
        if button(ui, &mut self.touches, pfl, Id::NULL.with(("pfl", c)), &format!("{}  CUE", icons::AUDIO), cp.pfl.then_some(t::CUE), semi(14.0)).clicked {
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

    /// BEAT FX: effect, beat value, channel, level and ON, laid out like a DJM's FX section.
    pub(crate) fn fx_bar(&mut self, ui: &Ui, r: Rect, v: &EngineView) {
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
            let lit = (fx.kind == *k).then_some(if fx.on { t::DANGER } else { t::ACCENT });
            if button(ui, &mut self.touches, br, Id::NULL.with(("fxk", i)), k.name(), lit, cond(14.0)).clicked {
                self.send(Cmd::Mix(MixParam::FxKind(*k)));
            }
            x += chip_w + 4.0;
        }
        x += 10.0;
        // Beat ◀ value ▶
        let l = Rect::from_min_size(pos2(x, y), vec2(32.0, h));
        if button_tip(ui, &mut self.touches, l, Id::NULL.with("fxb-"), icons::CARET_LEFT, None, semi(16.0), "Shorter beat value").clicked {
            self.send(Cmd::Mix(MixParam::FxBeat(-1)));
        }
        let label = if fx.kind == crate::engine::fx::FxKind::Reverb { format!("{}%", 10 + fx.beat_idx * 11) } else { beats_label(fx.beats()) };
        p.text(pos2(x + 32.0 + 26.0, r.center().y), Align2::CENTER_CENTER, label, mono(15.0), t::INK);
        let rr = Rect::from_min_size(pos2(x + 32.0 + 52.0, y), vec2(32.0, h));
        if button_tip(ui, &mut self.touches, rr, Id::NULL.with("fxb+"), icons::CARET_RIGHT, None, semi(16.0), "Longer beat value").clicked {
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
}
