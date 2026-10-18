//! The tap: a lock-free ring the audio thread writes the mix into, armed only while recording.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use super::*;

/// Single-producer (audio thread) / single-consumer (writer thread) ring of f32 samples.
pub struct Tap {
    pub(super) buf: Box<[AtomicU32]>,
    pub(super) write: AtomicUsize,
    pub(super) read: AtomicUsize,
    pub(super) armed: AtomicBool,
    /// Frames written into the ring while armed: the recording clock.
    pub(super) frames: AtomicU64,
    /// Samples lost because the writer fell more than [`RING`] behind.
    pub(super) dropped: AtomicU64,
    /// Sample rate of the audio being pushed.
    pub(super) rate: AtomicU32,
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
    pub(super) fn pop(&self, out: &mut Vec<f32>) {
        let r = self.read.load(Ordering::Relaxed);
        let w = self.write.load(Ordering::Acquire);
        out.extend((r..w).map(|i| f32::from_bits(self.buf[i & (RING - 1)].load(Ordering::Relaxed))));
        self.read.store(w, Ordering::Release);
    }

    /// Writer side: forget anything left over and start the clock.
    pub(super) fn arm(&self) {
        self.read.store(self.write.load(Ordering::Acquire), Ordering::Release);
        self.frames.store(0, Ordering::Relaxed);
        self.dropped.store(0, Ordering::Relaxed);
        self.armed.store(true, Ordering::Release);
    }

    pub(super) fn disarm(&self) {
        self.armed.store(false, Ordering::Release);
    }
}
