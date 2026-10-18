//! The browser model: a CDJ-style tree of sources (music folder, rekordbox USB drives),
//! playlists and tracks, navigated one level at a time.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use crossbeam_channel::Sender;

use crate::rekordbox::{self, Export};
use crate::track::{is_audio_file, names_from_path, TrackMeta};

#[derive(Clone, Debug)]
pub struct LibTrack {
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub key: String,
    pub bpm: Option<f64>,
    pub duration: Option<u32>,
    pub ext: String,
    /// (index into `Library::usbs`, rekordbox track id) for tracks from an export.
    pub rb: Option<(usize, u32)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Node {
    Root,
    Folder,
    Usb(usize),
    UsbAll(usize),
    /// Playlist folder (0 = top level).
    UsbPlaylists(usize, u32),
    UsbPlaylist(usize, u32),
    UsbHistory(usize),
    UsbHistoryList(usize, u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavKind {
    Folder,
    Drive,
    Playlist,
    Collection,
}

#[derive(Clone, Debug)]
pub enum Row {
    Nav { label: String, detail: String, kind: NavKind, node: Node },
    Track(LibTrack),
}

/// Sent from the background watchers.
pub enum LibEvent {
    LocalScanned(Vec<LibTrack>),
    UsbAdded(Arc<Export>),
    UsbRemoved(PathBuf),
    UsbError(PathBuf, String),
}

pub struct Library {
    pub local_root: PathBuf,
    pub local: Option<Vec<LibTrack>>,
    pub usbs: Vec<Arc<Export>>,
    /// Navigation stack: (node, selected row) per level, last = current.
    stack: Vec<(Node, usize)>,
    pub rows: Arc<Vec<Row>>,
    /// BPM/key learnt from tracks loaded on a deck, shown for plain files.
    learnt: HashMap<PathBuf, (Option<f64>, String)>,
    pub usb_errors: Vec<(PathBuf, String)>,
}

impl Library {
    pub fn new(local_root: PathBuf) -> Self {
        let mut l = Self {
            local_root,
            local: None,
            usbs: Vec::new(),
            stack: vec![(Node::Root, 0)],
            rows: Arc::new(Vec::new()),
            learnt: HashMap::new(),
            usb_errors: Vec::new(),
        };
        l.refresh();
        l
    }

    pub fn node(&self) -> Node {
        self.stack.last().map(|s| s.0).unwrap_or(Node::Root)
    }

    pub fn selected(&self) -> usize {
        self.stack.last().map(|s| s.1).unwrap_or(0)
    }

    pub fn depth(&self) -> usize {
        self.stack.len() - 1
    }

    pub fn select(&mut self, i: usize) {
        let n = self.rows.len();
        if let Some(top) = self.stack.last_mut() {
            top.1 = i.min(n.saturating_sub(1));
        }
    }

    pub fn move_by(&mut self, delta: i32) {
        let n = self.rows.len() as i64;
        if n == 0 {
            return;
        }
        let s = (self.selected() as i64 + delta as i64).clamp(0, n - 1);
        self.select(s as usize);
    }

    pub fn selected_row(&self) -> Option<&Row> {
        self.rows.get(self.selected())
    }

    /// Open the selected row if it leads somewhere. Returns true if it did.
    pub fn enter(&mut self) -> bool {
        if let Some(Row::Nav { node, .. }) = self.selected_row().cloned() {
            self.stack.push((node, 0));
            self.refresh();
            return true;
        }
        false
    }

    pub fn enter_row(&mut self, i: usize) -> bool {
        self.select(i);
        self.enter()
    }

    pub fn back(&mut self) {
        if self.stack.len() > 1 {
            self.stack.pop();
            self.refresh();
        }
    }

    /// Breadcrumb labels for the current location.
    pub fn crumbs(&self) -> Vec<String> {
        self.stack
            .iter()
            .map(|(n, _)| match *n {
                Node::Root => "Sources".to_string(),
                Node::Folder => "Music folder".to_string(),
                Node::Usb(u) => self.usbs.get(u).map(|e| e.name.clone()).unwrap_or_default(),
                Node::UsbAll(_) => "All tracks".into(),
                Node::UsbPlaylists(u, 0) | Node::UsbHistory(u) if self.usbs.get(u).is_none() => String::new(),
                Node::UsbPlaylists(_, 0) => "Playlists".into(),
                Node::UsbPlaylists(u, id) | Node::UsbPlaylist(u, id) => self
                    .usbs
                    .get(u)
                    .and_then(|e| e.db.playlists.iter().find(|p| p.id == id))
                    .map(|p| p.name.clone())
                    .unwrap_or_default(),
                Node::UsbHistory(_) => "History".into(),
                Node::UsbHistoryList(u, id) => self
                    .usbs
                    .get(u)
                    .and_then(|e| e.db.history.iter().find(|h| h.0 == id))
                    .map(|h| h.1.clone())
                    .unwrap_or_default(),
            })
            .collect()
    }

    pub fn learn(&mut self, path: &Path, bpm: Option<f64>, key: &str) {
        self.learnt.insert(path.to_path_buf(), (bpm, key.to_string()));
        if let Some(local) = &mut self.local {
            for t in local.iter_mut().filter(|t| t.path == path) {
                t.bpm = t.bpm.or(bpm);
            }
        }
        self.refresh();
    }

    pub fn handle(&mut self, ev: LibEvent) {
        match ev {
            LibEvent::LocalScanned(v) => {
                let mut v = v;
                for t in &mut v {
                    if let Some((b, _)) = self.learnt.get(&t.path) {
                        t.bpm = *b;
                    }
                }
                self.local = Some(v);
            }
            LibEvent::UsbAdded(ex) => {
                self.usb_errors.retain(|(p, _)| *p != ex.root);
                if let Some(i) = self.usbs.iter().position(|e| e.root == ex.root) {
                    self.usbs[i] = ex; // re-exported: replace in place
                } else {
                    self.usbs.push(ex);
                }
            }
            LibEvent::UsbRemoved(root) => {
                if let Some(i) = self.usbs.iter().position(|e| e.root == root) {
                    self.usbs.remove(i);
                    // Indices shift: simplest correct thing is to go back to the top.
                    self.stack.truncate(1);
                }
            }
            LibEvent::UsbError(root, e) => {
                self.usb_errors.retain(|(p, _)| *p != root);
                self.usb_errors.push((root, e));
            }
        }
        self.refresh();
    }

    fn rb_track(&self, u: usize, ex: &Export, id: u32) -> Option<LibTrack> {
        let t = ex.track(id)?;
        let path = ex.audio_path(t);
        Some(LibTrack {
            ext: path.extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default(),
            path,
            title: t.title.clone(),
            artist: ex.artist(t).to_string(),
            key: ex.key(t).to_string(),
            bpm: (t.tempo > 0).then(|| t.tempo as f64 / 100.0),
            duration: (t.duration > 0).then_some(t.duration as u32),
            rb: Some((u, id)),
        })
    }

    pub fn refresh(&mut self) {
        let node = self.node();
        let nav = |label: String, detail: String, kind: NavKind, node: Node| Row::Nav { label, detail, kind, node };
        let mut rows = Vec::new();
        match node {
            Node::Root => {
                for (u, ex) in self.usbs.iter().enumerate() {
                    rows.push(nav(
                        ex.name.clone(),
                        format!("rekordbox USB · {} tracks", ex.db.tracks.len()),
                        NavKind::Drive,
                        Node::Usb(u),
                    ));
                }
                let detail = match &self.local {
                    Some(l) => format!("{} · {} tracks", self.local_root.display(), l.len()),
                    None => format!("{} · scanning…", self.local_root.display()),
                };
                rows.push(nav("Music folder".into(), detail, NavKind::Folder, Node::Folder));
            }
            Node::Folder => {
                rows.extend(self.local.iter().flatten().cloned().map(Row::Track));
            }
            Node::Usb(u) => {
                if let Some(ex) = self.usbs.get(u) {
                    rows.push(nav("All tracks".into(), format!("{} tracks", ex.db.tracks.len()), NavKind::Collection, Node::UsbAll(u)));
                    let top = ex.playlist_children(0).len();
                    rows.push(nav("Playlists".into(), format!("{top} items"), NavKind::Folder, Node::UsbPlaylists(u, 0)));
                    if !ex.db.history.is_empty() {
                        rows.push(nav("History".into(), format!("{} sessions", ex.db.history.len()), NavKind::Folder, Node::UsbHistory(u)));
                    }
                }
            }
            Node::UsbAll(u) => {
                if let Some(ex) = self.usbs.get(u) {
                    let mut tracks: Vec<LibTrack> =
                        ex.db.tracks.iter().filter_map(|t| self.rb_track(u, ex, t.id)).collect();
                    tracks.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
                    rows.extend(tracks.into_iter().map(Row::Track));
                }
            }
            Node::UsbPlaylists(u, parent) => {
                if let Some(ex) = self.usbs.get(u) {
                    for p in ex.playlist_children(parent) {
                        if p.is_folder {
                            let n = ex.playlist_children(p.id).len();
                            rows.push(nav(p.name.clone(), format!("{n} items"), NavKind::Folder, Node::UsbPlaylists(u, p.id)));
                        } else {
                            let n = ex.playlist_tracks(p.id).len();
                            rows.push(nav(p.name.clone(), format!("{n} tracks"), NavKind::Playlist, Node::UsbPlaylist(u, p.id)));
                        }
                    }
                }
            }
            Node::UsbPlaylist(u, id) => {
                if let Some(ex) = self.usbs.get(u) {
                    rows.extend(ex.playlist_tracks(id).into_iter().filter_map(|t| self.rb_track(u, ex, t)).map(Row::Track));
                }
            }
            Node::UsbHistory(u) => {
                if let Some(ex) = self.usbs.get(u) {
                    for (id, name) in &ex.db.history {
                        let n = ex.db.history_entries.get(id).map(|v| v.len()).unwrap_or(0);
                        rows.push(nav(name.clone(), format!("{n} tracks"), NavKind::Playlist, Node::UsbHistoryList(u, *id)));
                    }
                }
            }
            Node::UsbHistoryList(u, id) => {
                if let Some(ex) = self.usbs.get(u) {
                    let ids: Vec<u32> = ex.db.history_entries.get(&id).map(|v| v.iter().map(|e| e.1).collect()).unwrap_or_default();
                    rows.extend(ids.into_iter().filter_map(|t| self.rb_track(u, ex, t)).map(Row::Track));
                }
            }
        }
        // Fill BPM/key learnt from loaded files.
        for r in rows.iter_mut() {
            if let Row::Track(t) = r {
                if let Some((b, k)) = self.learnt.get(&t.path) {
                    t.bpm = t.bpm.or(*b);
                    if t.key.is_empty() {
                        t.key = k.clone();
                    }
                }
            }
        }
        self.rows = Arc::new(rows);
        let n = self.rows.len();
        if let Some(top) = self.stack.last_mut() {
            top.1 = top.1.min(n.saturating_sub(1));
        }
    }

    /// Everything the loader thread needs to load a track, rekordbox analysis included.
    pub fn load_request(&self, t: &LibTrack) -> (PathBuf, TrackMeta, Option<(Arc<Export>, u32)>) {
        let meta = TrackMeta {
            title: Some(t.title.clone()).filter(|_| t.rb.is_some()),
            artist: Some(t.artist.clone()).filter(|_| t.rb.is_some()),
            key: Some(t.key.clone()).filter(|k| !k.is_empty()),
            analysis: None,
        };
        let rb = t.rb.and_then(|(u, id)| self.usbs.get(u).map(|e| (e.clone(), id)));
        (t.path.clone(), meta, rb)
    }
}

pub fn scan(root: &Path) -> Vec<LibTrack> {
    let mut out = Vec::new();
    walk(root, 0, &mut out);
    out.sort_by(|a, b| {
        (a.artist.to_lowercase(), a.title.to_lowercase()).cmp(&(b.artist.to_lowercase(), b.title.to_lowercase()))
    });
    out
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<LibTrack>) {
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
            walk(&p, depth + 1, out);
        } else if is_audio_file(&p) {
            let (title, artist) = names_from_path(&p);
            let ext = p.extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default();
            out.push(LibTrack { path: p, title, artist, key: String::new(), bpm: None, duration: None, ext, rb: None });
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

/// Background work: scan the music folder once, then watch for rekordbox USB drives
/// coming and going (and being re-exported).
pub fn spawn_watchers(local_root: PathBuf, extra_usb: Vec<PathBuf>, tx: Sender<LibEvent>, wake: impl Fn() + Send + Sync + 'static) {
    let wake = Arc::new(wake);
    let (tx1, w1) = (tx.clone(), wake.clone());
    std::thread::Builder::new()
        .name("scan".into())
        .spawn(move || {
            let _ = tx1.send(LibEvent::LocalScanned(scan(&local_root)));
            w1();
        })
        .ok();
    std::thread::Builder::new()
        .name("usb".into())
        .spawn(move || {
            let mut known: HashMap<PathBuf, Option<SystemTime>> = HashMap::new();
            loop {
                let found = rekordbox::mount_candidates(&extra_usb);
                let mut changed = false;
                for root in &found {
                    let stamp = rekordbox::pdb_path(root)
                        .and_then(|p| std::fs::metadata(p).ok())
                        .and_then(|m| m.modified().ok());
                    if known.get(root) == Some(&stamp) {
                        continue;
                    }
                    known.insert(root.clone(), stamp);
                    changed = true;
                    let ev = match Export::open(root) {
                        Ok(ex) => LibEvent::UsbAdded(Arc::new(ex)),
                        Err(e) => LibEvent::UsbError(root.clone(), e),
                    };
                    if tx.send(ev).is_err() {
                        return;
                    }
                }
                let gone: Vec<PathBuf> = known.keys().filter(|k| !found.contains(k)).cloned().collect();
                for root in gone {
                    known.remove(&root);
                    changed = true;
                    if tx.send(LibEvent::UsbRemoved(root)).is_err() {
                        return;
                    }
                }
                if changed {
                    wake();
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigates_a_rekordbox_export() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rekordbox/demo_tracks");
        let mut lib = Library::new(PathBuf::from("/nonexistent"));
        lib.handle(LibEvent::LocalScanned(Vec::new()));
        lib.handle(LibEvent::UsbAdded(Arc::new(Export::open(&root).unwrap())));
        assert!(matches!(lib.rows[0], Row::Nav { node: Node::Usb(0), .. }));
        assert!(lib.enter()); // into the drive
        assert!(lib.enter()); // All tracks
        assert_eq!(lib.rows.len(), 2);
        let Row::Track(t) = &lib.rows[0] else { panic!() };
        assert_eq!(t.title, "Demo Track 1");
        assert_eq!(t.bpm, Some(128.0));
        assert_eq!(t.key, "Fm");
        assert_eq!(lib.crumbs(), vec!["Sources", "demo_tracks", "All tracks"]);
        lib.back();
        lib.back();
        assert_eq!(lib.node(), Node::Root);
        lib.handle(LibEvent::UsbRemoved(root));
        assert_eq!(lib.rows.len(), 1);
    }
}
