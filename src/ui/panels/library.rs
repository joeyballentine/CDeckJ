//! The library panel under the decks: BACK, where you are, search, TAG and LOAD, and the
//! track list.

use crate::ui::prelude::*;
use crate::library::Row;
use super::tracklist::ListOpts;
use crate::ui::App;

impl App {
    /// The track list with BACK, where you are, search and LOAD. `big` sizes it for thumbs and
    /// adds TAG (the display layout, where it's the main thing to touch).
    pub(crate) fn browser(&mut self, ui: &mut Ui, r: Rect, big: bool) {
        let p = ui.painter().clone();
        card(&p, r);
        let inner = r.shrink2(vec2(t::SP_M, 8.0));
        let bh = if big { 40.0 } else { 30.0 };
        let head = Rect::from_min_size(inner.min, vec2(inner.width(), bh + 2.0));
        let load_w = if big { 112.0 } else { 96.0 };
        let tag_w = if big { 96.0 } else { 0.0 };

        // Back + breadcrumb.
        let mut x = head.min.x;
        if self.lib.depth() > 0 || !self.lib.filter.is_empty() {
            let br = Rect::from_min_size(pos2(x, head.min.y + 1.0), vec2(if big { 92.0 } else { 74.0 }, bh));
            if button_tip(ui, &mut self.touches, br, Id::NULL.with("back"), &format!("{}  BACK", icons::CARET_LEFT), None, semi(if big { 15.0 } else { 14.0 }), "Up one level, or clear the search  ( Backspace )").clicked {
                self.browse_back();
            }
            x += br.width() + 10.0;
        }
        let search_w = if big { 230.0 } else { 216.0 };
        let right_w = 2.0 * load_w + if big { tag_w } else { 0.0 } + search_w;
        let g = p.layout_job(crumbs_job(&self.lib.crumbs(), head.max.x - x - right_w - 10.0));
        p.galley(pos2(x, head.center().y - g.size().y / 2.0), g, t::INK);

        // Search.
        let sr = Rect::from_min_size(pos2(head.max.x - right_w, head.min.y + 1.0), vec2(search_w, bh));
        let resp = search_field(ui, sr, &mut self.search);
        if std::mem::take(&mut self.focus_search) {
            resp.request_focus();
        }
        if resp.changed() {
            let s = self.search.clone();
            self.lib.set_filter(&s);
            self.scroll_to_selected = true;
        }
        if resp.has_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
            self.search.clear();
            self.lib.set_filter("");
            resp.surrender_focus();
        }

        let sel = match self.lib.selected_row() {
            Some(Row::Track(t)) => Some(t.clone()),
            _ => None,
        };
        let can_load = sel.is_some();
        if big {
            let tagged = sel.as_ref().is_some_and(|s| self.lib.is_tagged(&s.path));
            let tr = Rect::from_min_size(pos2(head.max.x - 2.0 * load_w - tag_w, head.min.y + 1.0), vec2(tag_w, bh));
            let tip = if tagged { "Remove from the tag list  ( T )" } else { "Add to the tag list  ( T )" };
            let label = format!("{}  {}", icons::TAG, if tagged { "TAGGED" } else { "TAG" });
            if button_tip(ui, &mut self.touches, tr, Id::NULL.with("ptag"), &label, tagged.then_some(t::ACCENT), t::button_font(), tip).clicked {
                if let Some(s) = sel.clone() {
                    self.lib.toggle_tag(&s);
                }
            }
        }
        for d in 0..DECKS {
            let br = Rect::from_min_size(pos2(head.max.x - (DECKS - d) as f32 * load_w, head.min.y + 1.0), vec2(load_w, bh));
            let tip = if d == 0 { "Load the selected track on deck 1  ( Q )" } else { "Load the selected track on deck 2  ( P )" };
            if button_tip(ui, &mut self.touches, br, Id::NULL.with(("load", d)), &format!("LOAD {}", d + 1), can_load.then_some(t::DECK_TINT[d]), t::button_font(), tip).clicked {
                self.load_selected(d);
            }
        }

        let list = Rect::from_min_max(pos2(inner.min.x, head.max.y + 6.0), inner.max);
        let opts = if big { ListOpts { row_h: sz(42.0, 50.0), title: sz(17.0, 20.0), ..ListOpts::compact() } } else { ListOpts::compact() };
        if let Some((done, total)) = self.lib.analysis_progress {
            text_fit(&p, pos2(list.max.x, r.max.y - 2.0), Align2::RIGHT_BOTTOM, &format!("Analysing BPM {done}/{total}"), caption(), t::INK_FAINT, 200.0);
        }
        if self.lib.rows.is_empty() {
            self.empty_list(ui, list);
        } else {
            self.rows_list(ui, list, opts);
        }
    }
}
