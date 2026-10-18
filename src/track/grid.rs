//! Beat grids: where the beats are, from a rekordbox export, a saved fix or our own detection.

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

    pub(crate) fn from_rekordbox(a: &crate::rekordbox::Analysis, sample_rate: u32, offset_ms: f64) -> Option<Grid> {
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
