//! Decoded audio held in fixed-size chunks, so a track can play while the rest decodes and the
//! audio thread never waits on a reallocation.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::OnceLock;

pub(crate) const CHUNK_SHIFT: usize = 16;
pub(crate) const CHUNK_FRAMES: usize = 1 << CHUNK_SHIFT;
pub(crate) const CHUNK_MASK: usize = CHUNK_FRAMES - 1;
/// 8192 × 65536 frames ≈ 3 hours at 48 kHz.
pub(crate) const MAX_CHUNKS: usize = 8192;

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
