//! rekordbox "My Settings" on an export stick (`PIONEER/MYSETTING.DAT`, `MYSETTING2.DAT`,
//! `DEVSETTING.DAT`, `DJMMYSETTING.DAT`): the player and mixer preferences a CDJ applies
//! when you plug the stick in.
//!
//! Layout (little-endian): a u32 string-block length (0x60), three 32-byte strings (brand,
//! "rekordbox", version), a u32 data length, the data, then a CRC-16 and two zero bytes.
//! Field offsets follow the rekordcrate project's reverse-engineering. Every value is
//! optional: unknown or out-of-range bytes are ignored rather than guessed.

use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum WaveColor {
    Blue,
    Rgb,
    ThreeBand,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AutoCueLevel {
    /// Cue at the first sound louder than this (dBFS).
    Db(f32),
    /// Cue at the first memory cue.
    Memory,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FaderCurve {
    SteepTop,
    Linear,
    SteepBottom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CrossfaderCurve {
    ConstantPower,
    SlowCut,
    FastCut,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MySettings {
    // DEVSETTING
    pub waveform_color: Option<WaveColor>,
    /// Overview drawn mirrored (full) rather than from the bottom (half).
    pub overview_full: Option<bool>,
    /// Playhead toward the left of the scrolling waveform rather than centred.
    pub playhead_left: Option<bool>,
    // MYSETTING
    pub quantize: Option<bool>,
    pub quantize_beats: Option<f64>,
    pub auto_cue: Option<bool>,
    pub auto_cue_level: Option<AutoCueLevel>,
    pub hotcue_autoload: Option<bool>,
    pub time_remain: Option<bool>,
    pub jog_vinyl: Option<bool>,
    pub master_tempo: Option<bool>,
    /// Index into the tempo ranges ±6 / ±10 / ±16 % / WIDE.
    pub tempo_range: Option<usize>,
    // MYSETTING2
    pub beat_jump: Option<f64>,
    // DJMMYSETTING
    pub fader_curve: Option<FaderCurve>,
    pub crossfader_curve: Option<CrossfaderCurve>,
    pub beat_fx_quantize: Option<bool>,
}

/// The data block of a settings file.
fn data(bytes: &[u8]) -> Option<&[u8]> {
    let n = u32::from_le_bytes(bytes.get(0..4)?.try_into().ok()?) as usize;
    let len = u32::from_le_bytes(bytes.get(4 + n..8 + n)?.try_into().ok()?) as usize;
    bytes.get(8 + n..8 + n + len)
}

fn flag(b: Option<&u8>) -> Option<bool> {
    match b? {
        0x80 => Some(false),
        0x81 => Some(true),
        _ => None,
    }
}

impl MySettings {
    pub fn is_empty(&self) -> bool {
        *self == MySettings::default()
    }

    /// Read whichever settings files the stick has. `root` is the drive (contains `PIONEER`).
    pub fn read(root: &Path) -> MySettings {
        let mut s = MySettings::default();
        let dir = root.join("PIONEER");
        let file = |name: &str| std::fs::read(dir.join(name)).ok();
        if let Some(f) = file("DEVSETTING.DAT") {
            s.read_dev(data(&f).unwrap_or(&[]));
        }
        if let Some(f) = file("MYSETTING.DAT") {
            s.read_my(data(&f).unwrap_or(&[]));
        }
        if let Some(f) = file("MYSETTING2.DAT") {
            s.read_my2(data(&f).unwrap_or(&[]));
        }
        if let Some(f) = file("DJMMYSETTING.DAT") {
            s.read_djm(data(&f).unwrap_or(&[]));
        }
        s
    }

    fn read_dev(&mut self, d: &[u8]) {
        self.overview_full = match d.get(9) {
            Some(1) => Some(false),
            Some(2) => Some(true),
            _ => None,
        };
        self.waveform_color = match d.get(10) {
            Some(1) => Some(WaveColor::Blue),
            Some(3) => Some(WaveColor::Rgb),
            Some(4) => Some(WaveColor::ThreeBand),
            _ => None,
        };
        self.playhead_left = match d.get(13) {
            Some(1) => Some(false),
            Some(2) => Some(true),
            _ => None,
        };
    }

    fn read_my(&mut self, d: &[u8]) {
        self.quantize = flag(d.get(10));
        self.auto_cue_level = match d.get(11) {
            Some(0x88) => Some(AutoCueLevel::Memory),
            Some(&b @ 0x80..=0x87) => Some(AutoCueLevel::Db(-36.0 - 6.0 * (b - 0x80) as f32)),
            _ => None,
        };
        self.quantize_beats = match d.get(24) {
            Some(&b @ 0x80..=0x83) => Some(1.0 / (1u32 << (b - 0x80)) as f64),
            _ => None,
        };
        // 0x82 = "rekordbox setting" (per track); treat it as on.
        self.hotcue_autoload = match d.get(25) {
            Some(0x80) => Some(false),
            Some(0x81 | 0x82) => Some(true),
            _ => None,
        };
        self.time_remain = flag(d.get(32));
        self.jog_vinyl = flag(d.get(33));
        self.auto_cue = flag(d.get(34));
        self.master_tempo = flag(d.get(35));
        self.tempo_range = match d.get(36) {
            Some(&b @ 0x80..=0x83) => Some((b - 0x80) as usize),
            _ => None,
        };
    }

    fn read_my2(&mut self, d: &[u8]) {
        self.beat_jump = match d.get(12) {
            Some(&b @ 0x80..=0x87) => Some(0.5 * (1u32 << (b - 0x80)) as f64),
            _ => None,
        };
    }

    fn read_djm(&mut self, d: &[u8]) {
        self.fader_curve = match d.get(12) {
            Some(0x80) => Some(FaderCurve::SteepTop),
            Some(0x81) => Some(FaderCurve::Linear),
            Some(0x82) => Some(FaderCurve::SteepBottom),
            _ => None,
        };
        self.crossfader_curve = match d.get(13) {
            Some(0x80) => Some(CrossfaderCurve::ConstantPower),
            Some(0x81) => Some(CrossfaderCurve::SlowCut),
            Some(0x82) => Some(CrossfaderCurve::FastCut),
            _ => None,
        };
        self.beat_fx_quantize = flag(d.get(16));
    }

    /// Short human summary for the "applied" message.
    pub fn summary(&self) -> String {
        let mut v: Vec<String> = Vec::new();
        if let Some(c) = self.waveform_color {
            v.push(match c {
                WaveColor::Blue => "blue waveform",
                WaveColor::Rgb => "RGB waveform",
                WaveColor::ThreeBand => "3-band waveform",
            }
            .into());
        }
        if let Some(q) = self.quantize {
            v.push(if q { "quantize on".into() } else { "quantize off".into() });
        }
        if let Some(r) = self.tempo_range {
            v.push(["±6 %", "±10 %", "±16 %", "WIDE"].get(r).unwrap_or(&"").to_string());
        }
        if let Some(m) = self.master_tempo {
            v.push(if m { "Master Tempo on".into() } else { "Master Tempo off".into() });
        }
        if let Some(j) = self.jog_vinyl {
            v.push(if j { "vinyl jog".into() } else { "CDJ jog".into() });
        }
        if let Some(a) = self.auto_cue {
            v.push(match (a, self.auto_cue_level) {
                (false, _) => "auto cue off".into(),
                (true, Some(AutoCueLevel::Memory)) => "auto cue to memory".into(),
                (true, Some(AutoCueLevel::Db(db))) => format!("auto cue {db:.0} dB"),
                (true, None) => "auto cue on".into(),
            });
        }
        if let Some(t) = self.time_remain {
            v.push(if t { "remaining time".into() } else { "elapsed time".into() });
        }
        if let Some(b) = self.beat_jump {
            v.push(format!("beat jump {}", fmt_beats(b)));
        }
        if let Some(c) = self.crossfader_curve {
            v.push(
                match c {
                    CrossfaderCurve::ConstantPower => "smooth crossfader",
                    CrossfaderCurve::SlowCut => "slow-cut crossfader",
                    CrossfaderCurve::FastCut => "fast-cut crossfader",
                }
                .into(),
            );
        }
        v.join(", ")
    }
}

pub fn fmt_beats(b: f64) -> String {
    if b < 1.0 { format!("1/{}", (1.0 / b).round()) } else { format!("{b:.0}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_demo_sticks_settings() {
        let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/rekordbox/demo_tracks"));
        let s = MySettings::read(root);
        assert_eq!(s.waveform_color, Some(WaveColor::Blue));
        assert_eq!(s.overview_full, Some(false));
        assert_eq!(s.playhead_left, Some(false));
        assert_eq!(s.quantize, Some(true));
        assert_eq!(s.auto_cue_level, Some(AutoCueLevel::Memory));
        assert_eq!(s.quantize_beats, Some(1.0));
        assert_eq!(s.hotcue_autoload, Some(true));
        assert_eq!(s.time_remain, Some(true));
        assert_eq!(s.jog_vinyl, Some(true));
        assert_eq!(s.auto_cue, Some(true));
        assert_eq!(s.master_tempo, Some(false));
        assert_eq!(s.tempo_range, Some(1));
        assert_eq!(s.beat_jump, Some(16.0));
        assert_eq!(s.fader_curve, Some(FaderCurve::Linear));
        assert_eq!(s.crossfader_curve, Some(CrossfaderCurve::FastCut));
        assert_eq!(s.beat_fx_quantize, Some(true));
        assert!(s.summary().contains("blue waveform"));
    }

    #[test]
    fn missing_or_garbage_files_give_nothing() {
        assert!(MySettings::read(Path::new("/nonexistent")).is_empty());
        let mut s = MySettings::default();
        s.read_my(&[0xff; 40]);
        assert!(s.is_empty());
    }
}
