//! Reader for rekordbox's DeviceSQL `export.pdb` (the "Device Library" written to USB sticks).
//!
//! Layout follows Deep Symmetry's crate-digger analysis: fixed-size pages, each table a linked
//! list of pages, rows located through row groups that grow backwards from the end of a page.
//! The reader never panics on malformed data: bad pages and rows are skipped.

use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)]
pub enum Table {
    Tracks = 0,
    Genres = 1,
    Artists = 2,
    Albums = 3,
    Labels = 4,
    Keys = 5,
    Colors = 6,
    PlaylistTree = 7,
    PlaylistEntries = 8,
    HistoryPlaylists = 11,
    HistoryEntries = 12,
    Artwork = 13,
}

const PAGE_HEADER: usize = 0x28;
const ROW_GROUP_SIZE: usize = 0x24;

#[derive(Clone, Debug, Default)]
#[allow(dead_code)]
pub struct TrackRow {
    pub id: u32,
    pub title: String,
    pub artist_id: u32,
    pub album_id: u32,
    pub genre_id: u32,
    pub key_id: u32,
    pub color_id: u8,
    pub rating: u8,
    /// BPM × 100 at the start of the track.
    pub tempo: u32,
    pub duration: u16,
    pub sample_rate: u32,
    pub bitrate: u32,
    pub file_path: String,
    pub analyze_path: String,
    pub comment: String,
    pub date_added: String,
}

#[derive(Clone, Debug)]
pub struct PlaylistNode {
    pub id: u32,
    pub parent_id: u32,
    pub sort_order: u32,
    pub is_folder: bool,
    pub name: String,
}

#[derive(Clone, Debug, Default)]
pub struct Database {
    pub tracks: Vec<TrackRow>,
    pub artists: HashMap<u32, String>,
    pub albums: HashMap<u32, String>,
    pub genres: HashMap<u32, String>,
    pub keys: HashMap<u32, String>,
    pub colors: HashMap<u32, String>,
    pub playlists: Vec<PlaylistNode>,
    /// playlist id → (entry index, track id), sorted by entry index.
    pub playlist_entries: HashMap<u32, Vec<(u32, u32)>>,
    pub history: Vec<(u32, String)>,
    pub history_entries: HashMap<u32, Vec<(u32, u32)>>,
}

struct Reader<'a> {
    data: &'a [u8],
    page_len: usize,
}

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    b.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]]))
}
fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    b.get(o..o + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// Decode a DeviceSQL string starting at `o` within `page`.
pub fn read_string(page: &[u8], o: usize) -> Option<String> {
    let kind = *page.get(o)?;
    match kind {
        0x40 => {
            let len = u16_at(page, o + 1)? as usize;
            let body = page.get(o + 4..o + len.max(4))?;
            Some(String::from_utf8_lossy(body).trim_end_matches('\0').to_string())
        }
        0x90 => {
            let len = u16_at(page, o + 1)? as usize;
            let body = page.get(o + 4..o + len.max(4))?;
            let units: Vec<u16> = body.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            Some(String::from_utf16_lossy(&units).trim_end_matches('\0').to_string())
        }
        k if k & 1 == 1 => {
            let len = (k >> 1) as usize;
            let body = page.get(o + 1..o + len.max(1))?;
            Some(String::from_utf8_lossy(body).to_string())
        }
        _ => None,
    }
}

impl<'a> Reader<'a> {
    fn page(&self, index: u32) -> Option<&'a [u8]> {
        let start = (index as usize).checked_mul(self.page_len)?;
        self.data.get(start..start + self.page_len)
    }

    /// Visit every present row of a table: `f(page, row_offset_in_page)`.
    fn for_each_row(&self, first: u32, last: u32, table: u32, mut f: impl FnMut(&'a [u8], usize)) {
        let mut seen = HashSet::new();
        let mut idx = first;
        loop {
            if !seen.insert(idx) {
                break; // cycle
            }
            let Some(page) = self.page(idx) else { break };
            if u32_at(page, 8) != Some(table) {
                break;
            }
            let packed = u32::from_le_bytes([page[0x18], page[0x19], page[0x1A], 0]);
            let num_offsets = (packed & 0x1FFF) as usize;
            let flags = page[0x1B];
            let is_data = flags & 0x40 == 0;
            if is_data && num_offsets > 0 {
                let groups = num_offsets.div_ceil(16);
                for g in 0..groups {
                    let base = self.page_len - g * ROW_GROUP_SIZE;
                    let Some(present) = u16_at(page, base - 4) else { break };
                    let in_group = (num_offsets - g * 16).min(16);
                    for r in 0..in_group {
                        if present & (1 << r) == 0 {
                            continue;
                        }
                        let Some(ofs) = u16_at(page, base - 6 - 2 * r) else { continue };
                        let row = PAGE_HEADER + ofs as usize;
                        if row < self.page_len {
                            f(page, row);
                        }
                    }
                }
            }
            if idx == last {
                break;
            }
            match u32_at(page, 0x0C) {
                Some(n) => idx = n,
                None => break,
            }
        }
    }
}

/// Name rows that use the near/far offset scheme (artists, albums).
fn named_row(page: &[u8], row: usize, id_at: usize, near_at: usize, far_at: usize) -> Option<(u32, String)> {
    let subtype = u16_at(page, row)?;
    let id = u32_at(page, row + id_at)?;
    let ofs = if subtype & 0x04 == 0x04 {
        u16_at(page, row + far_at)? as usize
    } else {
        *page.get(row + near_at)? as usize
    };
    Some((id, read_string(page, row + ofs)?))
}

fn track_row(page: &[u8], row: usize) -> Option<TrackRow> {
    let s = |i: usize| -> String {
        u16_at(page, row + 0x5E + 2 * i)
            .and_then(|o| read_string(page, row + o as usize))
            .unwrap_or_default()
    };
    Some(TrackRow {
        sample_rate: u32_at(page, row + 0x08)?,
        key_id: u32_at(page, row + 0x20)?,
        bitrate: u32_at(page, row + 0x30)?,
        tempo: u32_at(page, row + 0x38)?,
        genre_id: u32_at(page, row + 0x3C)?,
        album_id: u32_at(page, row + 0x40)?,
        artist_id: u32_at(page, row + 0x44)?,
        id: u32_at(page, row + 0x48)?,
        duration: u16_at(page, row + 0x54)?,
        color_id: *page.get(row + 0x58)?,
        rating: *page.get(row + 0x59)?,
        date_added: s(10),
        analyze_path: s(14),
        comment: s(16),
        title: s(17),
        file_path: s(20),
    })
}

impl Database {
    pub fn parse(data: &[u8]) -> Result<Database, String> {
        let page_len = u32_at(data, 4).ok_or("file too short")? as usize;
        if !(512..=65536).contains(&page_len) || !page_len.is_power_of_two() {
            return Err(format!("not a rekordbox database (page size {page_len})"));
        }
        let num_tables = u32_at(data, 8).ok_or("file too short")? as usize;
        if num_tables > 64 {
            return Err("not a rekordbox database (table count)".into());
        }
        let rd = Reader { data, page_len };
        let mut db = Database::default();
        for t in 0..num_tables {
            let o = 0x1C + t * 16;
            let (Some(kind), Some(first), Some(last)) = (u32_at(data, o), u32_at(data, o + 8), u32_at(data, o + 12)) else {
                break;
            };
            db.read_table(&rd, kind, first, last);
        }
        for v in db.playlist_entries.values_mut().chain(db.history_entries.values_mut()) {
            v.sort_unstable();
        }
        db.playlists.sort_by_key(|p| (p.parent_id, p.sort_order, p.id));
        Ok(db)
    }

    fn read_table(&mut self, rd: &Reader<'_>, kind: u32, first: u32, last: u32) {
        let id_name = |page: &[u8], row: usize, name_at: usize| -> Option<(u32, String)> {
            Some((u32_at(page, row)?, read_string(page, row + name_at)?))
        };
        match kind {
            k if k == Table::Tracks as u32 => rd.for_each_row(first, last, kind, |p, r| {
                if let Some(t) = track_row(p, r) {
                    self.tracks.push(t);
                }
            }),
            k if k == Table::Artists as u32 => rd.for_each_row(first, last, kind, |p, r| {
                if let Some((id, n)) = named_row(p, r, 4, 9, 0x0A) {
                    self.artists.insert(id, n);
                }
            }),
            k if k == Table::Albums as u32 => rd.for_each_row(first, last, kind, |p, r| {
                if let Some((id, n)) = named_row(p, r, 0x0C, 0x15, 0x16) {
                    self.albums.insert(id, n);
                }
            }),
            k if k == Table::Genres as u32 => rd.for_each_row(first, last, kind, |p, r| {
                if let Some((id, n)) = id_name(p, r, 4) {
                    self.genres.insert(id, n);
                }
            }),
            k if k == Table::Keys as u32 => rd.for_each_row(first, last, kind, |p, r| {
                if let Some((id, n)) = id_name(p, r, 8) {
                    self.keys.insert(id, n);
                }
            }),
            k if k == Table::Colors as u32 => rd.for_each_row(first, last, kind, |p, r| {
                if let (Some(id), Some(n)) = (u16_at(p, r + 5), read_string(p, r + 8)) {
                    self.colors.insert(id as u32, n);
                }
            }),
            k if k == Table::PlaylistTree as u32 => rd.for_each_row(first, last, kind, |p, r| {
                let node = (|| {
                    Some(PlaylistNode {
                        parent_id: u32_at(p, r)?,
                        sort_order: u32_at(p, r + 8)?,
                        id: u32_at(p, r + 12)?,
                        is_folder: u32_at(p, r + 16)? != 0,
                        name: read_string(p, r + 20)?,
                    })
                })();
                if let Some(n) = node {
                    self.playlists.push(n);
                }
            }),
            k if k == Table::PlaylistEntries as u32 => rd.for_each_row(first, last, kind, |p, r| {
                if let (Some(i), Some(t), Some(pl)) = (u32_at(p, r), u32_at(p, r + 4), u32_at(p, r + 8)) {
                    self.playlist_entries.entry(pl).or_default().push((i, t));
                }
            }),
            k if k == Table::HistoryPlaylists as u32 => rd.for_each_row(first, last, kind, |p, r| {
                if let Some((id, n)) = id_name(p, r, 4) {
                    self.history.push((id, n));
                }
            }),
            k if k == Table::HistoryEntries as u32 => rd.for_each_row(first, last, kind, |p, r| {
                if let (Some(t), Some(pl), Some(i)) = (u32_at(p, r), u32_at(p, r + 4), u32_at(p, r + 8)) {
                    self.history_entries.entry(pl).or_default().push((i, t));
                }
            }),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEMO: &[u8] = include_bytes!("../../tests/fixtures/rekordbox/demo_tracks/PIONEER/rekordbox/export.pdb");
    const LARGE: &[u8] = include_bytes!("../../tests/fixtures/rekordbox/large_export.pdb");

    #[test]
    fn strings() {
        assert_eq!(read_string(&[0x03], 0).as_deref(), Some(""));
        assert_eq!(read_string(&[0x09, b'a', b'b', b'c'], 0).as_deref(), Some("abc"));
        let utf16 = [0x90, 0x08, 0x00, 0x00, 0x48, 0x00, 0xE9, 0x00];
        assert_eq!(read_string(&utf16, 0).as_deref(), Some("Hé"));
    }

    #[test]
    fn demo_export() {
        let db = Database::parse(DEMO).unwrap();
        let titles: Vec<&str> = db.tracks.iter().map(|t| t.title.as_str()).collect();
        assert!(titles.contains(&"Demo Track 1"), "{titles:?}");
        // Older deleted rows (e.g. SINEWAVE) are still in the file but must be skipped.
        assert_eq!(titles.len(), 2, "{titles:?}");
        assert!(titles.contains(&"Demo Track 2"));
        let t = db.tracks.iter().find(|t| t.title == "Demo Track 1").unwrap();
        assert_eq!(t.file_path, "/Contents/Loopmasters/UnknownAlbum/Demo Track 1.mp3");
        assert!(t.analyze_path.starts_with("/PIONEER/USBANLZ/"), "{}", t.analyze_path);
        assert!(t.tempo > 100 * 60 && t.tempo < 100 * 200, "tempo {}", t.tempo);
        assert_eq!(db.artists.get(&t.artist_id).map(|s| s.as_str()), Some("Loopmasters"));
        assert!(t.duration > 0);
        assert_eq!(db.keys.get(&t.key_id).map(|s| s.as_str()), Some("Fm"));
    }

    #[test]
    fn large_export_row_counts() {
        // Expected counts from the rekordcrate project's test suite for the same file.
        let db = Database::parse(LARGE).unwrap();
        assert_eq!(db.tracks.len(), 3886);
        assert_eq!(db.artists.len(), 2216);
        assert_eq!(db.albums.len(), 2226);
        assert_eq!(db.genres.len(), 315);
        assert_eq!(db.keys.len(), 67);
        assert_eq!(db.colors.len(), 8);
        assert_eq!(db.playlists.len(), 104);
        assert_eq!(db.playlist_entries.values().map(|v| v.len()).sum::<usize>(), 7440);
        assert_eq!(db.history_entries.values().map(|v| v.len()).sum::<usize>(), 73);
        assert!(db.tracks.iter().all(|t| !t.file_path.is_empty()));
    }

    #[test]
    fn garbage_is_rejected_not_panicking() {
        assert!(Database::parse(&[0u8; 10]).is_err());
        let mut junk = DEMO.to_vec();
        for (i, b) in junk.iter_mut().enumerate().skip(0x1000) {
            *b = (i * 7919 % 251) as u8;
        }
        let _ = Database::parse(&junk);
    }
}
