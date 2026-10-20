//! Musical keys as rekordbox writes them ("Fm", "C#m", "Abm", "8A", "Open Key 1m"…), mapped
//! onto the Camelot wheel so CDeckJ can tell which keys mix well together.

/// Camelot position: number 1–12 and whether it's minor ("A") or major ("B").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Camelot {
    pub num: u8,
    pub minor: bool,
}

/// Pitch class (C = 0) → Camelot number, for minor and major keys.
const MINOR: [u8; 12] = [5, 12, 7, 2, 9, 4, 11, 6, 1, 8, 3, 10];
const MAJOR: [u8; 12] = [8, 3, 10, 5, 12, 7, 2, 9, 4, 11, 6, 1];

pub fn parse(s: &str) -> Option<Camelot> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    // Camelot ("8A") or Open Key ("1m", "6d").
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() {
        let n: u8 = digits.parse().ok()?;
        let rest = t[digits.len()..].trim().to_ascii_lowercase();
        if !(1..=12).contains(&n) {
            return None;
        }
        return match rest.as_str() {
            "a" => Some(Camelot { num: n, minor: true }),
            "b" => Some(Camelot { num: n, minor: false }),
            // Open Key: 1m = 6A, 1d = 6B.
            "m" => Some(Camelot { num: (n + 4) % 12 + 1, minor: true }),
            "d" => Some(Camelot { num: (n + 4) % 12 + 1, minor: false }),
            _ => None,
        };
    }
    // Musical names: root, optional accidental, then minor/major marker.
    let mut chars = t.chars();
    let root = match chars.next()?.to_ascii_uppercase() {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let rest: String = chars.collect();
    let (acc, quality) = match rest.chars().next() {
        Some('#') | Some('♯') => (1i32, &rest[rest.char_indices().nth(1).map(|x| x.0).unwrap_or(rest.len())..]),
        Some('b') | Some('♭') if rest.len() > 0 => (-1, &rest[rest.char_indices().nth(1).map(|x| x.0).unwrap_or(rest.len())..]),
        _ => (0, rest.as_str()),
    };
    // "M" alone means major; lowercase "m" means minor.
    let q = if quality.trim() == "M" { "maj".to_string() } else { quality.trim().to_ascii_lowercase() };
    let minor = matches!(q.as_str(), "m" | "min" | "minor" | "-");
    if !minor && !matches!(q.as_str(), "" | "maj" | "major") {
        return None;
    }
    let pc = ((root + acc).rem_euclid(12)) as usize;
    Some(Camelot { num: if minor { MINOR[pc] } else { MAJOR[pc] }, minor })
}

/// Harmonically compatible: the same key, one step around the wheel, or its relative
/// major/minor.
pub fn compatible(a: Camelot, b: Camelot) -> bool {
    let step = (a.num as i32 - b.num as i32).rem_euclid(12);
    (a.minor == b.minor && (step == 0 || step == 1 || step == 11)) || (a.num == b.num)
}

pub fn label(c: Camelot) -> String {
    format!("{}{}", c.num, if c.minor { "A" } else { "B" })
}

/// The key a semitone count higher (lower when negative): a semitone is 7 steps round the
/// wheel.
pub fn transpose(c: Camelot, semitones: i32) -> Camelot {
    Camelot { num: ((c.num as i32 - 1 + 7 * semitones).rem_euclid(12) + 1) as u8, minor: c.minor }
}

/// How far playing at `rate` without Master Tempo moves the pitch, to the nearest semitone.
pub fn semitones(rate: f64) -> i32 {
    if rate > 0.0 { (12.0 * rate.log2()).round() as i32 } else { 0 }
}

/// A key as it sounds on a deck playing at `rate`.
pub fn sounding(c: Camelot, rate: f64, keylock: bool) -> Camelot {
    if keylock { c } else { transpose(c, semitones(rate)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rekordbox_and_camelot_notation() {
        assert_eq!(parse("Fm"), Some(Camelot { num: 4, minor: true }));
        assert_eq!(parse("Abm"), Some(Camelot { num: 1, minor: true }));
        assert_eq!(parse("C#m"), Some(Camelot { num: 12, minor: true }));
        assert_eq!(parse("C"), Some(Camelot { num: 8, minor: false }));
        assert_eq!(parse("8A"), Some(Camelot { num: 8, minor: true }));
        assert_eq!(parse("12B"), Some(Camelot { num: 12, minor: false }));
        assert_eq!(parse("1m"), Some(Camelot { num: 6, minor: true }));
        assert_eq!(parse(""), None);
        assert_eq!(parse("13A"), None);
        assert_eq!(parse("H"), None);
    }

    #[test]
    fn compatibility_follows_the_wheel() {
        let k = |s| parse(s).unwrap();
        assert!(compatible(k("8A"), k("8A")));
        assert!(compatible(k("8A"), k("9A")));
        assert!(compatible(k("12A"), k("1A")));
        assert!(compatible(k("8A"), k("8B")));
        assert!(!compatible(k("8A"), k("10A")));
        assert!(!compatible(k("8A"), k("9B")));
        assert!(compatible(k("Fm"), k("Ab")), "relative major");
    }

    #[test]
    fn tempo_moves_the_key_without_master_tempo() {
        let k = |s| parse(s).unwrap();
        assert_eq!(transpose(k("C"), 1), k("C#"));
        assert_eq!(transpose(k("Am"), -1), k("G#m"));
        assert_eq!(transpose(k("8A"), 12), k("8A"));
        // +6 % is a semitone (5.9 %); +2 % rounds to none.
        assert_eq!(sounding(k("8A"), 1.06, false), k("3A"));
        assert_eq!(sounding(k("8A"), 1.02, false), k("8A"));
        assert_eq!(sounding(k("8A"), 1.06, true), k("8A"));
    }
}
