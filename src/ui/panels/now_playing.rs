//! Both decks at a glance, for the views that hide the decks; tap to go back to playing.

use crate::ui::prelude::*;
use crate::ui::App;

impl App {
    /// Both decks at a glance, for the views that hide the decks. Tap it to go back to playing.
    pub(crate) fn now_playing(&mut self, ui: &Ui, r: Rect, v: &EngineView) {
        let p = ui.painter().clone();
        let w = (r.width() - 6.0) / 2.0;
        for d in 0..DECKS {
            let rr = Rect::from_min_size(pos2(r.min.x + d as f32 * (w + 6.0), r.min.y), vec2(w, r.height()));
            card(&p, rr);
            let dv = &v.decks[d];
            let tint = t::DECK_TINT[d];
            let inner = rr.shrink2(vec2(10.0, 6.0));
            let badge = pos2(inner.min.x + 11.0, inner.center().y);
            p.circle_filled(badge, 11.0, tint.gamma_multiply(0.18));
            p.circle_stroke(badge, 11.0, Stroke::new(1.0, tint));
            p.text(badge, Align2::CENTER_CENTER, format!("{}", d + 1), semi(14.0), tint);
            let mut x = inner.min.x + 30.0;
            let track = self.decks[d].track.clone().filter(|_| dv.loaded);
            if let (Some(key), true) = (self.decks[d].art.clone(), track.is_some()) {
                if let Some(tex) = self.art.get(&key) {
                    let ar = Rect::from_min_size(pos2(x, inner.min.y), vec2(inner.height(), inner.height()));
                    p.image(tex.id(), ar, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                    x += inner.height() + 8.0;
                }
            }
            // Right side: state, time, BPM.
            let right_w = sz(250.0, 290.0);
            let title_w = (inner.max.x - right_w - x).max(40.0);
            match &track {
                Some(tr) => {
                    text_fit(&p, pos2(x, inner.min.y), Align2::LEFT_TOP, &tr.title, semi(sz(16.0, 20.0)), t::INK, title_w);
                    text_fit(&p, pos2(x, inner.min.y + sz(20.0, 25.0)), Align2::LEFT_TOP, &tr.artist, cond(sz(14.0, 16.0)), t::INK_MUTED, title_w);
                }
                None => {
                    let s = self.decks[d].loading.clone().map(|n| format!("Loading {n}…")).unwrap_or_else(|| "Empty deck".into());
                    text_fit(&p, pos2(x, inner.center().y), Align2::LEFT_CENTER, &s, semi(sz(16.0, 20.0)), t::INK_FAINT, title_w);
                }
            }
            let rx = inner.max.x - right_w + 10.0;
            let (state, col) = if dv.playing { ("PLAYING", t::PLAY) } else if dv.loaded { ("PAUSED", t::CUE) } else { ("", t::INK_FAINT) };
            p.text(pos2(rx, inner.min.y + 2.0), Align2::LEFT_TOP, state, caption(), col);
            let sr = dv.sample_rate.max(1) as f64;
            let remain = (dv.length as f64 - dv.pos) / sr;
            p.text(pos2(rx, inner.max.y), Align2::LEFT_BOTTOM, format!("-{}", fmt_time(remain.max(0.0))), mono(sz(17.0, 22.0)), if dv.loaded { t::INK } else { t::INK_FAINT });
            let bpm = dv.bpm.map(|b| format!("{b:.1}")).unwrap_or_else(|| "--.-".into());
            p.text(pos2(inner.max.x, inner.max.y), Align2::RIGHT_BOTTOM, bpm, mono(sz(17.0, 22.0)), if dv.synced { t::SYNC } else { t::INK });
            p.text(pos2(inner.max.x, inner.min.y + 2.0), Align2::RIGHT_TOP, "BPM", caption(), t::INK_FAINT);
            // Progress along the bottom edge.
            if dv.loaded && dv.length > 0 {
                let f = (dv.pos / dv.length as f64).clamp(0.0, 1.0) as f32;
                let bar = Rect::from_min_size(pos2(rr.min.x + 6.0, rr.max.y - 4.0), vec2(rr.width() - 12.0, 2.0));
                p.rect_filled(bar, 1, t::INSET);
                p.rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * f, 2.0)), 1, mix(tint, t::INK, 0.2));
            }
            let tip = format!("Deck {}: tap to go back to playing", d + 1);
            if invisible_button(ui, &mut self.touches, rr, Id::NULL.with(("np", d)), &tip, &tip).clicked {
                let back = self.last_play_view;
                self.set_view(back);
            }
        }
    }
}
