//! Tracks: progressive decoding into a lock-free sample store, beat grids, cues and tags.
//!
//! A track becomes playable about a second after loading starts; decoding continues in the
//! background while the deck plays. The audio thread reads samples through atomics, so it
//! never waits on the decoder.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use arc_swap::ArcSwapOption;
use parking_lot::{Mutex, RwLock};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, StandardTag};
use symphonia::core::units::Duration;

use crate::analysis::{self, WaveBuilder};

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
// Sample store
// ---------------------------------------------------------------------------------------------

const CHUNK_SHIFT: usize = 16;
const CHUNK_FRAMES: usize = 1 << CHUNK_SHIFT;
const CHUNK_MASK: usize = CHUNK_FRAMES - 1;
/// 8192 × 65536 frames ≈ 3 hours at 48 kHz.
const MAX_CHUNKS: usize = 8192;

/// Append-only stereo sample store: one writer (the loader), any number of readers.
/// Chunks are allocated as decoding proceeds and never move, so readers need no locks.
pub struct Audio {
    chunks: Box<[OnceLock<Box<[AtomicU32]>>]>,
    decoded: AtomicUsize,
    complete: AtomicBool,
}

impl Default for Audio {
    fn default() -> Self {
        Self::new()
    }
}

impl Audio {
    pub fn new() -> Self {
        Self {
            chunks: (0..MAX_CHUNKS).map(|_| OnceLock::new()).collect(),
            decoded: AtomicUsize::new(0),
            complete: AtomicBool::new(false),
        }
    }

    /// Frames decoded so far.
    #[inline]
    pub fn frames(&self) -> usize {
        self.decoded.load(Ordering::Acquire)
    }

    pub fn is_complete(&self) -> bool {
        self.complete.load(Ordering::Acquire)
    }

    /// One stereo frame; silence past what has been decoded.
    #[inline]
    pub fn frame(&self, i: usize) -> (f32, f32) {
        if i >= self.frames() {
            return (0.0, 0.0);
        }
        match self.chunks.get(i >> CHUNK_SHIFT).and_then(|c| c.get()) {
            Some(c) => {
                let k = (i & CHUNK_MASK) * 2;
                (f32::from_bits(c[k].load(Ordering::Relaxed)), f32::from_bits(c[k + 1].load(Ordering::Relaxed)))
            }
            None => (0.0, 0.0),
        }
    }

    /// Append interleaved stereo. Single writer only.
    pub fn push(&self, interleaved: &[f32]) {
        let mut n = self.decoded.load(Ordering::Relaxed);
        for f in interleaved.chunks_exact(2) {
            let ci = n >> CHUNK_SHIFT;
            if ci >= MAX_CHUNKS {
                break;
            }
            let c = self.chunks[ci].get_or_init(|| (0..CHUNK_FRAMES * 2).map(|_| AtomicU32::new(0)).collect());
            let k = (n & CHUNK_MASK) * 2;
            c[k].store(f[0].to_bits(), Ordering::Relaxed);
            c[k + 1].store(f[1].to_bits(), Ordering::Relaxed);
            n += 1;
        }
        self.decoded.store(n, Ordering::Release);
    }

    pub fn finish(&self) {
        self.complete.store(true, Ordering::Release);
    }
}

// ---------------------------------------------------------------------------------------------
// Beat grid
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridSource {
    Detected,
    Rekordbox,
    Saved,
}

/// Beat positions (frames, ascending) — constant or variable tempo. Positions outside the
/// listed beats are extrapolated from the nearest beat interval.
#[derive(Clone, Debug)]
pub struct Grid {
    pub beats: Vec<f64>,
    /// Exact tempo per beat when known (rekordbox stores whole-millisecond beat times, so
    /// tempos derived from positions would be slightly off).
    pub bpm_list: Vec<f64>,
    /// Beat `k` is a downbeat when `(k + bar_phase) % 4 == 0`.
    pub bar_phase: usize,
    /// Tempo at the start of the track.
    pub bpm: f64,
    pub source: GridSource,
    sample_rate: f64,
}

impl Grid {
    /// A constant-tempo grid covering at least `length` frames.
    pub fn constant(bpm: f64, first_beat: f64, sample_rate: u32, length: usize, source: GridSource) -> Grid {
        let sr = sample_rate as f64;
        let bl = sr * 60.0 / bpm;
        let mut b = first_beat.rem_euclid(bl);
        let mut beats = Vec::new();
        let end = length.max(sample_rate as usize * 2) as f64 + bl;
        while b < end {
            beats.push(b);
            b += bl;
        }
        Grid { beats, bpm_list: Vec::new(), bar_phase: 0, bpm, source, sample_rate: sr }
    }

    fn from_rekordbox(a: &crate::rekordbox::Analysis, sample_rate: u32, offset_ms: f64) -> Option<Grid> {
        let mut beats = a.beats.clone();
        beats.dedup_by(|b, a| b.time_ms <= a.time_ms);
        if beats.len() < 2 {
            return None;
        }
        let sr = sample_rate as f64;
        let bpm_list: Vec<f64> = if beats.iter().all(|b| b.tempo > 0) {
            beats.iter().map(|b| b.tempo as f64 / 100.0).collect()
        } else {
            Vec::new()
        };
        let pos: Vec<f64> = beats.iter().map(|b| (b.time_ms as f64 + offset_ms) * sr / 1000.0).collect();
        let bpm = bpm_list.first().copied().unwrap_or(60.0 * sr / (pos[1] - pos[0]));
        Some(Grid {
            beats: pos,
            bpm_list,
            bar_phase: (beats[0].number.clamp(1, 4) as usize - 1) % 4,
            bpm,
            source: GridSource::Rekordbox,
            sample_rate: sr,
        })
    }

    pub fn beat_at(&self, pos: f64) -> f64 {
        let g = &self.beats;
        let n = g.len();
        let i = g.partition_point(|&b| b <= pos).saturating_sub(1).min(n - 2);
        i as f64 + (pos - g[i]) / (g[i + 1] - g[i])
    }

    pub fn pos_of_beat(&self, beat: f64) -> f64 {
        let g = &self.beats;
        let n = g.len();
        let i = (beat.floor().max(0.0) as usize).min(n - 2);
        g[i] + (beat - i as f64) * (g[i + 1] - g[i])
    }

    pub fn bpm_at(&self, pos: f64) -> f64 {
        if self.bpm_list.len() == self.beats.len() && !self.bpm_list.is_empty() {
            let i = self.beats.partition_point(|&b| b <= pos).saturating_sub(1);
            return self.bpm_list[i];
        }
        let b = self.beat_at(pos).floor();
        60.0 * self.sample_rate / (self.pos_of_beat(b + 1.0) - self.pos_of_beat(b))
    }

    pub fn is_downbeat(&self, k: i64) -> bool {
        (k + self.bar_phase as i64).rem_euclid(4) == 0
    }

    /// The same grid at another tempo octave (×2 or ÷2), to fix half/double detections.
    pub fn scaled(&self, factor: f64) -> Grid {
        let len = self.beats.last().copied().unwrap_or(0.0) as usize;
        let mut g = Grid::constant(self.bpm * factor, self.first_beat(), self.sample_rate as u32, len, GridSource::Saved);
        g.bar_phase = self.bar_phase;
        g
    }

    pub fn first_beat(&self) -> f64 {
        self.beats.first().copied().unwrap_or(0.0)
    }

    pub fn is_constant(&self) -> bool {
        self.bpm_list.is_empty() || self.bpm_list.iter().all(|b| (b - self.bpm).abs() < 1e-9)
    }
}

// ---------------------------------------------------------------------------------------------
// Track
// ---------------------------------------------------------------------------------------------

/// A cue that came with the track (rekordbox export or Platter's own saved cues).
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

/// A cue in seconds, as saved by Platter.
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
            band_scale: rb_waves.as_deref().map(crate::wave::band_scale).unwrap_or(1.0),
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

// ---------------------------------------------------------------------------------------------
// Tags
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Tags {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub key: Option<String>,
    pub bpm: Option<f64>,
    pub duration: Option<f64>,
}

/// Open an audio file for reading packets and metadata.
pub fn open_format(path: &Path) -> Result<Box<dyn FormatReader>, String> {
    open(path)
}

fn open(path: &Path) -> Result<Box<dyn FormatReader>, String> {
    let file = File::open(path).map_err(|e| format!("can't open file: {e}"))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .map_err(|e| format!("unsupported format ({e})"))
}

fn tags_of(format: &mut Box<dyn FormatReader>) -> Tags {
    let mut t = Tags::default();
    if let Some(track) = format.default_track(TrackType::Audio) {
        let sr = track.codec_params.as_ref().and_then(|p| p.audio()).and_then(|a| a.sample_rate);
        if let (Some(n), Some(sr)) = (track.num_frames, sr) {
            t.duration = Some(n as f64 / sr as f64);
        }
    }
    let mut md = format.metadata();
    if let Some(rev) = md.skip_to_latest() {
        for tag in &rev.media.tags {
            match &tag.std {
                Some(StandardTag::TrackTitle(s)) => t.title = Some(s.to_string()),
                Some(StandardTag::Artist(s)) => t.artist = Some(s.to_string()),
                Some(StandardTag::InitialKey(s)) => t.key = Some(s.to_string()),
                Some(StandardTag::Bpm(b)) if *b > 0 => t.bpm = Some(*b as f64),
                _ => {}
            }
        }
    }
    t
}

/// Read title/artist/key/BPM/duration without decoding audio.
pub fn read_tags(path: &Path) -> Result<Tags, String> {
    let mut f = open(path)?;
    Ok(tags_of(&mut f))
}

// ---------------------------------------------------------------------------------------------
// Progressive loading
// ---------------------------------------------------------------------------------------------

/// Seconds that must be decoded before a track is handed to a deck.
const READY_SECS: f64 = 1.0;

/// Decode `path` progressively. `ready` is called once, as soon as the first second is
/// decoded (or the file ends); the track keeps filling in afterwards. Returns when decoding
/// and analysis are finished.
pub fn load_progressive(path: &Path, mut meta: TrackMeta, ready: impl FnOnce(Arc<Track>)) -> Result<Arc<Track>, String> {
    if !path.exists() {
        return Err(format!("file not found: {}", path.display()));
    }
    let mut format = open(path)?;
    let tags = tags_of(&mut format);
    if meta.title.is_none() && meta.analysis.is_none() {
        meta.title = tags.title.clone();
        meta.artist = tags.artist.clone();
    }
    if meta.key.is_none() {
        meta.key = tags.key.clone();
    }
    let track = format.default_track(TrackType::Audio).ok_or("no audio track")?;
    let track_id = track.id;
    let params = track.codec_params.as_ref().and_then(|p| p.audio()).ok_or("no audio codec parameters")?.clone();
    let num_frames = track.num_frames;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .map_err(|e| format!("unsupported codec ({e})"))?;

    let mut ready = Some(ready);
    let mut tr: Option<Arc<Track>> = None;
    let mut wave = None::<WaveBuilder>;
    let mut bins: Vec<WaveBin> = Vec::new();
    let mut flushed_bins = 0usize;
    let mut mono: Vec<f32> = Vec::new();
    let mut need_grid = false;
    let mut tmp: Vec<f32> = Vec::new();
    let mut stereo: Vec<f32> = Vec::new();

    let finish_err = |tr: &Option<Arc<Track>>, e: String| -> Result<(), String> {
        match tr {
            Some(t) if t.frames() > 0 => {
                *t.error.lock() = Some(e);
                Ok(())
            }
            _ => Err(e),
        }
    };

    loop {
        let mut packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => break,
            Err(e) => {
                finish_err(&tr, format!("read error ({e})"))?;
                break;
            }
        };
        if packet.track_id != track_id {
            continue;
        }
        // No gapless trimming: rekordbox's timeline (beat grids, cues) includes the MP3 encoder
        // delay, so untrimmed decoding lines up with exported analysis.
        packet.trim_start = Duration::ZERO;
        packet.trim_end = Duration::ZERO;
        let buf = match decoder.decode(&packet) {
            Ok(b) => b,
            Err(SymError::DecodeError(_)) => continue,
            Err(e) => {
                finish_err(&tr, format!("decode error ({e})"))?;
                break;
            }
        };
        let ch = buf.spec().channels().count().max(1);
        let sr = buf.spec().rate();
        tmp.resize(buf.samples_interleaved(), 0.0);
        buf.copy_to_slice_interleaved(&mut tmp);
        let t = tr.get_or_insert_with(|| {
            let expected = num_frames
                .map(|n| n as usize)
                .or(meta.duration_hint.map(|d| (d * sr as f64) as usize))
                .unwrap_or(0);
            let t = Arc::new(Track::new(path.to_path_buf(), sr, expected, &meta));
            need_grid = t.grid.load().is_none();
            if need_grid {
                mono.reserve(expected);
            }
            wave = Some(WaveBuilder::new(sr));
            t
        });
        stereo.clear();
        let wb = wave.as_mut().expect("wave builder");
        for frame in tmp.chunks_exact(ch) {
            let l = frame[0];
            let r = if ch > 1 { frame[1] } else { l };
            stereo.push(l);
            stereo.push(r);
            wb.push(l, r, &mut bins);
            if need_grid {
                mono.push(0.5 * (l + r));
            }
        }
        t.audio.push(&stereo);
        // Publish waveform progress in modest batches (unless a cached one is already shown).
        if meta.cached_wave.is_none() && bins.len() - flushed_bins >= 30 {
            t.wave.write().extend_from_slice(&bins[flushed_bins..]);
            flushed_bins = bins.len();
        }
        if ready.is_some() && t.frames() as f64 >= READY_SECS * sr as f64 {
            if let Some(f) = ready.take() {
                f(t.clone());
            }
        }
    }
    let t = tr.ok_or("file contains no audio")?;
    if let Some(wb) = wave.as_mut() {
        wb.finish(&mut bins);
    }
    if meta.cached_wave.is_none() {
        t.wave.write().extend_from_slice(&bins[flushed_bins..]);
    }
    t.audio.finish();
    if let Some(f) = ready.take() {
        f(t.clone());
    }
    if need_grid {
        t.detect_grid(&mono, &meta);
    }
    Ok(t)
}

/// Background library analysis: tags plus a detected grid (bpm, first beat in seconds),
/// decoding to mono only. Much lighter than a full load.
pub fn analyze_file(path: &Path, bpm_range: (f64, f64)) -> Result<(Tags, Option<(f64, f64)>, Vec<WaveBin>), String> {
    let mut format = open(path)?;
    let mut tags = tags_of(&mut format);
    let track = format.default_track(TrackType::Audio).ok_or("no audio track")?;
    let track_id = track.id;
    let params = track.codec_params.as_ref().and_then(|p| p.audio()).ok_or("no audio codec parameters")?.clone();
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .map_err(|e| format!("unsupported codec ({e})"))?;
    let mut mono: Vec<f32> = Vec::new();
    let mut tmp: Vec<f32> = Vec::new();
    let mut sr = 0;
    let mut wave: Option<WaveBuilder> = None;
    let mut bins = Vec::new();
    loop {
        let mut packet = match format.next_packet() {
            Ok(Some(p)) => p,
            _ => break,
        };
        if packet.track_id != track_id {
            continue;
        }
        packet.trim_start = Duration::ZERO;
        packet.trim_end = Duration::ZERO;
        let Ok(buf) = decoder.decode(&packet) else { continue };
        let ch = buf.spec().channels().count().max(1);
        sr = buf.spec().rate();
        tmp.resize(buf.samples_interleaved(), 0.0);
        buf.copy_to_slice_interleaved(&mut tmp);
        let wb = wave.get_or_insert_with(|| WaveBuilder::new(sr));
        for f in tmp.chunks_exact(ch) {
            let (l, r) = (f[0], if ch > 1 { f[1] } else { f[0] });
            wb.push(l, r, &mut bins);
            mono.push(0.5 * (l + r));
        }
    }
    if let Some(wb) = wave.as_mut() {
        wb.finish(&mut bins);
    }
    if sr == 0 || mono.is_empty() {
        return Err("no audio".into());
    }
    tags.duration = Some(mono.len() as f64 / sr as f64);
    let grid = analysis::detect_beat_grid_mono(&mono, sr)
        .map(|g| (analysis::fold_bpm(g.bpm, bpm_range.0, bpm_range.1), g.first_beat / sr as f64));
    Ok((tags, grid, bins))
}

/// Blocking load (tests and tools).
#[allow(dead_code)]
pub fn load(path: &Path, meta: TrackMeta) -> Result<Arc<Track>, String> {
    load_progressive(path, meta, |_| {})
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn write_wav(path: &Path, secs: f32, sr: u32, bpm: f64) {
        let n = (secs * sr as f32) as usize;
        let s = crate::analysis::click_track(bpm, 0.2, secs as f64, sr);
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
