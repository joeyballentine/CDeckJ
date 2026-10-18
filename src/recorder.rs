//! Recording the mix: a lock-free tap the audio thread writes into, a writer thread that
//! turns it into 24-bit WAV files on disk, and a tracklist built from what was audible.
//!
//! The recording takes the master mix before the MASTER knob (like a DJM's REC OUT), so
//! turning the room down doesn't make the recording quieter.

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::engine::mixer::{fader_gain_curve, xfade_gains_curve};
use crate::engine::{EngineView, DECKS};

/// Default folder name for recordings (inside the music folder); the library skips it.
pub const DIR_NAME: &str = "Platter Recordings";

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
// Tap (audio thread → writer)
// ---------------------------------------------------------------------------------------------

/// Single-producer (audio thread) / single-consumer (writer thread) ring of f32 samples.
pub struct Tap {
    buf: Box<[AtomicU32]>,
    write: AtomicUsize,
    read: AtomicUsize,
    armed: AtomicBool,
    /// Frames written into the ring while armed: the recording clock.
    frames: AtomicU64,
    /// Samples lost because the writer fell more than [`RING`] behind.
    dropped: AtomicU64,
    /// Sample rate of the audio being pushed.
    rate: AtomicU32,
}

impl Default for Tap {
    fn default() -> Self {
        Self {
            buf: (0..RING).map(|_| AtomicU32::new(0)).collect(),
            write: AtomicUsize::new(0),
            read: AtomicUsize::new(0),
            armed: AtomicBool::new(false),
            frames: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            rate: AtomicU32::new(48000),
        }
    }
}

impl Tap {
    pub fn armed(&self) -> bool {
        self.armed.load(Ordering::Relaxed)
    }

    pub fn set_rate(&self, rate: u32) {
        self.rate.store(rate, Ordering::Relaxed);
    }

    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    /// Audio thread: append interleaved stereo. Never blocks or allocates.
    pub fn push(&self, samples: &[f32]) {
        if !self.armed() {
            return;
        }
        let w = self.write.load(Ordering::Relaxed);
        let r = self.read.load(Ordering::Acquire);
        if samples.len() > RING - (w - r) {
            self.dropped.fetch_add(samples.len() as u64, Ordering::Relaxed);
            return;
        }
        for (i, s) in samples.iter().enumerate() {
            self.buf[(w + i) & (RING - 1)].store(s.to_bits(), Ordering::Relaxed);
        }
        self.write.store(w + samples.len(), Ordering::Release);
        self.frames.fetch_add(samples.len() as u64 / 2, Ordering::Relaxed);
    }

    /// Writer thread: take everything available.
    fn pop(&self, out: &mut Vec<f32>) {
        let r = self.read.load(Ordering::Relaxed);
        let w = self.write.load(Ordering::Acquire);
        out.extend((r..w).map(|i| f32::from_bits(self.buf[i & (RING - 1)].load(Ordering::Relaxed))));
        self.read.store(w, Ordering::Release);
    }

    /// Writer side: forget anything left over and start the clock.
    fn arm(&self) {
        self.read.store(self.write.load(Ordering::Acquire), Ordering::Release);
        self.frames.store(0, Ordering::Relaxed);
        self.dropped.store(0, Ordering::Relaxed);
        self.armed.store(true, Ordering::Release);
    }

    fn disarm(&self) {
        self.armed.store(false, Ordering::Release);
    }
}

// ---------------------------------------------------------------------------------------------
// WAV writer
// ---------------------------------------------------------------------------------------------

/// A 24-bit stereo WAV whose header is rewritten every second, so a crash or power cut
/// leaves a playable file with at most the last second missing.
struct WavFile {
    out: BufWriter<File>,
    data_bytes: u64,
}

impl WavFile {
    fn create(path: &Path, rate: u32) -> std::io::Result<WavFile> {
        let mut out = BufWriter::with_capacity(1 << 20, File::create(path)?);
        out.write_all(&header(rate, 0))?;
        Ok(WavFile { out, data_bytes: 0 })
    }

    fn write(&mut self, samples: &[f32], scratch: &mut Vec<u8>) -> std::io::Result<()> {
        scratch.clear();
        for &s in samples {
            let v = (s.clamp(-1.0, 1.0) * 8_388_607.0).round() as i32;
            scratch.extend_from_slice(&v.to_le_bytes()[..3]);
        }
        self.out.write_all(scratch)?;
        self.data_bytes += scratch.len() as u64;
        Ok(())
    }

    /// Flush and fix the sizes in the header, then go back to the end.
    fn checkpoint(&mut self, rate: u32, sync: bool) -> std::io::Result<()> {
        self.out.flush()?;
        let f = self.out.get_mut();
        f.seek(SeekFrom::Start(0))?;
        f.write_all(&header(rate, self.data_bytes))?;
        f.seek(SeekFrom::End(0))?;
        if sync {
            f.sync_data()?;
        }
        Ok(())
    }
}

fn header(rate: u32, data_bytes: u64) -> [u8; 44] {
    let data = data_bytes.min(u32::MAX as u64 - 36) as u32;
    let (ch, bits) = (2u16, 24u16);
    let block = ch * bits / 8;
    let mut h = [0u8; 44];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + data).to_le_bytes());
    h[8..12].copy_from_slice(b"WAVE");
    h[12..16].copy_from_slice(b"fmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
    h[22..24].copy_from_slice(&ch.to_le_bytes());
    h[24..28].copy_from_slice(&rate.to_le_bytes());
    h[28..32].copy_from_slice(&(rate * block as u32).to_le_bytes());
    h[32..34].copy_from_slice(&block.to_le_bytes());
    h[34..36].copy_from_slice(&bits.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data.to_le_bytes());
    h
}

/// Bytes free for an unprivileged user on the filesystem holding `path`.
fn free_space(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid NUL-terminated path and `s` is a properly sized out-parameter.
    (unsafe { libc::statvfs(c.as_ptr(), &mut s) } == 0).then(|| s.f_bavail as u64 * s.f_frsize as u64)
}

/// Local time as "2026-10-18 01-42" (no colons, so the name works on any filesystem).
pub fn local_stamp() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as libc::time_t).unwrap_or(0);
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers are valid for the duration of the call.
    unsafe { libc::localtime_r(&now, &mut tm) };
    format!("{:04}-{:02}-{:02} {:02}-{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min)
}

/// One output file. A recording has more than one if it runs past ~3¾ h or the audio
/// device's sample rate changes mid-set.
#[derive(Clone, Debug)]
pub struct Part {
    pub path: PathBuf,
    pub start_frame: u64,
    pub rate: u32,
}

#[derive(Clone, Debug, Default)]
pub struct RecStatus {
    pub parts: Vec<Part>,
    pub bytes: u64,
    pub low_space: bool,
    /// The writer stopped by itself (disk full, write error).
    pub error: Option<String>,
    pub finished: bool,
}

fn writer(tap: Arc<Tap>, base: PathBuf, stop: Arc<AtomicBool>, status: Arc<Mutex<RecStatus>>) {
    let fail = |status: &Mutex<RecStatus>, e: String| {
        let mut st = status.lock();
        st.error = Some(e);
        st.finished = true;
    };
    let dir = base.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut samples = Vec::with_capacity(RING);
    let mut scratch = Vec::with_capacity(RING * 3);
    let mut rate = tap.rate.load(Ordering::Relaxed);
    let part_path = |n: usize| -> PathBuf {
        let name = base.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        if n == 1 { base.with_file_name(format!("{name}.wav")) } else { base.with_file_name(format!("{name} part {n}.wav")) }
    };
    let mut written_frames = 0u64;
    let open = |n: usize, rate: u32, start: u64| -> Result<WavFile, String> {
        let p = part_path(n);
        let f = WavFile::create(&p, rate).map_err(|e| format!("couldn't create {}: {e}", p.display()))?;
        status.lock().parts.push(Part { path: p, start_frame: start, rate });
        Ok(f)
    };
    let mut n = 1;
    let mut wav = match open(n, rate, 0) {
        Ok(w) => w,
        Err(e) => return fail(&status, e),
    };
    tap.arm();
    let mut last_checkpoint = Instant::now();
    let mut last_space = Instant::now() - Duration::from_secs(60);
    loop {
        let stopping = stop.load(Ordering::Acquire);
        if stopping {
            tap.disarm();
            // Let a push that saw "armed" just before finish landing in the ring.
            std::thread::sleep(Duration::from_millis(30));
        }
        samples.clear();
        tap.pop(&mut samples);
        // A new device rate, or nearly 4 GiB: carry on in a new file.
        let now_rate = tap.rate.load(Ordering::Relaxed);
        if (now_rate != rate || wav.data_bytes > PART_BYTES) && !samples.is_empty() {
            if let Err(e) = wav.checkpoint(rate, true) {
                return fail(&status, format!("couldn't write the recording: {e}"));
            }
            rate = now_rate;
            n += 1;
            wav = match open(n, rate, written_frames) {
                Ok(w) => w,
                Err(e) => return fail(&status, e),
            };
        }
        if !samples.is_empty() {
            if let Err(e) = wav.write(&samples, &mut scratch) {
                tap.disarm();
                let _ = wav.checkpoint(rate, true);
                return fail(&status, format!("couldn't write the recording: {e}"));
            }
            written_frames += samples.len() as u64 / 2;
        }
        if stopping {
            let r = wav.checkpoint(rate, true);
            let mut st = status.lock();
            st.bytes = st.parts.len() as u64 * 44 + written_frames * 6;
            if let Err(e) = r {
                st.error = Some(format!("couldn't finish the recording: {e}"));
            }
            st.finished = true;
            return;
        }
        if last_checkpoint.elapsed() >= Duration::from_secs(1) {
            last_checkpoint = Instant::now();
            if let Err(e) = wav.checkpoint(rate, false) {
                tap.disarm();
                return fail(&status, format!("couldn't write the recording: {e}"));
            }
            status.lock().bytes = written_frames * 6;
        }
        if last_space.elapsed() >= Duration::from_secs(10) {
            last_space = Instant::now();
            if let Some(free) = free_space(&dir) {
                status.lock().low_space = free < LOW_SPACE;
                if free < STOP_SPACE {
                    tap.disarm();
                    let _ = wav.checkpoint(rate, true);
                    return fail(&status, "the disk is almost full, so recording stopped (what was recorded is saved)".into());
                }
            }
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}

// ---------------------------------------------------------------------------------------------
// Tracklist
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// Recording frame where the track became audible.
    pub frame: u64,
    pub deck: usize,
    pub artist: String,
    pub title: String,
}

/// Turns "which deck is audible" into a list of tracks with start times.
#[derive(Default)]
pub struct Tracklist {
    pub entries: Vec<Entry>,
    /// Per deck: the loaded track generation, when it became audible, and whether it's listed.
    deck: [(u64, Option<u64>, bool); DECKS],
}

/// How much of a deck reaches the master (fader × crossfader), 0..1.
pub fn deck_level(v: &EngineView, d: usize) -> f32 {
    let (a, b) = xfade_gains_curve(v.mixer.crossfader, v.mixer.crossfader_curve);
    fader_gain_curve(v.mixer.ch[d].fader, v.mixer.fader_curve) * if d == 0 { a } else { b }
}

impl Tracklist {
    /// Call regularly while recording. `meta` gives (artist, title) for each deck's track.
    /// Returns true when an entry was added.
    pub fn observe(&mut self, v: &EngineView, frame: u64, rate: u32, meta: [Option<(String, String)>; DECKS]) -> bool {
        let mut added = false;
        for d in 0..DECKS {
            let dv = &v.decks[d];
            let st = &mut self.deck[d];
            if st.0 != dv.track_gen {
                *st = (dv.track_gen, None, false);
            }
            let audible = dv.loaded && dv.playing && deck_level(v, d) > 0.15;
            if !audible {
                st.1 = None;
                continue;
            }
            let since = *st.1.get_or_insert(frame);
            if !st.2 && (frame - since) as f64 >= TRACK_AUDIBLE_SECS * rate.max(1) as f64 {
                if let Some((artist, title)) = meta[d].clone() {
                    st.2 = true;
                    self.entries.push(Entry { frame: since, deck: d, artist, title });
                    self.entries.sort_by_key(|e| e.frame);
                    added = true;
                }
            }
        }
        added
    }
}

/// Seconds from the start of the recording to `frame`, across parts with different rates.
fn secs_at(parts: &[Part], frame: u64) -> f64 {
    let mut secs = 0.0;
    for (i, p) in parts.iter().enumerate() {
        let end = parts.get(i + 1).map(|n| n.start_frame).unwrap_or(u64::MAX);
        if frame < end {
            return secs + (frame - p.start_frame) as f64 / p.rate as f64;
        }
        secs += (end - p.start_frame) as f64 / p.rate as f64;
    }
    secs
}

fn hms(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{:02}:{:02}", s / 60, s % 60) }
}

fn display_name(e: &Entry) -> String {
    if e.artist.is_empty() { e.title.clone() } else { format!("{} - {}", e.artist, e.title) }
}

/// "00:00 Artist - Title" lines, ready to paste into a mix description.
pub fn tracklist_text(entries: &[Entry], parts: &[Part]) -> String {
    entries.iter().map(|e| format!("{} {}\n", hms(secs_at(parts, e.frame)), display_name(e))).collect()
}

/// A CUE sheet so players and splitters see track markers in the WAV(s).
pub fn cue_sheet(entries: &[Entry], parts: &[Part], title: &str) -> String {
    let esc = |s: &str| s.replace('"', "'");
    let mut s = format!("PERFORMER \"Platter\"\nTITLE \"{}\"\n", esc(title));
    let mut current: Option<usize> = None;
    for (i, e) in entries.iter().enumerate() {
        let pi = parts.iter().rposition(|p| p.start_frame <= e.frame).unwrap_or(0);
        let Some(p) = parts.get(pi) else { break };
        if current != Some(pi) {
            let name = p.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            s += &format!("FILE \"{}\" WAVE\n", esc(&name));
            current = Some(pi);
        }
        let t = (e.frame - p.start_frame) as f64 / p.rate as f64;
        let cd = (t * 75.0).round() as u64;
        s += &format!(
            "  TRACK {:02} AUDIO\n    PERFORMER \"{}\"\n    TITLE \"{}\"\n    INDEX 01 {:02}:{:02}:{:02}\n",
            i + 1,
            esc(&e.artist),
            esc(&e.title),
            cd / 75 / 60,
            cd / 75 % 60,
            cd % 75
        );
    }
    s
}

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
        // "2026-10-18 01-42 Platter mix", made unique if a recording started this minute.
        let stamp = local_stamp();
        let mut base = dir.join(format!("{stamp} Platter mix"));
        let mut k = 2;
        while base.with_file_name(format!("{}.wav", base.file_name().unwrap().to_string_lossy())).exists() {
            base = dir.join(format!("{stamp} Platter mix {k}"));
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
