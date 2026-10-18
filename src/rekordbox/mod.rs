//! rekordbox USB exports ("Device Library"): find them on mounted drives, read the track
//! database and per-track analysis (beat grid, hot cues, memory cues).

pub mod anlz;
pub mod pdb;
pub mod settings;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub use anlz::Analysis;
pub use pdb::{Database, PlaylistNode, TrackRow};

pub struct Export {
    /// Mount point of the drive (the folder that contains `PIONEER`).
    pub root: PathBuf,
    pub name: String,
    pub db: Database,
    by_id: HashMap<u32, usize>,
}

/// Child of `dir` whose name matches `name` case-insensitively (FAT/exFAT sticks vary).
fn child_ci(dir: &Path, name: &str) -> Option<PathBuf> {
    let exact = dir.join(name);
    if exact.exists() {
        return Some(exact);
    }
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(name))
        .map(|e| e.path())
}

/// Resolve a path stored in the export (always `/`-separated, relative to the drive root).
fn resolve(root: &Path, stored: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    for part in stored.split('/').filter(|s| !s.is_empty()) {
        p = child_ci(&p, part).unwrap_or_else(|| p.join(part));
    }
    p
}

pub fn pdb_path(root: &Path) -> Option<PathBuf> {
    let pioneer = child_ci(root, "PIONEER")?;
    let rb = child_ci(&pioneer, "rekordbox")?;
    child_ci(&rb, "export.pdb").filter(|p| p.is_file())
}

impl Export {
    pub fn open(root: &Path) -> Result<Export, String> {
        let path = pdb_path(root).ok_or("no PIONEER/rekordbox/export.pdb on this drive")?;
        let data = std::fs::read(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
        let db = Database::parse(&data)?;
        let by_id = db.tracks.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "USB".into());
        Ok(Export { root: root.to_path_buf(), name, db, by_id })
    }

    pub fn track(&self, id: u32) -> Option<&TrackRow> {
        self.by_id.get(&id).map(|&i| &self.db.tracks[i])
    }

    pub fn audio_path(&self, t: &TrackRow) -> PathBuf {
        resolve(&self.root, &t.file_path)
    }

    /// The track's cover on the stick, preferring the larger `_m` version CDJ-3000s use.
    pub fn artwork_path(&self, t: &TrackRow) -> Option<PathBuf> {
        let stored = self.db.artwork.get(&t.artwork_id)?;
        let p = resolve(&self.root, stored);
        let stem = p.file_stem()?.to_string_lossy().into_owned();
        let big = p.with_file_name(format!("{stem}_m.jpg"));
        Some(if big.exists() { big } else { p })
    }

    pub fn artist(&self, t: &TrackRow) -> &str {
        self.db.artists.get(&t.artist_id).map(|s| s.as_str()).unwrap_or("")
    }

    pub fn key(&self, t: &TrackRow) -> &str {
        self.db.keys.get(&t.key_id).map(|s| s.as_str()).unwrap_or("")
    }

    /// Children of a playlist folder (0 = root), in rekordbox's order.
    pub fn playlist_children(&self, parent: u32) -> Vec<&PlaylistNode> {
        self.db.playlists.iter().filter(|p| p.parent_id == parent).collect()
    }

    pub fn playlist_tracks(&self, id: u32) -> Vec<u32> {
        self.db
            .playlist_entries
            .get(&id)
            .map(|v| v.iter().map(|&(_, t)| t).collect())
            .unwrap_or_default()
    }

    /// Read the beat grid and cues for a track (`.DAT`, then `.EXT` which supersedes its cues).
    pub fn analysis(&self, t: &TrackRow) -> Option<Analysis> {
        if t.analyze_path.is_empty() {
            return None;
        }
        let dat = resolve(&self.root, &t.analyze_path);
        let mut a = Analysis::default();
        let mut ok = false;
        if let Ok(d) = std::fs::read(&dat) {
            ok |= a.read(&d).is_ok();
        }
        let ext = dat.with_extension("EXT");
        let ext = if ext.exists() { ext } else { dat.with_extension("ext") };
        if let Ok(d) = std::fs::read(&ext) {
            ok |= a.read(&d).is_ok();
        }
        // CDJ-3000-era 3-band waveform.
        let ex2 = dat.with_extension("2EX");
        let ex2 = if ex2.exists() { ex2 } else { dat.with_extension("2ex") };
        if let Ok(d) = std::fs::read(&ex2) {
            ok |= a.read(&d).is_ok();
        }
        ok.then_some(a)
    }
}

/// Where removable drives get mounted: SteamOS/most desktops use /run/media/$USER.
pub fn mount_candidates(extra: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = extra.to_vec();
    let user = std::env::var("USER").unwrap_or_default();
    let mut bases = vec![
        PathBuf::from("/run/media").join(&user),
        PathBuf::from("/media").join(&user),
        PathBuf::from("/media"),
        PathBuf::from("/mnt"),
    ];
    if let Some(h) = std::env::var_os("HOME") {
        bases.push(PathBuf::from(h).join("rekordbox-usb"));
    }
    for base in bases {
        if let Ok(rd) = std::fs::read_dir(&base) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() && !out.contains(&p) {
                    out.push(p);
                }
            }
        }
    }
    out.retain(|p| pdb_path(p).is_some());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_export_end_to_end() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rekordbox/demo_tracks");
        let ex = Export::open(&root).unwrap();
        let t = ex.db.tracks.iter().find(|t| t.title == "Demo Track 1").unwrap();
        assert!(ex.audio_path(t).ends_with("Contents/Loopmasters/UnknownAlbum/Demo Track 1.mp3"));
        let a = ex.analysis(t).expect("analysis");
        assert!(!a.beats.is_empty());
        // The grid agrees with the database tempo.
        assert_eq!(a.beats[0].tempo as u32, t.tempo);
        // Playlists resolve to known tracks.
        for p in ex.playlist_children(0) {
            for id in ex.playlist_tracks(p.id) {
                assert!(ex.track(id).is_some());
            }
        }
        assert!(mount_candidates(&[root.clone()]).contains(&root));
    }
}

