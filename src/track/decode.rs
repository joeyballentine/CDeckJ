//! Opening audio files: tags, progressive decoding (playable after about a second) and the
//! background analysis of music-folder tracks.

use std::fs::File;
use std::path::Path;
use std::sync::Arc;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, StandardTag};
use symphonia::core::units::Duration;
use super::analysis::WaveBuilder;

use super::*;

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

pub(crate) fn open(path: &Path) -> Result<Box<dyn FormatReader>, String> {
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

pub(crate) fn tags_of(format: &mut Box<dyn FormatReader>) -> Tags {
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


/// Seconds that must be decoded before a track is handed to a deck.
pub(crate) const READY_SECS: f64 = 1.0;

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
