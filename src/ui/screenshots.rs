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
    ("Marco Vell", "Late Shift (Extended Mix)", 124.0, "8A", 156.0 * 240.0 / 124.0),
    ("Ruth Malone & Sid K", "Hold That Thought (Original Mix)", 126.0, "9A", 168.0 * 240.0 / 126.0),
    ("Okafor", "Rental Car (Dub)", 122.0, "7A", 388.0),
    ("Halfway House", "Backroom (Extended Mix)", 125.0, "8B", 341.0),
    ("Lena Brandt", "Slow Burner (Club Mix)", 120.0, "5A", 412.0),
    ("Pasko", "Concrete (Original Mix)", 128.0, "10A", 365.0),
    ("Marco Vell", "Cash Only (Extended Mix)", 124.0, "8A", 297.0),
    ("Tessa Ray", "Overtime (Original Mix)", 123.0, "6A", 356.0),
    ("Hollis Grant", "Sunday Service (Edit)", 118.0, "11B", 402.0),
    ("Ruth Malone", "Ground Floor (Extended Mix)", 127.0, "9B", 334.0),
    ("Kemba Nile", "Tight Spot (Original Mix)", 124.0, "12A", 371.0),
    ("Northside Social", "Last Orders (Extended Mix)", 126.0, "8A", 318.0),
    ("Okafor", "Delivery (Original Mix)", 121.0, "7B", 393.0),
    ("Jules Fern", "Bus Lane (Dub)", 130.0, "4A", 305.0),
    ("Halfway House", "Cold Brew (Extended Mix)", 122.0, "3A", 428.0),
    ("Sam Ellis feat. Nadia Ruiz", "Tell Me Twice (Vocal Mix)", 125.0, "9A", 347.0),
    ("Lena Brandt", "Afters (Original Mix)", 119.0, "2A", 381.0),
    ("Pasko", "Floor Two (Extended Mix)", 128.0, "10B", 359.0),
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

    let mut app = demo_app(music.clone(), store);
    for (i, &(artist, title, ..)) in TRACKS.iter().enumerate() {
        app.art.provide(crate::ui::cache::art::ArtKey::Embedded(music.join(format!("{artist} - {title}.wav"))), cover(i));
    }
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
    // The load it last measured varies from run to run.
    app.view.lock().dsp_load = 0.01;
    app.demo_audio = Some("DDJ-GRV6".into());
    app.lib.jump_to(Node::Folder);
    app.lib.select(2);
    app.scroll_to_selected = true;
    let tagged: Vec<_> = app.lib.local.clone().unwrap_or_default().into_iter().filter(|t| ["Rental Car (Dub)", "Last Orders (Extended Mix)", "Tell Me Twice (Vocal Mix)"].contains(&t.title.as_str())).collect();
    for t in &tagged {
        app.lib.toggle_tag(t);
    }
    // A past set in which Concrete followed Late Shift, for the suggestions.
    let past = |title: &str| {
        let t = app.lib.local.iter().flatten().find(|t| t.title == title).unwrap();
        crate::library::history::HistEntry { title: t.title.clone(), artist: t.artist.clone(), path: t.path.clone(), ..Default::default() }
    };
    let session = crate::library::history::Session { started: "2026-10-17 22-00".into(), entries: vec![past("Late Shift (Extended Mix)"), past("Concrete (Original Mix)")] };
    app.lib.handle(LibEvent::History(vec![session]));

    let options = SnapshotOptions::new().output_path(Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/screenshots"));
    let mut h = Harness::builder()
        .with_size(egui::vec2(1280.0, 800.0))
        .with_pixels_per_point(1.0)
        // Short steps keep each shot inside the first half second of the blink.
        .with_step_dt(1.0 / 60.0)
        .with_options(options)
        .wgpu()
        .build_eframe(move |cc| {
            theme::install(&cc.egui_ctx);
            app
        });

    fn perform(a: &mut App) {
        a.set_view(View::Perform);
        a.lib.jump_to(Node::Folder);
        a.lib.select(2);
        a.scroll_to_selected = true;
        a.config.perform_layout = PerformLayout::Full;
        *a.midi.lock() = None;
    }
    fn display(a: &mut App) {
        a.set_view(View::Perform);
        a.lib.jump_to(Node::Folder);
        a.lib.select(2);
        a.scroll_to_selected = true;
        a.config.perform_layout = PerformLayout::Display;
        let profiles = crate::midi::builtin();
        let grv6 = profiles.iter().find(|p| p.name == "AlphaTheta DDJ-GRV6");
        *a.midi.lock() = Some(crate::midi::Connected {
            port: "DDJ-GRV6".into(),
            profile: std::sync::Arc::new(crate::midi::resolve(&profiles, grv6, &Default::default()).0),
            known: true,
        });
    }
    fn browse(a: &mut App) {
        a.set_view(View::Browse);
        a.lib.jump_to(Node::Folder);
        a.lib.select(2);
        a.scroll_to_selected = true;
    }
    fn controller(a: &mut App) {
        a.config.glance = false;
        a.set_view(View::Settings);
        a.settings.page = 3;
        let profiles = crate::midi::builtin();
        let grv6 = profiles.iter().find(|p| p.name == "AlphaTheta DDJ-GRV6");
        let mut user = crate::midi::ControllerConfig::default();
        user.bind.insert("deck1.master_tempo".into(), crate::midi::Msgs::One("90 1A".into()));
        *a.midi.lock() = Some(crate::midi::Connected {
            port: "DDJ-GRV6:DDJ-GRV6 MIDI 1 20:0".into(),
            profile: std::sync::Arc::new(crate::midi::resolve(&profiles, grv6, &user).0),
            known: true,
        });
        a.settings.learning = Some(crate::midi::Control::Sync(0));
    }
    fn install(a: &mut App) {
        a.config.glance = false;
        let target = PathBuf::from("/home/deck/Applications/cdeckj");
        a.install = Some(crate::ui::views::install::Prompt::new(crate::platform::install::Offer::Install, target));
    }
    let shots: [(&str, fn(&mut App)); 13] = [
        ("perform", perform),
        ("display", display),
        ("wave", |a| a.set_view(View::Wave)),
        ("browse", browse),
        ("suggest", |a| {
            a.lib.jump_to(Node::Suggest);
            a.scroll_to_selected = true;
        }),
        ("settings", |a| a.set_view(View::Settings)),
        ("glance-settings", |a| a.config.glance = true),
        ("glance-perform", perform),
        ("glance-display", display),
        ("glance-wave", |a| a.set_view(View::Wave)),
        ("glance-browse", browse),
        ("controller", controller),
        ("install", install),
    ];
    let mut failed = Vec::new();
    for (name, setup) in shots {
        setup(h.state_mut());
        // Back to the start of the clock: paused decks blink CUE and PLAY at 1 Hz, and the
        // hover checks of earlier shots have run it on.
        h.input_mut().time = Some(0.0);
        h.run_steps(3);
        if let Err(e) = h.try_snapshot(name) {
            failed.push(e.to_string());
        }
        // The install screen has only its two buttons; every other screen has dozens.
        let min_controls = if name == "install" { 2 } else { 20 };
        failed.extend(hover_shifts(&mut h, min_controls).into_iter().map(|p| format!("{name}: {p}")));
    }
    let _ = std::fs::remove_dir_all(&root);
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

/// Hover the centre of every control in turn and report any control that moved or resized:
/// hover may change colours, never layout.
fn hover_shifts(h: &mut Harness<'_, App>, min_controls: usize) -> Vec<String> {
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
    assert!(base.len() >= min_controls, "only {} accessible nodes", base.len());
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
            midi_user: Default::default(),
            gamepad: None,
            locks: Default::default(),
            config,
            banner: None,
            install: None,
            check_for_update: false,
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

/// A made-up sleeve for track `i`: a flat colour and one bold shape, like a small label's
/// house design.
fn cover(i: usize) -> egui::ColorImage {
    type Rgb = [u8; 3];
    // (background, shape, detail)
    const PALETTES: [(Rgb, Rgb, Rgb); 8] = [
        ([0xE9, 0xE4, 0xD8], [0x1A, 0x1A, 0x1A], [0xD6, 0x45, 0x2B]),
        ([0x14, 0x2D, 0x4C], [0xF0, 0xC2, 0x4B], [0xE8, 0x6A, 0x3F]),
        ([0x2B, 0x2B, 0x2B], [0x9A, 0xD8, 0xCB], [0xF5, 0xF5, 0xF0]),
        ([0xC4, 0x4F, 0x37], [0xF2, 0xE6, 0xD5], [0x23, 0x1F, 0x20]),
        ([0x5E, 0x7D, 0x64], [0xF1, 0xEE, 0xDF], [0x1E, 0x2A, 0x22]),
        ([0xF2, 0xF0, 0xEB], [0x29, 0x4C, 0x9B], [0xE3, 0x3B, 0x5A]),
        ([0x31, 0x24, 0x5C], [0xFF, 0x8A, 0x5C], [0xF4, 0xE9, 0xD8]),
        ([0xF4, 0xD3, 0x5E], [0x0D, 0x3B, 0x66], [0xFA, 0xF0, 0xCA]),
    ];
    let (bg, fg, detail) = PALETTES[(i * 3) % PALETTES.len()];
    let motif = i % 6;
    let paint = |u: f32, v: f32| -> Rgb {
        let near = |cx: f32, cy: f32, r: f32| (u - cx).hypot(v - cy) < r;
        let r = (u - 0.5).hypot(v - 0.5);
        match motif {
            // A big disc off centre, a small one below.
            0 if near(0.62, 0.42, 0.3) => fg,
            0 if near(0.28, 0.76, 0.08) => detail,
            // Diagonal stripes with a corner block.
            1 if u < 0.3 && v < 0.3 => detail,
            1 if ((u + v) * 6.0) as i32 % 2 == 0 => fg,
            // A sun on the horizon.
            2 if v > 0.62 => fg,
            2 if near(0.5, 0.62, 0.22) => detail,
            // A grid of dots, one picked out.
            3 if near((u * 4.0).floor() / 4.0 + 0.125, (v * 4.0).floor() / 4.0 + 0.125, 0.07) => {
                if (u * 4.0) as i32 == 2 && (v * 4.0) as i32 == 1 { detail } else { fg }
            }
            // Rings round a centre spot.
            4 if r < 0.06 => detail,
            4 if r < 0.46 && (r * 14.0) as i32 % 2 == 0 => fg,
            // Blocks.
            5 if u < 0.5 => fg,
            5 if v > 0.5 => detail,
            _ => bg,
        }
    };
    let n = crate::ui::cache::art::SIZE as usize;
    let mut px = Vec::with_capacity(n * n * 4);
    for y in 0..n {
        for x in 0..n {
            // 3 × 3 samples a pixel smooth the edges.
            let mut sum = [0u32; 3];
            for s in 0..9 {
                let u = (x as f32 + (s % 3) as f32 / 3.0 + 1.0 / 6.0) / n as f32;
                let v = (y as f32 + (s / 3) as f32 / 3.0 + 1.0 / 6.0) / n as f32;
                for (acc, c) in sum.iter_mut().zip(paint(u, v)) {
                    *acc += c as u32;
                }
            }
            px.extend(sum.map(|c| (c / 9) as u8));
            px.push(255);
        }
    }
    egui::ColorImage::from_rgba_unmultiplied([n, n], &px)
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
