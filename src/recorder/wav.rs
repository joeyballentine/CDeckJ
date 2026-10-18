//! The writer thread: WAV files split before 4 GB, header checkpoints so a crash leaves a
//! playable file, and a disk-space guard.

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use parking_lot::Mutex;

use super::*;

/// A 24-bit stereo WAV whose header is rewritten every second, so a crash or power cut
/// leaves a playable file with at most the last second missing.
pub(crate) struct WavFile {
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

pub(crate) fn header(rate: u32, data_bytes: u64) -> [u8; 44] {
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
pub(crate) fn free_space(path: &Path) -> Option<u64> {
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

pub(crate) fn writer(tap: Arc<Tap>, base: PathBuf, stop: Arc<AtomicBool>, status: Arc<Mutex<RecStatus>>) {
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
