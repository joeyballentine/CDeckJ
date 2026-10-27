//! CDeckJ — a minimal CDJ-style two-deck DJ player for the Steam Deck and a DDJ-GRV6.

mod audio;
mod config;
mod engine;
mod library;
mod midi;
mod platform;
mod recorder;
mod rekordbox;
mod track;
mod ui;

use std::any::Any;
use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::{bounded, unbounded};
use parking_lot::Mutex;

use config::{Config, Store};
use platform::{autolaunch, desktop};
use engine::{CueRouting, Engine, EngineView};

const HELP: &str = "\
cdeckj — CDJ-style DJ player

USAGE:
    cdeckj [OPTIONS] [TRACK1] [TRACK2]

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
    --install-autolaunch Start CDeckJ whenever the controller is plugged in (no root needed;
                         also does --install-desktop)
    --install-desktop    Add CDeckJ to the applications menu with its icon, and give its Steam
                         shortcut library artwork
    --uninstall-desktop  Take it out of the applications menu again
    --uninstall-autolaunch
                         Turn that off again
    --watch              The watcher that --install-autolaunch runs in the background
    -h, --help           Show this help

Settings live in ~/.config/cdeckj/config.toml; saved cues and analysis in
~/.local/share/cdeckj/tracks.json. Command-line options override settings for one run.
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
    watch: bool,
    install_autolaunch: bool,
    install_desktop: bool,
    uninstall_desktop: bool,
    uninstall_autolaunch: bool,
    /// Started by the in-app installer from the copy it just installed.
    installed: bool,
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
            "--watch" => a.watch = true,
            "--install-autolaunch" => a.install_autolaunch = true,
            "--install-desktop" => a.install_desktop = true,
            "--uninstall-desktop" => a.uninstall_desktop = true,
            "--uninstall-autolaunch" => a.uninstall_autolaunch = true,
            "--installed" => a.installed = true,
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
    config::migrate_old_dirs();
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
    if args.install_autolaunch || args.uninstall_autolaunch || args.install_desktop || args.uninstall_desktop {
        let r = if args.install_autolaunch {
            autolaunch::install()
        } else if args.uninstall_autolaunch {
            autolaunch::uninstall()
        } else if args.uninstall_desktop {
            desktop::uninstall()
        } else {
            std::env::current_exe().map_err(|e| e.to_string()).and_then(|exe| desktop::install(&exe))
        };
        match r {
            Ok(msg) => println!("{msg}"),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        return Ok(());
    }
    if args.watch {
        autolaunch::watch(&cfg.midi.port_hints);
    }
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
    let tap = Arc::new(recorder::Tap::default());
    engine.lock().set_tap(tap.clone());
    for c in ui::restore_commands(&cfg) {
        let _ = cmd_tx.send(c);
    }

    let audio_opts = audio::AudioOptions {
        device: args.audio.clone().or(cfg.audio_device.clone()),
        force_alsa: alsa,
        buffer_frames: args.buffer.unwrap_or(cfg.buffer_frames),
        split_cue: args.split_cue || cfg.split_cue,
        cue_profile_hints: cfg.cue_profile.then(|| cfg.midi.port_hints.clone()),
        direct_hints: cfg.direct_audio.then(|| cfg.midi.port_hints.clone()),
    };
    let audio = audio::start(&audio_opts, &engine);
    match &audio {
        Ok(a) => eprintln!("audio: {} — {} ch @ {} Hz ({:?})", a.description, a.channels, a.sample_rate, a.routing),
        Err(e) => eprintln!("audio: {e}"),
    }
    let store = Arc::new(Mutex::new(Store::open_default()));
    // Once CDeckJ is a Steam shortcut, give it library artwork (never replacing art you set).
    std::thread::spawn(|| {
        if let Ok(exe) = std::env::current_exe() {
            if desktop::steam_art(&exe) > 0 {
                eprintln!("steam: added library artwork for CDeckJ (shows after Steam restarts)");
            }
        }
    });

    // Opened from the download rather than the installed copy: offer to install it.
    let install_target = platform::install::target();
    let install = std::env::current_exe()
        .ok()
        .zip(install_target.clone())
        .and_then(|(exe, t)| platform::install::offer(&exe, &t).map(|o| (o, t)));
    let installed_note = args.installed.then_some(install_target).flatten().map(|t| platform::install::installed_note(&t));

    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("CDeckJ")
            .with_app_id("cdeckj")
            .with_icon(desktop::window_icon())
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([1100.0, 700.0])
            .with_fullscreen(fullscreen),
        ..Default::default()
    };
    eframe::run_native(
        "cdeckj",
        native,
        Box::new(move |cc| {
            ui::theme::install(&cc.egui_ctx);
            let (ui_tx, ui_rx) = unbounded();
            let ctx = cc.egui_ctx.clone();
            let repaint: Arc<dyn Fn() + Send + Sync> = Arc::new(move || ctx.request_repaint());
            let locks = midi::PickupLocks::default();
            let midi = midi::spawn(midi_port, cfg.midi.clone(), locks.clone(), cmd_tx.clone(), ui_tx.clone(), repaint.clone(), view.clone());
            let gamepad = cfg.gamepad.then(|| platform::spawn_gamepad(cmd_tx.clone(), ui_tx, repaint, view.clone()));
            let keep_awake = cfg.keep_awake.then(platform::keep_awake);
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
                    banner: cfg_warning.or(installed_note),
                    install,
                    store,
                    fullscreen,
                    preload: args.tracks,
                    keep_awake,
                    keep,
                    tap,
                },
            )))
        }),
    )
}
