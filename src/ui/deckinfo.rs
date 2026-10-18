//! Facts derived from a deck's track and position, shared by the screens: where it is in the
//! bar, the next cue, the bars.beats countdown, and whether its key mixes with the other deck.

use crate::ui::prelude::*;
use crate::ui::App;
use crate::engine::DeckView;

/// Where a deck is in the bar: (beat index in bar 0–3, fraction through the beat).
pub(crate) fn bar_position(app: &App, d: usize, dv: &DeckView) -> Option<(i64, f64)> {
    let tr = app.decks[d].track.as_ref()?;
    let g = tr.grid.load_full()?;
    let b = g.beat_at(dv.pos);
    let k = b.floor() as i64;
    Some(((k + g.bar_phase as i64).rem_euclid(4), b - b.floor()))
}

/// Beats to the next cue (hot cue, memory cue or the CUE point) ahead of the playhead,
/// and its label.
pub(crate) fn next_cue(app: &App, d: usize, dv: &DeckView) -> Option<(f64, String)> {
    let tr = app.decks[d].track.as_ref()?;
    let g = tr.grid.load_full()?;
    let now = g.beat_at(dv.pos);
    let mut cands: Vec<(f64, String)> = Vec::new();
    for (i, h) in dv.hot.iter().enumerate() {
        if let Some(h) = h {
            cands.push((h.pos, crate::ui::HOT_LETTERS[i].to_string()));
        }
    }
    for m in dv.memory.iter().flatten() {
        cands.push((*m, "MEMORY".into()));
    }
    cands.push((dv.cue, "CUE".into()));
    cands
        .into_iter()
        .map(|(pos, l)| (g.beat_at(pos) - now, l))
        .filter(|(b, _)| *b > 0.05)
        .min_by(|a, b| a.0.total_cmp(&b.0))
}

/// "4.2" = four bars and two beats to go, like a CDJ's beat countdown.
pub(crate) fn bars_beats(beats: f64) -> String {
    let whole = beats.ceil() as i64;
    format!("{}.{}", whole / 4, whole % 4)
}

/// The key colour: green when it mixes with the other loaded deck.
pub(crate) fn key_colour(app: &App, d: usize) -> Color32 {
    let k = |i: usize| app.decks[i].track.as_ref().and_then(|t| crate::library::keys::parse(&t.key));
    match (k(d), k(1 - d)) {
        (Some(a), Some(b)) if crate::library::keys::compatible(a, b) => t::PLAY,
        _ => t::INK_MUTED,
    }
}
