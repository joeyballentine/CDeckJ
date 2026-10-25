//! The track list (shared by the PERFORM view's browser and the BROWSE view) and the
//! full-screen BROWSE view: side bar, columns, track filter, INFO panel, on-screen keyboard.

use crate::ui::prelude::*;
use crate::ui::panels::tracklist::{lead_deck, ListOpts};
use crate::ui::App;


use crate::engine::{EngineView, DECKS};
use crate::library::{self, Node, Row, TrackFilter};

/// Height of the BROWSE header (BACK/search row and the tools row).
const HEADER_H: f32 = 96.0;

impl App {

    /// The full-screen BROWSE (and TAG) view.
    pub(crate) fn browse_view(&mut self, ui: &mut Ui, r: Rect, v: &EngineView) {
        self.previews.poll();
        let p = ui.painter().clone();
        // Narrow windows give the list more room; so does glance mode's bigger type.
        let narrow = r.width() < sz(1180.0, 1260.0);
        let side_w = if narrow { 172.0 } else { 210.0 };
        let info_w = if !self.config.browse_info { 0.0 } else if narrow { 248.0 } else { 300.0 };
        let side = Rect::from_min_size(r.min, vec2(side_w, r.height()));
        let info = Rect::from_min_max(pos2(r.max.x - info_w, r.min.y), r.max);
        let main = Rect::from_min_max(pos2(side.max.x, r.min.y), pos2(info.min.x, r.max.y));

        // Side bar: each stick's lists, the music folder, history and the tag list.
        card(&p, side);
        let current = self.lib.root_node();
        let roots = self.lib.roots();
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(side.shrink2(vec2(0.0, 8.0))));
        child.spacing_mut().item_spacing.y = 0.0;
        let mut picked = None;
        // Scrolls when sticks, playlists and history don't all fit.
        egui::ScrollArea::vertical().id_salt("browse_side").auto_shrink([false, false]).show(&mut child, |ui| {
            for (i, (label, node, indent)) in roots.into_iter().enumerate() {
                let h = if indent { 32.0 } else { 36.0 };
                let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
                let br = Rect::from_min_max(row.min + vec2(if indent { 14.0 } else { 0.0 }, 0.0), row.max);
                let on = current == node;
                let (c, resp) = control(ui, &mut self.touches, br, Id::NULL.with(("side", i)), || WidgetInfo::selected(egui::Role::Button, true, on, &label));
                let p = ui.painter();
                if on {
                    p.rect_filled(br, CornerRadius::ZERO, t::ACCENT.gamma_multiply(0.2));
                    p.rect_filled(Rect::from_min_max(br.min, pos2(br.min.x + 3.0, br.max.y)), CornerRadius::ZERO, t::ACCENT);
                } else if c.hovered {
                    p.rect_filled(br, CornerRadius::ZERO, t::RAISED);
                }
                focus_ring(p, br, &resp, t::R_CONTROL);
                let font = if indent { cond(sz(15.0, 17.0)) } else { semi(sz(16.0, 18.0)) };
                let ink = if on { t::INK } else if indent { t::INK_MUTED } else { t::INK };
                let (icon, icol) = match node {
                    Node::Usb(_) => (icons::USB, t::ACCENT),
                    Node::UsbAll(_) => (icons::TRACKS, t::INK_MUTED),
                    Node::UsbPlaylists(..) => (icons::PLAYLIST, t::INK_MUTED),
                    Node::UsbHistory(_) | Node::History => (icons::HISTORY, t::INK_MUTED),
                    Node::Folder => (icons::FOLDER, t::INK_MUTED),
                    Node::Tag => (icons::TAG, t::ACCENT),
                    Node::Suggest => (icons::SPARKLE, t::ACCENT),
                    _ => (icons::FOLDER, t::INK_MUTED),
                };
                let icol = if on { t::INK } else { icol };
                p.text(pos2(br.min.x + 17.0, br.center().y), Align2::CENTER_CENTER, icon, cond(if indent { 16.0 } else { 18.0 }), icol);
                text_fit(p, pos2(br.min.x + 34.0, br.center().y), Align2::LEFT_CENTER, &label, font, ink, br.width() - 40.0);
                if c.clicked {
                    picked = Some(node);
                }
            }
        });
        if let Some(node) = picked {
            self.lib.jump_to(node);
            self.search.clear();
            self.scroll_to_selected = true;
            if node == Node::Tag {
                self.view_mode = crate::ui::views::View::Tag;
            } else if self.view_mode == crate::ui::views::View::Tag {
                self.view_mode = crate::ui::views::View::Browse;
            }
        }

        // Main list.
        card(&p, main);
        let inner = main.shrink2(vec2(t::SP_M, 8.0));
        self.browse_header(ui, inner, v);
        let list = Rect::from_min_max(pos2(inner.min.x, inner.min.y + HEADER_H), inner.max);
        let kb_h = if self.keyboard_open { 236.0 } else { 0.0 };
        let list = Rect::from_min_max(list.min, pos2(list.max.x, list.max.y - kb_h));
        if self.lib.rows.is_empty() {
            let (icon, msg) = if self.lib.track_filter.is_active() {
                (icons::FILTER, "No tracks match the filter. Tap FILTER to change or clear it.")
            } else if self.lib.node() == Node::Tag {
                (icons::TAG, "Tag tracks with TAG (or T) while browsing; they collect here for the set.")
            } else if self.lib.node() == Node::Suggest && self.lib.seed.is_none() {
                (icons::SPARKLE, "Load a track on a deck: tracks that mix with it show up here.")
            } else if self.lib.node() == Node::Suggest {
                (icons::SPARKLE, "Nothing in the library is close in tempo with a key that mixes.")
            } else if !self.lib.filter.is_empty() {
                (icons::SEARCH, "Nothing matches the search. BACK clears it.")
            } else {
                (icons::PLAYLIST, "Nothing here.")
            };
            p.text(list.center() - vec2(0.0, 30.0), Align2::CENTER_CENTER, icon, cond(34.0), t::INK_FAINT);
            p.text(list.center() + vec2(0.0, 8.0), Align2::CENTER_CENTER, msg, t::body(), t::INK_MUTED);
        } else {
            // Sized for a thumb on the Deck's 7" screen; A− / A+ trade size for rows, and glance
            // mode is one step up.
            let step = self.config.browse_text.min(2) as usize + usize::from(t::glance());
            let row_h = [34.0, 44.0, 54.0, 64.0][step];
            let title = [15.0, 17.0, 20.0, 23.0][step];
            self.rows_list(ui, list, ListOpts { row_h, title, cols: self.config.browse_cols, art: true });
        }
        if self.keyboard_open {
            let kr = Rect::from_min_max(pos2(main.min.x, main.max.y - kb_h), main.max);
            self.on_screen_keyboard(ui, kr);
        }
        if let Some(pop) = self.browse_popup {
            self.browse_popup_ui(ui, inner, pop, v);
        }

        if info_w > 0.0 {
            self.info_panel(ui, info, v);
        }
    }

    fn browse_header(&mut self, ui: &mut Ui, inner: Rect, v: &EngineView) {
        let p = ui.painter().clone();
        // Row 1: BACK, where you are, search.
        let row1 = Rect::from_min_size(inner.min, vec2(inner.width(), 40.0));
        let mut x = row1.min.x;
        if self.lib.depth() > 0 || !self.lib.filter.is_empty() {
            let br = Rect::from_min_size(pos2(x, row1.min.y + 1.0), vec2(84.0, 38.0));
            if button_tip(ui, &mut self.touches, br, Id::NULL.with("bback"), &format!("{}  BACK", icons::CARET_LEFT), None, semi(14.0), "Up one level, or clear the search  ( Backspace )").clicked {
                self.browse_back();
            }
            x += 94.0;
        }
        let search_w = 250.0;
        let g = p.layout_job(crumbs_job(&self.lib.crumbs(), row1.max.x - x - search_w - 70.0));
        p.galley(pos2(x, row1.center().y - g.size().y / 2.0), g, t::INK);
        let sr = Rect::from_min_size(pos2(row1.max.x - search_w - 58.0, row1.min.y + 1.0), vec2(search_w, 38.0));
        let resp = search_field(ui, sr, &mut self.search);
        if std::mem::take(&mut self.focus_search) {
            resp.request_focus();
        }
        if resp.changed() {
            let s = self.search.clone();
            self.lib.set_filter(&s);
            self.scroll_to_selected = true;
        }
        // On a touch screen, tapping the field brings up the on-screen keyboard.
        if resp.clicked() && self.touches.touch_mode() {
            self.keyboard_open = true;
        }
        let kr = Rect::from_min_size(pos2(row1.max.x - 58.0, row1.min.y + 1.0), vec2(58.0, 38.0));
        if button_tip(ui, &mut self.touches, kr, Id::NULL.with("kbd"), icons::KEYBOARD, self.keyboard_open.then_some(t::ACCENT), cond(20.0), "On-screen keyboard for search").clicked {
            self.keyboard_open = !self.keyboard_open;
        }

        // Row 2: filter, columns, text size, info | tag, load.
        let y2 = row1.max.y + 8.0;
        let h = 40.0;
        let mut x = inner.min.x;
        let f = self.lib.track_filter;
        let ftip = match (f.bpm, f.key) {
            (None, None) => "Show only tracks that mix with the leading deck (BPM range, compatible key)".to_string(),
            _ => format!(
                "Filter on: {}{}",
                f.bpm.map(|(b, pct)| format!("{b:.1} BPM ±{pct:.0} %  ")).unwrap_or_default(),
                f.key.map(|k| format!("key {} and neighbours", crate::library::keys::label(k))).unwrap_or_default()
            ),
        };
        let mut tool = |app: &mut App, w: f32, id: &str, label: &str, lit: bool, tip: &str| -> bool {
            let br = Rect::from_min_size(pos2(x, y2), vec2(w, h));
            x += w;
            button_tip(ui, &mut app.touches, br, Id::NULL.with(id), label, lit.then_some(t::ACCENT), semi(14.0), tip).clicked
        };
        let small = inner.width() < 700.0;
        let (filter_l, cols_l, info_l) = if small {
            (icons::FILTER.to_string(), icons::COLUMNS.to_string(), icons::INFO.to_string())
        } else {
            (format!("{}  FILTER", icons::FILTER), format!("{}  COLUMNS", icons::COLUMNS), format!("{}  INFO", icons::INFO))
        };
        if tool(self, if small { 44.0 } else { 92.0 }, "bfilter", &filter_l, f.is_active() || self.browse_popup == Some(Popup::Filter), &ftip) {
            self.browse_popup = if self.browse_popup == Some(Popup::Filter) { None } else { Some(Popup::Filter) };
        }
        if tool(self, if small { 44.0 } else { 108.0 }, "bcols", &cols_l, self.browse_popup == Some(Popup::Columns), "Choose the columns shown") {
            self.browse_popup = if self.browse_popup == Some(Popup::Columns) { None } else { Some(Popup::Columns) };
        }
        if tool(self, 40.0, "btext-", "A−", false, "Smaller text") {
            self.config.browse_text = self.config.browse_text.saturating_sub(1);
            self.scroll_to_selected = true;
        }
        if tool(self, 40.0, "btext+", "A+", false, "Larger text") {
            self.config.browse_text = (self.config.browse_text + 1).min(2);
            self.scroll_to_selected = true;
        }
        if tool(self, if small { 44.0 } else { 76.0 }, "binfo", &info_l, self.config.browse_info, "Show details for the highlighted track") {
            self.config.browse_info = !self.config.browse_info;
        }
        let _ = v;
        // Right: TAG, LOAD 1, LOAD 2.
        let sel = match self.lib.selected_row() {
            Some(Row::Track(t)) => Some(t.clone()),
            _ => None,
        };
        let mut rx = inner.max.x;
        let lw = if inner.width() < 700.0 { 80.0 } else { 92.0 };
        for d in (0..DECKS).rev() {
            rx -= lw;
            let br = Rect::from_min_size(pos2(rx, y2), vec2(lw, h));
            let tip = if d == 0 { "Load the selected track on deck 1  ( Q )" } else { "Load the selected track on deck 2  ( P )" };
            if button_tip(ui, &mut self.touches, br, Id::NULL.with(("bload", d)), &format!("LOAD {}", d + 1), sel.is_some().then_some(t::DECK_TINT[d]), semi(15.0), tip).clicked {
                self.load_selected(d);
            }
        }
        let tagged = sel.as_ref().is_some_and(|s| self.lib.is_tagged(&s.path));
        let tag_w = if tagged { 98.0 } else { 74.0 };
        rx -= tag_w;
        let br = Rect::from_min_size(pos2(rx, y2), vec2(tag_w, h));
        let tip = if tagged { "Remove from the tag list  ( T )" } else { "Add to the tag list  ( T )" };
        if button_tip(ui, &mut self.touches, br, Id::NULL.with("btag"), &format!("{}  {}", icons::TAG, if tagged { "TAGGED" } else { "TAG" }), tagged.then_some(t::ACCENT), semi(14.0), tip).clicked {
            if let Some(s) = sel {
                self.lib.toggle_tag(&s);
            }
        }
        if self.lib.node() == Node::Tag && !self.lib.tags.is_empty() {
            rx -= 122.0;
            let br = Rect::from_min_size(pos2(rx, y2), vec2(122.0, h));
            let confirm = self.clear_tags_confirm.is_some_and(|t| t.elapsed().as_secs_f32() < 3.0);
            if button_tip(ui, &mut self.touches, br, Id::NULL.with("bclear"), &format!("{}  {}", icons::TRASH, if confirm { "CLEAR ALL?" } else { "CLEAR ALL" }), confirm.then_some(t::DANGER), semi(14.0), "Empty the tag list (press twice)").clicked {
                if confirm {
                    self.lib.clear_tags();
                    self.clear_tags_confirm = None;
                } else {
                    self.clear_tags_confirm = Some(std::time::Instant::now());
                }
            }
        }
    }

    fn browse_popup_ui(&mut self, ui: &mut Ui, inner: Rect, pop: Popup, v: &EngineView) {
        let p = ui.painter().clone();
        let r = Rect::from_min_size(pos2(inner.min.x, inner.min.y + HEADER_H), vec2(470.0, if pop == Popup::Filter { 196.0 } else { 120.0 }));
        p.rect_filled(r.expand(4.0), CornerRadius::ZERO, Color32::from_black_alpha(120));
        panel(&p, r);
        // Covers the track rows underneath, so a tap between the popup's buttons doesn't
        // select a row. Registered before the buttons, which stay on top of it.
        let _ = ui.interact(r.expand(4.0), Id::NULL.with("browse_popup"), Sense::click_and_drag());
        let inner = r.shrink(12.0);
        match pop {
            Popup::Filter => {
                let lead = lead_deck(v);
                let lead_bpm = lead.and_then(|d| v.decks[d].bpm);
                let lead_key = lead.and_then(|d| self.decks[d].track.as_ref()).and_then(|t| crate::library::keys::parse(&t.key));
                let src = match (lead, lead_bpm) {
                    (Some(d), Some(b)) => format!(
                        "Matching deck {}: {b:.1} BPM{}",
                        d + 1,
                        lead_key.map(|k| format!(", key {}", crate::library::keys::label(k))).unwrap_or_default()
                    ),
                    _ => "Load a track to filter against it".into(),
                };
                p.text(inner.min, Align2::LEFT_TOP, &src, cond(15.0), t::INK_MUTED);
                heading(&p, inner.min + vec2(0.0, 30.0), "BPM RANGE (HALF AND DOUBLE TEMPO COUNT TOO)");
                let pcts = [0.0, 2.0, 4.0, 6.0, 10.0];
                let cur = self.lib.track_filter.bpm.map(|(_, pct)| pcts.iter().position(|&x| x == pct).unwrap_or(0)).unwrap_or(0);
                let sr = Rect::from_min_size(inner.min + vec2(0.0, 48.0), vec2(inner.width(), 34.0));
                if let Some(i) = segmented(self, ui, sr, "fbpm", &["OFF", "±2 %", "±4 %", "±6 %", "±10 %"], cur, "Only tracks near the leading deck's BPM") {
                    let mut f = self.lib.track_filter;
                    f.bpm = (i > 0).then_some(()).and(lead_bpm).map(|b| (b, pcts[i]));
                    self.lib.set_track_filter(f);
                }
                heading(&p, inner.min + vec2(0.0, 92.0), "KEY");
                let kr = Rect::from_min_size(inner.min + vec2(0.0, 110.0), vec2(inner.width(), 34.0));
                let kcur = usize::from(self.lib.track_filter.key.is_some());
                if let Some(i) = segmented(self, ui, kr, "fkey", &["ANY KEY", "COMPATIBLE KEYS"], kcur, "Same key, a step around the wheel, or relative major/minor") {
                    let mut f = self.lib.track_filter;
                    f.key = if i == 1 { lead_key } else { None };
                    self.lib.set_track_filter(f);
                }
                if self.lib.track_filter.is_active() {
                    let cr = Rect::from_min_size(pos2(inner.max.x - 90.0, inner.max.y - 30.0), vec2(90.0, 30.0));
                    if button(ui, &mut self.touches, cr, Id::NULL.with("fclear"), "CLEAR", None, semi(14.0)).clicked {
                        self.lib.set_track_filter(TrackFilter::default());
                    }
                }
            }
            Popup::Columns => {
                heading(&p, inner.min, "COLUMNS");
                let c = self.config.browse_cols;
                let items: [(&str, bool); 6] =
                    [("ARTIST", c.artist), ("PREVIEW", c.preview), ("BPM", c.bpm), ("KEY", c.key), ("TIME", c.time), ("TYPE", c.format)];
                let w = inner.width() / 6.0;
                for (i, (name, on)) in items.iter().enumerate() {
                    let br = Rect::from_min_size(inner.min + vec2(i as f32 * w, 22.0), vec2(w, 34.0));
                    if button(ui, &mut self.touches, br, Id::NULL.with(("colt", i)), name, on.then_some(t::ACCENT), cond(14.0)).clicked {
                        let c = &mut self.config.browse_cols;
                        match i {
                            0 => c.artist = !c.artist,
                            1 => c.preview = !c.preview,
                            2 => c.bpm = !c.bpm,
                            3 => c.key = !c.key,
                            4 => c.time = !c.time,
                            _ => c.format = !c.format,
                        }
                    }
                }
            }
        }
        // Tap outside the popup to close it.
        if ui.input(|i| i.pointer.any_pressed()) {
            if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                let header_btns = Rect::from_min_size(pos2(r.min.x, r.min.y - 50.0), vec2(212.0, 46.0));
                if !r.expand(4.0).contains(pos) && !header_btns.contains(pos) {
                    self.browse_popup = None;
                }
            }
        }
    }

}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Popup {
    Filter,
    Columns,
}

#[allow(dead_code)]
fn _unused(_: &library::Library) {}
