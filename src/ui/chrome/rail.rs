//! The column of big screen buttons down the left, like the SOURCE / BROWSE / TAG LIST /
//! SEARCH / MENU buttons beside a CDJ's screen: always in the same place, and big enough to
//! hit with a thumb on a 7" screen (about 11 × 9 mm on the Steam Deck).

use crate::ui::prelude::*;
use crate::ui::App;

use std::time::Duration;

use crate::engine::EngineView;

/// Rail width for a window at least `WIDE` points wide, and below it.
pub const RAIL_W: f32 = 84.0;
pub const RAIL_W_NARROW: f32 = 72.0;
/// Glance mode: icons only.
pub const RAIL_W_ICONS: f32 = 62.0;
pub const WIDE: f32 = 1180.0;

#[derive(Clone, Copy, PartialEq)]
enum Item {
    View(View),
    Search,
    Exit,
}

const ITEMS: [Item; 6] = [
    Item::View(View::Perform),
    Item::View(View::Wave),
    Item::View(View::Browse),
    Item::View(View::Tag),
    Item::Search,
    Item::View(View::Settings),
];

impl App {
    /// The rail in `r`: the views and SEARCH from the top, EXIT at the bottom.
    pub(crate) fn rail(&mut self, ui: &Ui, r: Rect, v: &EngineView) {
        let gap = 6.0;
        let exit_h = 56.0;
        let n = ITEMS.len() as f32;
        let tile_h = ((r.height() - exit_h - 16.0 - gap * (n - 1.0)) / n).clamp(50.0, 80.0);
        let mut y = r.min.y;
        for it in ITEMS {
            self.rail_tile(ui, Rect::from_min_size(pos2(r.min.x, y), vec2(r.width(), tile_h)), it, v);
            y += tile_h + gap;
        }
        self.rail_tile(ui, Rect::from_min_max(pos2(r.min.x, r.max.y - exit_h), r.max), Item::Exit, v);
    }

    /// SEARCH is lit while searching in BROWSE; BROWSE while browsing without a search.
    fn searching(&self) -> bool {
        self.view_mode == View::Browse && (self.keyboard_open || !self.search.is_empty())
    }

    fn rail_tile(&mut self, ui: &Ui, r: Rect, it: Item, v: &EngineView) {
        let confirm = self.exit_confirm.is_some_and(|t| t.elapsed() < Duration::from_secs(3));
        let (icon, label, tip, on, danger) = match it {
            Item::View(view) => {
                let on = self.view_mode == view && !(view == View::Browse && self.searching());
                (view.icon(), view.name(), view.tip().to_string(), on, false)
            }
            Item::Search => (icons::SEARCH, "SEARCH", "Search the library  ( / )".to_string(), self.searching(), false),
            Item::Exit => {
                let busy = self.recorder.is_recording() || v.decks.iter().any(|d| d.playing);
                let tip = match (confirm, busy) {
                    (true, true) => "Press again to quit: playback stops and any recording is saved",
                    (true, false) => "Press again to quit",
                    (false, _) => "Quit CDeckJ. Press twice  ( Ctrl+Q )",
                };
                (icons::POWER, if confirm { "EXIT?" } else { "EXIT" }, tip.to_string(), false, confirm)
            }
        };
        let name = label.trim_end_matches('?').to_string();
        let (c, resp) = control(ui, &mut self.touches, r, Id::NULL.with(("rail", label)), || WidgetInfo::selected(egui::Role::Button, true, on, &name));
        let p = ui.painter();
        let accent = if danger { t::DANGER } else { t::ACCENT };
        let (fill, stroke) = if on || danger {
            (accent.gamma_multiply(if c.down { 0.3 } else { 0.2 }), accent)
        } else if c.down {
            (t::HOVER, t::INK_FAINT)
        } else if c.hovered {
            (t::HOVER, t::BORDER)
        } else {
            (t::SURFACE, t::BORDER)
        };
        let radius = CornerRadius::same(t::R_CARD);
        p.rect_filled(r, radius, fill);
        p.rect_stroke(r, radius, Stroke::new(1.0, stroke), StrokeKind::Inside);
        focus_ring(p, r, &resp, t::R_CARD);
        let (icon_col, label_col) = if on || danger { (accent, mix(accent, t::INK, 0.55)) } else { (t::INK_MUTED, t::INK_MUTED) };
        let c_y = r.center().y;
        if t::glance() && !danger {
            // Icons only; the EXIT? confirmation keeps its word.
            p.text(pos2(r.center().x, c_y), Align2::CENTER_CENTER, icon, cond((r.width() * 0.5).min(32.0)), icon_col);
        } else {
            let icon_size = (r.height() * 0.34).clamp(20.0, 26.0);
            p.text(pos2(r.center().x, c_y - 9.0), Align2::CENTER_CENTER, icon, cond(icon_size), icon_col);
            p.text(pos2(r.center().x, c_y + icon_size * 0.5 + 5.0), Align2::CENTER_CENTER, label, semi(12.5), label_col);
        }
        if !self.touches.touch_mode() {
            resp.on_hover_text(tip);
        }
        if !c.clicked {
            return;
        }
        match it {
            Item::View(view) => {
                if view == View::Browse && self.searching() {
                    // From SEARCH back to plain browsing.
                    self.keyboard_open = false;
                    self.search.clear();
                    self.lib.set_filter("");
                }
                self.set_view(view);
            }
            Item::Search => {
                self.set_view(View::Browse);
                // Like a CDJ's SEARCH: straight to the keyboard (a physical one works too).
                self.focus_search = true;
                self.keyboard_open = true;
            }
            Item::Exit => self.exit_press(ui.ctx()),
        }
    }
}
