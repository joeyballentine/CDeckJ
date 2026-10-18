//! Waveform columns in the three CDJ colour modes (Blue, RGB, 3Band), from rekordbox's own
//! waveforms when the track came from an export, otherwise from Platter's analysis.

use std::path::{Path, PathBuf};

use egui::Color32;

use crate::rekordbox::anlz::Waves;
use crate::rekordbox::settings::WaveColor;
use crate::track::{Track, WaveBin, WAVE_BINS_PER_SEC};

/// Up to three layers drawn in order (each centred, later ones on top).
#[derive(Clone, Copy, Debug, Default)]
pub struct Column {
    pub layers: [(f32, Color32); 3],
    pub n: usize,
}

impl Column {
    fn one(h: f32, c: Color32) -> Column {
        Column { layers: [(h.clamp(0.0, 1.0), c), Default::default(), Default::default()], n: 1 }
    }

    /// Tallest layer.
    #[cfg(test)]
    pub fn height(&self) -> f32 {
        self.layers[..self.n].iter().fold(0.0, |m, l| m.max(l.0))
    }
}

pub const BAND_LOW: Color32 = Color32::from_rgb(32, 83, 217);
pub const BAND_MID: Color32 = Color32::from_rgb(242, 170, 60);
pub const BAND_HIGH: Color32 = Color32::from_rgb(255, 255, 255);

/// Blue mode: deep blue for bass-heavy sound, toward white for bright, transient sound.
fn blue(whiteness: f32) -> Color32 {
    let w = whiteness.clamp(0.0, 1.0);
    let l = |a: f32, b: f32| (a + (b - a) * w).round() as u8;
    Color32::from_rgb(l(0.0, 205.0), l(96.0, 228.0), l(204.0, 250.0))
}

fn rgb3(r: u16, g: u16, b: u16) -> Color32 {
    if r + g + b == 0 {
        return Color32::from_gray(90);
    }
    let s = |v: u16| (v as f32 * 255.0 / 7.0).round() as u8;
    Color32::from_rgb(s(r), s(g), s(b))
}

/// Which source a mode draws from: rekordbox's matching waveform if the export has it.
fn rb_for(w: &Waves, mode: WaveColor) -> Option<WaveColor> {
    let has = |m: WaveColor| match m {
        WaveColor::Blue => !w.blue.is_empty(),
        WaveColor::Rgb => !w.rgb.is_empty(),
        WaveColor::ThreeBand => !w.band.is_empty(),
    };
    // The chosen mode, else another one rekordbox exported (it's still the real analysis).
    [mode, WaveColor::Rgb, WaveColor::Blue, WaveColor::ThreeBand].into_iter().find(|&m| has(m))
}

/// Bin index for a time in seconds (rekordbox waveforms shift with the grid offset).
pub fn bin_at(track: &Track, secs: f64) -> f64 {
    let off = if track.rb_waves.is_some() { track.rb_offset_s } else { 0.0 };
    (secs - off) * WAVE_BINS_PER_SEC
}

/// The column summarising bins `a..b` (the loudest moment in that span).
pub fn column(track: &Track, own: &[WaveBin], mode: WaveColor, a: usize, b: usize) -> Option<Column> {
    let b = b.max(a + 1);
    if let Some(w) = &track.rb_waves {
        if let Some(m) = rb_for(w, mode) {
            return rb_column(w, m, track.band_scale, a, b);
        }
    }
    let span = own.get(a..b.min(own.len()))?;
    let bin = span.iter().max_by_key(|x| x.peak)?;
    Some(own_column(bin, mode))
}

fn rb_column(w: &Waves, m: WaveColor, band_scale: f32, a: usize, b: usize) -> Option<Column> {
    match m {
        WaveColor::Blue => {
            let e = *w.blue.get(a..b.min(w.blue.len()))?.iter().max_by_key(|e| *e & 31)?;
            Some(Column::one((e & 31) as f32 / 31.0, blue((e >> 5) as f32 / 7.0)))
        }
        WaveColor::Rgb => {
            let e = *w.rgb.get(a..b.min(w.rgb.len()))?.iter().max_by_key(|e| (*e >> 2) & 31)?;
            let h = ((e >> 2) & 31) as f32 / 31.0;
            Some(Column::one(h, rgb3((e >> 13) & 7, (e >> 10) & 7, (e >> 7) & 7)))
        }
        WaveColor::ThreeBand => {
            let span = w.band.get(a..b.min(w.band.len()))?;
            if span.is_empty() {
                return None;
            }
            let mut mx = [0u8; 3];
            for e in span {
                for k in 0..3 {
                    mx[k] = mx[k].max(e[k]);
                }
            }
            let h = |v: u8| v as f32 * band_scale;
            Some(Column { layers: [(h(mx[0]), BAND_LOW), (h(mx[1]), BAND_MID), (h(mx[2]), BAND_HIGH)], n: 3 })
        }
    }
}

fn own_column(bin: &WaveBin, mode: WaveColor) -> Column {
    let pk = bin.peak as f32 / 255.0;
    let (lo, mid, hi) = (bin.low as f32 / 255.0, bin.mid as f32 / 255.0, bin.high as f32 / 255.0);
    match mode {
        WaveColor::Blue => {
            let share = hi / (lo + mid + hi + 1e-3);
            Column::one(pk, blue(share * 1.8))
        }
        WaveColor::Rgb => {
            let m = lo.max(mid).max(hi).max(1e-3);
            let s = |v: f32| ((v / m).powf(0.8) * 255.0).round() as u8;
            Column::one(pk, Color32::from_rgb(s(lo), s(mid), s(hi)))
        }
        WaveColor::ThreeBand => Column {
            layers: [(pk * (0.35 + 0.65 * lo), BAND_LOW), (pk * 0.75 * mid, BAND_MID), (pk * 0.5 * hi, BAND_HIGH)],
            n: 3,
        },
    }
}

/// A whole-track summary in `n` columns (browser previews), from rekordbox's waveforms if
/// given, else Platter's own.
pub fn summarize(rb: Option<&Waves>, own: &[WaveBin], mode: WaveColor, n: usize) -> Vec<Column> {
    let n = n.max(1);
    if let Some(w) = rb {
        if let Some(m) = rb_for(w, mode) {
            let total = match m {
                WaveColor::Blue => w.blue.len(),
                WaveColor::Rgb => w.rgb.len(),
                WaveColor::ThreeBand => w.band.len(),
            };
            let scale = band_scale(w);
            return (0..n).filter_map(|i| rb_column(w, m, scale, i * total / n, ((i + 1) * total / n).max(i * total / n + 1))).collect();
        }
    }
    let total = own.len();
    (0..n)
        .filter_map(|i| {
            let (a, b) = (i * total / n, ((i + 1) * total / n).max(i * total / n + 1).min(total));
            own.get(a..b)?.iter().max_by_key(|x| x.peak).map(|bin| own_column(bin, mode))
        })
        .collect()
}

/// Scale for 3-band heights so the loudest moment of the track fills the lane.
pub fn band_scale(w: &Waves) -> f32 {
    let mx = w.band.iter().flat_map(|e| e.iter().copied()).max().unwrap_or(0);
    if mx == 0 { 1.0 } else { 0.97 / mx as f32 }
}

pub fn mode_name(m: WaveColor) -> &'static str {
    match m {
        WaveColor::Blue => "BLUE",
        WaveColor::Rgb => "RGB",
        WaveColor::ThreeBand => "3BAND",
    }
}

pub fn next_mode(m: WaveColor) -> WaveColor {
    match m {
        WaveColor::Blue => WaveColor::Rgb,
        WaveColor::Rgb => WaveColor::ThreeBand,
        WaveColor::ThreeBand => WaveColor::Blue,
    }
}

// ---------------------------------------------------------------------------------------------
// Cache of Platter's own waveforms, so plain files show their whole waveform the moment they
// load (filled by the background analysis and by the first full load).
// ---------------------------------------------------------------------------------------------

const MAGIC: &[u8; 4] = b"PWB1";

/// Stable 64-bit FNV-1a (std's hasher may change between Rust releases).
fn fnv(data: &[u8], mut h: u64) -> u64 {
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// Cache file for an audio file, keyed by its path, size and modification time.
pub fn cache_path(path: &Path) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let m = std::fs::metadata(path).ok()?;
    let mtime = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    let mut h = fnv(path.as_os_str().as_bytes(), 0xcbf2_9ce4_8422_2325);
    h = fnv(&m.len().to_le_bytes(), h);
    h = fnv(&mtime.to_le_bytes(), h);
    Some(crate::config::data_dir().join("waves").join(format!("{h:016x}.bin")))
}

/// Two bytes per bin: peak in 7 bits, then low/mid/high in 3 bits each (like rekordbox's
/// own colour waveform), so an hour of audio is about 1 MB.
fn pack(b: &WaveBin) -> u16 {
    let q3 = |v: u8| (v as u16 * 7 + 127) / 255;
    ((b.peak as u16 >> 1) << 9) | (q3(b.low) << 6) | (q3(b.mid) << 3) | q3(b.high)
}

fn unpack(v: u16) -> WaveBin {
    let e3 = |x: u16| ((x & 7) * 255 / 7) as u8;
    WaveBin { peak: (((v >> 9) & 0x7f) << 1) as u8 | ((v >> 9) & 1) as u8, low: e3(v >> 6), mid: e3(v >> 3), high: e3(v) }
}

pub fn load_cached(path: &Path) -> Option<Vec<WaveBin>> {
    let d = std::fs::read(cache_path(path)?).ok()?;
    if d.get(0..4)? != MAGIC {
        return None;
    }
    let n = u32::from_le_bytes(d.get(4..8)?.try_into().ok()?) as usize;
    let body = d.get(8..8 + n * 2)?;
    Some(body.chunks_exact(2).map(|c| unpack(u16::from_le_bytes([c[0], c[1]]))).collect())
}

pub fn save_cached(path: &Path, bins: &[WaveBin]) {
    let Some(p) = cache_path(path) else { return };
    if bins.is_empty() {
        return;
    }
    let mut d = Vec::with_capacity(8 + bins.len() * 2);
    d.extend_from_slice(MAGIC);
    d.extend_from_slice(&(bins.len() as u32).to_le_bytes());
    for b in bins {
        d.extend_from_slice(&pack(b).to_le_bytes());
    }
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = p.with_extension("tmp");
    if std::fs::write(&tmp, &d).is_ok() {
        let _ = std::fs::rename(tmp, p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packing_keeps_shape_and_colour() {
        for b in [WaveBin { peak: 255, low: 255, mid: 0, high: 37 }, WaveBin { peak: 10, low: 0, mid: 200, high: 255 }] {
            let u = unpack(pack(&b));
            assert!((u.peak as i32 - b.peak as i32).abs() <= 1, "{b:?} {u:?}");
            for (x, y) in [(u.low, b.low), (u.mid, b.mid), (u.high, b.high)] {
                assert!((x as i32 - y as i32).abs() <= 19, "{b:?} {u:?}");
            }
        }
    }

    #[test]
    fn colour_modes_from_rekordbox_and_own_data() {
        let mut meta = crate::track::TrackMeta::default();
        let mut a = crate::rekordbox::Analysis::default();
        a.waves.rgb = vec![(7 << 13) | (31 << 2), (7 << 7) | (16 << 2)];
        meta.analysis = Some(a);
        let t = Track::from_parts(PathBuf::from("x.wav"), 48000, vec![0.0; 96000], meta);
        let c = column(&t, &[], WaveColor::Rgb, 0, 1).unwrap();
        assert_eq!(c.layers[0].1, Color32::from_rgb(255, 0, 0));
        assert!((c.height() - 1.0).abs() < 1e-6);
        // Blue requested but only RGB exported: still rekordbox's analysis.
        assert!(column(&t, &[], WaveColor::Blue, 1, 2).is_some());
        // Own data in 3-band mode gives three layers.
        let own = [WaveBin { peak: 200, low: 255, mid: 100, high: 50 }];
        let t2 = Track::from_parts(PathBuf::from("y.wav"), 48000, vec![0.0; 96000], Default::default());
        assert_eq!(column(&t2, &own, WaveColor::ThreeBand, 0, 1).unwrap().n, 3);
    }
}
