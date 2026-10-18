//! Platter — a minimal CDJ-style two-deck DJ player for the Steam Deck and a DDJ-GRV6.

mod analysis;
mod audio;
mod engine;
mod library;
mod midi;
mod theme;
mod track;
mod ui;

use std::any::Any;
use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel::{bounded, unbounded};
use parking_lot::Mutex;

use engine::{CueRouting, Engine, EngineView};

const HELP: &str = "\
platter — CDJ-style DJ player

USAGE:
    platter [OPTIONS] [TRACK1] [TRACK2]

OPTIONS:
    --music <DIR>        Music folder to browse (default: ~/Music)
    --audio <NAME>       Output device (substring of name/id; default: the controller, else system default)
    --alsa               Use ALSA directly instead of PipeWire/PulseAudio
    --buffer <FRAMES>    Audio buffer size (default: 256)
    --split-cue          On a stereo device, send master to the left ear and cue to the right
    --midi <NAME>        MIDI port to use (default: first port matching GRV6/DDJ/Pioneer)
    --windowed           Don't start fullscreen
    --list               List audio devices and MIDI ports, then exit
    --midi-monitor       Print incoming MIDI messages and their mapping, then exit with Ctrl+C
    -h, --help           Show this help
";

struct Args {
    music: PathBuf,
    audio: Option<String>,
    alsa: bool,
    buffer: u32,
    split_cue: bool,
    midi: Option<String>,
    windowed: bool,
    list: bool,
    monitor: bool,
    tracks: Vec<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        music: library::default_root(),
        audio: None,
        alsa: false,
        buffer: 256,
        split_cue: false,
        midi: None,
        windowed: false,
        list: false,
        monitor: false,
        tracks: Vec::new(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut val = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--music" => a.music = PathBuf::from(val("--music")?),
            "--audio" => a.audio = Some(val("--audio")?),
            "--alsa" => a.alsa = true,
            "--buffer" => {
                a.buffer = val("--buffer")?.parse().map_err(|_| "--buffer needs a number".to_string())?
            }
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
    if args.list {
        audio::list_devices(args.alsa);
        midi::list_ports();
        return Ok(());
    }
    if args.monitor {
        midi::monitor(args.midi.as_deref());
        return Ok(());
    }

    let (cmd_tx, cmd_rx) = bounded::<engine::Cmd>(4096);
    let (garbage_tx, garbage_rx) = bounded::<Arc<track::Track>>(8);
    std::thread::Builder::new()
        .name("free".into())
        .spawn(move || for t in garbage_rx { drop(t) })
        .expect("spawn");
    let view = Arc::new(Mutex::new(EngineView::new(0, 2, CueRouting::MasterOnly)));

    let opts = audio::AudioOptions {
        device: args.audio.clone(),
        force_alsa: args.alsa,
        buffer_frames: args.buffer,
        split_cue: args.split_cue,
    };
    let audio = audio::start(&opts, |sr, ch, routing| {
        {
            let mut v = view.lock();
            v.sample_rate = sr;
            v.channels = ch;
            v.routing = routing;
        }
        Engine::new(sr, ch, routing, cmd_rx.clone(), garbage_tx.clone(), view.clone())
    });
    let mut keep: Vec<Box<dyn Any>> = Vec::new();
    let audio_status = match audio {
        Ok(a) => {
            eprintln!("audio: {} — {} ch @ {} Hz ({:?})", a.description, a.channels, a.sample_rate, a.routing);
            let s = a.description.clone();
            keep.push(Box::new(a));
            Ok(s)
        }
        Err(e) => {
            eprintln!("audio: {e}");
            Err(e)
        }
    };

    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Platter")
            .with_app_id("platter")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([1100.0, 700.0])
            .with_fullscreen(!args.windowed),
        ..Default::default()
    };
    eframe::run_native(
        "platter",
        native,
        Box::new(move |cc| {
            theme::install(&cc.egui_ctx);
            let (ui_tx, ui_rx) = unbounded();
            let ctx = cc.egui_ctx.clone();
            let handles = midi::connect(
                args.midi.as_deref(),
                cmd_tx.clone(),
                ui_tx,
                Arc::new(move || ctx.request_repaint()),
                view.clone(),
            );
            let midi_status = handles.port_name.clone();
            if let Some(n) = &midi_status {
                eprintln!("midi: {n}");
            }
            keep.push(Box::new(handles));
            Ok(Box::new(ui::App::new(
                &cc.egui_ctx,
                ui::AppInit {
                    cmd: cmd_tx,
                    view,
                    ui_rx,
                    music_root: args.music,
                    audio_status,
                    midi_status,
                    fullscreen: !args.windowed,
                    preload: args.tracks,
                    keep,
                },
            )))
        }),
    )
}
