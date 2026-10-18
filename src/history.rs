//! Play history: every track that went on air (playing with its fader open for a few
//! seconds) is saved per session in `~/.local/share/platter/history/`, like a CDJ's HISTORY.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// How many past sessions the browser lists.
const KEEP: usize = 200;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HistEntry {
    /// Local time it went on air, "21:34".
    pub time: String,
    pub title: String,
    pub artist: String,
    pub key: String,
    pub bpm: Option<f64>,
    pub duration: Option<u32>,
    pub path: PathBuf,
    /// For tracks from a rekordbox stick: the path inside the export and the stick's name,
    /// so the track is found again when the stick is mounted somewhere else next time.
    pub rb_path: Option<String>,
    pub usb: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    /// "2026-10-18 21-30" (also the file name).
    pub started: String,
    pub entries: Vec<HistEntry>,
}

impl Session {
    pub fn new() -> Session {
        Session { started: crate::recorder::local_stamp(), entries: Vec::new() }
    }

    /// "2026-10-18 21:30".
    pub fn label(&self) -> String {
        match self.started.rsplit_once(' ') {
            Some((date, time)) => format!("{date} {}", time.replace('-', ":")),
            None => self.started.clone(),
        }
    }

    fn file(&self) -> PathBuf {
        dir().join(format!("{}.json", self.started))
    }

    /// Save on a short-lived thread (tiny file; never blocks the UI).
    pub fn save(&self) {
        if self.entries.is_empty() {
            return;
        }
        let (path, data) = (self.file(), serde_json::to_vec_pretty(self));
        std::thread::spawn(move || {
            let Ok(data) = data else { return };
            let _ = std::fs::create_dir_all(dir());
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, data).is_ok() {
                let _ = std::fs::rename(tmp, path);
            }
        });
    }
}

pub fn dir() -> PathBuf {
    crate::config::data_dir().join("history")
}

/// Past sessions, newest first.
pub fn load_all() -> Vec<Session> {
    let Ok(rd) = std::fs::read_dir(dir()) else { return Vec::new() };
    let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "json")).collect();
    files.sort();
    files.reverse();
    files
        .into_iter()
        .take(KEEP)
        .filter_map(|p| serde_json::from_slice::<Session>(&std::fs::read(p).ok()?).ok())
        .filter(|s| !s.entries.is_empty())
        .collect()
}

/// "21:34" now.
pub fn clock() -> String {
    let s = crate::recorder::local_stamp();
    s.rsplit_once(' ').map(|(_, t)| t.replace('-', ":")).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_round_trip_and_label() {
        let mut s = Session { started: "2026-10-18 21-30".into(), entries: vec![] };
        assert_eq!(s.label(), "2026-10-18 21:30");
        s.entries.push(HistEntry { title: "One".into(), rb_path: Some("/Contents/a.mp3".into()), ..Default::default() });
        let j = serde_json::to_string(&s).unwrap();
        let back: Session = serde_json::from_str(&j).unwrap();
        assert_eq!(back, s);
        // Older or partial files still load.
        let partial: Session = serde_json::from_str(r#"{"started":"x","entries":[{"title":"T"}]}"#).unwrap();
        assert_eq!(partial.entries[0].title, "T");
    }
}
