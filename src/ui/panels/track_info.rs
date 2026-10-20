//! BROWSE's INFO panel: artwork, preview and details of the highlighted track, CDJ style.

use crate::ui::prelude::*;
use crate::library::Row;
use super::tracklist::{draw_preview, lead_deck};
use crate::ui::App;

impl App {
    /// Details of the highlighted track, CDJ INFO style.
    pub(crate) fn info_panel(&mut self, ui: &mut Ui, r: Rect, v: &EngineView) {
        let p = ui.painter().clone();
        card(&p, r);
        let inner = r.shrink(12.0);
        let Some(Row::Track(e)) = self.lib.selected_row().cloned() else {
            p.text(inner.center(), Align2::CENTER_CENTER, "Highlight a track to see its details", cond(15.0), t::INK_FAINT);
            return;
        };
        let mut y = inner.min.y;
        let art_s = 128.0;
        let ar = Rect::from_min_size(pos2(inner.center().x - art_s / 2.0, y), vec2(art_s, art_s));
        let key = self.art_key(&e);
        match key.as_ref().and_then(|k| self.art.get(k)) {
            Some(tex) => {
                p.image(tex.id(), ar, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
            None => {
                p.rect_filled(ar, CornerRadius::same(4), t::INSET);
                p.text(ar.center() - vec2(0.0, 10.0), Align2::CENTER_CENTER, icons::IMAGE, cond(34.0), t::BORDER.gamma_multiply(1.6));
                p.text(ar.center() + vec2(0.0, 22.0), Align2::CENTER_CENTER, "NO ARTWORK", caption(), t::INK_FAINT);
            }
        }
        y += art_s + 10.0;
        let tg = p.layout(e.title.clone(), semi(18.0), t::INK, inner.width());
        let th = tg.size().y.min(48.0);
        p.galley(pos2(inner.min.x, y), tg, t::INK);
        y += th + 2.0;
        text_fit(&p, pos2(inner.min.x, y), Align2::LEFT_TOP, &e.artist, cond(16.0), t::INK_MUTED, inner.width());
        y += 24.0;
        // Whole-track preview.
        let pr = Rect::from_min_size(pos2(inner.min.x, y), vec2(inner.width(), 40.0));
        p.rect_filled(pr, CornerRadius::same(3), t::INSET);
        let src = self.preview_source(&e);
        if let Some(pv) = self.previews.get(&e.path, self.config.waveform_color, || src) {
            draw_preview(&p, pr, &pv, None);
        }
        y += 48.0;
        // Facts.
        let mut facts: Vec<(&str, String)> = Vec::new();
        let mut rating = 0;
        facts.push(("BPM", e.bpm.map(|b| format!("{b:.2}")).unwrap_or_else(|| "–".into())));
        let lead = lead_deck(v).and_then(|d| Some((d, crate::library::keys::parse(&self.decks[d].track.as_ref()?.key)?)));
        let key_s = match crate::library::keys::parse(&e.key) {
            Some(k) => {
                // Camelot in brackets only when the tag uses another notation.
                let camelot = crate::library::keys::label(k);
                let mut s = if camelot.eq_ignore_ascii_case(e.key.trim()) { camelot } else { format!("{}  ({camelot})", e.key) };
                if let Some((d, _)) = lead.filter(|(_, l)| crate::library::keys::compatible(*l, k)) {
                    s +=&format!("  {} mixes with deck {}", icons::CHECK, d + 1);
                }
                s
            }
            None => e.key.clone(),
        };
        facts.push(("KEY", if key_s.is_empty() { "–".into() } else { key_s }));
        facts.push(("TIME", e.duration.map(|d| format!("{}:{:02}", d / 60, d % 60)).unwrap_or_else(|| "–".into())));
        if let Some((u, id)) = e.rb {
            if let Some((ex, row)) = self.lib.usbs.get(u).and_then(|ex| ex.track(id).map(|t| (ex.clone(), t.clone()))) {
                if let Some(a) = ex.db.albums.get(&row.album_id) {
                    facts.push(("ALBUM", a.clone()));
                }
                if let Some(g) = ex.db.genres.get(&row.genre_id) {
                    facts.push(("GENRE", g.clone()));
                }
                facts.push(("FORMAT", format!("{} · {} kbps · {:.1} kHz", e.ext, row.bitrate, row.sample_rate as f64 / 1000.0)));
                rating = row.rating.min(5) as usize;
                if !row.comment.is_empty() {
                    facts.push(("COMMENT", row.comment.clone()));
                }
                if !row.date_added.is_empty() {
                    facts.push(("ADDED", row.date_added.clone()));
                }
            }
        } else {
            facts.push(("FILE", e.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
        }
        if rating > 0 {
            // Drawn after the loop as stars; reserve its row here.
            facts.push(("RATING", String::new()));
        }
        if self.lib.played.contains(&e.path) {
            facts.push(("PLAYED", "Already played this session".into()));
        }
        for (k, val) in facts {
            if y > inner.max.y - 44.0 {
                break;
            }
            p.text(pos2(inner.min.x, y), Align2::LEFT_TOP, k, caption(), t::INK_FAINT);
            if k == "RATING" {
                for i in 0..5 {
                    let (font, col) = if i < rating { (icons::fill(14.0), t::CUE) } else { (cond(14.0), t::INK_FAINT) };
                    p.text(pos2(inner.min.x + 72.0 + i as f32 * 17.0, y + 7.0), Align2::LEFT_CENTER, icons::STAR, font, col);
                }
            }
            text_fit(&p, pos2(inner.min.x + 72.0, y - 1.0), Align2::LEFT_TOP, &val, cond(15.0), t::INK, inner.width() - 72.0);
            y += 22.0;
        }
    }
}
