//! The custom controls and drawing helpers every screen is built from. Each control takes
//! input from touch (per finger), mouse or keyboard through [`control`], and reports itself
//! to screen readers.

use crate::ui::prelude::*;
use crate::ui::App;
use crate::library::NavKind;

pub(crate) fn text_fit(p: &Painter, pos: Pos2, anchor: Align2, s: &str, font: FontId, color: Color32, max_w: f32) {
    let mut job = LayoutJob::simple_singleline(s.to_owned(), font, color);
    job.wrap = TextWrapping::truncate_at_width(max_w.max(1.0));
    let g = p.layout_job(job);
    let r = anchor.anchor_size(pos, g.size());
    p.galley(r.min, g, color);
}

pub(crate) fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

pub(crate) fn card(p: &Painter, r: Rect) {
    tile(p, r, t::SURFACE);
}

/// A flat, square block of colour. Tiles are laid edge to edge; each leaves a 1 px seam of
/// whatever is behind it, so neighbours still read as separate keys without outlines.
pub(crate) fn tile(p: &Painter, r: Rect, fill: Color32) {
    p.rect_filled(r.shrink(0.5), CornerRadius::ZERO, fill);
}

/// The "on" mark of a lit tile: a bar along its bottom edge, like the LED strip on a key.
pub(crate) fn lit_bar(p: &Painter, r: Rect, col: Color32) {
    let r = r.shrink(0.5);
    p.rect_filled(Rect::from_min_max(pos2(r.min.x, r.max.y - 3.0), r.max), CornerRadius::ZERO, col);
}

/// Shared input for every custom control: touch (per finger) or mouse/keyboard.
/// A focused control activated with Enter/Space reports `key = true`.
pub(crate) fn control(ui: &Ui, tc: &mut Touches, r: Rect, id: Id, info: impl Fn() -> WidgetInfo) -> (crate::ui::touch::Ctl, egui::Response) {
    let resp = ui.interact(r, id, Sense::click_and_drag());
    resp.widget_info(info);
    // Only the visible part takes touches (a control scrolled out of view takes none).
    let mut c = tc.ctl(r.intersect(ui.clip_rect()), id, &resp);
    c.key = resp.has_focus() && ui.input(|i| i.key_pressed(Key::Enter) || i.key_pressed(Key::Space));
    // A press and release inside one frame (a quick tap, or a synthetic click) never shows
    // as `down`: report it as a momentary press so hold-style controls still respond.
    if c.clicked && !c.released && !c.down {
        c.key = true;
        c.pressed = true;
    }
    // Keyboard activation counts as a click in touch mode too.
    c.clicked |= c.key;
    (c, resp)
}

/// A tap target over something already drawn (a readout), with a focus ring and tooltip.
pub(crate) fn invisible_button(ui: &Ui, tc: &mut Touches, r: Rect, id: Id, name: &str, tip: &str) -> crate::ui::touch::Ctl {
    let n = name.to_string();
    let (c, resp) = control(ui, tc, r, id, || WidgetInfo::labeled(egui::Role::Button, true, &n));
    focus_ring(ui.painter(), r, &resp, t::R_CONTROL);
    if !tc.touch_mode() {
        resp.on_hover_text(tip);
    }
    c
}

/// −1 / +1 when ← / → is pressed while `resp` has keyboard focus.
pub(crate) fn focused_arrow(ui: &Ui, resp: &egui::Response) -> f64 {
    if !resp.has_focus() {
        return 0.0;
    }
    lock_arrows(ui, resp);
    ui.input(|i| i.key_pressed(Key::ArrowRight) as i32 - i.key_pressed(Key::ArrowLeft) as i32) as f64
}

/// Keep focus on a control that uses the arrow keys itself. Otherwise egui moves focus to the
/// neighbouring control on the first press.
pub(crate) fn lock_arrows(ui: &Ui, resp: &egui::Response) {
    if resp.has_focus() {
        let filter = egui::EventFilter { horizontal_arrows: true, vertical_arrows: true, ..Default::default() };
        ui.memory_mut(|m| m.set_focus_lock_filter(resp.id, filter));
    }
}

/// Like [`button`], with a tooltip (for terse labels and to show the keyboard shortcut).
#[allow(clippy::too_many_arguments)]
pub(crate) fn button_tip(ui: &Ui, tc: &mut Touches, r: Rect, id: Id, label: &str, lit: Option<Color32>, font: FontId, tip: &str) -> crate::ui::touch::Ctl {
    let (c, resp) = button_resp(ui, tc, r, id, label, lit, font);
    if crate::ui::icons::strip(label).is_empty() {
        // Icon-only: the tooltip names it for screen readers.
        let (name, sel) = (tip.split("  (").next().unwrap_or(tip).to_string(), lit.is_some());
        resp.widget_info(|| WidgetInfo::selected(egui::Role::Button, true, sel, &name));
    }
    if !tc.touch_mode() {
        resp.on_hover_text(tip);
    }
    c
}

/// A ring just inside a focused control's edge (controls sit flush, so outside it would
/// cover the neighbours).
pub(crate) fn focus_ring(p: &Painter, r: Rect, resp: &egui::Response, radius: u8) {
    if resp.has_focus() {
        p.rect_stroke(r.shrink(0.5), CornerRadius::same(radius), Stroke::new(2.0, t::ACCENT), StrokeKind::Inside);
    }
}

/// A flat hardware-style button. `lit` lights it in the given colour.
pub(crate) fn button(ui: &Ui, tc: &mut Touches, r: Rect, id: Id, label: &str, lit: Option<Color32>, font: FontId) -> crate::ui::touch::Ctl {
    button_resp(ui, tc, r, id, label, lit, font).0
}

pub(crate) fn button_resp(ui: &Ui, tc: &mut Touches, r: Rect, id: Id, label: &str, lit: Option<Color32>, font: FontId) -> (crate::ui::touch::Ctl, egui::Response) {
    let l = crate::ui::icons::strip(label);
    let selected = lit.is_some();
    let (c, resp) = control(ui, tc, r, id, || WidgetInfo::selected(egui::Role::Button, true, selected, &l));
    let p = ui.painter();
    let (fill, ink) = match lit {
        // Label mixed toward INK so it keeps ≥ 4.5:1 on its own tinted fill (REV, DELETE).
        Some(col) => (col.gamma_multiply(0.22), mix(col, t::INK, 0.4)),
        None if c.down || c.hovered => (t::HOVER, t::INK),
        None => (t::RAISED, t::INK_MUTED),
    };
    let fill = if c.down && lit.is_some() { fill.gamma_multiply(1.4) } else { fill };
    tile(p, r, fill);
    if let Some(col) = lit {
        lit_bar(p, r, col);
    }
    // Shrink a label that would overflow (glance mode on a narrow window).
    let room = r.width() - 6.0;
    let w = p.layout_no_wrap(label.to_owned(), font.clone(), ink).size().x;
    let font = if w > room { FontId::new(font.size * room / w, font.family) } else { font };
    p.text(r.center(), Align2::CENTER_CENTER, label, font, ink);
    focus_ring(p, r, &resp, t::R_CONTROL);
    (c, resp)
}

/// A big square transport button. With `icon`, the icon (solid) is drawn and `label` only names it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn round_button(ui: &Ui, tc: &mut Touches, r: Rect, id: Id, label: &str, icon: Option<&str>, col: Color32, lit: bool) -> crate::ui::touch::Ctl {
    let l = label.to_string();
    let (c, resp) = control(ui, tc, r, id, || WidgetInfo::labeled(egui::Role::Button, true, &l));
    let p = ui.painter();
    let center = r.center();
    let rad = r.width().min(r.height()) / 2.0;
    let fill = if lit { col.gamma_multiply(if c.down { 0.35 } else { 0.22 }) } else if c.down { t::HOVER } else { t::RAISED };
    tile(p, r, fill);
    lit_bar(p, r, if lit { col } else { col.gamma_multiply(0.25) });
    let ink = if lit { col } else { t::INK_MUTED };
    match icon {
        Some(i) => p.text(center, Align2::CENTER_CENTER, i, icons::fill(rad * 0.78), ink),
        None => p.text(center, Align2::CENTER_CENTER, label, semi(sz(15.0, 17.0)), ink),
    };
    focus_ring(p, r, &resp, t::R_CONTROL);
    c
}

pub(crate) fn arc_points(c: Pos2, r: f32, a0: f32, a1: f32) -> Vec<Pos2> {
    let n = (((a1 - a0).abs() * r / 3.0) as usize).max(4);
    (0..=n)
        .map(|i| {
            let a = a0 + (a1 - a0) * i as f32 / n as f32;
            c + r * Vec2::angled(a)
        })
        .collect()
}

/// Rotary knob: drag to turn, double-tap to centre, arrow keys when focused.
#[allow(clippy::too_many_arguments)]
pub(crate) fn knob(ui: &Ui, tc: &mut Touches, r: Rect, id: Id, value: f32, label: &str, color: Color32, bipolar: bool) -> Option<f32> {
    let l = label.to_string();
    let (c, resp) = control(ui, tc, r, id, || WidgetInfo::slider(true, value as f64, &l));
    let p = ui.painter();
    let label_h = 14.0;
    let kr = ((r.height() - label_h).min(r.width()) * 0.5 - 3.0).max(6.0);
    let center = pos2(r.center().x, r.min.y + 2.0 + kr);
    use std::f32::consts::PI;
    let (start, sweep) = (0.75 * PI, 1.5 * PI);
    let ang = |v: f32| start + sweep * v.clamp(0.0, 1.0);
    p.circle_filled(center, kr, t::RAISED);
    p.add(Shape::line(arc_points(center, kr - 2.0, ang(0.0), ang(1.0)), Stroke::new(3.0, t::INSET)));
    let from = if bipolar { 0.5 } else { 0.0 };
    if (value - from).abs() > 0.004 {
        p.add(Shape::line(arc_points(center, kr - 2.0, ang(from.min(value)), ang(from.max(value))), Stroke::new(3.0, color)));
    }
    let tip = center + (kr - 6.0) * Vec2::angled(ang(value));
    p.line_segment([center + (kr * 0.25) * Vec2::angled(ang(value)), tip], Stroke::new(2.0, t::INK));
    if c.hovered || c.down {
        p.circle_stroke(center, kr, Stroke::new(1.0, t::INK_FAINT));
    }
    if resp.has_focus() {
        p.circle_stroke(center, kr + 3.0, Stroke::new(2.0, t::ACCENT));
    }
    p.text(pos2(r.center().x, r.max.y), Align2::CENTER_BOTTOM, label, caption(), t::INK_MUTED);
    if c.double {
        return Some(if bipolar { 0.5 } else { value });
    }
    if c.down && c.delta != Vec2::ZERO {
        let nv = (value + (c.delta.x - c.delta.y) / 160.0).clamp(0.0, 1.0);
        if nv != value {
            return Some(nv);
        }
    }
    if resp.has_focus() {
        lock_arrows(ui, &resp);
        let step = ui.input(|i| {
            (i.key_pressed(Key::ArrowUp) || i.key_pressed(Key::ArrowRight)) as i32 - (i.key_pressed(Key::ArrowDown) || i.key_pressed(Key::ArrowLeft)) as i32
        });
        if step != 0 {
            return Some((value + step as f32 * 0.05).clamp(0.0, 1.0));
        }
    }
    None
}

/// Vertical fader. `value` 1.0 = top.
#[allow(clippy::too_many_arguments)]
pub(crate) fn vfader(ui: &Ui, tc: &mut Touches, r: Rect, id: Id, value: f32, color: Color32, label: &str) -> Option<f32> {
    let l = label.to_string();
    let (c, resp) = control(ui, tc, r, id, || WidgetInfo::slider(true, value as f64, &l));
    let p = ui.painter();
    let track = Rect::from_center_size(r.center(), vec2(4.0, r.height() - 16.0));
    p.rect_filled(track, 0, t::INSET);
    let y = track.max.y - value.clamp(0.0, 1.0) * track.height();
    let cap = Rect::from_center_size(pos2(r.center().x, y), vec2(r.width() - 2.0, 16.0));
    p.rect_filled(cap, CornerRadius::ZERO, if c.down { t::HOVER } else { t::RAISED });
    focus_ring(p, cap, &resp, t::R_CONTROL);
    p.hline(cap.x_range().shrink(3.0), y, Stroke::new(2.0, color));
    if c.down || c.clicked {
        if let Some(pos) = c.pos {
            return Some(((track.max.y - pos.y) / track.height()).clamp(0.0, 1.0));
        }
    }
    if resp.has_focus() {
        lock_arrows(ui, &resp);
        let step = ui.input(|i| i.key_pressed(Key::ArrowUp) as i32 - i.key_pressed(Key::ArrowDown) as i32);
        if step != 0 {
            return Some((value + step as f32 * 0.05).clamp(0.0, 1.0));
        }
    }
    None
}

/// Horizontal slider: drag or tap to set, ← → when focused. The label is drawn inside.
pub(crate) fn hslider(ui: &Ui, tc: &mut Touches, r: Rect, id: Id, value: f32, color: Color32, label: &str) -> Option<f32> {
    let l = label.to_string();
    let (c, resp) = control(ui, tc, r, id, || WidgetInfo::slider(true, value as f64, &l));
    let p = ui.painter();
    p.rect_filled(r, CornerRadius::same(t::R_CONTROL), t::INSET);
    let fill = Rect::from_min_size(r.min, vec2(r.width() * value.clamp(0.0, 1.0), r.height()));
    p.rect_filled(fill, CornerRadius::same(t::R_CONTROL), color.gamma_multiply(0.25));
    p.vline(fill.max.x, r.y_range().shrink(4.0), Stroke::new(2.0, color));
    p.text(r.left_center() + vec2(8.0, 0.0), Align2::LEFT_CENTER, "LEVEL", caption(), t::INK_MUTED);
    p.text(r.right_center() - vec2(8.0, 0.0), Align2::RIGHT_CENTER, format!("{:.0}", value * 100.0), mono(12.0), t::INK_MUTED);
    focus_ring(p, r, &resp, t::R_CONTROL);
    if c.down || c.clicked {
        if let Some(pos) = c.pos {
            return Some(((pos.x - r.min.x) / r.width()).clamp(0.0, 1.0));
        }
    }
    let step = focused_arrow(ui, &resp);
    if step != 0.0 {
        return Some((value + step as f32 * 0.05).clamp(0.0, 1.0));
    }
    None
}

pub(crate) fn meter(p: &Painter, r: Rect, level: f32) {
    p.rect_filled(r, 0, t::INSET);
    let segs = 14;
    let gap = 2.0;
    let seg_h = (r.height() - gap * (segs - 1) as f32) / segs as f32;
    let db = 20.0 * level.max(1e-6).log10();
    let lit = (((db + 42.0) / 42.0) * segs as f32).ceil() as i32;
    for i in 0..segs {
        let y1 = r.max.y - i as f32 * (seg_h + gap);
        let seg = Rect::from_min_max(pos2(r.min.x, y1 - seg_h), pos2(r.max.x, y1));
        let col = if i >= segs - 2 { t::DANGER } else if i >= segs - 5 { t::CUE } else { t::PLAY };
        p.rect_filled(seg, 0, if (i as i32) < lit { col } else { col.gamma_multiply(0.12) });
    }
}

/// rekordbox colour if the export assigned one, else the default pad colour.
pub(crate) fn hot_color(i: usize, rgb: Option<[u8; 3]>) -> Color32 {
    rgb.map(|[r, g, b]| Color32::from_rgb(r, g, b)).unwrap_or(t::HOT_CUE[i % 8])
}

pub(crate) fn fmt_time(secs: f64) -> String {
    // Round to tenths first so 119.97 s shows as 02:00.0, not 01:60.0.
    let tenths = (secs.max(0.0) * 10.0).round() as i64;
    format!("{:02}:{:02}.{}", tenths / 600, (tenths / 10) % 60, tenths % 10)
}

/// Small painted glyphs for browser rows (folder, drive, playlist, collection).
pub(crate) fn nav_icon(p: &Painter, c: Pos2, kind: NavKind) {
    let (icon, col) = match kind {
        NavKind::Folder => (icons::FOLDER, t::INK_MUTED),
        NavKind::Drive => (icons::USB, t::ACCENT),
        NavKind::Playlist => (icons::PLAYLIST, t::INK_MUTED),
        NavKind::Collection => (icons::TRACKS, t::INK_MUTED),
        NavKind::History => (icons::HISTORY, t::INK_MUTED),
        NavKind::Tag => (icons::TAG, t::ACCENT),
        NavKind::Suggest => (icons::SPARKLE, t::ACCENT),
    };
    p.text(c, Align2::CENTER_CENTER, icon, cond(18.0), col);
}

/// A full-width navigation row (side bars): icon, label, accent tint when selected.
pub(crate) fn nav_row(ui: &Ui, tc: &mut Touches, r: Rect, id: Id, icon: &str, label: &str, on: bool) -> crate::ui::touch::Ctl {
    let l = label.to_string();
    let (c, resp) = control(ui, tc, r, id, || WidgetInfo::selected(egui::Role::Button, true, on, &l));
    let p = ui.painter();
    if on {
        p.rect_filled(r, CornerRadius::ZERO, t::ACCENT.gamma_multiply(0.2));
        p.rect_filled(Rect::from_min_max(r.min, pos2(r.min.x + 3.0, r.max.y)), CornerRadius::ZERO, t::ACCENT);
    } else if c.hovered || c.down {
        p.rect_filled(r, CornerRadius::ZERO, t::RAISED);
    }
    focus_ring(p, r, &resp, t::R_CONTROL);
    let ink = if on { t::INK } else { t::INK_MUTED };
    p.text(pos2(r.min.x + 18.0, r.center().y), Align2::CENTER_CENTER, icon, cond(sz(18.0, 20.0)), if on { t::ACCENT } else { t::INK_FAINT });
    text_fit(p, pos2(r.min.x + 38.0, r.center().y), Align2::LEFT_CENTER, label, semi(sz(15.0, 17.0)), ink, r.width() - 44.0);
    c
}

/// The magnifier inside a search field (the field gets a matching left margin).
pub(crate) fn search_field(ui: &mut Ui, r: Rect, text: &mut String) -> egui::Response {
    let te = egui::TextEdit::singleline(text)
        .hint_text("Search  ( / )")
        .font(cond(sz(16.0, 18.0)))
        .margin(egui::Margin { left: 30, right: 6, top: 3, bottom: 3 })
        .desired_width(r.width());
    let resp = ui.put(r, te);
    let col = if resp.has_focus() { t::INK_MUTED } else { t::INK_FAINT };
    ui.painter().text(pos2(r.min.x + 15.0, r.center().y), Align2::CENTER_CENTER, icons::SEARCH, cond(16.0), col);
    resp
}

/// Breadcrumbs: earlier levels faint, the current one bright, carets between.
pub(crate) fn crumbs_job(crumbs: &[String], max_w: f32) -> LayoutJob {
    let last = crumbs.len().saturating_sub(1);
    let mut job = LayoutJob::default();
    for (i, c) in crumbs.iter().enumerate() {
        let (font, col) = if i == last { (semi(sz(16.0, 18.0)), t::INK) } else { (cond(sz(16.0, 18.0)), t::INK_FAINT) };
        job.append(c, 0.0, egui::TextFormat::simple(font, col));
        if i != last {
            job.append(&format!("  {}  ", icons::CARET_RIGHT), 0.0, egui::TextFormat::simple(cond(13.0), t::INK_FAINT));
        }
    }
    job.wrap = TextWrapping::truncate_at_width(max_w.max(40.0));
    job
}

/// Sort-direction caret drawn next to a column header.
pub(crate) fn caret(p: &Painter, at: Pos2, up: bool, col: Color32) {
    let s = if up { -1.0 } else { 1.0 };
    p.add(Shape::convex_polygon(vec![pos2(at.x - 4.0, at.y - 2.0 * s), pos2(at.x + 4.0, at.y - 2.0 * s), pos2(at.x, at.y + 3.0 * s)], col, Stroke::NONE));
}

/// Small helper for pages: a section heading.
pub(crate) fn heading(p: &egui::Painter, pos: egui::Pos2, s: &str) {
    p.text(pos, Align2::LEFT_TOP, s, caption(), t::INK_FAINT);
}

/// A row of mutually exclusive choices. Returns the index tapped.
pub(crate) fn segmented(app: &mut App, ui: &Ui, r: Rect, id: &str, labels: &[&str], selected: usize, tip: &str) -> Option<usize> {
    let w = r.width() / labels.len().max(1) as f32;
    let mut out = None;
    for (i, l) in labels.iter().enumerate() {
        let br = Rect::from_min_size(pos2(r.min.x + i as f32 * w, r.min.y), vec2(w, r.height()));
        if button_tip(ui, &mut app.touches, br, Id::NULL.with((id, i)), l, (i == selected).then_some(t::ACCENT), cond(sz(14.0, 16.0)), tip).clicked {
            out = Some(i);
        }
    }
    out
}

/// A panel for overlays: a raised block over whatever is behind it.
pub(crate) fn panel(p: &egui::Painter, r: Rect) {
    p.rect_filled(r, CornerRadius::ZERO, t::SURFACE);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_controls_reach_the_accessibility_tree() {
        let ctx = egui::Context::default();
        crate::ui::theme::install(&ctx);
        ctx.enable_accesskit();
        let mut tc = Touches::default();
        let mut out = None;
        for _ in 0..2 {
            let mut o = ctx.run_ui(Default::default(), |ui| {
                let r = Rect::from_min_size(pos2(10.0, 10.0), vec2(60.0, 32.0));
                button_tip(ui, &mut tc, r, Id::NULL.with("t"), "PLAY", None, semi(13.0), "Play/pause ( Space )");
            });
            o.textures_delta.clear(); // no renderer in tests
            out = Some(o);
        }
        let o = out.unwrap();
        let update = o.platform_output.accesskit_update.expect("accesskit update");
        let labels: Vec<String> = update.nodes.iter().filter_map(|(_, n)| n.label().map(str::to_string)).collect();
        assert!(labels.iter().any(|l| l == "PLAY"), "{labels:?}");
    }

    #[test]
    fn arrow_keys_turn_the_focused_knob_and_keep_focus() {
        let ctx = egui::Context::default();
        crate::ui::theme::install(&ctx);
        let mut tc = Touches::default();
        let ids = [Id::NULL.with("k1"), Id::NULL.with("k2")];
        let mut turns = [0; 2];
        for frame in 0..5 {
            let mut input = egui::RawInput::default();
            if frame >= 2 {
                input.events.push(egui::Event::Key { key: Key::ArrowRight, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() });
            }
            let mut o = ctx.run_ui(input, |ui| {
                for (i, id) in ids.iter().enumerate() {
                    let r = Rect::from_min_size(pos2(10.0 + i as f32 * 70.0, 10.0), vec2(60.0, 60.0));
                    if knob(ui, &mut tc, r, *id, 0.5, "K", t::ACCENT, true).is_some() {
                        turns[i] += 1;
                    }
                }
                if frame == 0 {
                    ui.memory_mut(|m| m.request_focus(ids[0]));
                }
            });
            o.textures_delta.clear(); // no renderer in tests
        }
        assert_eq!(turns, [3, 0]);
        assert_eq!(ctx.memory(|m| m.focused()), Some(ids[0]));
    }

    #[test]
    fn time_format_rounds_cleanly() {
        assert_eq!(super::fmt_time(119.97), "02:00.0");
        assert_eq!(super::fmt_time(61.24), "01:01.2");
        assert_eq!(super::fmt_time(-3.0), "00:00.0");
    }
}
