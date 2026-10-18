//! Reader for rekordbox ANLZ analysis files (`ANLZ0000.DAT` / `.EXT` / `.2EX`): beat grid,
//! cue points and the scrolling waveforms.
//! Big-endian, a "PMAI" header followed by type-tagged sections.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Beat {
    /// Position in the bar, 1 = downbeat.
    pub number: u16,
    /// BPM × 100.
    pub tempo: u16,
    pub time_ms: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cue {
    /// 0 = memory cue, 1 = hot cue A, 2 = B, …
    pub hot: u32,
    pub time_ms: u32,
    /// Loop end, if this cue is a loop.
    pub loop_end_ms: Option<u32>,
    pub color: Option<[u8; 3]>,
    pub comment: String,
}

#[derive(Clone, Debug, Default)]
pub struct Analysis {
    pub beats: Vec<Beat>,
    pub hot_cues: Vec<Cue>,
    pub memory_cues: Vec<Cue>,
    /// Set when the cues came from the extended (nxs2+) list, which wins over the basic one.
    extended_cues: bool,
    /// Waveforms at 150 entries per second, as CDJs draw them. Any may be empty.
    pub waves: Waves,
}

/// The three detailed waveforms rekordbox exports, one per colour setting.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Waves {
    /// `PWV3` (Blue): height in bits 0–4, "whiteness" (high-frequency content) in bits 5–7.
    pub blue: Vec<u8>,
    /// `PWV5` (RGB): big-endian; red (lows) bits 13–15, green (mids) 10–12, blue (highs) 7–9,
    /// height 2–6.
    pub rgb: Vec<u16>,
    /// `PWV7` (3Band, in `.2EX`): low, mid, high levels.
    pub band: Vec<[u8; 3]>,
}

impl Waves {
    pub fn is_empty(&self) -> bool {
        self.blue.is_empty() && self.rgb.is_empty() && self.band.is_empty()
    }
}

fn be16(b: &[u8], o: usize) -> Option<u16> {
    b.get(o..o + 2).map(|s| u16::from_be_bytes([s[0], s[1]]))
}
fn be32(b: &[u8], o: usize) -> Option<u32> {
    b.get(o..o + 4).map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

impl Analysis {
    /// Merge one ANLZ file into this analysis. Unknown sections are skipped.
    pub fn read(&mut self, data: &[u8]) -> Result<(), String> {
        if data.get(0..4) != Some(b"PMAI") {
            return Err("not an ANLZ file".into());
        }
        let mut pos = be32(data, 4).ok_or("truncated")? as usize;
        while pos + 12 <= data.len() {
            let tag = &data[pos..pos + 4];
            let len_tag = be32(data, pos + 8).unwrap_or(0) as usize;
            if len_tag < 12 || pos + len_tag > data.len() {
                break;
            }
            let body = &data[pos + 12..pos + len_tag];
            // Waveform tags: entry size and count, then the entries after the header.
            let entries = |size: usize| -> &[u8] {
                let hlen = be32(data, pos + 4).unwrap_or(0) as usize;
                let n = be32(body, 4).unwrap_or(0) as usize;
                data.get(pos + hlen..(pos + hlen + n * size).min(pos + len_tag)).unwrap_or(&[])
            };
            match tag {
                b"PWV3" => self.waves.blue = entries(1).to_vec(),
                b"PWV5" => self.waves.rgb = entries(2).chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect(),
                b"PWV7" => self.waves.band = entries(3).chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect(),
                b"PQTZ" => self.read_grid(body),
                b"PCOB" if !self.extended_cues => self.read_cues(body),
                b"PCO2" => self.read_cues_ext(body),
                _ => {}
            }
            pos += len_tag;
        }
        Ok(())
    }

    fn read_grid(&mut self, b: &[u8]) {
        let Some(n) = be32(b, 8) else { return };
        let mut beats = Vec::with_capacity(n.min(100_000) as usize);
        for i in 0..n as usize {
            let o = 12 + i * 8;
            let (Some(number), Some(tempo), Some(time_ms)) = (be16(b, o), be16(b, o + 2), be32(b, o + 4)) else {
                break;
            };
            beats.push(Beat { number, tempo, time_ms });
        }
        if !beats.is_empty() {
            self.beats = beats;
        }
    }

    fn push(&mut self, cue: Cue) {
        if cue.hot > 0 {
            self.hot_cues.push(cue);
        } else {
            self.memory_cues.push(cue);
        }
    }

    fn read_cues(&mut self, b: &[u8]) {
        let Some(n) = be16(b, 6) else { return };
        let mut o = 12;
        for _ in 0..n {
            if b.get(o..o + 4) != Some(b"PCPT") {
                break;
            }
            let Some(len) = be32(b, o + 8).map(|v| v as usize) else { break };
            if len < 40 {
                break;
            }
            let e = &b[o..(o + len).min(b.len())];
            if let (Some(hot), Some(kind), Some(time), Some(lt)) = (be32(e, 12), e.get(28), be32(e, 32), be32(e, 36)) {
                if time != u32::MAX {
                    self.push(Cue {
                        hot,
                        time_ms: time,
                        loop_end_ms: (*kind == 2 && lt != u32::MAX && lt > time).then_some(lt),
                        color: None,
                        comment: String::new(),
                    });
                }
            }
            o += len;
        }
    }

    fn read_cues_ext(&mut self, b: &[u8]) {
        let Some(n) = be16(b, 4) else { return };
        if !self.extended_cues {
            // First extended list seen: discard anything from the basic lists.
            self.hot_cues.clear();
            self.memory_cues.clear();
            self.extended_cues = true;
        }
        let mut o = 8;
        for _ in 0..n {
            if b.get(o..o + 4) != Some(b"PCP2") {
                break;
            }
            let Some(len) = be32(b, o + 8).map(|v| v as usize) else { break };
            if len < 28 {
                break;
            }
            let e = &b[o..(o + len).min(b.len())];
            let hot = be32(e, 12).unwrap_or(0);
            let kind = e.get(16).copied().unwrap_or(1);
            let time = be32(e, 20).unwrap_or(u32::MAX);
            let lt = be32(e, 24).unwrap_or(u32::MAX);
            let mut comment = String::new();
            let mut color = None;
            if len > 43 {
                let lc = be32(e, 40).unwrap_or(0) as usize;
                if let Some(cb) = e.get(44..44 + lc) {
                    let units: Vec<u16> = cb.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
                    comment = String::from_utf16_lossy(&units).trim_end_matches('\0').to_string();
                }
                if let Some(rgb) = e.get(44 + lc + 1..44 + lc + 4) {
                    if rgb != [0, 0, 0] {
                        color = Some([rgb[0], rgb[1], rgb[2]]);
                    }
                }
            }
            if time != u32::MAX {
                self.push(Cue {
                    hot,
                    time_ms: time,
                    loop_end_ms: (kind == 2 && lt != u32::MAX && lt > time).then_some(lt),
                    color,
                    comment,
                });
            }
            o += len;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/rekordbox");

    /// The sweep fixture runs 20 kHz → 20 Hz: highs first, lows last, in every waveform.
    #[test]
    fn sweep_waveforms_have_the_documented_layout() {
        let mut a = Analysis::default();
        for ext in ["DAT", "EXT", "2EX"] {
            a.read(&std::fs::read(format!("{DIR}/anlz_sweep/ANLZ0000.{ext}")).unwrap()).unwrap();
        }
        let w = &a.waves;
        assert_eq!((w.blue.len(), w.rgb.len(), w.band.len()), (22500, 22500, 22500));
        let at = |sec: usize| sec * 150;
        // Seconds 6 (high frequencies) and 45 (low frequencies) of the stereo pass.
        let (hi, lo) = (at(6), at(45));
        let rgb = |v: u16| ((v >> 13) & 7, (v >> 10) & 7, (v >> 7) & 7, (v >> 2) & 31);
        assert_eq!(rgb(w.rgb[hi]).2, 7, "highs are blue");
        assert_eq!(rgb(w.rgb[lo]).0, 7, "lows are red");
        assert!(rgb(w.rgb[lo]).3 > 20);
        assert!(w.band[hi][2] > 50 && w.band[hi][0] == 0, "{:?}", w.band[hi]);
        assert!(w.band[lo][0] > 50 && w.band[lo][2] == 0, "{:?}", w.band[lo]);
        assert_eq!(w.blue[hi] >> 5, 7, "highs are white");
        assert_eq!(w.blue[lo] >> 5, 0);
    }

    #[test]
    fn demo_track_grid_and_cues() {
        let mut found_grid = false;
        for entry in walk(&format!("{DIR}/demo_tracks/PIONEER/USBANLZ")) {
            let mut a = Analysis::default();
            if entry.ends_with(".DAT") {
                a.read(&std::fs::read(&entry).unwrap()).unwrap();
                let ext = entry.replace(".DAT", ".EXT");
                a.read(&std::fs::read(&ext).unwrap()).unwrap();
                if a.beats.len() > 16 {
                    found_grid = true;
                    // Beats are increasing, numbered 1..=4, tempo sane.
                    assert!(a.beats.windows(2).all(|w| w[1].time_ms > w[0].time_ms));
                    assert!(a.beats.iter().all(|b| (1..=4).contains(&b.number)));
                    assert!(a.beats.iter().all(|b| b.tempo > 6000 && b.tempo < 20000));
                }
            }
        }
        assert!(found_grid);
    }

    fn pcp2(hot: u32, kind: u8, time: u32, loop_time: u32, comment: &str, rgb: [u8; 3]) -> Vec<u8> {
        let wide: Vec<u8> = comment.encode_utf16().chain([0]).flat_map(|u| u.to_be_bytes()).collect();
        let len = 44 + wide.len() + 4 + 4;
        let mut e = b"PCP2".to_vec();
        e.extend(0x10u32.to_be_bytes());
        e.extend((len as u32).to_be_bytes());
        e.extend(hot.to_be_bytes());
        e.extend([kind, 0, 0x03, 0xE8]);
        e.extend(time.to_be_bytes());
        e.extend(loop_time.to_be_bytes());
        e.extend([0u8; 8]); // colour id + padding
        e.extend([0u8; 4]); // loop numerator / denominator
        e.extend((wide.len() as u32).to_be_bytes());
        e.extend(&wide);
        e.extend([0x2A, rgb[0], rgb[1], rgb[2]]);
        e.extend([0u8; 4]);
        assert_eq!(e.len(), len);
        e
    }

    fn file_with(tags: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut f = b"PMAI".to_vec();
        f.extend(28u32.to_be_bytes());
        f.extend(0u32.to_be_bytes());
        f.resize(28, 0);
        for (tag, body) in tags {
            f.extend(*tag);
            f.extend(0x14u32.to_be_bytes());
            f.extend(((body.len() + 12) as u32).to_be_bytes());
            f.extend(body);
        }
        f
    }

    #[test]
    fn extended_cues_with_colours_and_loops() {
        let mut body = 1u32.to_be_bytes().to_vec(); // hot cue list
        body.extend(3u16.to_be_bytes());
        body.extend([0, 0]);
        body.extend(pcp2(1, 1, 1500, u32::MAX, "Drop", [0x30, 0x5A, 0xFF]));
        body.extend(pcp2(4, 2, 32_000, 39_500, "", [0xE6, 0x28, 0x28]));
        body.extend(pcp2(0, 1, 500, u32::MAX, "", [0, 0, 0]));
        let mut grid = vec![0u8; 8];
        grid.extend(2u32.to_be_bytes());
        for (n, ms) in [(3u16, 120u32), (4, 589)] {
            grid.extend(n.to_be_bytes());
            grid.extend(12800u16.to_be_bytes());
            grid.extend(ms.to_be_bytes());
        }
        let mut a = Analysis::default();
        a.read(&file_with(&[(b"PQTZ", grid), (b"PCO2", body)])).unwrap();
        assert_eq!(a.beats.len(), 2);
        assert_eq!(a.beats[0], Beat { number: 3, tempo: 12800, time_ms: 120 });
        assert_eq!(a.hot_cues.len(), 2);
        assert_eq!(a.hot_cues[0].comment, "Drop");
        assert_eq!(a.hot_cues[0].color, Some([0x30, 0x5A, 0xFF]));
        assert_eq!(a.hot_cues[1].hot, 4);
        assert_eq!(a.hot_cues[1].loop_end_ms, Some(39_500));
        assert_eq!(a.memory_cues.len(), 1);
        assert_eq!(a.memory_cues[0].time_ms, 500);

        // A later basic PCOB list must not override the extended one.
        let mut pcob = 1u32.to_be_bytes().to_vec();
        pcob.extend([0, 0, 0, 0, 0, 0, 0, 0]);
        a.read(&file_with(&[(b"PCOB", pcob)])).unwrap();
        assert_eq!(a.hot_cues.len(), 2);
    }

    #[test]
    fn rejects_garbage() {
        let mut a = Analysis::default();
        assert!(a.read(b"nope").is_err());
        let mut junk = b"PMAI\0\0\0\x1c\0\0\0\x40".to_vec();
        junk.resize(64, 0xAB);
        assert!(a.read(&junk).is_ok());
    }

    #[test]
    fn sweep_file_parses() {
        let mut a = Analysis::default();
        a.read(&std::fs::read(format!("{DIR}/anlz_sweep/ANLZ0000.DAT")).unwrap()).unwrap();
        a.read(&std::fs::read(format!("{DIR}/anlz_sweep/ANLZ0000.EXT")).unwrap()).unwrap();
    }

    fn walk(dir: &str) -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    out.extend(walk(&p.to_string_lossy()));
                } else {
                    out.push(p.to_string_lossy().into_owned());
                }
            }
        }
        out
    }
}
