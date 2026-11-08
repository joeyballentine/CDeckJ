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
pub const R_CONTROL: u8 = 0;
pub const R_CARD: u8 = 0;

pub const COND: &str = "cond";
pub const COND_SEMI: &str = "cond-semi";
const ICONS: &str = "icons";
const ICON_SCALE: f32 = 1.0;
const ICON_Y: f32 = -0.06;

pub fn cond(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(COND.into()))
}
pub fn semi(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(COND_SEMI.into()))
}
pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

thread_local! {
    static GLANCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Glance mode: bigger type and fewer words, for reading the screen from arm's length.
/// Set once per frame from the config.
pub fn set_glance(on: bool) {
    GLANCE.with(|l| l.set(on));
}
pub fn glance() -> bool {
    GLANCE.with(|l| l.get())
}
/// `detailed` normally, `glance` in glance mode.
pub fn sz(detailed: f32, glance: f32) -> f32 {
    if self::glance() { glance } else { detailed }
}

// Text roles. View code picks one of these for labels, prose and buttons; only badges,
// glyphs, readouts and headings fitted to a fixed box size their own type.
pub fn caption() -> FontId {
    semi(sz(12.5, 14.0))
}
/// Small numbers: DSP load, levels, the controller log.
pub fn mono_small() -> FontId {
    mono(sz(12.5, 14.0))
}
/// Secondary text: status, hints, messages, values beside a label.
pub fn small() -> FontId {
    cond(sz(14.0, 16.0))
}
/// Labels on buttons.
pub fn button_font() -> FontId {
    semi(sz(14.0, 16.0))
}
pub fn body() -> FontId {
    cond(sz(16.0, 18.0))
}
/// Body with weight: list and side-bar labels, the current breadcrumb.
pub fn strong() -> FontId {
    semi(sz(16.0, 18.0))
}
pub fn title() -> FontId {
    semi(sz(21.0, 25.0))
}
pub fn readout() -> FontId {
    mono(sz(24.0, 30.0))
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    // Barlow's line box (ascent 1.0 em, descent 0.2 em) puts capitals 0.05 em below the
    // middle, so centred labels sit low. Lift the glyphs so caps are optically centred.
    let barlow = FontTweak { y_offset_factor: -0.05, ..Default::default() };
    fonts.font_data.insert(
        COND.into(),
        Arc::new(FontData::from_static(include_bytes!("../../assets/fonts/BarlowCondensed-Medium.ttf")).tweak(barlow.clone())),
    );
    fonts.font_data.insert(
        COND_SEMI.into(),
        Arc::new(FontData::from_static(include_bytes!("../../assets/fonts/BarlowCondensed-SemiBold.ttf")).tweak(barlow)),
    );
    fonts.font_data.insert(
        "mono".into(),
        Arc::new(
            FontData::from_static(include_bytes!("../../assets/fonts/JetBrainsMono.ttf")).tweak(FontTweak {
                coords: VariationCoords::new([(b"wght", 560.0)]),
                ..Default::default()
            }),
        ),
    );
    // Phosphor icons. Bold sits right after the text font in every family, so icon
    // codepoints render inline in any label; Fill is its own family for solid glyphs.
    // Glyphs fill 0.84 em from 0.016 em above the baseline: lift and scale them so their
    // centre lands on the capitals' centre.
    let icon = FontTweak { scale: ICON_SCALE, y_offset_factor: ICON_Y, ..Default::default() };
    fonts.font_data.insert(ICONS.into(), Arc::new(FontData::from_static(include_bytes!("../../assets/fonts/Phosphor-Bold.ttf")).tweak(icon.clone())));
    fonts.font_data.insert(
        crate::ui::icons::FILL_FAMILY.into(),
        Arc::new(FontData::from_static(include_bytes!("../../assets/fonts/Phosphor-Fill.ttf")).tweak(icon)),
    );
    let fallback: Vec<String> = fonts.families[&FontFamily::Proportional].clone();
    let with_fallback = |first: &str| {
        let mut v = vec![first.to_owned(), ICONS.to_owned()];
        v.extend(fallback.iter().cloned());
        v
    };
    fonts.families.insert(FontFamily::Name(COND.into()), with_fallback(COND));
    fonts.families.insert(FontFamily::Name(COND_SEMI.into()), with_fallback(COND_SEMI));
    fonts.families.insert(FontFamily::Name(crate::ui::icons::FILL_FAMILY.into()), vec![crate::ui::icons::FILL_FAMILY.to_owned()]);
    fonts.families.entry(FontFamily::Proportional).or_default().splice(0..0, [COND.to_owned(), ICONS.to_owned()]);
    fonts.families.entry(FontFamily::Monospace).or_default().splice(0..0, ["mono".to_owned(), ICONS.to_owned()]);
    ctx.set_fonts(fonts);
}

fn apply(style: &mut Style) {
    use egui::TextStyle;
    style.text_styles = [
        (TextStyle::Small, small()),
        (TextStyle::Body, body()),
        (TextStyle::Button, button_font()),
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
    v.window_stroke = Stroke::NONE;
    v.hyperlink_color = ACCENT;
    v.error_fg_color = DANGER;
    v.warn_fg_color = CUE;
    v.selection.bg_fill = ACCENT.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, INK);
    // Flat and square: no outline except on the widget being used.
    let w = |bg: Color32, stroke: Option<Color32>, fg: Color32| WidgetVisuals {
        bg_fill: bg,
        weak_bg_fill: bg,
        bg_stroke: stroke.map_or(Stroke::NONE, |c| Stroke::new(1.0, c)),
        corner_radius: CornerRadius::same(R_CONTROL),
        fg_stroke: Stroke::new(1.0, fg),
        expansion: 0.0,
    };
    v.widgets.noninteractive = w(SURFACE, None, INK);
    v.widgets.inactive = w(RAISED, None, INK);
    v.widgets.hovered = w(HOVER, None, INK);
    v.widgets.active = w(HOVER, Some(ACCENT), INK);
    v.widgets.open = w(RAISED, None, INK);
}

pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);
    ctx.style_mut_of(Theme::Dark, apply);
    ctx.style_mut_of(Theme::Light, apply);
    ctx.set_theme(ThemePreference::Dark);
    // Zoom is managed by the app (fit to window); plain −/= are waveform zoom.
    ctx.options_mut(|o| o.zoom_with_keyboard = false);
}
