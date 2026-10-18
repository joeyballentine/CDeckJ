//! Track decoding (symphonia) and the analysed, in-memory track the decks play from.

use std::fs::File;
use std::path::{Path, PathBuf};

use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Duration;

use crate::analysis;

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

/// A cue that came with the track (from a rekordbox export).
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridSource {
    None,
    Detected,
    Rekordbox,
}

/// Metadata known before decoding (from a rekordbox database, or nothing for plain files).
#[derive(Clone, Debug, Default)]
pub struct TrackMeta {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub key: Option<String>,
    pub analysis: Option<crate::rekordbox::Analysis>,
}

/// A fully decoded track, shared read-only between the UI and the audio thread.
pub struct Track {
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub key: String,
    pub sample_rate: u32,
    /// Interleaved stereo f32.
    pub samples: Vec<f32>,
    pub frames: usize,
    /// Tempo at the start of the track, if it has a beat grid.
    pub bpm: Option<f64>,
    /// Beat positions in frames, ascending. May be variable-tempo (rekordbox grids).
    pub grid: Vec<f64>,
    /// Exact tempo per beat (BPM) when the grid came from rekordbox, parallel to `grid`.
    /// Grid times are stored in whole milliseconds, so tempos derived from them would be
    /// slightly off; sync uses these instead.
    pub grid_bpm: Vec<f64>,
    /// Beat `k` is a downbeat when `(k + bar_phase) % 4 == 0`.
    pub bar_phase: usize,
    pub grid_source: GridSource,
    pub cues: Vec<PresetCue>,
    pub wave: Vec<WaveBin>,
}

impl Track {
    #[allow(dead_code)]
    pub fn duration_secs(&self) -> f64 {
        self.frames as f64 / self.sample_rate as f64
    }

    /// Fractional beat index at a frame position (extrapolated outside the grid).
    pub fn beat_at(&self, pos: f64) -> Option<f64> {
        let g = &self.grid;
        let n = g.len();
        if n < 2 {
            return None;
        }
        let i = g.partition_point(|&b| b <= pos).saturating_sub(1).min(n - 2);
        Some(i as f64 + (pos - g[i]) / (g[i + 1] - g[i]))
    }

    /// Frame position of a fractional beat index (inverse of [`Self::beat_at`]).
    pub fn pos_of_beat(&self, beat: f64) -> Option<f64> {
        let g = &self.grid;
        let n = g.len();
        if n < 2 {
            return None;
        }
        let i = (beat.floor().max(0.0) as usize).min(n - 2);
        Some(g[i] + (beat - i as f64) * (g[i + 1] - g[i]))
    }

    /// Length of the beat around `pos`, in frames.
    pub fn beat_len_at(&self, pos: f64) -> Option<f64> {
        let b = self.beat_at(pos)?;
        Some(self.pos_of_beat(b.floor() + 1.0)? - self.pos_of_beat(b.floor())?)
    }

    pub fn bpm_at(&self, pos: f64) -> Option<f64> {
        if self.grid_bpm.len() == self.grid.len() && !self.grid_bpm.is_empty() {
            let i = self.grid.partition_point(|&b| b <= pos).saturating_sub(1);
            return Some(self.grid_bpm[i]);
        }
        self.beat_len_at(pos).map(|bl| 60.0 * self.sample_rate as f64 / bl)
    }

    pub fn is_downbeat(&self, k: i64) -> bool {
        (k + self.bar_phase as i64).rem_euclid(4) == 0
    }

    /// Build a track from raw interleaved stereo samples (used by decoding and by tests).
    #[allow(dead_code)]
    pub fn from_samples(path: PathBuf, sample_rate: u32, samples: Vec<f32>) -> Track {
        Self::from_parts(path, sample_rate, samples, TrackMeta::default())
    }

    pub fn from_parts(path: PathBuf, sample_rate: u32, samples: Vec<f32>, meta: TrackMeta) -> Track {
        let frames = samples.len() / 2;
        let (title, artist) = names_from_path(&path);
        let wave = analysis::waveform(&samples, sample_rate);
        let sr = sample_rate as f64;
        let ms = |t: u32| t as f64 * sr / 1000.0;

        let mut grid = Vec::new();
        let mut grid_bpm = Vec::new();
        let mut bar_phase = 0;
        let mut bpm = None;
        let mut source = GridSource::None;
        let mut cues = Vec::new();
        if let Some(a) = &meta.analysis {
            if a.beats.len() >= 2 {
                let mut beats = a.beats.clone();
                beats.dedup_by(|b, a| b.time_ms <= a.time_ms); // keep strictly increasing
                grid = beats.iter().map(|b| ms(b.time_ms)).collect();
                if beats.iter().all(|b| b.tempo > 0) {
                    grid_bpm = beats.iter().map(|b| b.tempo as f64 / 100.0).collect();
                }
                bar_phase = (a.beats[0].number.clamp(1, 4) as usize - 1) % 4;
                bpm = Some(a.beats[0].tempo as f64 / 100.0).filter(|b| *b > 0.0);
                source = GridSource::Rekordbox;
            }
            for c in a.hot_cues.iter().chain(a.memory_cues.iter()) {
                cues.push(PresetCue {
                    hot: (c.hot > 0 && c.hot <= 8).then(|| c.hot as usize - 1),
                    pos: ms(c.time_ms).min(frames as f64),
                    loop_end: c.loop_end_ms.map(|e| ms(e).min(frames as f64)),
                    color: c.color,
                    comment: c.comment.clone(),
                });
            }
            cues.sort_by(|a, b| a.pos.total_cmp(&b.pos));
        }
        if grid.len() < 2 {
            if let Some(g) = analysis::detect_beat_grid(&samples, sample_rate) {
                let bl = sr * 60.0 / g.bpm;
                let mut b = g.first_beat;
                while b < frames as f64 + bl {
                    grid.push(b);
                    b += bl;
                }
                bpm = Some(g.bpm);
                bar_phase = 0;
                source = GridSource::Detected;
            }
        }
        if grid.len() >= 2 && bpm.is_none() {
            bpm = Some(60.0 * sr / (grid[1] - grid[0]));
        }
        Track {
            path,
            title: meta.title.filter(|t| !t.is_empty()).unwrap_or(title),
            artist: meta.artist.filter(|a| !a.is_empty()).unwrap_or(artist),
            key: meta.key.unwrap_or_default(),
            sample_rate,
            samples,
            frames,
            bpm,
            grid,
            grid_bpm,
            bar_phase,
            grid_source: source,
            cues,
            wave,
        }
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

/// Decode an audio file to interleaved stereo f32 and analyse it.
pub fn load(path: &Path, meta: TrackMeta) -> Result<Track, String> {
    if !path.exists() {
        return Err(format!("file not found: {}", path.display()));
    }
    let (sample_rate, samples) = decode(path)?;
    if samples.len() < 2 {
        return Err("file contains no audio".into());
    }
    Ok(Track::from_parts(path.to_path_buf(), sample_rate, samples, meta))
}

fn decode(path: &Path) -> Result<(u32, Vec<f32>), String> {
    let file = File::open(path).map_err(|e| format!("open: {e}"))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut format = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .map_err(|e| format!("unsupported format: {e}"))?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or("no audio track")?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or("no audio codec parameters")?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|e| format!("unsupported codec: {e}"))?;

    let mut out: Vec<f32> = Vec::new();
    let mut tmp: Vec<f32> = Vec::new();
    let mut sample_rate = 0u32;
    loop {
        let mut packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => break,
            Err(e) => {
                if out.is_empty() {
                    return Err(format!("read: {e}"));
                }
                break; // keep what decoded so far
            }
        };
        if packet.track_id != track_id {
            continue;
        }
        // No gapless trimming: rekordbox's timeline (beat grids, cues) includes the MP3 encoder
        // delay, so untrimmed decoding lines up with exported analysis.
        packet.trim_start = Duration::ZERO;
        packet.trim_end = Duration::ZERO;
        match decoder.decode(&packet) {
            Ok(buf) => {
                let ch = buf.spec().channels().count().max(1);
                sample_rate = buf.spec().rate();
                tmp.resize(buf.samples_interleaved(), 0.0);
                buf.copy_to_slice_interleaved(&mut tmp);
                out.reserve(tmp.len() / ch * 2);
                for frame in tmp.chunks_exact(ch) {
                    let l = frame[0];
                    let r = if ch > 1 { frame[1] } else { l };
                    out.push(l);
                    out.push(r);
                }
            }
            Err(SymError::DecodeError(_)) => continue,
            Err(e) => return Err(format!("decode: {e}")),
        }
    }
    if sample_rate == 0 {
        return Err("could not determine sample rate".into());
    }
    out.shrink_to_fit();
    Ok((sample_rate, out))
}

impl std::fmt::Debug for Track {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Track({})", self.path.display())
    }
}
