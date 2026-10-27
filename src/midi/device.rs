//! The controller connection: finding and reconnecting the port, picking its profile, the input
//! thread, LEDs and level meters, MIDI learn, the controller test log and `--midi-monitor`.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use crossbeam_channel::Sender;
use midir::{Ignore, MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};
use parking_lot::Mutex;
use crate::engine::{Cmd, EngineView, SharedView};

use super::*;

/// Ports that are never a controller.
fn is_system_port(name: &str) -> bool {
    let l = name.to_lowercase();
    l.contains("midi through") || l.contains("through port")
}

/// The port to use and the built-in profile its name selects: the port named by `--midi`,
/// else the first with a profile, else the first that isn't a system port.
pub(crate) fn pick_port<'a>(names: &[String], wanted: Option<&str>, profiles: &'a [Profile]) -> Option<(String, Option<&'a Profile>)> {
    if let Some(w) = wanted {
        let w = w.to_lowercase();
        let n = names.iter().find(|n| n.to_lowercase().contains(&w))?;
        return Some((n.clone(), profile_for_port(profiles, n)));
    }
    if let Some((n, p)) = names.iter().find_map(|n| profile_for_port(profiles, n).map(|p| (n, p))) {
        return Some((n.clone(), Some(p)));
    }
    names.iter().find(|n| !is_system_port(n)).map(|n| (n.clone(), None))
}

/// The bindings for a port: `[controller] profile` if set, else the port's own profile.
pub fn resolve_for(profiles: &[Profile], port_profile: Option<&Profile>, user: &ControllerConfig) -> (Resolved, Vec<String>) {
    let forced = user.profile.as_deref().and_then(|n| profiles.iter().find(|p| p.name == n));
    let (r, mut errors) = resolve(profiles, forced.or(port_profile), user);
    if let (Some(n), None) = (&user.profile, forced) {
        errors.push(format!("[controller] profile \"{n}\" isn't a built-in profile"));
    }
    (r, errors)
}

fn input_names(input: &MidiInput) -> Vec<String> {
    input.ports().iter().filter_map(|p| input.port_name(p).ok()).collect()
}

pub fn list_ports() {
    match MidiInput::new("cdeckj-list") {
        Ok(mi) => {
            let profiles = builtin();
            println!("MIDI inputs:");
            for n in input_names(&mi) {
                match profile_for_port(&profiles, &n) {
                    Some(p) => println!("  {n}  ({})", p.name),
                    None => println!("  {n}"),
                }
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

/// A message caught while learning.
#[derive(Clone, Copy, Debug)]
pub struct Learned {
    pub msg: [u8; 3],
    /// A software SHIFT was held.
    pub shift: bool,
    pub at: Instant,
}

/// Some while MIDI learn is waiting: incoming messages are collected here and not acted on.
pub static LEARN: Mutex<Option<Vec<Learned>>> = parking_lot::const_mutex(None);

pub(crate) struct Link {
    name: String,
    /// A built-in profile matched the port.
    known: bool,
    mapper: Arc<Mutex<Mapper>>,
    _input: MidiInputConnection<()>,
    output: Option<MidiOutputConnection>,
    leds: HashMap<(u8, u8), u8>,
    /// When the profile's keep-alive messages last went out.
    keepalive: Instant,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn try_connect(
    wanted: Option<&str>,
    profiles: &[Profile],
    user: &ControllerConfig,
    locks: &PickupLocks,
    engine_tx: &Sender<Cmd>,
    ui_tx: &Sender<UiAction>,
    repaint: &Arc<dyn Fn() + Send + Sync>,
    view: &SharedView,
) -> Option<Link> {
    let mut input = MidiInput::new("cdeckj").ok()?;
    input.ignore(Ignore::All);
    let (name, port_profile) = pick_port(&input_names(&input), wanted, profiles)?;
    let port = input.ports().into_iter().find(|p| input.port_name(p).is_ok_and(|n| n == name))?;
    let (resolved, errors) = resolve_for(profiles, port_profile, user);
    for e in errors {
        eprintln!("midi: {e}");
    }
    let mapper = Arc::new(Mutex::new(Mapper::new(Arc::new(resolved), locks.clone())));
    let (m, etx, utx, rp, vw) = (mapper.clone(), engine_tx.clone(), ui_tx.clone(), repaint.clone(), view.clone());
    let conn = input
        .connect(
            &port,
            "cdeckj-in",
            move |_, msg, _| {
                let snapshot = vw.try_lock().map(|g| *g);
                let mut mapper = m.lock();
                let action = mapper.decode_with(msg, snapshot.as_ref());
                let shift = mapper.shifted();
                drop(mapper);
                if let Some(caught) = LEARN.lock().as_mut() {
                    if let &[a, b, c, ..] = msg {
                        caught.push(Learned { msg: [a, b, c], shift, at: Instant::now() });
                    }
                    rp();
                    return;
                }
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
    // The output port normally has the same name; else take one from the same device.
    let client = name.split(':').next().unwrap_or(&name).to_string();
    let output = MidiOutput::new("cdeckj-out").ok().and_then(|out| {
        let ports = out.ports();
        let p = ports
            .iter()
            .find(|p| out.port_name(p).is_ok_and(|n| n == name))
            .or_else(|| ports.iter().find(|p| out.port_name(p).is_ok_and(|n| n.split(':').next() == Some(client.as_str()))))?
            .clone();
        out.connect(&p, "cdeckj-leds").ok()
    });
    let mut link = Link { name, known: port_profile.is_some(), mapper, _input: conn, output, leds: HashMap::new(), keepalive: Instant::now() };
    if let Some(out) = link.output.as_mut() {
        for m in &link.mapper.lock().profile.init {
            let _ = out.send(m);
        }
    }
    Some(link)
}

/// Channel level (linear peak, pre-fader) → the controller's meter scale. −42 dB and below
/// is dark, 0 dB fills it, matching the on-screen meters.
pub fn meter_value(peak: f32) -> u8 {
    let db = 20.0 * peak.max(1e-6).log10();
    (((db + 42.0) / 42.0).clamp(0.0, 1.0) * 127.0).round() as u8
}

fn led_value(c: Control, v: &EngineView, r: &Resolved, blink: bool) -> Option<u8> {
    let lit = |b: bool| Some(if b { r.led_on } else { r.led_off });
    use Control::*;
    match c {
        Play(d) => {
            let dv = &v.decks[d];
            lit(dv.playing || (dv.loaded && !dv.playing && blink))
        }
        Cue(d) => {
            let dv = &v.decks[d];
            lit(if dv.playing || dv.at_cue { dv.loaded } else { dv.loaded && blink })
        }
        Sync(d) => lit(v.decks[d].synced),
        LoopIn(d) | LoopOut(d) | Reloop(d) => lit(v.decks[d].loop_active),
        HotCue(d, i) | HotCueDelete(d, i) => lit(v.decks[d].hot[i].is_some()),
        Pfl(c) => lit(v.mixer.ch[c].pfl),
        // BEAT FX ON blinks while on, like the hardware.
        FxOn => lit(v.mixer.fx.on && blink),
        Meter(d) => Some(meter_value(v.decks[d].peak)),
        _ => None,
    }
}

/// The LED and meter messages for the engine's state: (status, data 1, value). Buttons light
/// on the message they send, unless the profile's `[leds]` names another; `shift` bindings
/// don't drive LEDs.
pub(crate) fn led_targets(v: &EngineView, r: &Resolved, blink: bool) -> Vec<(u8, u8, u8)> {
    let mut want = Vec::with_capacity(r.bindings.len());
    let elsewhere: Vec<Control> = r.leds.iter().map(|(c, _)| *c).collect();
    for &(c, b) in r.bindings.iter().filter(|(c, _)| !elsewhere.contains(c)).chain(&r.leds) {
        if b.shift {
            continue;
        }
        let (st, d1) = match b.msg {
            Msg::Note(ch, n) => (0x90 | ch, n),
            Msg::Cc(ch, n) => (0xB0 | ch, n),
            _ => continue,
        };
        if let Some(val) = led_value(c, v, r, blink) {
            want.push((st, d1, val));
        }
    }
    want
}

/// Keep a controller connected: connect when it appears, drop and retry when it is
/// unplugged, apply new bindings from the settings, and drive its LEDs. Engine commands go
/// straight to the audio thread; browser actions go to the UI (with a repaint nudge).
pub fn spawn(
    wanted: Option<String>,
    user: MidiUser,
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
            let profiles = builtin();
            let mut link: Option<Link> = None;
            let mut last_check = Instant::now() - Duration::from_secs(10);
            let mut seen_gen = user.lock().0;
            let t0 = Instant::now();
            let publish = |l: &Link| {
                *st.lock() = Some(Connected { port: l.name.clone(), profile: l.mapper.lock().profile.clone(), known: l.known });
                repaint();
            };
            loop {
                if last_check.elapsed() > Duration::from_secs(2) {
                    last_check = Instant::now();
                    let names = MidiInput::new("cdeckj-probe").map(|mi| input_names(&mi)).unwrap_or_default();
                    // Unplugged, or a port without a profile while one with a profile is there.
                    let drop_it = link.as_ref().is_some_and(|l| {
                        !names.contains(&l.name) || (!l.known && wanted.is_none() && names.iter().any(|n| profile_for_port(&profiles, n).is_some()))
                    });
                    if drop_it {
                        link = None;
                        *st.lock() = None;
                        repaint();
                    }
                    // No ALSA sequencer (no MIDI at all): don't spam retries.
                    let seq = cfg!(not(target_os = "linux")) || std::path::Path::new("/dev/snd/seq").exists();
                    if link.is_none() && seq && !names.is_empty() {
                        let u = user.lock().1.clone();
                        link = try_connect(wanted.as_deref(), &profiles, &u, &locks, &engine_tx, &ui_tx, &repaint, &view);
                        if let Some(l) = &link {
                            eprintln!("midi: {} ({})", l.name, l.mapper.lock().profile.name);
                            publish(l);
                        }
                    }
                }
                let (g, u) = {
                    let g = user.lock();
                    (g.0, (g.0 != seen_gen).then(|| g.1.clone()))
                };
                if let Some(u) = u {
                    seen_gen = g;
                    if let Some(l) = link.as_mut() {
                        let port_profile = profile_for_port(&profiles, &l.name);
                        let (resolved, errors) = resolve_for(&profiles, port_profile, &u);
                        for e in errors {
                            eprintln!("midi: {e}");
                        }
                        l.mapper.lock().set_profile(Arc::new(resolved));
                        l.leds.clear();
                        publish(l);
                    }
                }
                if let Some(l) = link.as_mut() {
                    if let Some(out) = l.output.as_mut() {
                        let v = *view.lock();
                        let blink = (t0.elapsed().as_millis() / 250) % 2 == 0;
                        let profile = l.mapper.lock().profile.clone();
                        if !profile.keepalive.is_empty() && l.keepalive.elapsed() >= KEEPALIVE {
                            l.keepalive = Instant::now();
                            for m in &profile.keepalive {
                                let _ = out.send(m);
                            }
                        }
                        for (s, note, val) in led_targets(&v, &profile, blink) {
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
pub fn monitor(wanted: Option<&str>, user: &ControllerConfig) {
    let mut input = match MidiInput::new("cdeckj-monitor") {
        Ok(m) => m,
        Err(e) => {
            eprintln!("midi: {e}");
            return;
        }
    };
    input.ignore(Ignore::All);
    let profiles = builtin();
    let Some((name, port_profile)) = pick_port(&input_names(&input), wanted, &profiles) else {
        eprintln!("no MIDI input ports found");
        return;
    };
    let Some(port) = input.ports().into_iter().find(|p| input.port_name(p).is_ok_and(|n| n == name)) else { return };
    let (resolved, errors) = resolve_for(&profiles, port_profile, user);
    for e in errors {
        eprintln!("midi: {e}");
    }
    println!("listening on {name}, profile {} (Ctrl+C to quit)", resolved.name);
    let mut mapper = Mapper::new(Arc::new(resolved), PickupLocks::default());
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
