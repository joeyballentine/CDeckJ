//! Platter — a minimal CDJ-style two-deck DJ player for the Steam Deck and a DDJ-GRV6.

mod analysis;
mod audio;
mod config;
mod engine;
mod library;
mod midi;
mod rekordbox;
mod system;
mod theme;
mod touch;
mod track;
mod ui;

use std::any::Any;
use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::{bounded, unbounded};
use parking_lot::Mutex;

use config::{Config, Store};
use engine::{CueRouting, Engine, EngineView};

const HELP: &str = "\
platter — CDJ-style DJ player

USAGE:
    platter [OPTIONS] [TRACK1] [TRACK2]

OPTIONS:
    --music <DIR>        Music folder to browse (default: ~/Music)
    --usb <DIR>          Also look for a rekordbox export here (repeatable; drives under
                         /run/media and /media are found automatically)
    --audio <NAME>       Output device (substring of name/id; default: the controller, else system default)
    --alsa               Use ALSA directly instead of PipeWire/PulseAudio
    --buffer <FRAMES>    Audio buffer size (default: 256)
    --split-cue          On a stereo device, send master to the left ear and cue to the right
    --midi <NAME>        MIDI port to use (default: first port matching GRV6/DDJ/Pioneer)
    --windowed           Don't start fullscreen
    --list               List audio devices and MIDI ports, then exit
    --midi-monitor       Print incoming MIDI messages and their mapping, then exit with Ctrl+C
    -h, --help           Show this help

Settings live in ~/.config/platter/config.toml; saved cues and analysis in
~/.local/share/platter/tracks.json. Command-line options override settings for one run.
";

#[derive(Default)]
struct Args {
    music: Option<PathBuf>,
    usb: Vec<PathBuf>,
    audio: Option<String>,
    alsa: bool,
    buffer: Option<u32>,
    split_cue: bool,
    midi: Option<String>,
    windowed: bool,
    list: bool,
    monitor: bool,
    tracks: Vec<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut val = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--music" => a.music = Some(PathBuf::from(val("--music")?)),
            "--usb" => a.usb.push(PathBuf::from(val("--usb")?)),
            "--audio" => a.audio = Some(val("--audio")?),
            "--alsa" => a.alsa = true,
            "--buffer" => a.buffer = Some(val("--buffer")?.parse().map_err(|_| "--buffer needs a number".to_string())?),
            "--split-cue" => a.split_cue = true,
            "--midi" => a.midi = Some(val("--midi")?),
            "--windowed" => a.windowed = true,
            "--list" => a.list = true,
            "--midi-monitor" => a.monitor = true,
            "-h" | "--help" => {
                print!("{HELP}");
                std::process::exit(0);
            }
            s if s.starts_with('-') => return Err(format!("unknown option {s}\n\n{HELP}")),
            s => a.tracks.push(PathBuf::from(s)),
        }
    }
    Ok(a)
}

fn main() -> eframe::Result {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let (cfg, cfg_warning) = Config::load();
    if let Some(w) = &cfg_warning {
        eprintln!("{w}");
    }
    let alsa = args.alsa || cfg.alsa;
    if args.list {
        audio::list_devices(alsa);
        midi::list_ports();
        return Ok(());
    }
    let midi_port = args.midi.clone().or(cfg.midi_port.clone());
    if args.monitor {
        midi::monitor(midi_port.as_deref(), cfg.midi.clone());
        return Ok(());
    }

    let music = args.music.clone().or(cfg.music_dir.clone()).unwrap_or_else(library::default_root);
    let mut usb = cfg.usb_paths.clone();
    usb.extend(args.usb.iter().cloned());
    let fullscreen = !args.windowed && cfg.fullscreen;

    let (cmd_tx, cmd_rx) = bounded::<engine::Cmd>(4096);
    let (garbage_tx, garbage_rx) = bounded::<Arc<track::Track>>(8);
    std::thread::Builder::new()
        .name("free".into())
        .spawn(move || for t in garbage_rx { drop(t) })
        .expect("spawn");
    let view = Arc::new(Mutex::new(EngineView::new(48000, 2, CueRouting::MasterOnly)));
    let engine = Arc::new(Mutex::new(Engine::new(48000, 2, CueRouting::MasterOnly, cmd_rx, garbage_tx, view.clone())));
    for c in ui::restore_commands(&cfg) {
        let _ = cmd_tx.send(c);
    }

    let audio_opts = audio::AudioOptions {
        device: args.audio.clone().or(cfg.audio_device.clone()),
        force_alsa: alsa,
        buffer_frames: args.buffer.unwrap_or(cfg.buffer_frames),
        split_cue: args.split_cue || cfg.split_cue,
    };
    let audio = audio::start(&audio_opts, &engine);
    match &audio {
        Ok(a) => eprintln!("audio: {} — {} ch @ {} Hz ({:?})", a.description, a.channels, a.sample_rate, a.routing),
        Err(e) => eprintln!("audio: {e}"),
    }
    let store = Arc::new(Mutex::new(Store::open_default()));

    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Platter")
            .with_app_id("platter")
            .with_icon(theme::icon())
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([1100.0, 700.0])
            .with_fullscreen(fullscreen),
        ..Default::default()
    };
    eframe::run_native(
        "platter",
        native,
        Box::new(move |cc| {
            theme::install(&cc.egui_ctx);
            let (ui_tx, ui_rx) = unbounded();
            let ctx = cc.egui_ctx.clone();
            let repaint: Arc<dyn Fn() + Send + Sync> = Arc::new(move || ctx.request_repaint());
            let locks = midi::PickupLocks::default();
            let midi = midi::spawn(midi_port, cfg.midi.clone(), locks.clone(), cmd_tx.clone(), ui_tx.clone(), repaint.clone(), view.clone());
            let gamepad = cfg.gamepad.then(|| system::spawn_gamepad(cmd_tx.clone(), ui_tx, repaint, view.clone()));
            let keep_awake = cfg.keep_awake.then(system::keep_awake);
            let keep: Vec<Box<dyn Any>> = Vec::new();
            Ok(Box::new(ui::App::new(
                &cc.egui_ctx,
                ui::AppInit {
                    cmd: cmd_tx,
                    view,
                    engine,
                    ui_rx,
                    music_root: music,
                    usb_paths: usb,
                    audio,
                    audio_opts,
                    midi,
                    gamepad,
                    locks,
                    config: cfg,
                    config_warning: cfg_warning,
                    store,
                    fullscreen,
                    preload: args.tracks,
                    keep_awake,
                    keep,
                },
            )))
        }),
    )
}
