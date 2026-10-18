//! Track decoding (symphonia) and the analysed, in-memory track the decks play from.

use std::fs::File;
use std::path::{Path, PathBuf};

use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

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

/// A fully decoded track, shared read-only between the UI and the audio thread.
pub struct Track {
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub sample_rate: u32,
    /// Interleaved stereo f32.
    pub samples: Vec<f32>,
    pub frames: usize,
    /// Detected tempo, if the track has a usable beat.
    pub bpm: Option<f64>,
    /// Position of the first beat-grid line, in frames (may be negative).
    pub first_beat: f64,
    pub wave: Vec<WaveBin>,
}

impl Track {
    #[allow(dead_code)]
    pub fn duration_secs(&self) -> f64 {
        self.frames as f64 / self.sample_rate as f64
    }

    pub fn beat_len_frames(&self) -> Option<f64> {
        self.bpm.map(|b| self.sample_rate as f64 * 60.0 / b)
    }

    /// Fractional beat index at a frame position.
    pub fn beat_at(&self, pos: f64) -> Option<f64> {
        self.beat_len_frames().map(|bl| (pos - self.first_beat) / bl)
    }

    /// Build a track from raw interleaved stereo samples (used by decoding and by tests).
    pub fn from_samples(path: PathBuf, sample_rate: u32, samples: Vec<f32>) -> Track {
        let frames = samples.len() / 2;
        let (title, artist) = names_from_path(&path);
        let wave = analysis::waveform(&samples, sample_rate);
        let grid = analysis::detect_beat_grid(&samples, sample_rate);
        Track {
            path,
            title,
            artist,
            sample_rate,
            samples,
            frames,
            bpm: grid.map(|g| g.bpm),
            first_beat: grid.map(|g| g.first_beat).unwrap_or(0.0),
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
pub fn load(path: &Path) -> Result<Track, String> {
    let (sample_rate, samples) = decode(path)?;
    if samples.len() < 2 {
        return Err("file contains no audio".into());
    }
    Ok(Track::from_samples(path.to_path_buf(), sample_rate, samples))
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
        let packet = match format.next_packet() {
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
