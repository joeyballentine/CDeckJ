//! Renders every screen with two loaded decks and a small library, and compares the images
//! with `docs/screenshots/*.png` (the README's screenshots). After a UI change, refresh them
//! with `UPDATE_SNAPSHOTS=1 cargo test screenshots`. Each screen is also checked for controls
//! that move when another is hovered.
//!
//! The tracks are synthesised here, so no audio files are checked in. Rendering needs a GPU
//! adapter; Mesa's lavapipe (`mesa-vulkan-drivers`) works headless.

use std::f64::consts::TAU;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crossbeam_channel::{bounded, unbounded};
use egui_kittest::{Harness, SnapshotOptions};
use parking_lot::Mutex;

use super::*;
use crate::audio::AudioOptions;
use crate::config::{PerformLayout, Record, Store};
use crate::engine::{CueRouting, Engine, MixParam};
use crate::library::Node;
use crate::track::{SavedCue, Tags};

const RATE: u32 = 44100;

/// (artist, title, BPM, key, seconds). The first two are synthesised in full and loaded.
const TRACKS: [(&str, &str, f64, &str, f64); 18] = [
    ("Kessler Drift", "Low Tide Signal", 124.0, "8A", 156.0 * 240.0 / 124.0),
    ("Ona Marrow", "Glasshouse", 126.0, "9A", 168.0 * 240.0 / 126.0),
    ("Halden Vale", "Night Ferry", 122.0, "7A", 388.0),
    ("Pell & Arkady", "Copper Wire", 125.0, "8B", 341.0),
    ("Mirelle Oduya", "Slow Orbit", 120.0, "5A", 412.0),
    ("Tamsin Roe", "Undertow", 128.0, "10A", 365.0),
    ("Kessler Drift", "Salt Flats", 124.0, "8A", 297.0),
    ("Brann Okoro", "Fieldwork", 123.0, "6A", 356.0),
    ("Lucent Hall", "Paper Lanterns", 118.0, "11B", 402.0),
    ("Ona Marrow", "Parallax", 127.0, "9B", 334.0),
    ("Ivo Sendra", "Marrowbone", 124.0, "12A", 371.0),
    ("The Quiet Ward", "Signal Box", 126.0, "8A", 318.0),
    ("Halden Vale", "Weathervane", 121.0, "7B", 393.0),
    ("Juno Castell", "Night Bus", 130.0, "4A", 305.0),
    ("Pell & Arkady", "Reed Organ", 122.0, "3A", 428.0),
    ("Sefa Lindqvist", "Coastline", 125.0, "9A", 347.0),
    ("Mirelle Oduya", "Afterimage", 119.0, "2A", 381.0),
    ("Tamsin Roe", "Bellwether", 128.0, "10B", 359.0),
];

#[test]
fn screenshots() {
    let root = crate::config::test_dir().join("screenshots");
    // Start without the tags, history and settings an earlier run saved.
    for dir in [root.clone(), crate::config::data_dir(), crate::config::config_dir()] {
        let _ = std::fs::remove_dir_all(dir);
    }
    let music = root.join("music");
    std::fs::create_dir_all(&music).unwrap();

    // Library: two real tracks, the rest one-second stand-ins whose tags are already cached.
    let mut store = Store::open(root.join("tracks.json"));
    let mut deck_tracks = Vec::new();
    for (i, &(artist, title, bpm, key, secs)) in TRACKS.iter().enumerate() {
        let path = music.join(format!("{artist} - {title}.wav"));
        if i < 2 {
            write_wav(&path, &synth(bpm, secs, [55.0, 61.74][i], i as u32 + 1));
        } else {
            write_wav(&path, &vec![0; RATE as usize]);
        }
        let tags = Tags { title: Some(title.into()), artist: Some(artist.into()), key: Some(key.into()), bpm: Some(bpm), duration: Some(secs) };
        store.update(&path, |r| {
            *r = Record { size: r.size, mtime: r.mtime, tags: Some(tags), grid: Some((bpm, 0.0)), ..Default::default() };
            if i < 2 {
                r.cues = Some(cues(bpm, i));
                r.cue_point = Some(bar(bpm, 16.0));
            }
        });
        if i < 2 {
            let lt = crate::library::LibTrack {
                path,
                title: title.into(),
                artist: artist.into(),
                key: key.into(),
                bpm: Some(bpm),
                duration: Some(secs as u32),
                ext: "wav".into(),
                rb: None,
            };
            deck_tracks.push(lt);
        }
    }

    let mut app = demo_app(music, store);
    for (d, lt) in deck_tracks.iter().enumerate() {
        app.load(d, lt);
    }
    wait(&mut app, "the decks and library to load", |app, v| {
        v.decks.iter().all(|d| d.loaded && d.complete) && app.lib.local.as_ref().is_some_and(|l| l.iter().all(|t| t.bpm.is_some()))
    });

    // Somewhere in the mix: deck 1 deep in its drop, deck 2 cued up and pitched to match.
    for c in [
        Cmd::Seek(0, 0.58),
        Cmd::Seek(1, 0.21),
        Cmd::TempoFader(1, -0.159),
        Cmd::MasterTempo(1, true),
        Cmd::Mix(MixParam::Fader(0, 0.9)),
        Cmd::Mix(MixParam::Fader(1, 0.55)),
        Cmd::Mix(MixParam::Crossfader(0.42)),
        Cmd::Mix(MixParam::Hi(1, 0.38)),
        Cmd::Mix(MixParam::Low(1, 0.2)),
        Cmd::Mix(MixParam::Filter(0, 0.62)),
    ] {
        app.send(c);
    }
    wait(&mut app, "the decks to seek", |_, v| (v.decks[0].pos / v.decks[0].length as f64 - 0.58).abs() < 0.01 && v.decks[1].rate < 1.0);
    // Freeze the engine so every run renders the same frame.
    app.silent = None;
    app.last_reconnect = Instant::now() + Duration::from_secs(3600);
    app.view.lock().routing = CueRouting::Channels34;
    app.demo_audio = Some("DDJ-GRV6".into());
    app.lib.jump_to(Node::Folder);
    app.lib.select(2);
    app.scroll_to_selected = true;
    let tagged: Vec<_> = app.lib.local.clone().unwrap_or_default().into_iter().filter(|t| ["Night Ferry", "Signal Box", "Coastline"].contains(&t.title.as_str())).collect();
    for t in &tagged {
        app.lib.toggle_tag(t);
    }

    let options = SnapshotOptions::new().output_path(Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/screenshots"));
    let mut h = Harness::builder()
        .with_size(egui::vec2(1280.0, 800.0))
        .with_pixels_per_point(1.0)
        // Short steps keep the whole run inside the first half second, so paused decks'
        // CUE and PLAY are lit in every shot (they blink at 1 Hz).
        .with_step_dt(1.0 / 60.0)
        .with_options(options)
        .wgpu()
        .build_eframe(move |cc| {
            theme::install(&cc.egui_ctx);
            app
        });

    let shots: [(&str, fn(&mut App)); 5] = [
        ("perform", |a| a.config.perform_layout = PerformLayout::Full),
        ("display", |a| {
            a.config.perform_layout = PerformLayout::Display;
            *a.midi.lock() = Some("DDJ-GRV6".into());
        }),
        ("wave", |a| a.set_view(View::Wave)),
        ("browse", |a| {
            a.set_view(View::Browse);
            a.lib.jump_to(Node::Folder);
            a.lib.select(2);
            a.scroll_to_selected = true;
        }),
        ("settings", |a| a.set_view(View::Settings)),
    ];
    let mut failed = Vec::new();
    for (name, setup) in shots {
        setup(h.state_mut());
        h.run_steps(3);
        if let Err(e) = h.try_snapshot(name) {
            failed.push(e.to_string());
        }
        failed.extend(hover_shifts(&mut h).into_iter().map(|p| format!("{name}: {p}")));
    }
    let _ = std::fs::remove_dir_all(&root);
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

/// Hover the centre of every control in turn and report any control that moved or resized:
/// hover may change colours, never layout.
fn hover_shifts(h: &mut Harness<'_, App>) -> Vec<String> {
    use egui_kittest::kittest::{By, NodeT as _, Queryable as _};
    let layout = |h: &Harness<'_, App>| {
        let ppp = h.ctx.pixels_per_point();
        let mut seen: std::collections::HashMap<String, usize> = Default::default();
        let mut out = std::collections::HashMap::new();
        for node in h.query_all(By::new().include_labels()) {
            let n = node.accesskit_node();
            let Some(b) = n.bounding_box() else { continue };
            let name = format!("{:?}|{}", n.role(), n.label().unwrap_or_default());
            let i = seen.entry(name.clone()).or_default();
            *i += 1;
            let r = egui::Rect::from_min_max(egui::pos2(b.x0 as f32, b.y0 as f32) / ppp, egui::pos2(b.x1 as f32, b.y1 as f32) / ppp);
            out.insert((name, *i), r);
        }
        out
    };
    let away = egui::pos2(-50.0, -50.0);
    h.hover_at(away);
    h.run_steps(2);
    let base = layout(h);
    assert!(base.len() > 20, "only {} accessible nodes", base.len());
    let mut targets: Vec<_> = base.iter().map(|(k, r)| (k.clone(), r.center())).collect();
    targets.sort_by(|a, b| a.0.cmp(&b.0));
    let mut problems = Vec::new();
    for (key, pos) in targets {
        h.hover_at(pos);
        h.run_steps(2);
        let now = layout(h);
        for (k, before) in &base {
            if let Some(after) = now.get(k) {
                if (after.min - before.min).length() > 0.01 || (after.size() - before.size()).length() > 0.01 {
                    problems.push(format!("hovering {:?} moved {:?}: {before:?} -> {after:?}", key.0, k.0));
                }
            }
        }
    }
    h.hover_at(away);
    h.run_steps(2);
    problems
}

/// An app with no sound card, controller or gamepad, reading `music`.
fn demo_app(music: PathBuf, store: Store) -> App {
    let (cmd_tx, cmd_rx) = bounded(4096);
    let (garbage_tx, garbage_rx) = bounded::<Arc<Track>>(8);
    std::thread::spawn(move || for t in garbage_rx { drop(t) });
    let view = Arc::new(Mutex::new(EngineView::new(48000, 2, CueRouting::MasterOnly)));
    let engine = Arc::new(Mutex::new(Engine::new(48000, 2, CueRouting::MasterOnly, cmd_rx, garbage_tx, view.clone())));
    let (_ui_tx, ui_rx) = unbounded();
    let config = Config { background_analysis: false, apply_usb_settings: false, fullscreen: false, ..Config::default() };
    App::new(
        &egui::Context::default(),
        AppInit {
            cmd: cmd_tx,
            view,
            engine,
            ui_rx,
            music_root: music,
            usb_paths: Vec::new(),
            audio: Err("no sound card in tests".into()),
            audio_opts: AudioOptions { device: None, force_alsa: false, buffer_frames: 256, split_cue: false, cue_profile_hints: None, direct_hints: None },
            midi: Default::default(),
            gamepad: None,
            locks: Default::default(),
            config,
            config_warning: None,
            store: Arc::new(Mutex::new(store)),
            fullscreen: false,
            preload: Vec::new(),
            keep_awake: None,
            keep: Vec::new(),
            tap: Default::default(),
        },
    )
}

/// Run the app's message handling (without drawing) until `done`, or fail after a minute.
fn wait(app: &mut App, what: &str, done: impl Fn(&App, &EngineView) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        app.poll();
        let v = *app.view.lock();
        if done(app, &v) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn bar(bpm: f64, n: f64) -> f64 {
    n * 240.0 / bpm
}

/// Hot cues at the sections of [`synth`]'s arrangement, and a memory cue at the drop.
fn cues(bpm: f64, deck: usize) -> Vec<SavedCue> {
    let hot = |slot: usize, b: f64, comment: &str| SavedCue { hot: Some(slot), at: bar(bpm, b), loop_end: None, color: None, comment: comment.into() };
    let mut v = vec![hot(0, 16.0, "Groove"), hot(1, 48.0, "Break"), hot(2, 64.0, "Drop"), hot(3, 96.0, "")];
    if deck == 0 {
        v.push(SavedCue { hot: Some(4), at: bar(bpm, 128.0), loop_end: Some(bar(bpm, 129.0)), color: None, comment: String::new() });
    }
    v.push(SavedCue { hot: None, at: bar(bpm, 64.0), loop_end: None, color: None, comment: String::new() });
    v
}

/// A house-ish track: 16 bars of drums, a groove with bass, a 16-bar breakdown, the drop,
/// and 16 bars of drums out. Kick (lows), bass (low mids), pad (mids) and hats (highs) give
/// each waveform colour mode something to show.
fn synth(bpm: f64, secs: f64, root: f64, seed: u32) -> Vec<i16> {
    let n = (secs * RATE as f64) as usize;
    let beat = 60.0 / bpm;
    let bars = secs / (4.0 * beat);
    let mut rng = seed.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
    let mut prev_noise = 0.0;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f64 / RATE as f64;
        let b = t / beat;
        let bar_i = b / 4.0;
        let in_beat = b.fract() * beat;
        let off = (b + 0.5).fract() * beat;
        let sixteenth = (b * 4.0).fract() * beat / 4.0;
        let (intro, brk, outro) = (bar_i < 16.0, (48.0..64.0).contains(&bar_i), bar_i >= bars - 16.0);
        let groove = !intro && !brk && !outro;
        let drop = bar_i >= 64.0 && !outro;

        rng = rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let noise = (rng >> 8) as f64 / (1u32 << 24) as f64 * 2.0 - 1.0;
        let hiss = noise - prev_noise;
        prev_noise = noise;

        let mut s = 0.0;
        if !brk {
            let phase = 45.0 * in_beat + 90.0 / 30.0 * (1.0 - (-30.0 * in_beat).exp());
            s += 0.9 * (TAU * phase).sin() * (-in_beat * 7.0).exp();
        }
        let hat_level = if brk { 0.04 } else { 0.1 };
        s += hat_level * hiss * (-off * 45.0).exp();
        if groove || drop {
            s += 0.05 * hiss * (-sixteenth * 90.0).exp();
            let env = (1.0 - (-off * 90.0).exp()) * (-off * 6.0).exp();
            let note = root * if (bar_i as i64) % 4 == 3 { 1.335 } else { 1.0 };
            s += 0.45 * env * ((TAU * note * t).sin() + 0.35 * (TAU * 2.0 * note * t).sin());
        }
        let pad_level = if brk { 0.22 * ((bar_i - 48.0) / 16.0).min(1.0) } else if drop { 0.07 } else { 0.0 };
        if pad_level > 0.0 {
            let swell = 0.75 + 0.25 * (TAU * t / (8.0 * beat)).sin();
            let chord: f64 = [4.0, 4.76, 5.99, 8.0].iter().map(|m| (TAU * root * m * t).sin()).sum();
            s += pad_level * swell * chord / 2.0;
        }
        out.push(((s * 0.6).tanh() * 30000.0) as i16);
    }
    out
}

fn write_wav(path: &Path, samples: &[i16]) {
    let data_len = (samples.len() * 2) as u32;
    let mut w = Vec::with_capacity(44 + data_len as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&1u16.to_le_bytes()); // mono
    w.extend_from_slice(&RATE.to_le_bytes());
    w.extend_from_slice(&(RATE * 2).to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        w.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, w).unwrap();
}
