//! The controller connection: finding and reconnecting the port, the input thread, LEDs and
//! level meters, the controller test log and `--midi-monitor`.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use crossbeam_channel::Sender;
use midir::{Ignore, MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};
use parking_lot::Mutex;
use crate::engine::{Cmd, EngineView, SharedView, DECKS};

use super::*;

pub(crate) fn port_matches(name: &str, wanted: Option<&str>, hints: &[String]) -> bool {
    let lname = name.to_lowercase();
    match wanted {
        Some(w) => lname.contains(&w.to_lowercase()),
        None => hints.iter().any(|h| lname.contains(&h.to_lowercase())),
    }
}

pub fn list_ports() {
    match MidiInput::new("cdeckj-list") {
        Ok(mi) => {
            println!("MIDI inputs:");
            for p in mi.ports() {
                println!("  {}", mi.port_name(&p).unwrap_or_default());
            }
        }
        Err(e) => println!("MIDI unavailable: {e}"),
    }
}

/// While the SETTINGS view's controller test is open, incoming messages and what they
/// mapped to are kept here (newest last).
pub static MIDI_LOG_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub static MIDI_LOG: Mutex<Vec<String>> = parking_lot::const_mutex(Vec::new());

pub(crate) fn log_midi(msg: &[u8], action: &Option<Action>) {
    if !MIDI_LOG_ON.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let bytes: Vec<String> = msg.iter().map(|b| format!("{b:02X}")).collect();
    let what = match action {
        Some(Action::Engine(c)) => format!("{c:?}"),
        Some(Action::Ui(a)) => format!("{a:?}"),
        None => "not mapped (or held back by soft takeover)".into(),
    };
    let mut log = MIDI_LOG.lock();
    log.push(format!("{}  →  {what}", bytes.join(" ")));
    let n = log.len();
    if n > 40 {
        log.drain(..n - 40);
    }
}

pub(crate) struct Link {
    name: String,
    _input: MidiInputConnection<()>,
    output: Option<MidiOutputConnection>,
    leds: HashMap<(u8, u8), u8>,
}

pub(crate) fn try_connect(
    wanted: Option<&str>,
    map: &MidiMap,
    locks: &PickupLocks,
    engine_tx: &Sender<Cmd>,
    ui_tx: &Sender<UiAction>,
    repaint: &Arc<dyn Fn() + Send + Sync>,
    view: &SharedView,
) -> Option<Link> {
    let mut input = MidiInput::new("cdeckj").ok()?;
    input.ignore(Ignore::All);
    let port = input
        .ports()
        .into_iter()
        .find(|p| input.port_name(p).map(|n| port_matches(&n, wanted, &map.port_hints)).unwrap_or(false))?;
    let name = input.port_name(&port).unwrap_or_default();
    let mut mapper = Mapper::new(map.clone(), locks.clone());
    let (etx, utx, rp, vw) = (engine_tx.clone(), ui_tx.clone(), repaint.clone(), view.clone());
    let conn = input
        .connect(
            &port,
            "cdeckj-in",
            move |_, msg, _| {
                let snapshot = vw.try_lock().map(|g| *g);
                let action = mapper.decode_with(msg, snapshot.as_ref());
                log_midi(msg, &action);
                match action {
                    Some(Action::Engine(c)) => {
                        let _ = etx.try_send(c);
                        rp();
                    }
                    Some(Action::Ui(a)) => {
                        let _ = utx.try_send(a);
                        rp();
                    }
                    None => {}
                }
            },
            (),
        )
        .ok()?;
    let output = MidiOutput::new("cdeckj-out").ok().and_then(|out| {
        let p = out
            .ports()
            .into_iter()
            .find(|p| out.port_name(p).map(|n| port_matches(&n, wanted, &map.port_hints)).unwrap_or(false))?;
        out.connect(&p, "cdeckj-leds").ok()
    });
    Some(Link { name, _input: conn, output, leds: HashMap::new() })
}

pub(crate) fn port_present(name: &str) -> bool {
    MidiInput::new("cdeckj-probe")
        .map(|mi| mi.ports().iter().any(|p| mi.port_name(p).is_ok_and(|n| n == name)))
        .unwrap_or(false)
}

/// Channel level (linear peak, pre-fader) → the controller's meter scale. −42 dB and below
/// is dark, 0 dB fills it, matching the on-screen meters.
pub fn meter_value(peak: f32) -> u8 {
    let db = 20.0 * peak.max(1e-6).log10();
    (((db + 42.0) / 42.0).clamp(0.0, 1.0) * 127.0).round() as u8
}

pub(crate) fn led_targets(v: &EngineView, map: &MidiMap, blink: bool) -> Vec<(u8, u8, u8)> {
    let on = |b: bool| if b { 0x7F } else { 0x00 };
    let mut want = Vec::with_capacity(64);
    for d in 0..DECKS {
        let dv = &v.decks[d];
        let paused_loaded = dv.loaded && !dv.playing;
        let play = dv.playing || (paused_loaded && blink);
        let cue = if dv.playing || dv.at_cue { dv.loaded } else { paused_loaded && blink };
        for st in [0x90 + d as u8, 0x92 + d as u8] {
            want.push((st, map.play, on(play)));
            want.push((st, map.cue, on(cue)));
            want.push((st, map.sync, on(dv.synced)));
            want.push((st, map.loop_in, on(dv.loop_active)));
            want.push((st, map.loop_out, on(dv.loop_active)));
            want.push((st, map.reloop, on(dv.loop_active)));
        }
        want.push((0x90 + d as u8, map.pfl, on(v.mixer.ch[d].pfl)));
        want.push((0xB0 + d as u8, map.cc_level_meter, meter_value(dv.peak)));
        // BEAT FX ON blinks while on, like the hardware.
        if d == 0 {
            want.push((0x90 + map.fx_channel, map.fx_on, on(v.mixer.fx.on && blink)));
        }
        for s in [0, 1] {
            let st = map.pad_status + 2 * d as u8 + s;
            for i in 0..8u8 {
                want.push((st, map.pad_hot_cue + i, on(dv.hot[i as usize].is_some())));
            }
        }
    }
    want
}

/// Keep a controller connected: connect when it appears, drop and retry when it is
/// unplugged, and drive its LEDs. Engine commands go straight to the audio thread;
/// browser actions go to the UI (with a repaint nudge).
pub fn spawn(
    wanted: Option<String>,
    map: MidiMap,
    locks: PickupLocks,
    engine_tx: Sender<Cmd>,
    ui_tx: Sender<UiAction>,
    repaint: Arc<dyn Fn() + Send + Sync>,
    view: SharedView,
) -> MidiStatus {
    let status: MidiStatus = Arc::new(Mutex::new(None));
    let st = status.clone();
    thread::Builder::new()
        .name("midi".into())
        .spawn(move || {
            let mut link: Option<Link> = None;
            let mut last_check = Instant::now() - Duration::from_secs(10);
            let t0 = Instant::now();
            loop {
                if last_check.elapsed() > Duration::from_secs(2) {
                    last_check = Instant::now();
                    if link.as_ref().is_some_and(|l| !port_present(&l.name)) {
                        link = None;
                        *st.lock() = None;
                        repaint();
                    }
                    // No ALSA sequencer (no MIDI at all): don't spam retries.
                    let seq = cfg!(not(target_os = "linux")) || std::path::Path::new("/dev/snd/seq").exists();
                    if link.is_none() && seq {
                        link = try_connect(wanted.as_deref(), &map, &locks, &engine_tx, &ui_tx, &repaint, &view);
                        if let Some(l) = &link {
                            eprintln!("midi: {}", l.name);
                            *st.lock() = Some(l.name.clone());
                            repaint();
                        }
                    }
                }
                if let Some(l) = link.as_mut() {
                    if let Some(out) = l.output.as_mut() {
                        let v = *view.lock();
                        let blink = (t0.elapsed().as_millis() / 250) % 2 == 0;
                        for (s, note, val) in led_targets(&v, &map, blink) {
                            if l.leds.get(&(s, note)) != Some(&val) {
                                if out.send(&[s, note, val]).is_err() {
                                    break;
                                }
                                l.leds.insert((s, note), val);
                            }
                        }
                    }
                }
                thread::sleep(Duration::from_millis(30));
            }
        })
        .ok();
    status
}

/// `--midi-monitor`: print raw messages and what they map to, until Ctrl+C.
pub fn monitor(wanted: Option<&str>, map: MidiMap) {
    let mut input = match MidiInput::new("cdeckj-monitor") {
        Ok(m) => m,
        Err(e) => {
            eprintln!("midi: {e}");
            return;
        }
    };
    input.ignore(Ignore::All);
    let ports = input.ports();
    let port = ports
        .iter()
        .find(|p| input.port_name(p).map(|n| port_matches(&n, wanted, &map.port_hints)).unwrap_or(false))
        .or(ports.first());
    let Some(port) = port.cloned() else {
        eprintln!("no MIDI input ports found");
        return;
    };
    println!("listening on {} (Ctrl+C to quit)", input.port_name(&port).unwrap_or_default());
    let mut mapper = Mapper::new(map, PickupLocks::default());
    let _conn = input.connect(
        &port,
        "cdeckj-monitor",
        move |_, msg, _| {
            let hex: Vec<String> = msg.iter().map(|b| format!("{b:02X}")).collect();
            match mapper.decode(msg) {
                Some(a) => println!("{:<10} → {a:?}", hex.join(" ")),
                None => println!("{:<10} (unmapped)", hex.join(" ")),
            }
        },
        (),
    );
    loop {
        thread::sleep(Duration::from_secs(1));
    }
}
