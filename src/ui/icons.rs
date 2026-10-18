//! Phosphor icons (phosphoricons.com, MIT), drawn as text.
//!
//! The Bold weight is installed as a fallback in every UI font family, so an icon codepoint
//! renders inline in any label: `format!("{} BROWSE", icons::BROWSE)`. The Fill weight is a
//! family of its own ([`fill`]) for solid glyphs: play, record, stars, the tag mark.
//!
//! The embedded fonts are subsets holding only the glyphs listed here. After adding one,
//! run `tools/subset-icons.sh` to rebuild them.

use egui::{FontFamily, FontId};

pub const FILL_FAMILY: &str = "icons-fill";

/// A solid (Fill weight) icon font at `size`.
pub fn fill(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(FILL_FAMILY.into()))
}

/// True for icon codepoints (the Private Use Area), so they can be left out of names read by
/// screen readers.
pub fn is_icon(c: char) -> bool {
    ('\u{E000}'..='\u{F8FF}').contains(&c)
}

/// `label` without icons, for accessibility names.
pub fn strip(label: &str) -> String {
    label.chars().filter(|&c| !is_icon(c)).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

// Views.
pub const PERFORM: &str = "\u{e228}"; // faders
pub const WAVE: &str = "\u{e802}"; // waveform
pub const BROWSE: &str = "\u{ebe0}"; // list-magnifying-glass
pub const TAG: &str = "\u{e0ea}"; // bookmark-simple
pub const SETTINGS: &str = "\u{e272}"; // gear-six

// Top bar.
pub const POWER: &str = "\u{e3da}";
pub const SPEAKER: &str = "\u{e44a}"; // speaker-high
pub const SPEAKER_OFF: &str = "\u{e45a}"; // speaker-slash
pub const CONTROLLER: &str = "\u{e946}"; // plug
pub const GAMEPAD: &str = "\u{e26e}"; // game-controller
pub const MAGNET: &str = "\u{e682}"; // magnet-straight
pub const ZOOM_OUT: &str = "\u{e30e}"; // magnifying-glass-minus
pub const ZOOM_IN: &str = "\u{e310}"; // magnifying-glass-plus
pub const RECORD: &str = "\u{e3ee}";

// Transport.
pub const PLAY_PAUSE: &str = "\u{e8be}";
pub const CARET_LEFT: &str = "\u{e138}";
pub const CARET_RIGHT: &str = "\u{e13a}";
pub const CARET_DOUBLE_LEFT: &str = "\u{e128}";
pub const CARET_DOUBLE_RIGHT: &str = "\u{e12a}";

// Library.
pub const SEARCH: &str = "\u{e30c}"; // magnifying-glass
pub const FILTER: &str = "\u{e266}"; // funnel
pub const COLUMNS: &str = "\u{e546}";
pub const INFO: &str = "\u{e2ce}";
pub const KEYBOARD: &str = "\u{e2d8}";
pub const USB: &str = "\u{e956}";
pub const FOLDER: &str = "\u{e24a}";
pub const PLAYLIST: &str = "\u{e6aa}";
pub const TRACKS: &str = "\u{e340}"; // music-notes
pub const HISTORY: &str = "\u{e1a0}"; // clock-counter-clockwise
pub const STAR: &str = "\u{e46a}";
pub const CHECK: &str = "\u{e182}";
pub const BACKSPACE: &str = "\u{e0ae}";
pub const TRASH: &str = "\u{e4a6}";
pub const X: &str = "\u{e4f6}";
pub const IMAGE: &str = "\u{e2ca}";

// Settings pages.
pub const PLAYER: &str = "\u{ecac}"; // vinyl-record
pub const MIXER: &str = "\u{e432}"; // sliders
pub const AUDIO: &str = "\u{e2a6}"; // headphones
pub const SYSTEM: &str = "\u{e560}"; // desktop

