//! The scrolling track and folder list shared by PERFORM's library panel and the BROWSE view:
//! columns, sorting, deck badges, played ticks, tag marks, artwork and previews.

use crate::ui::prelude::*;
use std::collections::HashMap;
use std::path::PathBuf;
use egui::Mesh;
use crate::config::BrowseCols;
use crate::library::{LibTrack, Row, SortKey};
use crate::ui::App;

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
pub(crate) enum Col {
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
pub(crate) fn layout(w: f32, c: &BrowseCols) -> Vec<(Col, f32, f32)> {
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
    let marks = 40.0;
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

pub(crate) fn col_name(c: Col) -> Option<(&'static str, Option<SortKey>)> {
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
pub(crate) fn lead_deck(v: &EngineView) -> Option<usize> {
    (0..DECKS)
        .find(|&d| v.decks[d].is_master && v.decks[d].loaded && v.decks[d].playing)
        .or_else(|| (0..DECKS).find(|&d| v.decks[d].playing))
        .or_else(|| (0..DECKS).find(|&d| v.decks[d].loaded))
}

impl App {
    pub(crate) fn preview_source(&self, e: &LibTrack) -> crate::ui::cache::preview::Source {
        match e.rb.and_then(|(u, id)| self.lib.usbs.get(u).map(|ex| (ex.clone(), id))) {
            Some((ex, id)) => crate::ui::cache::preview::Source::Rekordbox(ex, id),
            None => crate::ui::cache::preview::Source::Local,
        }
    }

    /// The scrolling track/folder list.
    pub(crate) fn rows_list(&mut self, ui: &mut Ui, list: Rect, o: ListOpts) {
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
        let arts: HashMap<usize, Option<crate::ui::cache::art::ArtKey>> = visible
            .clone()
            .filter_map(|i| match &rows[i] {
                Row::Track(e) if e.rb.is_some() && o.art => Some((i, self.art_key(e))),
                _ => None,
            })
            .collect();
        let mode = self.config.waveform_color;
        let mut previews: HashMap<usize, std::sync::Arc<Vec<crate::track::wave::Column>>> = HashMap::new();
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
                        p.text(pos2(rr.max.x - 12.0, y), Align2::CENTER_CENTER, icons::CARET_RIGHT, cond(15.0), t::INK_FAINT);
                        if resp.clicked() {
                            entered = Some(i);
                        }
                    }
                    Row::Track(e) => {
                        resp.widget_info(|| WidgetInfo::selected(egui::Role::Button, true, i == selected, format!("{} — {}", e.title, e.artist)));
                        let was_played = played.contains(&e.path);
                        let is_loaded = loaded.iter().any(|lp| lp.as_ref() == Some(&e.path));
                        // Marks column: deck badge (or played tick) centred 18 pt in, then the
                        // tag bookmark; the title starts right after.
                        let mut bx = rr.min.x + 18.0;
                        let mut used = false;
                        if was_played && !is_loaded {
                            // Played this session: a tick, and the row in muted ink.
                            p.text(pos2(bx, y), Align2::CENTER_CENTER, icons::CHECK, cond(16.0), t::PLAY);
                            used = true;
                        }
                        for (d, lp) in loaded.iter().enumerate() {
                            if lp.as_ref() == Some(&e.path) {
                                p.circle_filled(pos2(bx, y), 9.0, mix(t::DECK_TINT[d], t::SURFACE, 0.7));
                                p.text(pos2(bx, y), Align2::CENTER_CENTER, format!("{}", d + 1), semi(12.0), t::DECK_TINT[d]);
                                // On both decks: the second badge overlaps the first.
                                bx += 11.0;
                                used = true;
                            }
                        }
                        if tags.contains(&e.path) {
                            let x = if used { rr.min.x + 34.0 } else { rr.min.x + 18.0 };
                            p.text(pos2(x, y), Align2::CENTER_CENTER, icons::TAG, icons::fill(16.0), t::ACCENT);
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
}

/// Draw a whole-track preview into `r` (bars from the bottom), with a playhead if given.
pub(crate) fn draw_preview(p: &egui::Painter, r: Rect, cols: &[crate::track::wave::Column], play: Option<f32>) {
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
