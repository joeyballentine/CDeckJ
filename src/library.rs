//! Music folder scan. Runs on a background thread; no tag reading, so it's fast on big folders.

use std::path::{Path, PathBuf};

use crate::track::{is_audio_file, names_from_path};

#[derive(Clone, Debug)]
pub struct Entry {
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    /// Folder relative to the music root ("" for the root itself).
    pub folder: String,
    pub ext: String,
}

pub fn scan(root: &Path) -> Vec<Entry> {
    let mut out = Vec::new();
    walk(root, root, 0, &mut out);
    out.sort_by(|a, b| {
        (a.folder.to_lowercase(), a.artist.to_lowercase(), a.title.to_lowercase())
            .cmp(&(b.folder.to_lowercase(), b.artist.to_lowercase(), b.title.to_lowercase()))
    });
    out
}

fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<Entry>) {
    if depth > 8 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            walk(root, &p, depth + 1, out);
        } else if is_audio_file(&p) {
            let (title, artist) = names_from_path(&p);
            let folder = p
                .parent()
                .and_then(|f| f.strip_prefix(root).ok())
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default();
            let ext = p
                .extension()
                .map(|e| e.to_string_lossy().to_uppercase())
                .unwrap_or_default();
            out.push(Entry { path: p, title, artist, folder, ext });
        }
    }
}

/// `~/Music`, falling back to the current directory.
pub fn default_root() -> PathBuf {
    std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join("Music"))
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| PathBuf::from("."))
}
