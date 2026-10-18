//! Whole-track mini waveforms for the browser (PREVIEW column and INFO panel), read on a
//! worker thread: rekordbox's own waveform for stick tracks, the cached analysis otherwise.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::rekordbox::settings::WaveColor;
use crate::rekordbox::Export;
use crate::track::wave::Column;

/// Columns per preview (drawn scaled to whatever width the cell has).
pub const COLS: usize = 160;
const MAX: usize = 600;

pub enum Source {
    Rekordbox(Arc<Export>, u32),
    Local,
}

type Key = (PathBuf, WaveColor);

pub struct PreviewCache {
    done: HashMap<Key, Option<Arc<Vec<Column>>>>,
    pending: HashSet<Key>,
    order: Vec<Key>,
    req: Sender<(Key, Source)>,
    rx: Receiver<(Key, Option<Vec<Column>>)>,
}

impl PreviewCache {
    pub fn new(repaint: impl Fn() + Send + 'static) -> PreviewCache {
        let (req, jobs) = unbounded::<(Key, Source)>();
        let (tx, rx) = unbounded();
        std::thread::Builder::new()
            .name("previews".into())
            .spawn(move || {
                for (key, src) in jobs {
                    let cols = build(&key, src);
                    if tx.send((key, cols)).is_err() {
                        break;
                    }
                    repaint();
                }
            })
            .ok();
        PreviewCache { done: HashMap::new(), pending: HashSet::new(), order: Vec::new(), req, rx }
    }

    pub fn poll(&mut self) {
        while let Ok((key, cols)) = self.rx.try_recv() {
            self.pending.remove(&key);
            self.done.insert(key.clone(), cols.map(Arc::new));
            self.order.push(key);
            if self.order.len() > MAX {
                let old = self.order.remove(0);
                self.done.remove(&old);
            }
        }
    }

    /// The preview if ready; otherwise ask for it once.
    pub fn get(&mut self, path: &PathBuf, mode: WaveColor, src: impl FnOnce() -> Source) -> Option<Arc<Vec<Column>>> {
        let key = (path.clone(), mode);
        if let Some(v) = self.done.get(&key) {
            return v.clone();
        }
        if self.pending.insert(key.clone()) {
            let _ = self.req.send((key, src()));
        }
        None
    }
}

fn build(key: &Key, src: Source) -> Option<Vec<Column>> {
    let (path, mode) = key;
    let cols = match src {
        Source::Rekordbox(ex, id) => {
            let a = ex.track(id).and_then(|t| ex.analysis(t))?;
            if a.waves.is_empty() {
                return None;
            }
            crate::track::wave::summarize(Some(&a.waves), &[], *mode, COLS)
        }
        Source::Local => crate::track::wave::summarize(None, &crate::track::wave::load_cached(path)?, *mode, COLS),
    };
    (!cols.is_empty()).then_some(cols)
}
