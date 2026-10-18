//! Finding music: scanning the music folder, and background watchers for rekordbox sticks,
//! the folder and BPM analysis.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use crossbeam_channel::Sender;
use parking_lot::Mutex;
use crate::config::Store;
use crate::rekordbox::{self, Export};
use crate::track::{self, is_audio_file, names_from_path};

use super::*;

pub fn scan(root: &Path) -> Vec<LibTrack> {
    let mut out = Vec::new();
    walk(root, 0, &mut out);
    out.sort_by(|a, b| {
        (a.artist.to_lowercase(), a.title.to_lowercase()).cmp(&(b.artist.to_lowercase(), b.title.to_lowercase()))
    });
    out
}

pub(crate) fn walk(dir: &Path, depth: usize, out: &mut Vec<LibTrack>) {
    if depth > 8 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name();
        // Hidden files, and our own recordings (a WAV still being written isn't a track).
        if name.to_string_lossy().starts_with('.') || name == crate::recorder::DIR_NAME || name == "Platter Recordings" {
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
pub struct WatchOptions {
    pub local_root: PathBuf,
    pub extra_usb: Vec<PathBuf>,
    pub store: Arc<Mutex<Store>>,
    pub analyze: bool,
    pub bpm_range: (f64, f64),
}

pub fn spawn_watchers(opts: WatchOptions, tx: Sender<LibEvent>, wake: impl Fn() + Send + Sync + 'static) {
    let wake = Arc::new(wake);
    let (tx1, w1) = (tx.clone(), wake.clone());
    let WatchOptions { local_root, extra_usb, store, analyze, bpm_range } = opts;
    std::thread::Builder::new()
        .name("scan".into())
        .spawn(move || {
            let _ = tx1.send(LibEvent::History(crate::library::history::load_all()));
            let _ = tx1.send(LibEvent::Tags(crate::library::history::load_tags()));
            let mut local = scan(&local_root);
            // Apply what's cached, then fill in tags for the rest.
            let mut missing = Vec::new();
            {
                let st = store.lock();
                for t in local.iter_mut() {
                    match st.get(&t.path) {
                        Some(r) => {
                            if let Some(tags) = &r.tags {
                                apply_tags(t, tags);
                            }
                            if let Some((bpm, _)) = r.grid {
                                t.bpm = Some(bpm);
                            }
                            if r.tags.is_none() {
                                missing.push(t.path.clone());
                            }
                        }
                        None => missing.push(t.path.clone()),
                    }
                }
            }
            let paths: Vec<PathBuf> = local.iter().map(|t| t.path.clone()).collect();
            if tx1.send(LibEvent::LocalScanned(local)).is_err() {
                return;
            }
            w1();
            let mut batch = Vec::new();
            let mut last = std::time::Instant::now();
            for p in missing {
                if let Ok(tags) = track::read_tags(&p) {
                    store.lock().update(&p, |r| r.tags = Some(tags.clone()));
                    batch.push((p, Info { tags: Some(tags), bpm: None }));
                }
                if last.elapsed() > Duration::from_millis(400) {
                    let _ = tx1.send(LibEvent::Info(std::mem::take(&mut batch)));
                    // Serialise under the lock, write outside it, so the UI never waits.
                    let job = store.lock().snapshot(false);
                    crate::config::write_now(job);
                    w1();
                    last = std::time::Instant::now();
                }
            }
            let _ = tx1.send(LibEvent::Info(std::mem::take(&mut batch)));
            let job = store.lock().snapshot(true);
            crate::config::write_now(job);
            w1();
            if !analyze {
                return;
            }
            // Slow pass: detect BPM and draw the waveform for files that don't have them yet,
            // one at a time.
            let todo: Vec<PathBuf> = {
                let st = store.lock();
                paths.into_iter().filter(|p| st.get(p).is_none_or(|r| r.grid.is_none() || !r.wave)).collect()
            };
            let total = todo.len();
            for (i, p) in todo.into_iter().enumerate() {
                let _ = tx1.send(LibEvent::Progress(i, total));
                if let Ok((tags, grid, bins)) = track::analyze_file(&p, bpm_range) {
                    crate::track::wave::save_cached(&p, &bins);
                    store.lock().update(&p, |r| {
                        r.grid = r.grid.or(grid);
                        r.wave = !bins.is_empty();
                        if r.tags.is_none() {
                            r.tags = Some(tags);
                        }
                    });
                    let job = store.lock().snapshot(false);
                    crate::config::write_now(job);
                    if let Some((bpm, _)) = grid {
                        let _ = tx1.send(LibEvent::Info(vec![(p, Info { tags: None, bpm: Some(bpm) })]));
                        w1();
                    }
                }
                // Leave the CPU to the decks.
                std::thread::sleep(Duration::from_millis(300));
            }
            let _ = tx1.send(LibEvent::Progress(total, total));
            store.lock().flush(true);
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
