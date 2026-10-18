//! The track list (shared by the PERFORM view's browser and the BROWSE view) and the
//! full-screen BROWSE view: side bar, columns, track filter, INFO panel, on-screen keyboard.

use std::collections::HashMap;
use std::path::PathBuf;

use egui::text::{LayoutJob, TextWrapping};
use egui::{pos2, vec2, Align2, Color32, CornerRadius, Id, Key, Mesh, Rect, Sense, Stroke, StrokeKind, Ui, WidgetInfo};

use super::views::{panel, segmented};
use super::{button, button_tip, caret, card, control, focus_ring, nav_icon, text_fit, App};
use crate::config::BrowseCols;
use crate::engine::{EngineView, DECKS};
use crate::library::{self, LibTrack, Node, Row, SortKey, TrackFilter};
use crate::theme::{self as t, caption, cond, mono, semi};

pub struct ListOpts {
    pub row_h: f32,
    pub title: f32,
    pub cols: BrowseCols,
    pub art: bool,
}

impl ListOpts {
    /// The PERFORM view's browser: fixed columns, 30 pt rows.
    pub fn compact() -> ListOpts {
        ListOpts { row_h: 30.0, title: 16.0, cols: BrowseCols { preview: false, format: false, ..BrowseCols::default() }, art: true }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Col {
    Marks,
    Title,
    Artist,
    Preview,
    Bpm,
    Key,
    Time,
    Format,
}

/// Column x positions for a row width: (column, x, width).
fn layout(w: f32, c: &BrowseCols) -> Vec<(Col, f32, f32)> {
    let mut fixed: Vec<(Col, f32)> = Vec::new();
    if c.preview {
        fixed.push((Col::Preview, 150.0));
    }
    if c.bpm {
        fixed.push((Col::Bpm, 70.0));
    }
    if c.key {
        fixed.push((Col::Key, 54.0));
    }
    if c.time {
        fixed.push((Col::Time, 60.0));
    }
    if c.format {
        fixed.push((Col::Format, 56.0));
    }
    let marks = 44.0;
    let fixed_w: f32 = fixed.iter().map(|f| f.1).sum();
    let flex = (w - marks - fixed_w).max(120.0);
    let (tw, aw) = if c.artist { (flex * 0.58, flex * 0.42) } else { (flex, 0.0) };
    let mut v = vec![(Col::Marks, 0.0, marks), (Col::Title, marks, tw)];
    let mut x = marks + tw;
    if c.artist {
        v.push((Col::Artist, x, aw));
        x += aw;
    }
    for (col, cw) in fixed {
        v.push((col, x, cw));
        x += cw;
    }
    v
}

fn col_name(c: Col) -> Option<(&'static str, Option<SortKey>)> {
    Some(match c {
        Col::Title => ("TITLE", Some(SortKey::Title)),
        Col::Artist => ("ARTIST", Some(SortKey::Artist)),
        Col::Preview => ("PREVIEW", None),
        Col::Bpm => ("BPM", Some(SortKey::Bpm)),
        Col::Key => ("KEY", Some(SortKey::Key)),
        Col::Time => ("TIME", Some(SortKey::Time)),
        Col::Format => ("TYPE", None),
        Col::Marks => return None,
    })
}

/// Which deck leads (for the track filter and key matching): the sync master if loaded,
/// else a playing deck, else any loaded deck.
pub(super) fn lead_deck(v: &EngineView) -> Option<usize> {
    (0..DECKS)
        .find(|&d| v.decks[d].is_master && v.decks[d].loaded && v.decks[d].playing)
        .or_else(|| (0..DECKS).find(|&d| v.decks[d].playing))
        .or_else(|| (0..DECKS).find(|&d| v.decks[d].loaded))
}

impl App {
    fn preview_source(&self, e: &LibTrack) -> crate::preview::Source {
        match e.rb.and_then(|(u, id)| self.lib.usbs.get(u).map(|ex| (ex.clone(), id))) {
            Some((ex, id)) => crate::preview::Source::Rekordbox(ex, id),
            None => crate::preview::Source::Local,
        }
    }

    /// The scrolling track/folder list.
    pub(super) fn rows_list(&mut self, ui: &mut Ui, list: Rect, o: ListOpts) {
        let p = ui.painter().clone();
        let rows = self.lib.rows.clone();
        let tracks_view = rows.iter().any(|r| matches!(r, Row::Track(_)));
        let mut top = list.min.y;
        if tracks_view {
            for (i, (col, x, w)) in layout(list.width(), &o.cols).into_iter().enumerate() {
                let Some((name, key)) = col_name(col) else { continue };
                let x = list.min.x + x + 8.0;
                let Some(key) = key else {
                    p.text(pos2(x, top + 8.0), Align2::LEFT_CENTER, name, caption(), t::INK_FAINT);
                    continue;
                };
                let hr = Rect::from_min_size(pos2(x - 6.0, top - 3.0), vec2((w - 4.0).clamp(40.0, 72.0), 24.0));
                let (c, resp) = control(ui, &mut self.touches, hr, Id::NULL.with(("sort", i)), || WidgetInfo::labeled(egui::Role::Button, true, format!("Sort by {name}")));
                focus_ring(&p, hr, &resp, t::R_CONTROL);
                let active = self.lib.sort.filter(|(k, _)| *k == key);
                p.text(pos2(x, top + 8.0), Align2::LEFT_CENTER, name, caption(), if active.is_some() || c.hovered { t::INK_MUTED } else { t::INK_FAINT });
                if let Some((_, asc)) = active {
                    let tw = p.layout_no_wrap(name.to_string(), caption(), t::INK).size().x;
                    caret(&p, pos2(x + tw + 8.0, top + 8.0), asc, t::INK_MUTED);
                }
                if c.clicked {
                    self.lib.sort_by(key);
                }
            }
            top += 18.0;
        }
        let rows_rect = Rect::from_min_max(pos2(list.min.x, top), list.max);
        let row_h = o.row_h;
        let loaded: Vec<Option<PathBuf>> = self.decks.iter().map(|d| d.track.as_ref().map(|t| t.path.clone())).collect();
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rows_rect));
        child.spacing_mut().item_spacing.y = 0.0;
        let selected = self.lib.selected();
        let scroll_to = std::mem::take(&mut self.scroll_to_selected);
        let mut clicked: Option<usize> = None;
        let mut entered: Option<usize> = None;
        let mut load_req: Option<usize> = None;
        let id_salt = format!("rows{:?}{}{}", self.lib.node(), self.lib.depth(), o.row_h);
        let mut area = egui::ScrollArea::vertical().id_salt(id_salt).auto_shrink([false, false]);
        if scroll_to {
            // Rows outside the viewport aren't laid out, so scroll by arithmetic.
            let top = selected as f32 * row_h;
            let vis = rows_rect.height();
            let mut off = self.browser_offset;
            if top < off {
                off = top;
            } else if top + row_h > off + vis {
                off = top + row_h - vis;
            }
            area = area.vertical_scroll_offset(off.max(0.0));
        }
        // Covers and previews for the rows that might be visible (local files' embedded art
        // would mean opening every file, so only stick tracks list artwork).
        let first = (self.browser_offset / row_h).max(0.0) as usize;
        let vis_rows = (rows_rect.height() / row_h) as usize + 2;
        let visible = first..(first + vis_rows).min(rows.len());
        let arts: HashMap<usize, Option<crate::art::ArtKey>> = visible
            .clone()
            .filter_map(|i| match &rows[i] {
                Row::Track(e) if e.rb.is_some() && o.art => Some((i, self.art_key(e))),
                _ => None,
            })
            .collect();
        let mode = self.config.waveform_color;
        let mut previews: HashMap<usize, std::sync::Arc<Vec<crate::wave::Column>>> = HashMap::new();
        if o.cols.preview {
            for i in visible {
                if let Row::Track(e) = &rows[i] {
                    let src = self.preview_source(e);
                    if let Some(pv) = self.previews.get(&e.path, mode, || src) {
                        previews.insert(i, pv);
                    }
                }
            }
        }
        let played = &self.lib.played;
        let tags: std::collections::HashSet<PathBuf> = self.lib.tags.iter().map(|e| e.path.clone()).collect();
        let art = &mut self.art;
        let cols = layout(list.width() - 12.0, &o.cols);
        let small = (o.title - 3.0).max(12.0);
        let out = area.show_rows(&mut child, row_h, rows.len(), |ui, range| {
            for i in range {
                let (rr, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                let p = ui.painter();
                if i == selected {
                    p.rect_filled(rr, CornerRadius::same(t::R_CONTROL), t::ACCENT.gamma_multiply(0.2));
                    p.rect_stroke(rr, CornerRadius::same(t::R_CONTROL), Stroke::new(1.0, t::ACCENT.gamma_multiply(0.6)), StrokeKind::Inside);
                } else if resp.hovered() {
                    p.rect_filled(rr, CornerRadius::same(t::R_CONTROL), t::RAISED);
                } else if i % 2 == 1 {
                    p.rect_filled(rr, 0, t::CHROME.gamma_multiply(0.6));
                }
                let y = rr.center().y;
                match &rows[i] {
                    Row::Nav { label, detail, kind, .. } => {
                        resp.widget_info(|| WidgetInfo::labeled(egui::Role::Button, true, label));
                        nav_icon(p, pos2(rr.min.x + 20.0, y), *kind);
                        text_fit(p, pos2(rr.min.x + 42.0, y), Align2::LEFT_CENTER, label, semi(o.title), t::INK, rr.width() * 0.55);
                        text_fit(p, pos2(rr.max.x - 30.0, y), Align2::RIGHT_CENTER, detail, cond(small + 2.0), t::INK_FAINT, rr.width() * 0.4);
                        p.text(pos2(rr.max.x - 12.0, y), Align2::CENTER_CENTER, "›", semi(18.0), t::INK_FAINT);
                        if resp.clicked() {
                            entered = Some(i);
                        }
                    }
                    Row::Track(e) => {
                        resp.widget_info(|| WidgetInfo::selected(egui::Role::Button, true, i == selected, format!("{} — {}", e.title, e.artist)));
                        let was_played = played.contains(&e.path);
                        let is_loaded = loaded.iter().any(|lp| lp.as_ref() == Some(&e.path));
                        let mut bx = rr.min.x + 10.0;
                        if was_played && !is_loaded {
                            // Played this session: a tick, and the row in muted ink.
                            let c = pos2(bx + 4.0, y);
                            let st = Stroke::new(2.0, t::PLAY);
                            p.line_segment([c + vec2(-5.0, 0.0), c + vec2(-1.5, 3.5)], st);
                            p.line_segment([c + vec2(-1.5, 3.5), c + vec2(5.0, -4.0)], st);
                        }
                        for (d, lp) in loaded.iter().enumerate() {
                            if lp.as_ref() == Some(&e.path) {
                                p.circle_filled(pos2(bx, y), 8.0, t::DECK_TINT[d].gamma_multiply(0.25));
                                p.text(pos2(bx, y), Align2::CENTER_CENTER, format!("{}", d + 1), semi(12.0), t::DECK_TINT[d]);
                                bx += 18.0;
                            }
                        }
                        if tags.contains(&e.path) {
                            // Tagged: a small bookmark at the right of the marks.
                            let x = rr.min.x + 34.0;
                            let pts = vec![pos2(x - 4.0, y - 7.0), pos2(x + 4.0, y - 7.0), pos2(x + 4.0, y + 7.0), pos2(x, y + 3.0), pos2(x - 4.0, y + 7.0)];
                            p.add(egui::Shape::convex_polygon(pts, t::ACCENT, Stroke::NONE));
                        }
                        let title_ink = if was_played { t::INK_FAINT } else { t::INK };
                        for &(col, x, w) in &cols {
                            let x0 = rr.min.x + x + 8.0;
                            let w = w - 12.0;
                            match col {
                                Col::Marks => {}
                                Col::Title => {
                                    let mut tx = x0;
                                    if let Some(key) = arts.get(&i).cloned().flatten() {
                                        let s = (row_h - 6.0).min(30.0);
                                        if let Some(tex) = art.get(&key) {
                                            let ar = Rect::from_center_size(pos2(tx + s / 2.0, y), vec2(s, s));
                                            p.image(tex.id(), ar, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                                        }
                                        tx += s + 8.0;
                                    }
                                    text_fit(p, pos2(tx, y), Align2::LEFT_CENTER, &e.title, semi(o.title), title_ink, w - (tx - x0));
                                }
                                Col::Artist => text_fit(p, pos2(x0, y), Align2::LEFT_CENTER, &e.artist, cond(o.title), t::INK_MUTED, w),
                                Col::Preview => {
                                    if let Some(pv) = previews.get(&i) {
                                        let r = Rect::from_min_size(pos2(x0, rr.min.y + 4.0), vec2(w, row_h - 8.0));
                                        draw_preview(p, r, pv, None);
                                    }
                                }
                                Col::Bpm => {
                                    let bpm = e.bpm.map(|b| format!("{b:.1}")).unwrap_or_else(|| "–".into());
                                    p.text(pos2(x0, y), Align2::LEFT_CENTER, bpm, mono(small), if e.bpm.is_some() { t::INK } else { t::INK_FAINT });
                                }
                                Col::Key => text_fit(p, pos2(x0, y), Align2::LEFT_CENTER, &e.key, mono(small), t::INK_MUTED, w),
                                Col::Time => {
                                    let time = e.duration.map(|d| format!("{}:{:02}", d / 60, d % 60)).unwrap_or_default();
                                    p.text(pos2(x0, y), Align2::LEFT_CENTER, time, mono(small), t::INK_FAINT);
                                }
                                Col::Format => {
                                    p.text(pos2(x0, y), Align2::LEFT_CENTER, &e.ext, mono(small), t::INK_FAINT);
                                }
                            }
                        }
                        if resp.clicked() {
                            clicked = Some(i);
                        }
                        if resp.double_clicked() {
                            load_req = Some(i);
                        }
                    }
                }
            }
        });
        self.browser_offset = out.state.clamped_offset().y;
        if let Some(i) = clicked {
            self.lib.select(i);
        }
        if let Some(i) = entered {
            if self.lib.enter_row(i) {
                self.search.clear();
                self.scroll_to_selected = true;
            }
        }
        if let Some(i) = load_req {
            self.lib.select(i);
            // Double-tap loads to whichever deck isn't playing.
            let d = {
                let v = self.view.lock();
                if v.decks[0].playing && !v.decks[1].playing { 1 } else { 0 }
            };
            self.load_selected(d);
        }
    }

    /// The full-screen BROWSE (and TAG) view.
    pub(super) fn browse_view(&mut self, ui: &mut Ui, r: Rect, v: &EngineView) {
        self.previews.poll();
        let p = ui.painter().clone();
        // Narrow windows give the list more room.
        let narrow = r.width() < 1180.0;
        let side_w = if narrow { 172.0 } else { 210.0 };
        let info_w = if !self.config.browse_info { 0.0 } else if narrow { 248.0 } else { 300.0 };
        let side = Rect::from_min_size(r.min, vec2(side_w, r.height()));
        let info = Rect::from_min_max(pos2(r.max.x - info_w, r.min.y), r.max);
        let main = Rect::from_min_max(pos2(side.max.x + 6.0, r.min.y), pos2(info.min.x - if info_w > 0.0 { 6.0 } else { 0.0 }, r.max.y));

        // Side bar: each stick's lists, the music folder, history and the tag list.
        card(&p, side);
        let mut y = side.min.y + 8.0;
        let current = self.lib.root_node();
        for (i, (label, node, indent)) in self.lib.roots().into_iter().enumerate() {
            let h = if indent { 32.0 } else { 36.0 };
            let br = Rect::from_min_size(pos2(side.min.x + 6.0 + if indent { 14.0 } else { 0.0 }, y), vec2(side_w - 12.0 - if indent { 14.0 } else { 0.0 }, h));
            if br.max.y > side.max.y - 4.0 {
                break;
            }
            let on = current == node;
            let (c, resp) = control(ui, &mut self.touches, br, Id::NULL.with(("side", i)), || WidgetInfo::selected(egui::Role::Button, true, on, &label));
            if on {
                p.rect_filled(br, CornerRadius::same(t::R_CONTROL), t::ACCENT.gamma_multiply(0.2));
            } else if c.hovered {
                p.rect_filled(br, CornerRadius::same(t::R_CONTROL), t::RAISED);
            }
            focus_ring(&p, br, &resp, t::R_CONTROL);
            let font = if indent { cond(15.0) } else { semi(16.0) };
            let ink = if on { t::INK } else if indent { t::INK_MUTED } else { t::INK };
            text_fit(&p, pos2(br.min.x + 10.0, br.center().y), Align2::LEFT_CENTER, &label, font, ink, br.width() - 16.0);
            if c.clicked {
                self.lib.jump_to(node);
                self.search.clear();
                self.scroll_to_selected = true;
                if node == Node::Tag {
                    self.view_mode = super::views::View::Tag;
                } else if self.view_mode == super::views::View::Tag {
                    self.view_mode = super::views::View::Browse;
                }
            }
            y += h + 2.0;
        }

        // Main list.
        card(&p, main);
        let inner = main.shrink2(vec2(t::SP_M, 8.0));
        self.browse_header(ui, inner, v);
        let list = Rect::from_min_max(pos2(inner.min.x, inner.min.y + 80.0), inner.max);
        let kb_h = if self.keyboard_open { 236.0 } else { 0.0 };
        let list = Rect::from_min_max(list.min, pos2(list.max.x, list.max.y - kb_h));
        if self.lib.rows.is_empty() {
            let msg = if self.lib.track_filter.is_active() {
                "No tracks match the filter. Tap FILTER to change or clear it."
            } else if self.lib.node() == Node::Tag {
                "Tag tracks with TAG (or T) while browsing; they collect here for the set."
            } else if !self.lib.filter.is_empty() {
                "Nothing matches the search. BACK clears it."
            } else {
                "Nothing here."
            };
            p.text(list.center(), Align2::CENTER_CENTER, msg, t::body(), t::INK_MUTED);
        } else {
            let row_h = [28.0, 34.0, 42.0][self.config.browse_text.min(2) as usize];
            let title = [14.0, 16.0, 19.0][self.config.browse_text.min(2) as usize];
            self.rows_list(ui, list, ListOpts { row_h, title, cols: self.config.browse_cols, art: true });
        }
        if self.keyboard_open {
            let kr = Rect::from_min_max(pos2(main.min.x + 6.0, main.max.y - kb_h - 4.0), pos2(main.max.x - 6.0, main.max.y - 6.0));
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
        let row1 = Rect::from_min_size(inner.min, vec2(inner.width(), 34.0));
        let mut x = row1.min.x;
        if self.lib.depth() > 0 || !self.lib.filter.is_empty() {
            let br = Rect::from_min_size(pos2(x, row1.min.y + 1.0), vec2(74.0, 32.0));
            if button_tip(ui, &mut self.touches, br, Id::NULL.with("bback"), "‹ BACK", None, semi(14.0), "Up one level, or clear the search  ( Backspace )").clicked {
                self.browse_back();
            }
            x += 84.0;
        }
        let search_w = 250.0;
        let crumbs = self.lib.crumbs();
        let last = crumbs.len().saturating_sub(1);
        let mut job = LayoutJob::default();
        for (i, c) in crumbs.iter().enumerate() {
            let (font, col) = if i == last { (semi(16.0), t::INK) } else { (cond(16.0), t::INK_FAINT) };
            job.append(c, 0.0, egui::TextFormat::simple(font, col));
            if i != last {
                job.append("  ›  ", 0.0, egui::TextFormat::simple(cond(16.0), t::INK_FAINT));
            }
        }
        job.wrap = TextWrapping::truncate_at_width((row1.max.x - x - search_w - 70.0).max(40.0));
        let g = p.layout_job(job);
        p.galley(pos2(x, row1.center().y - g.size().y / 2.0), g, t::INK);
        let sr = Rect::from_min_size(pos2(row1.max.x - search_w - 64.0, row1.min.y + 2.0), vec2(search_w, 30.0));
        let te = egui::TextEdit::singleline(&mut self.search).hint_text("Search  ( / )").font(cond(16.0)).desired_width(sr.width());
        let resp = ui.put(sr, te);
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
        let kr = Rect::from_min_size(pos2(row1.max.x - 58.0, row1.min.y + 1.0), vec2(58.0, 32.0));
        if button_tip(ui, &mut self.touches, kr, Id::NULL.with("kbd"), "ABC", self.keyboard_open.then_some(t::ACCENT), semi(14.0), "On-screen keyboard for search").clicked {
            self.keyboard_open = !self.keyboard_open;
        }

        // Row 2: filter, columns, text size, info | tag, load.
        let y2 = row1.max.y + 8.0;
        let h = 34.0;
        let mut x = inner.min.x;
        let f = self.lib.track_filter;
        let ftip = match (f.bpm, f.key) {
            (None, None) => "Show only tracks that mix with the leading deck (BPM range, compatible key)".to_string(),
            _ => format!(
                "Filter on: {}{}",
                f.bpm.map(|(b, pct)| format!("{b:.1} BPM ±{pct:.0} %  ")).unwrap_or_default(),
                f.key.map(|k| format!("key {} and neighbours", crate::keys::label(k))).unwrap_or_default()
            ),
        };
        let mut tool = |app: &mut App, w: f32, id: &str, label: &str, lit: bool, tip: &str| -> bool {
            let br = Rect::from_min_size(pos2(x, y2), vec2(w, h));
            x += w + 6.0;
            button_tip(ui, &mut app.touches, br, Id::NULL.with(id), label, lit.then_some(t::ACCENT), semi(14.0), tip).clicked
        };
        let small = inner.width() < 700.0;
        if tool(self, if small { 70.0 } else { 82.0 }, "bfilter", "FILTER", f.is_active() || self.browse_popup == Some(Popup::Filter), &ftip) {
            self.browse_popup = if self.browse_popup == Some(Popup::Filter) { None } else { Some(Popup::Filter) };
        }
        if tool(self, if small { 52.0 } else { 90.0 }, "bcols", if small { "COLS" } else { "COLUMNS" }, self.browse_popup == Some(Popup::Columns), "Choose the columns shown") {
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
        if tool(self, if small { 52.0 } else { 64.0 }, "binfo", "INFO", self.config.browse_info, "Show details for the highlighted track") {
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
            rx -= lw + 4.0;
            let br = Rect::from_min_size(pos2(rx, y2), vec2(lw, h));
            let tip = if d == 0 { "Load the selected track on deck 1  ( Q )" } else { "Load the selected track on deck 2  ( P )" };
            if button_tip(ui, &mut self.touches, br, Id::NULL.with(("bload", d)), &format!("LOAD {}", d + 1), sel.is_some().then_some(t::DECK_TINT[d]), semi(15.0), tip).clicked {
                self.load_selected(d);
            }
        }
        rx -= 72.0;
        let tagged = sel.as_ref().is_some_and(|s| self.lib.is_tagged(&s.path));
        let br = Rect::from_min_size(pos2(rx, y2), vec2(68.0, h));
        let tip = if tagged { "Remove from the tag list  ( T )" } else { "Add to the tag list  ( T )" };
        if button_tip(ui, &mut self.touches, br, Id::NULL.with("btag"), if tagged { "TAGGED" } else { "TAG" }, tagged.then_some(t::ACCENT), semi(14.0), tip).clicked {
            if let Some(s) = sel {
                self.lib.toggle_tag(&s);
            }
        }
        if self.lib.node() == Node::Tag && !self.lib.tags.is_empty() {
            rx -= 112.0;
            let br = Rect::from_min_size(pos2(rx, y2), vec2(106.0, h));
            let confirm = self.clear_tags_confirm.is_some_and(|t| t.elapsed().as_secs_f32() < 3.0);
            if button_tip(ui, &mut self.touches, br, Id::NULL.with("bclear"), if confirm { "CLEAR ALL?" } else { "CLEAR ALL" }, confirm.then_some(t::DANGER), semi(14.0), "Empty the tag list (press twice)").clicked {
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
        let r = Rect::from_min_size(pos2(inner.min.x, inner.min.y + 80.0), vec2(470.0, if pop == Popup::Filter { 196.0 } else { 120.0 }));
        p.rect_filled(r.expand(4.0), CornerRadius::same(t::R_CARD), Color32::from_black_alpha(120));
        panel(&p, r);
        let inner = r.shrink(12.0);
        match pop {
            Popup::Filter => {
                let lead = lead_deck(v);
                let lead_bpm = lead.and_then(|d| v.decks[d].bpm);
                let lead_key = lead.and_then(|d| self.decks[d].track.as_ref()).and_then(|t| crate::keys::parse(&t.key));
                let src = match (lead, lead_bpm) {
                    (Some(d), Some(b)) => format!(
                        "Matching deck {}: {b:.1} BPM{}",
                        d + 1,
                        lead_key.map(|k| format!(", key {}", crate::keys::label(k))).unwrap_or_default()
                    ),
                    _ => "Load a track to filter against it".into(),
                };
                p.text(inner.min, Align2::LEFT_TOP, &src, cond(15.0), t::INK_MUTED);
                super::views::heading(&p, inner.min + vec2(0.0, 30.0), "BPM RANGE (HALF AND DOUBLE TEMPO COUNT TOO)");
                let pcts = [0.0, 2.0, 4.0, 6.0, 10.0];
                let cur = self.lib.track_filter.bpm.map(|(_, pct)| pcts.iter().position(|&x| x == pct).unwrap_or(0)).unwrap_or(0);
                let sr = Rect::from_min_size(inner.min + vec2(0.0, 48.0), vec2(inner.width(), 34.0));
                if let Some(i) = segmented(self, ui, sr, "fbpm", &["OFF", "±2 %", "±4 %", "±6 %", "±10 %"], cur, "Only tracks near the leading deck's BPM") {
                    let mut f = self.lib.track_filter;
                    f.bpm = (i > 0).then_some(()).and(lead_bpm).map(|b| (b, pcts[i]));
                    self.lib.set_track_filter(f);
                }
                super::views::heading(&p, inner.min + vec2(0.0, 92.0), "KEY");
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
                super::views::heading(&p, inner.min, "COLUMNS");
                let c = self.config.browse_cols;
                let items: [(&str, bool); 6] =
                    [("ARTIST", c.artist), ("PREVIEW", c.preview), ("BPM", c.bpm), ("KEY", c.key), ("TIME", c.time), ("TYPE", c.format)];
                let w = (inner.width() - 5.0 * 6.0) / 6.0;
                for (i, (name, on)) in items.iter().enumerate() {
                    let br = Rect::from_min_size(inner.min + vec2(i as f32 * (w + 6.0), 22.0), vec2(w, 34.0));
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
                let header_btns = Rect::from_min_size(pos2(r.min.x, r.min.y - 44.0), vec2(180.0, 40.0));
                if !r.expand(4.0).contains(pos) && !header_btns.contains(pos) {
                    self.browse_popup = None;
                }
            }
        }
    }

    /// Details of the highlighted track, CDJ INFO style.
    fn info_panel(&mut self, ui: &mut Ui, r: Rect, v: &EngineView) {
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
                p.text(ar.center(), Align2::CENTER_CENTER, "NO ARTWORK", caption(), t::INK_FAINT);
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
        facts.push(("BPM", e.bpm.map(|b| format!("{b:.2}")).unwrap_or_else(|| "–".into())));
        let lead_key = lead_deck(v).and_then(|d| self.decks[d].track.as_ref()).and_then(|t| crate::keys::parse(&t.key));
        let key_s = match crate::keys::parse(&e.key) {
            Some(k) => {
                let m = lead_key.is_some_and(|l| crate::keys::compatible(l, k));
                format!("{}  ({}){}", e.key, crate::keys::label(k), if m { "  · mixes with the playing deck" } else { "" })
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
                if row.rating > 0 {
                    facts.push(("RATING", "★".repeat(row.rating.min(5) as usize)));
                }
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
        if self.lib.played.contains(&e.path) {
            facts.push(("PLAYED", "Already played this session".into()));
        }
        for (k, val) in facts {
            if y > inner.max.y - 44.0 {
                break;
            }
            p.text(pos2(inner.min.x, y), Align2::LEFT_TOP, k, caption(), t::INK_FAINT);
            text_fit(&p, pos2(inner.min.x + 72.0, y - 1.0), Align2::LEFT_TOP, &val, cond(15.0), t::INK, inner.width() - 72.0);
            y += 22.0;
        }
    }

    /// QWERTY keyboard for searching without a physical keyboard (Game Mode).
    fn on_screen_keyboard(&mut self, ui: &mut Ui, r: Rect) {
        let p = ui.painter().clone();
        panel(&p, r);
        let rows: [&str; 4] = ["1234567890", "QWERTYUIOP", "ASDFGHJKL", "ZXCVBNM"];
        let inner = r.shrink(8.0);
        let gap = 5.0;
        let kh = (inner.height() - 4.0 * gap) / 5.0;
        let kw = (inner.width() - 9.0 * gap) / 10.0;
        let mut typed: Option<String> = None;
        let mut back = false;
        for (ri, row) in rows.iter().enumerate() {
            let n = row.chars().count() as f32;
            let off = (10.0 - n) * (kw + gap) / 2.0;
            for (ci, ch) in row.chars().enumerate() {
                let kr = Rect::from_min_size(pos2(inner.min.x + off + ci as f32 * (kw + gap), inner.min.y + ri as f32 * (kh + gap)), vec2(kw, kh));
                if button(ui, &mut self.touches, kr, Id::NULL.with(("key", ri, ci)), &ch.to_string(), None, semi(17.0)).clicked {
                    typed = Some(ch.to_ascii_lowercase().to_string());
                }
            }
        }
        // Bottom row: CLEAR, SPACE, DEL, DONE.
        let y = inner.min.y + 4.0 * (kh + gap);
        let specs: [(&str, f32); 4] = [("CLEAR", 2.0), ("SPACE", 4.0), ("DEL", 2.0), ("DONE", 2.0)];
        let mut x = inner.min.x;
        for (i, (label, units)) in specs.iter().enumerate() {
            let w = units * kw + (units - 1.0) * gap;
            let kr = Rect::from_min_size(pos2(x, y), vec2(w, kh));
            let lit = (*label == "DONE").then_some(t::ACCENT);
            if button(ui, &mut self.touches, kr, Id::NULL.with(("keyx", i)), label, lit, semi(15.0)).clicked {
                match i {
                    0 => {
                        self.search.clear();
                        typed = Some(String::new());
                    }
                    1 => typed = Some(" ".into()),
                    2 => back = true,
                    _ => self.keyboard_open = false,
                }
            }
            x += w + gap;
        }
        if back {
            self.search.pop();
            typed = Some(String::new());
        }
        if let Some(s) = typed {
            self.search.push_str(&s);
            let f = self.search.clone();
            self.lib.set_filter(&f);
            self.scroll_to_selected = true;
        }
        let _ = Key::A;
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Popup {
    Filter,
    Columns,
}

/// Draw a whole-track preview into `r` (bars from the bottom), with a playhead if given.
pub(super) fn draw_preview(p: &egui::Painter, r: Rect, cols: &[crate::wave::Column], play: Option<f32>) {
    if cols.is_empty() {
        return;
    }
    let mut mesh = Mesh::default();
    let n = cols.len() as f32;
    let w = r.width() / n;
    for (i, c) in cols.iter().enumerate() {
        let x = r.min.x + i as f32 * w;
        for &(h, col) in &c.layers[..c.n] {
            let hh = h * r.height();
            mesh.add_colored_rect(Rect::from_min_max(pos2(x, r.max.y - hh), pos2(x + w.max(1.0), r.max.y)), col.gamma_multiply(0.85));
        }
    }
    p.add(mesh);
    if let Some(f) = play {
        let x = r.min.x + f * r.width();
        p.vline(x, r.y_range(), Stroke::new(1.5, t::INK));
    }
}

#[allow(dead_code)]
fn _unused(_: &library::Library) {}
