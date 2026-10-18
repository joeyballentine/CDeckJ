//! The one place visual decisions live.
//!
//! Scene: a dark booth, a 7" handheld screen at arm's length, glanced at between moves.
//! Dark only on purpose: a bright screen in a dark room ruins your eyes and the crowd's.
//! Signature: the stacked, scrolling three-band waveforms across the top.
//! Colour vocabulary is borrowed from the hardware itself: PLAY is green, CUE is amber.
//! Everything else stays cool: blue accents, teal loops, blue-graphite surfaces.

use std::sync::Arc;

use egui::epaint::text::VariationCoords;
use egui::{
    style::WidgetVisuals, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId,
    FontTweak, Stroke, Style, Theme, ThemePreference,
};

const fn c(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

// Cool blue-graphite neutrals, ~3 % luminance steps.
pub const CHROME: Color32 = c(0x101318);
pub const SURFACE: Color32 = c(0x161A20);
pub const RAISED: Color32 = c(0x1F242C);
pub const INSET: Color32 = c(0x0A0C10);
pub const BORDER: Color32 = c(0x2A3039);
pub const HOVER: Color32 = c(0x283039);

pub const INK: Color32 = c(0xE4E9F0);
pub const INK_MUTED: Color32 = c(0xA9B4C3);
/// Captions and placeholders: still ≥ 4.5:1 on the surfaces they sit on.
pub const INK_FAINT: Color32 = c(0x8893A3);

pub const PLAY: Color32 = c(0x47D16C);
pub const CUE: Color32 = c(0xF2A13B);
pub const SYNC: Color32 = c(0x5FC4F5);
/// Loops and loop regions: teal, to keep the palette cool.
pub const LOOP: Color32 = c(0x3CD4C4);
pub const DANGER: Color32 = c(0xF0675F);
/// Memory cues (rekordbox): red, as on the players.
pub const MEMORY_CUE: Color32 = c(0xE5484D);
/// Selection, focus and "on" state accent: a cool blue.
pub const ACCENT: Color32 = c(0x4D9CFF);

/// Deck 1 sky blue, deck 2 violet.
pub const DECK_TINT: [Color32; 2] = [c(0x5FC4F5), c(0xB497FF)];

/// Rekordbox-like hot cue colours, one per pad.
pub const HOT_CUE: [Color32; 8] = [
    c(0x3CD07A),
    c(0xF2A13B),
    c(0x5FB8F0),
    c(0xF2779B),
    c(0xB48CF2),
    c(0xE9D25A),
    c(0x4FD6D0),
    c(0xF06A5F),
];

// Waveform bands.

#[allow(dead_code)]
pub const SP_XS: f32 = 4.0;
pub const SP_S: f32 = 8.0;
pub const SP_M: f32 = 12.0;
pub const R_CONTROL: u8 = 6;
pub const R_CARD: u8 = 10;

pub const COND: &str = "cond";
pub const COND_SEMI: &str = "cond-semi";

pub fn cond(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(COND.into()))
}
pub fn semi(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(COND_SEMI.into()))
}
pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

// Text roles.
pub fn caption() -> FontId {
    semi(12.5)
}
pub fn body() -> FontId {
    cond(16.0)
}
pub fn title() -> FontId {
    semi(21.0)
}
pub fn readout() -> FontId {
    mono(24.0)
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        COND.into(),
        Arc::new(FontData::from_static(include_bytes!("../assets/fonts/BarlowCondensed-Medium.ttf"))),
    );
    fonts.font_data.insert(
        COND_SEMI.into(),
        Arc::new(FontData::from_static(include_bytes!("../assets/fonts/BarlowCondensed-SemiBold.ttf"))),
    );
    fonts.font_data.insert(
        "mono".into(),
        Arc::new(
            FontData::from_static(include_bytes!("../assets/fonts/JetBrainsMono.ttf")).tweak(FontTweak {
                coords: VariationCoords::new([(b"wght", 560.0)]),
                ..Default::default()
            }),
        ),
    );
    let fallback: Vec<String> = fonts.families[&FontFamily::Proportional].clone();
    let with_fallback = |first: &str| {
        let mut v = vec![first.to_owned()];
        v.extend(fallback.iter().cloned());
        v
    };
    fonts.families.insert(FontFamily::Name(COND.into()), with_fallback(COND));
    fonts.families.insert(FontFamily::Name(COND_SEMI.into()), with_fallback(COND_SEMI));
    fonts.families.entry(FontFamily::Proportional).or_default().insert(0, COND.into());
    fonts.families.entry(FontFamily::Monospace).or_default().insert(0, "mono".into());
    ctx.set_fonts(fonts);
}

fn apply(style: &mut Style) {
    use egui::TextStyle;
    style.text_styles = [
        (TextStyle::Small, cond(13.0)),
        (TextStyle::Body, body()),
        (TextStyle::Button, semi(16.0)),
        (TextStyle::Monospace, mono(14.0)),
        (TextStyle::Heading, title()),
    ]
    .into();
    let s = &mut style.spacing;
    s.item_spacing = egui::vec2(SP_S, 6.0);
    s.button_padding = egui::vec2(12.0, 6.0);
    s.interact_size.y = 30.0;
    s.scroll = egui::style::ScrollStyle::floating();

    let v = &mut style.visuals;
    v.dark_mode = true;
    v.panel_fill = CHROME;
    v.window_fill = RAISED;
    v.extreme_bg_color = INSET;
    v.faint_bg_color = SURFACE;
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.hyperlink_color = ACCENT;
    v.error_fg_color = DANGER;
    v.warn_fg_color = CUE;
    v.selection.bg_fill = ACCENT.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, INK);
    let w = |bg: Color32, stroke: Color32, fg: Color32| WidgetVisuals {
        bg_fill: bg,
        weak_bg_fill: bg,
        bg_stroke: Stroke::new(1.0, stroke),
        corner_radius: CornerRadius::same(R_CONTROL),
        fg_stroke: Stroke::new(1.0, fg),
        expansion: 0.0,
    };
    v.widgets.noninteractive = w(SURFACE, BORDER, INK);
    v.widgets.inactive = w(RAISED, BORDER, INK);
    v.widgets.hovered = w(HOVER, INK_FAINT, INK);
    v.widgets.active = w(HOVER, ACCENT, INK);
    v.widgets.open = w(RAISED, BORDER, INK);
}

pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);
    ctx.style_mut_of(Theme::Dark, apply);
    ctx.style_mut_of(Theme::Light, apply);
    ctx.set_theme(ThemePreference::Dark);
    // Zoom is managed by the app (fit to window); plain −/= are waveform zoom.
    ctx.options_mut(|o| o.zoom_with_keyboard = false);
}

/// The window/taskbar icon: a platter (dark disc, accent rim, centre spindle), drawn in code
/// so there's no asset to ship.
pub fn icon() -> egui::IconData {
    const N: usize = 64;
    let mut rgba = vec![0u8; N * N * 4];
    let c = (N as f32 - 1.0) / 2.0;
    for y in 0..N {
        for x in 0..N {
            let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt() / c;
            let col: Option<Color32> = if d > 1.0 {
                None
            } else if d > 0.86 {
                Some(ACCENT)
            } else if d < 0.14 {
                Some(INK)
            } else if (0.3..0.36).contains(&d) || (0.56..0.6).contains(&d) {
                Some(BORDER)
            } else {
                Some(SURFACE)
            };
            if let Some(col) = col {
                let i = (y * N + x) * 4;
                // Soft edge.
                let a = ((1.0 - d) * c).clamp(0.0, 1.0);
                rgba[i..i + 4].copy_from_slice(&[col.r(), col.g(), col.b(), (a * 255.0) as u8]);
            }
        }
    }
    egui::IconData { rgba, width: N as u32, height: N as u32 }
}

#[cfg(test)]
mod tests {
    #[test]
    fn icon_is_a_disc() {
        let i = super::icon();
        assert_eq!((i.width, i.height), (64, 64));
        let px = |x: usize, y: usize| &i.rgba[(y * 64 + x) * 4..(y * 64 + x) * 4 + 4];
        assert_eq!(px(0, 0)[3], 0, "corners transparent");
        assert_eq!(px(32, 32)[3], 255, "centre opaque");
        if let Some(p) = std::env::var_os("PLATTER_ICON_DUMP") {
            std::fs::write(p, &i.rgba).unwrap();
        }
    }
}
