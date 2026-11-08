//! What the screen, keyboard, controller and gamepad ask the app to do: load, browse,
//! record, zoom, fix a grid, quit.

use crate::ui::prelude::*;
use std::path::PathBuf;
use std::sync::Arc;
use egui::ViewportCommand;
use crate::library::{self, LibTrack, Row};
use crate::track::{self, Track};
use crate::ui::{LoadMsg, WAVE_ZOOMS};
use crate::ui::{App, Banner};

impl App {
    pub(crate) fn send(&self, c: Cmd) {
        let _ = self.cmd.send(c);
    }

    /// Send a mixer/tempo change made on screen, and hold the hardware control back until it
    /// catches up (soft takeover).
    pub(crate) fn send_touched(&self, c: Cmd, ctl: MidiCtl) {
        self.locks.lock().insert(ctl);
        self.send(c);
    }

    pub(crate) fn load(&mut self, deck: usize, lt: &LibTrack) {
        let generation = self.next_gen;
        self.next_gen += 1;
        let du = &mut self.decks[deck];
        du.generation = generation;
        du.error = None;
        du.loading = Some(lt.title.clone());
        du.art = None;
        du.lib = Some(lt.clone());
        let art = self.art_key(lt);
        self.decks[deck].art = art;
        let (path, mut meta, rb) = self.lib.load_request(lt);
        meta.auto_cue = match (self.config.auto_cue, self.config.auto_cue_db) {
            (false, _) => track::AutoCue::Off,
            (true, None) => track::AutoCue::Memory,
            (true, Some(db)) => track::AutoCue::Level(db),
        };
        meta.hotcue_autoload = self.config.hotcue_autoload;
        meta.bpm_range = (self.config.bpm_range[0], self.config.bpm_range[1]);
        meta.offset_ms = self.config.offset_for(&path);
        meta.duration_hint = lt.duration.map(|d| d as f64);
        if let Some(r) = self.store.lock().get(&path).cloned() {
            if r.grid_fixed || rb.is_none() {
                meta.saved_grid = r.grid;
            }
            meta.saved_cues = r.cues;
            meta.saved_cue_point = r.cue_point;
        }
        let tx = self.load_tx.clone();
        let ctx = self.ctx.clone();
        let store = self.store.clone();
        std::thread::Builder::new()
            .name("loader".into())
            .spawn(move || {
                if let Some((ex, id)) = rb {
                    meta.analysis = ex.track(id).and_then(|t| ex.analysis(t));
                }
                // Without rekordbox's waveforms, show a cached one of our own straight away.
                let has_rb_wave = meta.analysis.as_ref().is_some_and(|a| !a.waves.is_empty());
                if !has_rb_wave {
                    meta.cached_wave = crate::track::wave::load_cached(&path);
                }
                let had_cache = meta.cached_wave.is_some();
                let tx2 = tx.clone();
                let c2 = ctx.clone();
                let res = track::load_progressive(&path, meta, move |track| {
                    let _ = tx2.send(LoadMsg::Ready { deck, generation, track });
                    c2.request_repaint();
                });
                if let (Ok(t), false, false) = (&res, had_cache, has_rb_wave) {
                    if t.is_complete() {
                        crate::track::wave::save_cached(&path, &t.wave.read());
                        store.lock().update(&path, |r| r.wave = true);
                    }
                }
                let _ = tx.send(match res {
                    Ok(track) => LoadMsg::Done { deck, generation, track },
                    Err(error) => LoadMsg::Failed { deck, generation, error },
                });
                ctx.request_repaint();
            })
            .ok();
    }

    /// Where a library track's cover comes from: the stick's artwork, else the file's tags.
    pub(crate) fn art_key(&self, lt: &LibTrack) -> Option<crate::ui::cache::art::ArtKey> {
        match lt.rb {
            Some((u, id)) => {
                let ex = self.lib.usbs.get(u)?;
                ex.track(id).and_then(|t| ex.artwork_path(t)).map(crate::ui::cache::art::ArtKey::File)
            }
            None => Some(crate::ui::cache::art::ArtKey::Embedded(lt.path.clone())),
        }
    }

    pub(crate) fn load_selected(&mut self, deck: usize) {
        if let Some(Row::Track(t)) = self.lib.selected_row().cloned() {
            self.load(deck, &t);
            self.after_load();
        }
    }

    pub(crate) fn browse(&mut self, delta: i32) {
        self.lib.move_by(delta);
        self.scroll_to_selected = true;
    }

    pub(crate) fn browse_enter(&mut self) {
        if self.lib.enter() {
            self.search.clear();
            self.scroll_to_selected = true;
        }
    }

    pub(crate) fn browse_back(&mut self) {
        self.lib.back();
        self.search = self.lib.filter.clone();
        self.scroll_to_selected = true;
    }

    pub(crate) fn recordings_dir(&self) -> PathBuf {
        self.config.recording_dir.clone().unwrap_or_else(|| library::default_root().join(crate::recorder::DIR_NAME))
    }

    /// Start recording, or stop on the second press within 3 s.
    pub(crate) fn record_toggle(&mut self) {
        if !self.recorder.is_recording() {
            self.rec_low_warned = false;
            self.rec_confirm = None;
            match self.recorder.start(&self.recordings_dir()) {
                Ok(base) => eprintln!("recording: {}", base.display()),
                Err(e) => self.banner = Some(Banner::notice(format!("Couldn't start recording: {e}"))),
            }
        } else if self.rec_confirm.is_some_and(|t| t.elapsed() < Duration::from_secs(3)) {
            self.rec_confirm = None;
            if let Some(f) = self.recorder.stop() {
                self.recording_finished(f);
            }
        } else {
            self.rec_confirm = Some(Instant::now());
        }
    }

    pub(crate) fn recording_finished(&mut self, f: crate::recorder::Finished) {
        let mut file = f.parts.first().map(|p| p.path.display().to_string()).unwrap_or_default();
        if let Some(home) = std::env::var_os("HOME").map(|h| h.to_string_lossy().into_owned()) {
            if !home.is_empty() && file.starts_with(&home) {
                file = format!("~{}", &file[home.len()..]);
            }
        }
        let len = crate::recorder::fmt_elapsed(f.secs);
        let tracks = match f.tracks {
            0 => String::new(),
            1 => ", 1 track".into(),
            n => format!(", {n} tracks"),
        };
        let parts = if f.parts.len() > 1 { format!(" (in {} files)", f.parts.len()) } else { String::new() };
        let lost = if f.dropped_secs > 0.05 { format!(" — {:.1} s lost because the disk was too slow", f.dropped_secs) } else { String::new() };
        self.banner = Some(Banner::notice(match f.error {
            Some(e) => format!("Recording stopped: {e}. Saved {len}{tracks} to {file}"),
            None => format!("Recording saved: {len}{tracks}{parts} — {file}{lost}"),
        }));
    }

    pub(crate) fn set_zoom(&mut self, dir: i32) {
        let cur = WAVE_ZOOMS.iter().position(|z| (*z - self.config.waveform_seconds).abs() < 0.01).unwrap_or(2) as i32;
        let i = (cur + dir).clamp(0, WAVE_ZOOMS.len() as i32 - 1) as usize;
        self.config.waveform_seconds = WAVE_ZOOMS[i];
    }

    /// Fix a half/double tempo detection on a deck (×2 or ÷2) and remember it.
    pub(crate) fn scale_grid(&mut self, d: usize, factor: f64) {
        let Some(track) = self.decks[d].track.clone() else { return };
        let Some(g) = track.grid.load_full() else { return };
        let ng = g.scaled(factor);
        let sr = track.sample_rate as f64;
        self.store.lock().update(&track.path, |r| {
            r.grid = Some((ng.bpm, ng.first_beat() / sr));
            r.grid_fixed = true;
        });
        track.grid.store(Some(Arc::new(ng)));
        self.lib.learn(&track.path, track.bpm(), &track.key);
    }

    /// Quit on the second press within 3 s, so a stray tap can't end a set.
    pub(crate) fn exit_press(&mut self, ctx: &egui::Context) {
        if self.exit_confirm.is_some_and(|t| t.elapsed() < Duration::from_secs(3)) {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        } else {
            self.exit_confirm = Some(Instant::now());
            ctx.request_repaint_after(Duration::from_secs(3));
        }
    }
}

pub(crate) fn preset_comment(track: &Track, slot: usize, pos: f64) -> String {
    track.cues.iter().find(|c| c.hot == Some(slot) && (c.pos - pos).abs() < 1.0).map(|c| c.comment.clone()).unwrap_or_default()
}
