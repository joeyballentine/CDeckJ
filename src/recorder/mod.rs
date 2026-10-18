//! Recording the mix: a lock-free tap the audio thread writes into, a writer thread that
//! turns it into 24-bit WAV files on disk, and a tracklist built from what was audible.
//!
//! The recording takes the master mix before the MASTER knob (like a DJM's REC OUT), so
//! turning the room down doesn't make the recording quieter.

mod tap;
mod wav;
mod tracklist;

pub use tap::*;
pub use wav::*;
pub use tracklist::*;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::engine::{EngineView, DECKS};

/// Default folder name for recordings (inside the music folder); the library skips it.
pub const DIR_NAME: &str = "CDeckJ Recordings";

/// Ring capacity in samples (interleaved stereo): about 11 s at 48 kHz, so a slow disk
/// can stall for a while without losing audio.
const RING: usize = 1 << 20;
/// Start a new file before a WAV's 4 GiB limit (about 3 h 45 min at 48 kHz).
const PART_BYTES: u64 = 3_900_000_000;
/// Warn below this much free space, and stop cleanly below the second figure.
const LOW_SPACE: u64 = 1 << 30;
const STOP_SPACE: u64 = 100 << 20;
/// A track goes in the tracklist once it has been audible this long.
const TRACK_AUDIBLE_SECS: f64 = 5.0;

// ---------------------------------------------------------------------------------------------
// Recorder (owned by the UI)
// ---------------------------------------------------------------------------------------------

pub struct Active {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    pub base: PathBuf,
    pub status: Arc<Mutex<RecStatus>>,
    pub tracklist: Tracklist,
}

pub struct Recorder {
    tap: Arc<Tap>,
    pub active: Option<Active>,
}

/// What happened when a recording ended.
pub struct Finished {
    pub parts: Vec<Part>,
    pub secs: f64,
    pub tracks: usize,
    pub error: Option<String>,
    pub dropped_secs: f64,
}

impl Recorder {
    pub fn new(tap: Arc<Tap>) -> Recorder {
        Recorder { tap, active: None }
    }

    pub fn is_recording(&self) -> bool {
        self.active.is_some()
    }

    /// Seconds recorded so far.
    pub fn elapsed(&self) -> f64 {
        match &self.active {
            Some(a) => secs_at(&a.status.lock().parts, self.tap.frames()),
            None => 0.0,
        }
    }

    pub fn start(&mut self, dir: &Path) -> Result<PathBuf, String> {
        if self.active.is_some() {
            return Err("already recording".into());
        }
        std::fs::create_dir_all(dir).map_err(|e| format!("couldn't create {}: {e}", dir.display()))?;
        if free_space(dir).is_some_and(|f| f < STOP_SPACE) {
            return Err(format!("not enough free space in {}", dir.display()));
        }
        // "2026-10-18 01-42 CDeckJ mix", made unique if a recording started this minute.
        let stamp = local_stamp();
        let mut base = dir.join(format!("{stamp} CDeckJ mix"));
        let mut k = 2;
        while base.with_file_name(format!("{}.wav", base.file_name().unwrap().to_string_lossy())).exists() {
            base = dir.join(format!("{stamp} CDeckJ mix {k}"));
            k += 1;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let status = Arc::new(Mutex::new(RecStatus::default()));
        let (t, b, s, st) = (self.tap.clone(), base.clone(), stop.clone(), status.clone());
        let thread = std::thread::Builder::new().name("recorder".into()).spawn(move || writer(t, b, s, st)).map_err(|e| e.to_string())?;
        // Wait briefly for the file to exist so an immediate failure is reported now.
        let t0 = Instant::now();
        while status.lock().parts.is_empty() && status.lock().error.is_none() && t0.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(5));
        }
        if let Some(e) = status.lock().error.clone() {
            let _ = thread.join();
            return Err(e);
        }
        self.active = Some(Active { stop, thread: Some(thread), base: base.clone(), status, tracklist: Tracklist::default() });
        Ok(base)
    }

    /// Feed the tracklist; writes the .txt/.cue when it changes. Returns a writer error
    /// (and ends the recording) if the writer stopped by itself.
    pub fn tick(&mut self, v: &EngineView, meta: [Option<(String, String)>; DECKS]) -> Option<Finished> {
        let a = self.active.as_mut()?;
        let frame = self.tap.frames();
        let rate = self.tap.rate.load(Ordering::Relaxed);
        if a.tracklist.observe(v, frame, rate, meta) {
            let parts = a.status.lock().parts.clone();
            write_tracklist(&a.base, &a.tracklist.entries, &parts);
        }
        if a.status.lock().finished { self.stop() } else { None }
    }

    /// Stop and wait for the file to be finalised (a few ms: the writer only has to drain).
    pub fn stop(&mut self) -> Option<Finished> {
        let mut a = self.active.take()?;
        a.stop.store(true, Ordering::Release);
        if let Some(t) = a.thread.take() {
            let _ = t.join();
        }
        self.tap.disarm();
        let st = a.status.lock().clone();
        write_tracklist(&a.base, &a.tracklist.entries, &st.parts);
        let frames = self.tap.frames();
        Some(Finished {
            secs: secs_at(&st.parts, frames),
            parts: st.parts,
            tracks: a.tracklist.entries.len(),
            error: st.error,
            dropped_secs: self.tap.dropped.load(Ordering::Relaxed) as f64 / 2.0 / self.tap.rate.load(Ordering::Relaxed).max(1) as f64,
        })
    }

    pub fn low_space(&self) -> bool {
        self.active.as_ref().is_some_and(|a| a.status.lock().low_space)
    }
}

fn write_tracklist(base: &Path, entries: &[Entry], parts: &[Part]) {
    if entries.is_empty() {
        return;
    }
    let name = base.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let _ = std::fs::write(base.with_file_name(format!("{name} tracklist.txt")), tracklist_text(entries, parts));
    let _ = std::fs::write(base.with_file_name(format!("{name}.cue")), cue_sheet(entries, parts, &name));
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn fmt_elapsed(secs: f64) -> String {
    hms(secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::CueRouting;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("platter-rec-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn ring_passes_samples_in_order_and_counts_overflow() {
        let tap = Tap::default();
        tap.push(&[1.0, 2.0]); // not armed: ignored
        tap.arm();
        tap.push(&[0.25, -0.5, 0.75, -1.0]);
        let mut out = Vec::new();
        tap.pop(&mut out);
        assert_eq!(out, vec![0.25, -0.5, 0.75, -1.0]);
        assert_eq!(tap.frames(), 2);
        let big = vec![0.0f32; RING];
        tap.push(&big);
        tap.push(&[1.0, 1.0]);
        assert_eq!(tap.dropped.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn records_a_valid_wav_and_survives_without_a_final_stop() {
        let dir = tmpdir("wav");
        let tap = Arc::new(Tap::default());
        tap.set_rate(44100);
        let mut rec = Recorder::new(tap.clone());
        let base = rec.start(&dir).unwrap();
        // One second of a 441 Hz sine, pushed like the audio thread would.
        let block: Vec<f32> = (0..44100).flat_map(|i| {
            let s = (i as f32 * 441.0 * std::f32::consts::TAU / 44100.0).sin() * 0.5;
            [s, -s]
        }).collect();
        for c in block.chunks(512) {
            tap.push(c);
        }
        // Mid-recording the header must already describe the data written so far.
        std::thread::sleep(Duration::from_millis(1300));
        let path = base.with_file_name(format!("{}.wav", base.file_name().unwrap().to_string_lossy()));
        let mid = std::fs::read(&path).unwrap();
        let data_len = u32::from_le_bytes(mid[40..44].try_into().unwrap()) as usize;
        assert_eq!(data_len, mid.len() - 44, "header kept in step with the data");
        let done = rec.stop().unwrap();
        assert!(done.error.is_none());
        assert!((done.secs - 1.0).abs() < 1e-6, "{}", done.secs);
        let wav = std::fs::read(&path).unwrap();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 44100);
        assert_eq!(wav.len(), 44 + 44100 * 6);
        // Sample 100, left channel, decoded back from 24-bit.
        let at = 44 + 100 * 6;
        let v = i32::from_le_bytes([0, wav[at], wav[at + 1], wav[at + 2]]) >> 8;
        let expect = block[200];
        assert!((v as f32 / 8_388_607.0 - expect).abs() < 1e-5);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rate_change_starts_a_new_part() {
        let dir = tmpdir("parts");
        let tap = Arc::new(Tap::default());
        tap.set_rate(48000);
        let mut rec = Recorder::new(tap.clone());
        rec.start(&dir).unwrap();
        tap.push(&vec![0.1; 48000 * 2]);
        std::thread::sleep(Duration::from_millis(150));
        tap.set_rate(44100);
        tap.push(&vec![0.1; 44100 * 2]);
        std::thread::sleep(Duration::from_millis(150));
        let done = rec.stop().unwrap();
        assert_eq!(done.parts.len(), 2);
        assert_eq!(done.parts[1].start_frame, 48000);
        assert!((done.secs - 2.0).abs() < 1e-6);
        assert!(done.parts[1].path.to_string_lossy().ends_with("part 2.wav"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tracklist_lists_tracks_once_they_are_audible() {
        let mut v = EngineView::new(48000, 2, CueRouting::MasterOnly);
        let mut tl = Tracklist::default();
        let meta = || [Some(("A".to_string(), "One".to_string())), Some(("B".to_string(), "Two".to_string()))];
        v.decks[0].loaded = true;
        v.decks[0].playing = true;
        v.decks[0].track_gen = 1;
        v.decks[1].loaded = true;
        v.decks[1].track_gen = 2;
        v.mixer.ch[1].fader = 0.0;
        for s in 0..10 {
            tl.observe(&v, s * 48000, 48000, meta());
        }
        // Deck 2 starts playing at 60 s but stays faded out until 90 s.
        v.decks[1].playing = true;
        for s in 60..90 {
            tl.observe(&v, s * 48000, 48000, meta());
        }
        v.mixer.ch[1].fader = 1.0;
        for s in 90..100 {
            tl.observe(&v, s * 48000, 48000, meta());
        }
        assert_eq!(tl.entries.len(), 2);
        assert_eq!(tl.entries[0].frame, 0);
        assert_eq!(tl.entries[1].frame, 90 * 48000);
        let parts = [Part { path: PathBuf::from("/x/mix.wav"), start_frame: 0, rate: 48000 }];
        assert_eq!(tracklist_text(&tl.entries, &parts), "00:00 A - One\n01:30 B - Two\n");
        let cue = cue_sheet(&tl.entries, &parts, "mix");
        assert!(cue.contains("FILE \"mix.wav\" WAVE"));
        assert!(cue.contains("INDEX 01 01:30:00"));
        // Reloading the same deck makes a new entry possible.
        v.decks[0].track_gen = 3;
        for s in 100..110 {
            tl.observe(&v, s * 48000, 48000, meta());
        }
        assert_eq!(tl.entries.len(), 3);
    }

    #[test]
    fn times_span_parts_with_different_rates() {
        let parts = [
            Part { path: PathBuf::from("a.wav"), start_frame: 0, rate: 48000 },
            Part { path: PathBuf::from("b.wav"), start_frame: 48000 * 60, rate: 44100 },
        ];
        assert!((secs_at(&parts, 48000 * 60 + 44100 * 30) - 90.0).abs() < 1e-9);
        assert_eq!(hms(3725.0), "1:02:05");
    }
}
