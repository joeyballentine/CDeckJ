//! What was played when: tracks that were audible for a while, written as a text tracklist and a
//! CUE sheet beside the recording.

use crate::engine::mixer::{fader_gain_curve, xfade_gains_curve};
use crate::engine::{EngineView, DECKS};

use super::*;

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// Recording frame where the track became audible.
    pub frame: u64,
    pub deck: usize,
    pub artist: String,
    pub title: String,
}

/// Turns "which deck is audible" into a list of tracks with start times.
#[derive(Default)]
pub struct Tracklist {
    pub entries: Vec<Entry>,
    /// Per deck: the loaded track generation, when it became audible, and whether it's listed.
    deck: [(u64, Option<u64>, bool); DECKS],
}

/// How much of a deck reaches the master (fader × crossfader), 0..1.
pub fn deck_level(v: &EngineView, d: usize) -> f32 {
    let (a, b) = xfade_gains_curve(v.mixer.crossfader, v.mixer.crossfader_curve);
    fader_gain_curve(v.mixer.ch[d].fader, v.mixer.fader_curve) * if d == 0 { a } else { b }
}

impl Tracklist {
    /// Call regularly while recording. `meta` gives (artist, title) for each deck's track.
    /// Returns true when an entry was added.
    pub fn observe(&mut self, v: &EngineView, frame: u64, rate: u32, meta: [Option<(String, String)>; DECKS]) -> bool {
        let mut added = false;
        for d in 0..DECKS {
            let dv = &v.decks[d];
            let st = &mut self.deck[d];
            if st.0 != dv.track_gen {
                *st = (dv.track_gen, None, false);
            }
            let audible = dv.loaded && dv.playing && deck_level(v, d) > 0.15;
            if !audible {
                st.1 = None;
                continue;
            }
            let since = *st.1.get_or_insert(frame);
            if !st.2 && (frame - since) as f64 >= TRACK_AUDIBLE_SECS * rate.max(1) as f64 {
                if let Some((artist, title)) = meta[d].clone() {
                    st.2 = true;
                    self.entries.push(Entry { frame: since, deck: d, artist, title });
                    self.entries.sort_by_key(|e| e.frame);
                    added = true;
                }
            }
        }
        added
    }
}

/// Seconds from the start of the recording to `frame`, across parts with different rates.
pub(crate) fn secs_at(parts: &[Part], frame: u64) -> f64 {
    let mut secs = 0.0;
    for (i, p) in parts.iter().enumerate() {
        let end = parts.get(i + 1).map(|n| n.start_frame).unwrap_or(u64::MAX);
        if frame < end {
            return secs + (frame - p.start_frame) as f64 / p.rate as f64;
        }
        secs += (end - p.start_frame) as f64 / p.rate as f64;
    }
    secs
}

pub(crate) fn hms(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{:02}:{:02}", s / 60, s % 60) }
}

pub(crate) fn display_name(e: &Entry) -> String {
    if e.artist.is_empty() { e.title.clone() } else { format!("{} - {}", e.artist, e.title) }
}

/// "00:00 Artist - Title" lines, ready to paste into a mix description.
pub fn tracklist_text(entries: &[Entry], parts: &[Part]) -> String {
    entries.iter().map(|e| format!("{} {}\n", hms(secs_at(parts, e.frame)), display_name(e))).collect()
}

/// A CUE sheet so players and splitters see track markers in the WAV(s).
pub fn cue_sheet(entries: &[Entry], parts: &[Part], title: &str) -> String {
    let esc = |s: &str| s.replace('"', "'");
    let mut s = format!("PERFORMER \"CDeckJ\"\nTITLE \"{}\"\n", esc(title));
    let mut current: Option<usize> = None;
    for (i, e) in entries.iter().enumerate() {
        let pi = parts.iter().rposition(|p| p.start_frame <= e.frame).unwrap_or(0);
        let Some(p) = parts.get(pi) else { break };
        if current != Some(pi) {
            let name = p.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            s += &format!("FILE \"{}\" WAVE\n", esc(&name));
            current = Some(pi);
        }
        let t = (e.frame - p.start_frame) as f64 / p.rate as f64;
        let cd = (t * 75.0).round() as u64;
        s += &format!(
            "  TRACK {:02} AUDIO\n    PERFORMER \"{}\"\n    TITLE \"{}\"\n    INDEX 01 {:02}:{:02}:{:02}\n",
            i + 1,
            esc(&e.artist),
            esc(&e.title),
            cd / 75 / 60,
            cd / 75 % 60,
            cd % 75
        );
    }
    s
}
