//! Settings (`~/.config/platter/config.toml`) and the per-track store
//! (`~/.local/share/platter/tracks.json`: saved cues, grid fixes, tags and analysis cache).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::midi::MidiMap;
use crate::rekordbox::settings::{CrossfaderCurve, FaderCurve, WaveColor};
use crate::track::{SavedCue, Tags};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct DeckPrefs {
    pub vinyl: bool,
    pub master_tempo: bool,
    pub tempo_range: usize,
}

impl Default for DeckPrefs {
    fn default() -> Self {
        Self { vinyl: true, master_tempo: false, tempo_range: crate::engine::deck::DEFAULT_RANGE }
    }
}

/// The beat FX selection (not ON: every set starts with effects off).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct FxPrefs {
    pub kind: crate::engine::fx::FxKind,
    pub beat_idx: usize,
    pub target: crate::engine::fx::FxTarget,
    pub level: f32,
}

impl Default for FxPrefs {
    fn default() -> Self {
        let p = crate::engine::fx::FxParams::default();
        Self { kind: p.kind, beat_idx: p.beat_idx, target: p.target, level: p.level }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub music_dir: Option<PathBuf>,
    pub usb_paths: Vec<PathBuf>,
    pub audio_device: Option<String>,
    pub alsa: bool,
    pub buffer_frames: u32,
    pub split_cue: bool,
    pub midi_port: Option<String>,
    pub fullscreen: bool,
    pub quantize: bool,
    pub decks: [DeckPrefs; 2],
    pub master_level: f32,
    pub phones_mix: f32,
    pub phones_level: f32,
    /// Seconds shown in the scrolling waveforms.
    pub waveform_seconds: f64,
    /// Detected tempos are folded into this range (e.g. [88, 176] for drum & bass).
    pub bpm_range: [f64; 2],
    /// Added to rekordbox grid and cue times, per file extension (e.g. mp3 = 0).
    pub grid_offset_ms: HashMap<String, f64>,
    /// Analyse BPM of music-folder tracks in the background.
    pub background_analysis: bool,
    /// Use the Steam Deck's own buttons and sticks.
    pub gamepad: bool,
    /// Ask the desktop not to dim or sleep while Platter runs.
    pub keep_awake: bool,
    /// Where recordings go (default: Music/Platter Recordings).
    pub recording_dir: Option<PathBuf>,
    /// Apply a rekordbox stick's My Settings when it's plugged in, like a CDJ.
    pub apply_usb_settings: bool,
    // Player and mixer preferences (My Settings). A stick's settings overwrite these.
    pub waveform_color: WaveColor,
    /// Overview mirrored (true) or drawn from the bottom like a CDJ's "half" view.
    pub overview_full: bool,
    /// Playhead at 30 % of the scrolling waveform instead of the centre.
    pub playhead_left: bool,
    /// Big time readout shows remaining (true) or elapsed time.
    pub time_remain: bool,
    /// Beats per BEAT JUMP press on screen (SHIFT: ×4).
    pub beat_jump: f64,
    /// Quantize resolution in beats: 1, 0.5, 0.25 or 0.125.
    pub quantize_beats: f64,
    pub auto_cue: bool,
    /// Auto cue to the first sound above this level (dB); unset = to the first memory cue.
    pub auto_cue_db: Option<f32>,
    pub hotcue_autoload: bool,
    pub fader_curve: FaderCurve,
    pub crossfader_curve: CrossfaderCurve,
    pub beat_fx_quantize: bool,
    pub fx: FxPrefs,
    pub midi: MidiMap,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            music_dir: None,
            usb_paths: Vec::new(),
            audio_device: None,
            alsa: false,
            buffer_frames: 256,
            split_cue: false,
            midi_port: None,
            fullscreen: true,
            quantize: true,
            decks: [DeckPrefs::default(), DeckPrefs::default()],
            master_level: 0.8,
            phones_mix: 0.0,
            phones_level: 0.8,
            waveform_seconds: 8.0,
            bpm_range: [70.0, 180.0],
            grid_offset_ms: HashMap::new(),
            background_analysis: true,
            gamepad: true,
            keep_awake: true,
            recording_dir: None,
            apply_usb_settings: true,
            waveform_color: WaveColor::Rgb,
            overview_full: true,
            playhead_left: false,
            time_remain: true,
            beat_jump: 4.0,
            quantize_beats: 1.0,
            auto_cue: true,
            auto_cue_db: None,
            hotcue_autoload: true,
            fader_curve: FaderCurve::Linear,
            crossfader_curve: CrossfaderCurve::SlowCut,
            beat_fx_quantize: true,
            fx: FxPrefs::default(),
            midi: MidiMap::default(),
        }
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config")).join("platter")
}

pub fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".local/share")).join("platter")
}

/// Write via a temporary file so a crash never leaves a half-written file.
fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(tmp, path)
}

impl Config {
    pub fn path() -> PathBuf {
        config_dir().join("config.toml")
    }

    /// Load settings; a broken file is kept aside and defaults are used.
    pub fn load() -> (Config, Option<String>) {
        let p = Self::path();
        match std::fs::read_to_string(&p) {
            Ok(s) => match toml::from_str(&s) {
                Ok(c) => (c, None),
                Err(e) => {
                    let bad = p.with_extension("toml.broken");
                    let _ = std::fs::rename(&p, &bad);
                    (Config::default(), Some(format!("Settings file was unreadable and was reset (kept as {}): {e}", bad.display())))
                }
            },
            Err(_) => (Config::default(), None),
        }
    }

    /// Save on a short-lived thread (errors are logged).
    pub fn save_in_background(&self) {
        match toml::to_string_pretty(self) {
            Ok(s) => write_in_background(Some((Self::path(), s.into_bytes()))),
            Err(e) => eprintln!("settings: {e}"),
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let s = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        write_atomic(&Self::path(), s.as_bytes()).map_err(|e| e.to_string())
    }

    pub fn offset_for(&self, path: &Path) -> f64 {
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        self.grid_offset_ms.get(&ext).copied().unwrap_or(0.0)
    }
}

// ---------------------------------------------------------------------------------------------
// Per-track store
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Record {
    pub size: u64,
    pub mtime: u64,
    pub tags: Option<Tags>,
    /// Detected or fixed grid: (bpm, first beat seconds).
    pub grid: Option<(f64, f64)>,
    /// The grid was fixed by hand (×2, ÷2) and must win over rekordbox/detection.
    pub grid_fixed: bool,
    pub cues: Option<Vec<SavedCue>>,
    pub cue_point: Option<f64>,
    /// A waveform for this file is in the cache.
    pub wave: bool,
}

fn stamp(path: &Path) -> (u64, u64) {
    std::fs::metadata(path)
        .map(|m| {
            let mt = m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
            (m.len(), mt)
        })
        .unwrap_or((0, 0))
}

pub struct Store {
    path: PathBuf,
    map: HashMap<String, Record>,
    dirty: bool,
    last_save: Instant,
}

impl Store {
    pub fn open(path: PathBuf) -> Store {
        let map = std::fs::read(&path).ok().and_then(|d| serde_json::from_slice(&d).ok()).unwrap_or_default();
        Store { path, map, dirty: false, last_save: Instant::now() }
    }

    pub fn open_default() -> Store {
        Self::open(data_dir().join("tracks.json"))
    }

    fn key(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    /// Record for a file, if it still matches the file on disk (size and mtime).
    pub fn get(&self, path: &Path) -> Option<&Record> {
        let r = self.map.get(&Self::key(path))?;
        let (size, mtime) = stamp(path);
        (r.size == size && r.mtime == mtime).then_some(r)
    }

    pub fn update(&mut self, path: &Path, f: impl FnOnce(&mut Record)) {
        let (size, mtime) = stamp(path);
        let r = self.map.entry(Self::key(path)).or_default();
        if r.size != size || r.mtime != mtime {
            *r = Record { size, mtime, ..Default::default() };
        }
        let before = r.clone();
        f(r);
        if *r != before {
            self.dirty = true;
        }
    }

    /// Serialise for saving if something changed (and it's been a while, unless `force`).
    /// Writing happens elsewhere, so the caller never blocks on the disk.
    pub fn snapshot(&mut self, force: bool) -> Option<(PathBuf, Vec<u8>)> {
        if !self.dirty || (!force && self.last_save.elapsed() < Duration::from_secs(2)) {
            return None;
        }
        let d = serde_json::to_vec(&self.map).ok()?;
        self.dirty = false;
        self.last_save = Instant::now();
        Some((self.path.clone(), d))
    }

    /// Save if something changed and the last save was a while ago (or `force`).
    pub fn flush(&mut self, force: bool) {
        if !self.dirty || (!force && self.last_save.elapsed() < Duration::from_secs(2)) {
            return;
        }
        if let Ok(d) = serde_json::to_vec(&self.map) {
            if write_atomic(&self.path, &d).is_ok() {
                self.dirty = false;
                self.last_save = Instant::now();
            }
        }
    }
}

/// Write a [`Store::snapshot`] now (from a background thread).
pub fn write_now(job: Option<(PathBuf, Vec<u8>)>) {
    if let Some((path, data)) = job {
        if let Err(e) = write_atomic(&path, &data) {
            eprintln!("store: couldn't save {}: {e}", path.display());
        }
    }
}

/// Write a [`Store::snapshot`] on a short-lived thread.
pub fn write_in_background(job: Option<(PathBuf, Vec<u8>)>) {
    if let Some((path, data)) = job {
        std::thread::spawn(move || {
            if let Err(e) = write_atomic(&path, &data) {
                eprintln!("store: couldn't save {}: {e}", path.display());
            }
        });
    }
}

#[allow(dead_code)]
pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trips_and_tolerates_missing_fields() {
        let mut c = Config::default();
        c.quantize = false;
        c.grid_offset_ms.insert("mp3".into(), 26.0);
        let s = toml::to_string_pretty(&c).unwrap();
        let back: Config = toml::from_str(&s).unwrap();
        assert_eq!(back, c);
        let partial: Config = toml::from_str("waveform_seconds = 12.0\n[midi]\nplay = 11\n").unwrap();
        assert_eq!(partial.waveform_seconds, 12.0);
        assert!(partial.quantize);
    }

    #[test]
    fn store_invalidates_when_file_changes() {
        let dir = std::env::temp_dir().join(format!("platter-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.wav");
        std::fs::write(&f, b"one").unwrap();
        let mut s = Store::open(dir.join("tracks.json"));
        s.update(&f, |r| r.cue_point = Some(1.5));
        s.flush(true);
        let s2 = Store::open(dir.join("tracks.json"));
        assert_eq!(s2.get(&f).unwrap().cue_point, Some(1.5));
        std::fs::write(&f, b"changed!").unwrap();
        assert!(s2.get(&f).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}
