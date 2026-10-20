//! Mixing suggestions: the whole library ranked against the leading deck by how far the
//! tempo has to move, how the keys sit on the Camelot wheel once the tempo is matched, and
//! what you've played straight after this track before.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use super::keys::{self, Camelot};
use super::LibTrack;

/// The furthest a suggestion's tempo moves to match, in percent.
pub const MAX_TEMPO: f64 = 8.0;
/// The longest suggestion list.
const MAX: usize = 60;

/// What the suggestions are for: the leading deck's track as it plays now.
#[derive(Clone, Debug)]
pub struct Seed {
    pub deck: usize,
    pub track: LibTrack,
    /// The leading deck's BPM after its tempo fader.
    pub bpm: f64,
    /// Its key as it sounds now.
    pub key: Option<Camelot>,
    /// Master Tempo on the deck a suggestion would load onto (the other one).
    pub keylock: bool,
    /// Tracks on a deck, never suggested.
    pub loaded: Vec<PathBuf>,
}

impl Seed {
    /// Close enough that the list needn't be ranked again: a small tempo move keeps it.
    pub fn same_as(&self, o: &Seed) -> bool {
        self.deck == o.deck
            && self.track.path == o.track.path
            && self.key == o.key
            && self.keylock == o.keylock
            && self.loaded == o.loaded
            && (self.bpm - o.bpm).abs() <= self.bpm * 0.0025
    }
}

/// Where a suggestion's key sits on the wheel from the seed's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyMatch {
    Same,
    /// One step clockwise or anticlockwise.
    Up,
    Down,
    /// The relative major or minor.
    Relative,
    /// Two steps clockwise: a whole tone up, for an energy lift.
    Boost,
    Clash,
    /// Either key isn't known.
    Unknown,
}

impl KeyMatch {
    pub fn mixes(self) -> bool {
        matches!(self, KeyMatch::Same | KeyMatch::Up | KeyMatch::Down | KeyMatch::Relative)
    }
}

/// Why a track is suggested.
#[derive(Clone, Debug, PartialEq)]
pub struct Why {
    /// The tempo change that matches it, in percent (+ = faster than recorded).
    pub tempo: Option<f64>,
    pub key: KeyMatch,
    /// Its key once its tempo is matched without Master Tempo, when that isn't its own.
    pub sounds: Option<Camelot>,
    /// The times you've played it straight after the seed track.
    pub after: u32,
    pub score: f32,
}

/// A track to rank, with what rekordbox adds for stick tracks.
pub struct Candidate {
    pub track: LibTrack,
    /// The history id (see `Library::track_id`).
    pub id: String,
    pub genre: Option<String>,
    pub rating: u8,
}

pub fn key_match(seed: Camelot, c: Camelot) -> KeyMatch {
    let step = (c.num as i32 - seed.num as i32).rem_euclid(12);
    match (c.minor == seed.minor, step) {
        (true, 0) => KeyMatch::Same,
        (true, 1) => KeyMatch::Up,
        (true, 11) => KeyMatch::Down,
        (true, 2) => KeyMatch::Boost,
        (false, 0) => KeyMatch::Relative,
        _ => KeyMatch::Clash,
    }
}

/// The tempo change in percent that brings `bpm` to `target`, at half or double time if
/// that's closer.
pub fn tempo_change(bpm: f64, target: f64) -> f64 {
    [bpm, bpm * 2.0, bpm / 2.0].into_iter().map(|b| (target / b - 1.0) * 100.0).min_by(|a, b| a.abs().total_cmp(&b.abs())).unwrap_or(0.0)
}

/// Rank `pool` against `seed`, best first. `follows` counts the times each track id was
/// played straight after the seed; `played` (this session's tracks) are left out.
pub fn rank(seed: &Seed, seed_genre: Option<&str>, pool: Vec<Candidate>, follows: &HashMap<String, u32>, played: &HashSet<PathBuf>) -> Vec<(LibTrack, Why)> {
    let mut seen = HashSet::new();
    let mut out: Vec<(LibTrack, Why)> = pool
        .into_iter()
        .filter_map(|c| {
            let t = &c.track;
            if t.path == seed.track.path || seed.loaded.contains(&t.path) || played.contains(&t.path) || !seen.insert(t.path.clone()) {
                return None;
            }
            let after = follows.get(&c.id).copied().unwrap_or(0);
            let tempo = t.bpm.filter(|b| *b > 0.0).map(|b| tempo_change(b, seed.bpm));
            // Your own history earns a place even out of tempo range or key.
            if !tempo.is_some_and(|p| p.abs() <= MAX_TEMPO) && after == 0 {
                return None;
            }
            let own = keys::parse(&t.key);
            let sounds = own.map(|k| keys::sounding(k, 1.0 + tempo.unwrap_or(0.0) / 100.0, seed.keylock));
            let key = match (seed.key, sounds) {
                (Some(s), Some(k)) => key_match(s, k),
                _ => KeyMatch::Unknown,
            };
            if key == KeyMatch::Clash && after == 0 {
                return None;
            }
            let tempo_score = tempo.map_or(0.0, |p| (1.0 - p.abs() / MAX_TEMPO).max(0.0)) as f32;
            let key_score = match key {
                KeyMatch::Same => 1.0,
                KeyMatch::Up | KeyMatch::Down => 0.85,
                KeyMatch::Relative => 0.8,
                KeyMatch::Boost => 0.5,
                KeyMatch::Unknown => 0.35,
                KeyMatch::Clash => 0.0,
            };
            let mut score = 0.5 * tempo_score + 0.4 * key_score;
            if after > 0 {
                score += 0.25 + 0.1 * (after.min(3) - 1) as f32;
            }
            if let (Some(a), Some(b)) = (seed_genre.filter(|g| !g.is_empty()), c.genre.as_deref()) {
                if a.eq_ignore_ascii_case(b) {
                    score += 0.1;
                }
            }
            score += 0.02 * c.rating.min(5) as f32;
            // Back to back by one artist is rarely what you want.
            if !seed.track.artist.is_empty() && seed.track.artist.eq_ignore_ascii_case(&t.artist) {
                score -= 0.1;
            }
            Some((c.track, Why { tempo, key, sounds: sounds.filter(|s| Some(*s) != own), after, score }))
        })
        .collect();
    out.sort_by(|a, b| b.1.score.total_cmp(&a.1.score).then_with(|| a.0.title.cmp(&b.0.title)));
    out.truncate(MAX);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lt(title: &str, artist: &str, bpm: f64, key: &str) -> LibTrack {
        LibTrack {
            path: PathBuf::from(format!("/m/{title}.mp3")),
            title: title.into(),
            artist: artist.into(),
            key: key.into(),
            bpm: Some(bpm),
            duration: None,
            ext: "MP3".into(),
            rb: None,
        }
    }

    fn cand(t: LibTrack) -> Candidate {
        Candidate { id: t.path.to_string_lossy().into_owned(), track: t, genre: None, rating: 0 }
    }

    fn seed(bpm: f64, key: &str, keylock: bool) -> Seed {
        Seed { deck: 0, track: lt("Seed", "Someone", bpm, key), bpm, key: keys::parse(key), keylock, loaded: Vec::new() }
    }

    fn titles(v: &[(LibTrack, Why)]) -> Vec<&str> {
        v.iter().map(|(t, _)| t.title.as_str()).collect()
    }

    #[test]
    fn tempo_counts_half_and_double_time() {
        assert!((tempo_change(125.0, 125.0)).abs() < 1e-9);
        assert!((tempo_change(62.5, 125.0)).abs() < 1e-9);
        assert!((tempo_change(250.0, 125.0)).abs() < 1e-9);
        assert!((tempo_change(120.0, 126.0) - 5.0).abs() < 1e-9);
    }

    #[test]
    fn key_relations_on_the_wheel() {
        let k = |s| keys::parse(s).unwrap();
        assert_eq!(key_match(k("8A"), k("8A")), KeyMatch::Same);
        assert_eq!(key_match(k("8A"), k("9A")), KeyMatch::Up);
        assert_eq!(key_match(k("1A"), k("12A")), KeyMatch::Down);
        assert_eq!(key_match(k("8A"), k("8B")), KeyMatch::Relative);
        assert_eq!(key_match(k("8A"), k("10A")), KeyMatch::Boost);
        assert_eq!(key_match(k("8A"), k("3A")), KeyMatch::Clash);
    }

    #[test]
    fn ranks_by_tempo_and_key_and_leaves_out_what_doesnt_mix() {
        let pool = vec![
            cand(lt("Far tempo", "A", 140.0, "8A")),
            cand(lt("Clash", "B", 124.0, "3A")),
            cand(lt("Step", "C", 125.0, "9A")),
            cand(lt("Perfect", "D", 124.0, "8A")),
            cand(lt("No key", "E", 124.0, "")),
            cand(lt("Half time", "F", 62.0, "8B")),
        ];
        let out = rank(&seed(124.0, "8A", true), None, pool, &HashMap::new(), &HashSet::new());
        assert_eq!(titles(&out), vec!["Perfect", "Half time", "Step", "No key"]);
        assert_eq!(out[1].1.key, KeyMatch::Relative);
    }

    #[test]
    fn matched_tempo_moves_the_key_without_master_tempo() {
        // 117 → 124 BPM is +6 %: a semitone up, so 3A sounds as 10A, a boost from 8A.
        let pool = vec![cand(lt("Slower", "A", 117.0, "3A"))];
        let free = rank(&seed(124.0, "8A", false), None, pool, &HashMap::new(), &HashSet::new());
        assert_eq!(free[0].1.key, KeyMatch::Boost);
        assert_eq!(free[0].1.sounds, keys::parse("10A"));
        let pool = vec![cand(lt("Slower", "A", 117.0, "3A"))];
        let locked = rank(&seed(124.0, "8A", true), None, pool, &HashMap::new(), &HashSet::new());
        assert!(locked.is_empty(), "with Master Tempo it stays 3A, a clash");
    }

    #[test]
    fn history_lifts_a_track_and_played_or_loaded_ones_drop_out() {
        let pool = vec![cand(lt("Perfect", "D", 124.0, "8A")), cand(lt("Usual next", "E", 130.0, "5B")), cand(lt("On deck", "F", 124.0, "8A")), cand(lt("Played", "G", 124.0, "8A"))];
        let mut s = seed(124.0, "8A", true);
        s.loaded.push(PathBuf::from("/m/On deck.mp3"));
        let follows = HashMap::from([("/m/Usual next.mp3".to_string(), 3)]);
        let played = HashSet::from([PathBuf::from("/m/Played.mp3")]);
        let out = rank(&s, None, pool, &follows, &played);
        assert_eq!(titles(&out), vec!["Perfect", "Usual next"]);
        assert_eq!(out[1].1.after, 3);
    }

    #[test]
    fn same_artist_and_genre_nudge_the_order() {
        let mut same_genre = cand(lt("Same genre", "B", 124.0, "8A"));
        same_genre.genre = Some("House".into());
        let pool = vec![cand(lt("Same artist", "Someone", 124.0, "8A")), cand(lt("Other", "C", 124.0, "8A")), same_genre];
        let out = rank(&seed(124.0, "8A", true), Some("house"), pool, &HashMap::new(), &HashSet::new());
        assert_eq!(titles(&out), vec!["Same genre", "Other", "Same artist"]);
    }
}
