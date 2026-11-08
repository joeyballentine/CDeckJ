//! Work done every frame outside drawing: messages from the library, loaders and controllers,
//! audio reconnects, saving settings and cues, history and recording.

use crate::ui::prelude::*;
use std::sync::atomic::Ordering;
use crate::audio;
use crate::config::Config;
use crate::engine::deck::HOT_CUES;
use crate::library::LibEvent;
use crate::midi::UiAction;
use crate::track::{GridSource, SavedCue};
use crate::ui::actions::preset_comment;
use crate::ui::{CueSnapshot, LoadMsg};
use crate::ui::{App, Banner};

impl App {
    pub(crate) fn poll(&mut self) {
        while let Ok(ev) = self.lib_rx.try_recv() {
            if let (LibEvent::UsbAdded(ex), true) = (&ev, self.config.apply_usb_settings) {
                let s = crate::rekordbox::settings::MySettings::read(&ex.root);
                let name = ex.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "the USB".into());
                if !s.is_empty() {
                    self.apply_my_settings(&s, &name);
                }
            }
            self.lib.handle(ev);
        }
        while let Ok(m) = self.load_rx.try_recv() {
            match m {
                LoadMsg::Ready { deck, generation, track } => {
                    let du = &mut self.decks[deck];
                    if generation != du.generation {
                        continue; // superseded by a newer load
                    }
                    du.loading = None;
                    du.track = Some(track.clone());
                    du.cue_held = false;
                    du.hot_held = [false; HOT_CUES];
                    du.saved = None;
                    let _ = self.cmd.send(Cmd::Load { deck, track, generation });
                }
                LoadMsg::Done { deck, generation, track } => {
                    if generation != self.decks[deck].generation {
                        continue;
                    }
                    self.lib.learn(&track.path, track.bpm(), &track.key);
                    // Cache detected grids so the next load (and the browser) has them.
                    if let Some(g) = track.grid.load_full().filter(|g| g.source == GridSource::Detected && g.is_constant()) {
                        let sr = track.sample_rate as f64;
                        self.store.lock().update(&track.path, |r| r.grid = Some((g.bpm, g.first_beat() / sr)));
                    }
                }
                LoadMsg::Failed { deck, generation, error } => {
                    let du = &mut self.decks[deck];
                    if generation == du.generation {
                        du.loading = None;
                        du.error = Some(error);
                    }
                }
            }
        }
        while let Ok(a) = self.ui_rx.try_recv() {
            match a {
                // In the WAVE view the browse knob zooms the waveforms, like a CDJ's rotary.
                UiAction::Browse(d) if self.view_mode == View::Wave => self.set_zoom(-d.signum()),
                UiAction::Browse(d) => self.browse(d),
                UiAction::BrowsePress => self.browse_enter(),
                UiAction::Back => self.browse_back(),
                UiAction::Load(d) => self.load_selected(d),
                UiAction::Record => self.record_toggle(),
                UiAction::CycleView(d) => self.cycle_view(d),
            }
        }
    }

    /// Reconnect audio after the device drops (or keep trying if it never opened).
    pub(crate) fn maintain_audio(&mut self) {
        let lost = audio::STREAM_LOST.load(Ordering::Relaxed);
        if lost && self.audio.is_some() {
            self.audio = None;
            self.audio_error = Some("audio device disconnected".into());
            self.silent = Some(audio::SilentClock::start(&self.engine));
        }
        if let Some(rx) = &self.reconnecting {
            match rx.try_recv() {
                Ok(Ok(a)) => {
                    eprintln!("audio: connected to {}", a.description);
                    self.silent = None;
                    self.audio = Some(a);
                    self.audio_error = None;
                    self.reconnecting = None;
                }
                Ok(Err(e)) => {
                    self.audio_error = Some(e);
                    // A failed attempt may have left the engine set up for that device.
                    if self.silent.is_some() {
                        self.engine.lock().set_output(48000, 2, crate::engine::CueRouting::MasterOnly);
                    }
                    self.reconnecting = None;
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {}
                Err(crossbeam_channel::TryRecvError::Disconnected) => self.reconnecting = None,
            }
        } else if self.audio.is_none() && self.last_reconnect.elapsed() > Duration::from_secs(2) {
            self.last_reconnect = Instant::now();
            // Probing devices can take a while: do it on a thread while the silent clock
            // keeps time, and hand over only once a stream exists.
            let (tx, rx) = crossbeam_channel::bounded(1);
            let (opts, engine) = (self.audio_opts.clone(), self.engine.clone());
            std::thread::spawn(move || {
                let _ = tx.send(audio::start(&opts, &engine));
            });
            self.reconnecting = Some(rx);
        }
    }

    /// Save toggles/levels to config and cue edits to the track store, both debounced.
    pub(crate) fn persist(&mut self, v: &EngineView) {
        let c = &mut self.config;
        c.quantize = v.quantize;
        c.fullscreen = self.fullscreen;
        for d in 0..DECKS {
            let dv = &v.decks[d];
            c.decks[d].vinyl = dv.vinyl;
            c.decks[d].master_tempo = dv.keylock;
            c.decks[d].tempo_range = dv.range_idx;
        }
        c.fx = crate::config::FxPrefs { kind: v.mixer.fx.kind, beat_idx: v.mixer.fx.beat_idx, target: v.mixer.fx.target, level: v.mixer.fx.level };
        c.master_level = v.mixer.master;
        c.phones_mix = v.mixer.head_mix;
        c.phones_level = v.mixer.head_level;
        if self.config != self.config_saved {
            let since = *self.config_dirty_since.get_or_insert_with(Instant::now);
            if since.elapsed() > Duration::from_secs(2) {
                self.config.save_in_background();
                self.config_saved = self.config.clone();
                self.config_dirty_since = None;
            }
        }

        for d in 0..DECKS {
            let dv = &v.decks[d];
            let Some(track) = self.decks[d].track.clone() else { continue };
            if !dv.loaded || dv.track_gen != self.decks[d].generation {
                continue;
            }
            let mut memory: Vec<i64> = dv.memory.iter().flatten().map(|m| *m as i64).collect();
            memory.sort_unstable();
            let snap = CueSnapshot { path: track.path.clone(), hot: dv.hot, memory, cue: dv.cue as i64 };
            match &self.decks[d].saved {
                None => self.decks[d].saved = Some(snap), // state as loaded: nothing to save
                Some(prev) if *prev != snap && !dv.playing => {
                    let sr = track.sample_rate as f64;
                    let mut cues: Vec<SavedCue> = dv
                        .hot
                        .iter()
                        .enumerate()
                        .filter_map(|(i, h)| {
                            h.map(|h| SavedCue {
                                hot: Some(i),
                                at: h.pos / sr,
                                loop_end: h.loop_end.map(|e| e / sr),
                                color: h.color,
                                comment: preset_comment(&track, i, h.pos),
                            })
                        })
                        .collect();
                    cues.extend(dv.memory.iter().flatten().map(|m| SavedCue { hot: None, at: m / sr, loop_end: None, color: None, comment: String::new() }));
                    let cue = dv.cue / sr;
                    let mut st = self.store.lock();
                    st.update(&track.path, |r| {
                        r.cues = Some(cues);
                        r.cue_point = Some(cue);
                    });
                    // Cue edits are precious: write now (off the UI thread) rather than on the debounce.
                    let job = st.snapshot(true);
                    drop(st);
                    crate::config::write_in_background(job);
                    self.decks[d].saved = Some(snap);
                }
                _ => {}
            }
        }
        let job = self.store.lock().snapshot(false);
        crate::config::write_in_background(job);
    }

    /// Take on a stick's My Settings, as a CDJ does when the stick is loaded.
    pub(crate) fn apply_my_settings(&mut self, s: &crate::rekordbox::settings::MySettings, name: &str) {
        use crate::rekordbox::settings::AutoCueLevel;
        let c = &mut self.config;
        if let Some(v) = s.waveform_color {
            c.waveform_color = v;
        }
        if let Some(v) = s.overview_full {
            c.overview_full = v;
        }
        if let Some(v) = s.playhead_left {
            c.playhead_left = v;
        }
        if let Some(v) = s.time_remain {
            c.time_remain = v;
        }
        if let Some(v) = s.beat_jump {
            c.beat_jump = v;
        }
        if let Some(v) = s.auto_cue {
            c.auto_cue = v;
        }
        if let Some(v) = s.auto_cue_level {
            c.auto_cue_db = match v {
                AutoCueLevel::Memory => None,
                AutoCueLevel::Db(db) => Some(db),
            };
        }
        if let Some(v) = s.hotcue_autoload {
            c.hotcue_autoload = v;
        }
        if let Some(v) = s.beat_fx_quantize {
            c.beat_fx_quantize = v;
        }
        if let Some(v) = s.quantize_beats {
            c.quantize_beats = v;
            self.send(Cmd::QuantizeBeats(v));
        }
        if let Some(v) = s.beat_fx_quantize {
            self.send(Cmd::Mix(MixParam::FxQuantize(v)));
        }
        if let Some(v) = s.fader_curve {
            self.config.fader_curve = v;
            self.send(Cmd::Mix(MixParam::FaderCurve(v)));
        }
        if let Some(v) = s.crossfader_curve {
            self.config.crossfader_curve = v;
            self.send(Cmd::Mix(MixParam::CrossfaderCurve(v)));
        }
        if let Some(v) = s.quantize {
            self.send(Cmd::Quantize(v));
        }
        for d in 0..DECKS {
            if let Some(v) = s.jog_vinyl {
                self.send(Cmd::Vinyl(d, v));
            }
            if let Some(v) = s.master_tempo {
                self.send(Cmd::MasterTempo(d, v));
            }
            if let Some(v) = s.tempo_range {
                self.send(Cmd::TempoRange(d, v));
            }
        }
        self.banner = Some(Banner::notice(format!("Using My Settings from {name}: {}", s.summary())));
    }

    /// Add tracks to the session history once they've been on air for a few seconds.
    pub(crate) fn history_tick(&mut self, v: &EngineView) {
        let meta: [Option<(String, String)>; DECKS] =
            std::array::from_fn(|d| self.decks[d].track.as_ref().map(|t| (t.artist.clone(), t.title.clone())));
        let ms = self.started.elapsed().as_millis() as u64;
        let before = self.onair.entries.len();
        if !self.onair.observe(v, ms, 1000, meta) {
            return;
        }
        let new: Vec<usize> = self.onair.entries[before..].iter().map(|e| e.deck).collect();
        for d in new {
            let (Some(lt), Some(tr)) = (self.decks[d].lib.clone(), self.decks[d].track.clone()) else { continue };
            let rb = lt.rb.and_then(|(u, id)| {
                let ex = self.lib.usbs.get(u)?;
                Some((ex.track(id)?.file_path.clone(), ex.name.clone()))
            });
            self.lib.record_play(crate::library::history::HistEntry {
                time: crate::library::history::clock(),
                title: tr.title.clone(),
                artist: tr.artist.clone(),
                key: tr.key.clone(),
                bpm: tr.bpm().or(lt.bpm),
                duration: lt.duration.or(Some((tr.length() as f64 / tr.sample_rate.max(1) as f64).round() as u32)),
                path: lt.path.clone(),
                rb_path: rb.as_ref().map(|r| r.0.clone()),
                usb: rb.map(|r| r.1),
            });
        }
    }

    pub(crate) fn recording_tick(&mut self, v: &EngineView) {
        if !self.recorder.is_recording() {
            return;
        }
        let meta: [Option<(String, String)>; DECKS] =
            std::array::from_fn(|d| self.decks[d].track.as_ref().map(|t| (t.artist.clone(), t.title.clone())));
        if let Some(f) = self.recorder.tick(v, meta) {
            self.recording_finished(f);
            return;
        }
        if self.recorder.low_space() && !self.rec_low_warned {
            self.rec_low_warned = true;
            self.banner = Some(Banner::notice("Less than 1 GB free for the recording (about an hour left). It will stop safely before the disk fills."));
        }
        if self.rec_confirm.is_some_and(|t| t.elapsed() >= Duration::from_secs(3)) {
            self.rec_confirm = None;
        }
    }
}

/// Startup commands that restore saved toggles and levels.
pub fn restore_commands(cfg: &Config) -> Vec<Cmd> {
    let mut v = vec![Cmd::Quantize(cfg.quantize)];
    for d in 0..DECKS {
        let p = &cfg.decks[d];
        v.push(Cmd::Vinyl(d, p.vinyl));
        v.push(Cmd::MasterTempo(d, p.master_tempo));
        v.push(Cmd::TempoRange(d, p.tempo_range));
    }
    v.push(Cmd::Mix(MixParam::Master(cfg.master_level)));
    v.push(Cmd::Mix(MixParam::HeadMix(cfg.phones_mix)));
    v.push(Cmd::Mix(MixParam::HeadLevel(cfg.phones_level)));
    v.push(Cmd::QuantizeBeats(cfg.quantize_beats));
    v.push(Cmd::Mix(MixParam::FaderCurve(cfg.fader_curve)));
    v.push(Cmd::Mix(MixParam::CrossfaderCurve(cfg.crossfader_curve)));
    v.push(Cmd::Mix(MixParam::FxKind(cfg.fx.kind)));
    v.push(Cmd::Mix(MixParam::FxBeatIdx(cfg.fx.beat_idx)));
    v.push(Cmd::Mix(MixParam::FxTarget(cfg.fx.target)));
    v.push(Cmd::Mix(MixParam::FxLevel(cfg.fx.level)));
    v.push(Cmd::Mix(MixParam::FxQuantize(cfg.beat_fx_quantize)));
    v
}
