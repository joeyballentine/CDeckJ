//! Tracks: progressive decoding into a lock-free sample store, beat grids, cues and tags.
//!
//! A track becomes playable about a second after loading starts; decoding continues in the
//! background while the deck plays. The audio thread reads samples through atomics, so it
//! never waits on the decoder.

pub mod analysis;
pub mod wave;

mod samples;
mod grid;
mod decode;

pub use samples::*;
pub use grid::*;
pub use decode::*;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use arc_swap::ArcSwapOption;
use parking_lot::{Mutex, RwLock};


/// Waveform resolution: summary bins per second of audio.
pub const WAVE_BINS_PER_SEC: f64 = 150.0;

/// One waveform column. All values 0..=255.
#[derive(Clone, Copy, Default, Debug)]
pub struct WaveBin {
    pub peak: u8,
    pub low: u8,
    pub mid: u8,
    pub high: u8,
}

// ---------------------------------------------------------------------------------------------
// Track
// ---------------------------------------------------------------------------------------------

/// A cue that came with the track (rekordbox export or CDeckJ's own saved cues).
#[derive(Clone, Debug, PartialEq)]
pub struct PresetCue {
    /// Hot cue slot (0 = A), or `None` for a memory cue.
    pub hot: Option<usize>,
    /// Frames.
    pub pos: f64,
    pub loop_end: Option<f64>,
    pub color: Option<[u8; 3]>,
    pub comment: String,
}

/// A cue in seconds, as saved by CDeckJ.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SavedCue {
    pub hot: Option<usize>,
    pub at: f64,
    #[serde(default)]
    pub loop_end: Option<f64>,
    #[serde(default)]
    pub color: Option<[u8; 3]>,
    #[serde(default)]
    pub comment: String,
}

/// Everything known about a track before decoding starts.
#[derive(Clone, Debug)]
pub struct TrackMeta {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub key: Option<String>,
    pub duration_hint: Option<f64>,
    pub analysis: Option<crate::rekordbox::Analysis>,
    /// Saved grid: (bpm, first beat in seconds).
    pub saved_grid: Option<(f64, f64)>,
    /// Saved cues replace the export's cues when present.
    pub saved_cues: Option<Vec<SavedCue>>,
    pub saved_cue_point: Option<f64>,
    /// Fold detected tempos into this range.
    pub bpm_range: (f64, f64),
    /// Added to rekordbox grid/cue times for this file type.
    pub offset_ms: f64,
    /// Where the CUE point starts (the stick's AUTO CUE setting).
    pub auto_cue: AutoCue,
    /// Put the export's hot cues on the pads (HOT CUE AUTO LOAD).
    pub hotcue_autoload: bool,
    /// A waveform computed earlier: shown whole from the start instead of filling in.
    pub cached_wave: Option<Vec<WaveBin>>,
}

/// Where a freshly loaded track's CUE point goes when no cue point was saved.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AutoCue {
    Off,
    /// First sound louder than this many dBFS.
    Level(f32),
    /// First memory cue, else the first sound.
    Memory,
}

impl AutoCue {
    pub const DEFAULT_DB: f32 = -50.0;
}

impl Default for TrackMeta {
    fn default() -> Self {
        Self {
            title: None,
            artist: None,
            key: None,
            duration_hint: None,
            analysis: None,
            saved_grid: None,
            saved_cues: None,
            saved_cue_point: None,
            bpm_range: (70.0, 180.0),
            offset_ms: 0.0,
            auto_cue: AutoCue::Memory,
            hotcue_autoload: true,
            cached_wave: None,
        }
    }
}

pub struct Track {
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub key: String,
    pub sample_rate: u32,
    pub audio: Audio,
    /// Best guess at the final length while decoding (0 if unknown).
    pub expected_frames: usize,
    pub grid: ArcSwapOption<Grid>,
    pub cues: Vec<PresetCue>,
    /// Cue point to start on (saved), else the first memory cue, else auto-cue.
    pub initial_cue: Option<f64>,
    pub wave: RwLock<Vec<WaveBin>>,
    /// rekordbox's own waveforms, when the track came from an export.
    pub rb_waves: Option<Arc<crate::rekordbox::anlz::Waves>>,
    pub band_scale: f32,
    /// Grid offset applied to the rekordbox waveforms too (seconds).
    pub rb_offset_s: f64,
    pub auto_cue: AutoCue,
    /// A decode error after the track became playable (the decoded part stays usable).
    pub error: Mutex<Option<String>>,
}

impl std::fmt::Debug for Track {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Track({})", self.path.display())
    }
}

impl Track {
    fn new(path: PathBuf, sample_rate: u32, expected: usize, meta: &TrackMeta) -> Track {
        let (title, artist) = names_from_path(&path);
        let sr = sample_rate as f64;
        let ms = |t: u32| (t as f64 + meta.offset_ms) * sr / 1000.0;
        let mut cues = Vec::new();
        let mut initial_cue = None;
        let mut grid = None;
        if let Some(a) = &meta.analysis {
            grid = Grid::from_rekordbox(a, sample_rate, meta.offset_ms);
            let hot = a.hot_cues.iter().filter(|_| meta.hotcue_autoload);
            for c in hot.chain(a.memory_cues.iter()) {
                cues.push(PresetCue {
                    hot: (c.hot > 0 && c.hot <= 8).then(|| c.hot as usize - 1),
                    pos: ms(c.time_ms),
                    loop_end: c.loop_end_ms.map(ms),
                    color: c.color,
                    comment: c.comment.clone(),
                });
            }
        }
        if let Some(saved) = &meta.saved_cues {
            cues = saved
                .iter()
                .map(|c| PresetCue {
                    hot: c.hot,
                    pos: c.at * sr,
                    loop_end: c.loop_end.map(|e| e * sr),
                    color: c.color,
                    comment: c.comment.clone(),
                })
                .collect();
        }
        cues.sort_by(|a, b| a.pos.total_cmp(&b.pos));
        if let Some(c) = meta.saved_cue_point {
            initial_cue = Some(c * sr);
        } else {
            match meta.auto_cue {
                AutoCue::Off => initial_cue = Some(0.0),
                AutoCue::Memory => initial_cue = cues.iter().find(|c| c.hot.is_none()).map(|c| c.pos),
                // Found from the audio when the deck loads it.
                AutoCue::Level(_) => {}
            }
        }
        let rb_waves = meta.analysis.as_ref().map(|a| &a.waves).filter(|w| !w.is_empty()).map(|w| Arc::new(w.clone()));
        if let Some((bpm, first)) = meta.saved_grid.filter(|g| g.0 > 0.0) {
            // A saved grid is a user fix (e.g. ×2): it wins over detection and the export.
            grid = Some(Grid::constant(bpm, first * sr, sample_rate, expected, GridSource::Saved));
        }
        Track {
            path,
            title: meta.title.clone().filter(|t| !t.is_empty()).unwrap_or(title),
            artist: meta.artist.clone().filter(|a| !a.is_empty()).unwrap_or(artist),
            key: meta.key.clone().unwrap_or_default(),
            sample_rate,
            audio: Audio::new(),
            expected_frames: expected,
            grid: ArcSwapOption::from(grid.map(Arc::new)),
            cues,
            initial_cue,
            wave: RwLock::new(meta.cached_wave.clone().unwrap_or_default()),
            band_scale: rb_waves.as_deref().map(crate::track::wave::band_scale).unwrap_or(1.0),
            rb_waves,
            rb_offset_s: meta.offset_ms / 1000.0,
            auto_cue: meta.auto_cue,
            error: Mutex::new(None),
        }
    }

    /// Level (linear) for finding the first sound when auto-cueing.
    pub fn auto_cue_threshold(&self) -> f32 {
        let db = match self.auto_cue {
            AutoCue::Level(db) => db,
            _ => AutoCue::DEFAULT_DB,
        };
        10f32.powf(db / 20.0)
    }

    /// Build a complete track from interleaved stereo in memory (tests, generated audio).
    #[allow(dead_code)]
    pub fn from_samples(path: PathBuf, sample_rate: u32, samples: Vec<f32>) -> Track {
        Self::from_parts(path, sample_rate, samples, TrackMeta::default())
    }

    #[allow(dead_code)]
    pub fn from_parts(path: PathBuf, sample_rate: u32, samples: Vec<f32>, meta: TrackMeta) -> Track {
        let frames = samples.len() / 2;
        let t = Track::new(path, sample_rate, frames, &meta);
        t.audio.push(&samples);
        t.audio.finish();
        *t.wave.write() = analysis::waveform(&samples, sample_rate);
        if t.grid.load().is_none() {
            let mono: Vec<f32> = samples.chunks_exact(2).map(|f| 0.5 * (f[0] + f[1])).collect();
            t.detect_grid(&mono, &meta);
        }
        t
    }

    #[cfg(test)]
    pub fn partial_for_test(sample_rate: u32, expected: usize) -> Track {
        Track::new(PathBuf::from("partial.wav"), sample_rate, expected, &TrackMeta::default())
    }

    fn detect_grid(&self, mono: &[f32], meta: &TrackMeta) {
        if let Some(g) = analysis::detect_beat_grid_mono(mono, self.sample_rate) {
            let bpm = analysis::fold_bpm(g.bpm, meta.bpm_range.0, meta.bpm_range.1);
            let grid = Grid::constant(bpm, g.first_beat, self.sample_rate, mono.len(), GridSource::Detected);
            self.grid.store(Some(Arc::new(grid)));
        }
    }

    /// Frames decoded so far.
    #[inline]
    pub fn frames(&self) -> usize {
        self.audio.frames()
    }

    /// Track length for display and seeking: the expected length while still decoding.
    pub fn length(&self) -> usize {
        let f = self.audio.frames();
        if self.audio.is_complete() { f } else { f.max(self.expected_frames) }
    }

    pub fn is_complete(&self) -> bool {
        self.audio.is_complete()
    }

    pub fn grid_source(&self) -> Option<GridSource> {
        self.grid.load().as_ref().map(|g| g.source)
    }

    pub fn bpm(&self) -> Option<f64> {
        self.grid.load().as_ref().map(|g| g.bpm)
    }

    pub fn beat_at(&self, pos: f64) -> Option<f64> {
        self.grid.load().as_ref().map(|g| g.beat_at(pos))
    }

    pub fn pos_of_beat(&self, beat: f64) -> Option<f64> {
        self.grid.load().as_ref().map(|g| g.pos_of_beat(beat))
    }

    pub fn bpm_at(&self, pos: f64) -> Option<f64> {
        self.grid.load().as_ref().map(|g| g.bpm_at(pos))
    }

    /// Nearest beat position to `pos` (quantize), or `pos` without a grid.
    #[allow(dead_code)]
    pub fn snap(&self, pos: f64) -> f64 {
        self.snap_to(pos, 1.0)
    }

    /// Nearest multiple of `beats` (e.g. ½ beat) to `pos`.
    pub fn snap_to(&self, pos: f64, beats: f64) -> f64 {
        let div = if beats > 0.0 { beats } else { 1.0 };
        self.beat_at(pos).and_then(|b| self.pos_of_beat((b / div).round() * div)).unwrap_or(pos)
    }
}

/// "Artist - Title.mp3" → ("Title", "Artist"); otherwise the file stem is the title.
pub fn names_from_path(path: &Path) -> (String, String) {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    match stem.split_once(" - ") {
        Some((a, t)) => (t.trim().to_string(), a.trim().to_string()),
        None => (stem, String::new()),
    }
}

pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "wav", "wave", "ogg", "oga", "m4a", "mp4", "aac", "aif", "aiff", "caf", "mka",
    "webm",
];

pub fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn write_wav(path: &Path, secs: f32, sr: u32, bpm: f64) {
        let n = (secs * sr as f32) as usize;
        let s = crate::track::analysis::click_track(bpm, 0.2, secs as f64, sr);
        let mut bytes = Vec::new();
        let data_len = (n * 4) as u32;
        bytes.extend(b"RIFF");
        bytes.extend((36 + data_len).to_le_bytes());
        bytes.extend(b"WAVEfmt ");
        bytes.extend(16u32.to_le_bytes());
        bytes.extend(1u16.to_le_bytes());
        bytes.extend(2u16.to_le_bytes());
        bytes.extend(sr.to_le_bytes());
        bytes.extend((sr * 4).to_le_bytes());
        bytes.extend(4u16.to_le_bytes());
        bytes.extend(16u16.to_le_bytes());
        bytes.extend(b"data");
        bytes.extend(data_len.to_le_bytes());
        for v in s.iter().take(n * 2) {
            bytes.extend(((v.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
        }
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn audio_store_reads_back_across_chunks() {
        let a = Audio::new();
        let n = CHUNK_FRAMES + 10;
        let s: Vec<f32> = (0..n * 2).map(|i| i as f32).collect();
        a.push(&s[..CHUNK_FRAMES]); // half a chunk's worth of frames
        assert_eq!(a.frames(), CHUNK_FRAMES / 2);
        assert_eq!(a.frame(CHUNK_FRAMES / 2), (0.0, 0.0)); // not decoded yet: silence
        a.push(&s[CHUNK_FRAMES..]);
        assert_eq!(a.frames(), n);
        let k = CHUNK_FRAMES + 3;
        assert_eq!(a.frame(k), ((k * 2) as f32, (k * 2 + 1) as f32));
        assert!(!a.is_complete());
        a.finish();
        assert!(a.is_complete());
    }

    #[test]
    fn progressive_load_is_ready_early_and_completes() {
        let dir = std::env::temp_dir().join(format!("platter-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("Artist - Song.wav");
        write_wav(&p, 20.0, 44100, 124.0);
        let early = std::sync::Mutex::new(None);
        let t = load_progressive(&p, TrackMeta::default(), |t| {
            *early.lock().unwrap() = Some((t.frames(), t.is_complete()));
        })
        .unwrap();
        let (frames_at_ready, complete_at_ready) = early.lock().unwrap().unwrap();
        assert!(frames_at_ready >= 44100 && frames_at_ready < 44100 * 19, "{frames_at_ready}");
        assert!(!complete_at_ready);
        assert!(t.is_complete());
        assert_eq!(t.frames(), 44100 * 20);
        assert_eq!(t.title, "Song");
        assert!((t.bpm().unwrap() - 124.0).abs() < 0.05);
        assert!(t.wave.read().len() >= 20 * 150);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn analyze_file_finds_tempo() {
        let dir = std::env::temp_dir().join(format!("platter-an-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x.wav");
        write_wav(&p, 20.0, 44100, 87.0);
        let (tags, grid, bins) = analyze_file(&p, (88.0, 176.0)).unwrap();
        assert!(bins.len() > 100);
        assert!((tags.duration.unwrap() - 20.0).abs() < 0.01);
        assert!((grid.unwrap().0 - 174.0).abs() < 0.1, "folded into range: {:?}", grid);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn grid_scaling_and_snap() {
        let g = Grid::constant(87.0, 1000.0, 48000, 48000 * 30, GridSource::Detected);
        let d = g.scaled(2.0);
        assert!((d.bpm - 174.0).abs() < 1e-9);
        assert!((d.bpm_at(48000.0 * 5.0) - 174.0).abs() < 1e-6);
        let t = Track::from_samples(PathBuf::from("x.wav"), 48000, vec![0.0; 48000 * 2]);
        t.grid.store(Some(Arc::new(Grid::constant(120.0, 0.0, 48000, 48000 * 10, GridSource::Saved))));
        assert_eq!(t.snap(48000.0 * 0.49), 24000.0);
    }
}
