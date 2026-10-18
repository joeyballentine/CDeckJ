//! The one place visual decisions live.
//!
//! Scene: a dark booth, a 7" handheld screen at arm's length, glanced at between moves.
//! Dark only on purpose: a bright screen in a dark room ruins your eyes and the crowd's.
//! Signature: the stacked, scrolling three-band waveforms across the top.
//! Colour vocabulary is borrowed from the hardware itself: PLAY is green, CUE is amber.

use std::sync::Arc;

use egui::epaint::text::VariationCoords;
use egui::{
    style::WidgetVisuals, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId,
    FontTweak, Stroke, Style, Theme, ThemePreference,
};

const fn c(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

// Warm graphite neutrals, ~3 % luminance steps.
pub const CHROME: Color32 = c(0x121214);
pub const SURFACE: Color32 = c(0x19191C);
pub const RAISED: Color32 = c(0x232327);
pub const INSET: Color32 = c(0x0C0C0E);
pub const BORDER: Color32 = c(0x2E2E33);
pub const HOVER: Color32 = c(0x2C2C31);

pub const INK: Color32 = c(0xE8E4DC);
pub const INK_MUTED: Color32 = c(0xA19C93);
pub const INK_FAINT: Color32 = c(0x6A6760);

pub const PLAY: Color32 = c(0x47D16C);
pub const CUE: Color32 = c(0xF2A13B);
pub const SYNC: Color32 = c(0x5FB8F0);
pub const LOOP: Color32 = c(0xE9D25A);
pub const DANGER: Color32 = c(0xF0675F);
/// Memory cues (rekordbox): red, as on the players.
pub const MEMORY_CUE: Color32 = c(0xE5484D);
/// Selection / focus accent.
pub const ACCENT: Color32 = CUE;

pub const DECK_TINT: [Color32; 2] = [c(0x5FB8F0), c(0xF2779B)];

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
pub const WAVE_LOW: Color32 = c(0x1F64FF);
pub const WAVE_MID: Color32 = c(0xF29B38);
pub const WAVE_HIGH: Color32 = c(0xF4EEE2);

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
}
