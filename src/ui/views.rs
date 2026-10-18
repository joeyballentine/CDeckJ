//! Switching between full-screen views, like a CDJ's screen buttons: PERFORM (everything),
//! WAVE (big waveforms), BROWSE (full library), TAG (tag list) and SETTINGS.

use egui::{pos2, vec2, Align2, Color32, CornerRadius, Id, Rect, Stroke, StrokeKind, Ui};

use super::{button_tip, card, fmt_time, invisible_button, mix, text_fit, App};
use crate::engine::{EngineView, DECKS};
use crate::theme::{self as t, caption, cond, mono, semi};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Perform,
    Wave,
    Browse,
    Tag,
    Settings,
}

pub const VIEWS: [View; 5] = [View::Perform, View::Wave, View::Browse, View::Tag, View::Settings];

impl View {
    pub fn name(self) -> &'static str {
        match self {
            View::Perform => "PERFORM",
            View::Wave => "WAVE",
            View::Browse => "BROWSE",
            View::Tag => "TAG",
            View::Settings => "SETTINGS",
        }
    }

    pub fn icon(self) -> &'static str {
        use crate::icons as i;
        match self {
            View::Perform => i::PERFORM,
            View::Wave => i::WAVE,
            View::Browse => i::BROWSE,
            View::Tag => i::TAG,
            View::Settings => i::SETTINGS,
        }
    }

    pub fn tip(self) -> &'static str {
        match self {
            View::Perform => "Decks, mixer and library together  ( F1 )",
            View::Wave => "Big waveforms with phase meter and cue countdown  ( F2 )",
            View::Browse => "The whole library, with search, filter and track info  ( F3 )",
            View::Tag => "Your tag list for this set  ( F4 )",
            View::Settings => "Player, mixer, audio and controller settings  ( F5 )",
        }
    }

    /// Views you play from (loading a track returns to the last one of these).
    pub fn is_play_view(self) -> bool {
        matches!(self, View::Perform | View::Wave)
    }
}

impl App {
    pub(super) fn set_view(&mut self, v: View) {
        if v == self.view_mode {
            return;
        }
        if self.view_mode.is_play_view() {
            self.last_play_view = self.view_mode;
        }
        match v {
            View::Tag => self.lib.jump_to(crate::library::Node::Tag),
            View::Browse if self.lib.node() == crate::library::Node::Tag => self.lib.jump_to(crate::library::Node::Root),
            _ => {}
        }
        if v == View::Settings {
            self.settings_opened();
        }
        self.keyboard_open = false;
        self.view_mode = v;
        self.scroll_to_selected = true;
    }

    pub(super) fn cycle_view(&mut self, dir: i32) {
        let i = VIEWS.iter().position(|&v| v == self.view_mode).unwrap_or(0) as i32;
        self.set_view(VIEWS[(i + dir).rem_euclid(VIEWS.len() as i32) as usize]);
    }

    /// After loading from a browse view, go back to playing (like a CDJ).
    pub(super) fn after_load(&mut self) {
        if !self.view_mode.is_play_view() {
            let back = self.last_play_view;
            self.set_view(back);
        }
    }

    /// Both decks at a glance, for the views that hide the decks. Tap it to go back to playing.
    pub(super) fn now_playing(&mut self, ui: &Ui, r: Rect, v: &EngineView) {
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
            let right_w = 250.0;
            let title_w = (inner.max.x - right_w - x).max(40.0);
            match &track {
                Some(tr) => {
                    text_fit(&p, pos2(x, inner.min.y), Align2::LEFT_TOP, &tr.title, semi(16.0), t::INK, title_w);
                    text_fit(&p, pos2(x, inner.min.y + 20.0), Align2::LEFT_TOP, &tr.artist, cond(14.0), t::INK_MUTED, title_w);
                }
                None => {
                    let s = self.decks[d].loading.clone().map(|n| format!("Loading {n}…")).unwrap_or_else(|| "Empty deck".into());
                    text_fit(&p, pos2(x, inner.center().y), Align2::LEFT_CENTER, &s, semi(16.0), t::INK_FAINT, title_w);
                }
            }
            let rx = inner.max.x - right_w + 10.0;
            let (state, col) = if dv.playing { ("PLAYING", t::PLAY) } else if dv.loaded { ("PAUSED", t::CUE) } else { ("", t::INK_FAINT) };
            p.text(pos2(rx, inner.min.y + 2.0), Align2::LEFT_TOP, state, caption(), col);
            let sr = dv.sample_rate.max(1) as f64;
            let remain = (dv.length as f64 - dv.pos) / sr;
            p.text(pos2(rx, inner.max.y), Align2::LEFT_BOTTOM, format!("-{}", fmt_time(remain.max(0.0))), mono(17.0), if dv.loaded { t::INK } else { t::INK_FAINT });
            let bpm = dv.bpm.map(|b| format!("{b:.1}")).unwrap_or_else(|| "--.-".into());
            p.text(pos2(inner.max.x, inner.max.y), Align2::RIGHT_BOTTOM, bpm, mono(17.0), if dv.synced { t::SYNC } else { t::INK });
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

/// Small helper for pages: a section heading.
pub(super) fn heading(p: &egui::Painter, pos: egui::Pos2, s: &str) {
    p.text(pos, Align2::LEFT_TOP, s, caption(), t::INK_FAINT);
}

/// A row of mutually exclusive choices. Returns the index tapped.
pub(super) fn segmented(app: &mut App, ui: &Ui, r: Rect, id: &str, labels: &[&str], selected: usize, tip: &str) -> Option<usize> {
    let n = labels.len().max(1) as f32;
    let w = (r.width() - 4.0 * (n - 1.0)) / n;
    let mut out = None;
    for (i, l) in labels.iter().enumerate() {
        let br = Rect::from_min_size(pos2(r.min.x + i as f32 * (w + 4.0), r.min.y), vec2(w, r.height()));
        if button_tip(ui, &mut app.touches, br, Id::NULL.with((id, i)), l, (i == selected).then_some(t::ACCENT), cond(14.0), tip).clicked {
            out = Some(i);
        }
    }
    out
}

/// A rounded panel outline (used for overlays).
pub(super) fn panel(p: &egui::Painter, r: Rect) {
    p.rect_filled(r, CornerRadius::same(t::R_CARD), t::SURFACE);
    p.rect_stroke(r, CornerRadius::same(t::R_CARD), Stroke::new(1.0, t::BORDER), StrokeKind::Inside);
}
