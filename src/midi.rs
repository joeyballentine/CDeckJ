//! Controller I/O. The default mapping targets the AlphaTheta (Pioneer DJ) DDJ-GRV6, whose MIDI
//! layout follows the DDJ-400/FLX family: decks on MIDI channels 1-4, browser and global mixer
//! controls on channel 7, performance pads on channels 8-15.
//!
//! Every number can be overridden in the `[midi]` table of `config.toml`; run
//! `platter --midi-monitor` to see what your unit sends.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use midir::{Ignore, MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::engine::{Cmd, EngineView, MixParam, SharedView, DECKS};

/// Note and CC numbers. Deck controls use status 0x90/0xB0 + deck channel (0..3),
/// global controls use channel `global_channel`, pads use `pad_status` + 2·deck (+1 = SHIFT).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MidiMap {
    pub port_hints: Vec<String>,
    pub invert_tempo: bool,
    /// Jog ticks per platter revolution (scratch sensitivity).
    pub jog_ticks_per_rev: f64,
    pub play: u8,
    pub shift_play: u8,
    pub cue: u8,
    pub shift_cue: u8,
    pub sync: u8,
    pub shift_sync: u8,
    pub shift: u8,
    pub jog_touch: u8,
    pub shift_jog_touch: u8,
    pub loop_in: u8,
    pub loop_out: u8,
    pub reloop: u8,
    pub shift_loop_in: u8,
    pub shift_loop_out: u8,
    pub shift_reloop: u8,
    pub pfl: u8,
    pub cc_tempo: u8,
    pub cc_trim: u8,
    pub cc_hi: u8,
    pub cc_mid: u8,
    pub cc_low: u8,
    pub cc_fader: u8,
    pub cc_jog_bend: u8,
    pub cc_jog_scratch: u8,
    pub cc_jog_bend_top: u8,
    pub cc_jog_search: Vec<u8>,
    pub global_channel: u8,
    pub browse_press: u8,
    pub shift_browse_press: u8,
    pub load_base: u8,
    pub cc_browse: u8,
    pub cc_crossfader: u8,
    pub cc_head_mix: u8,
    pub cc_head_level: u8,
    pub cc_color_fx_base: u8,
    pub pad_status: u8,
    pub pad_hot_cue: u8,
    pub pad_beat_jump: u8,
    pub pad_beat_loop: u8,
}

impl Default for MidiMap {
    fn default() -> Self {
        Self {
            port_hints: ["GRV6", "DDJ", "AlphaTheta", "Pioneer"].map(String::from).to_vec(),
            invert_tempo: false,
            jog_ticks_per_rev: 720.0,
            play: 0x0B,
            shift_play: 0x47,
            cue: 0x0C,
            shift_cue: 0x48,
            sync: 0x58,
            shift_sync: 0x5C,
            shift: 0x3F,
            jog_touch: 0x36,
            shift_jog_touch: 0x67,
            loop_in: 0x10,
            loop_out: 0x11,
            reloop: 0x4D,
            shift_loop_in: 0x4C,
            shift_loop_out: 0x4E,
            shift_reloop: 0x50,
            pfl: 0x54,
            cc_tempo: 0x00,
            cc_trim: 0x04,
            cc_hi: 0x07,
            cc_mid: 0x0B,
            cc_low: 0x0F,
            cc_fader: 0x13,
            cc_jog_bend: 0x21,
            cc_jog_scratch: 0x22,
            cc_jog_bend_top: 0x23,
            cc_jog_search: vec![0x26, 0x29],
            global_channel: 6,
            browse_press: 0x41,
            shift_browse_press: 0x42,
            load_base: 0x46,
            cc_browse: 0x40,
            cc_crossfader: 0x1F,
            cc_head_mix: 0x0C,
            cc_head_level: 0x0D,
            cc_color_fx_base: 0x17,
            pad_status: 0x97,
            pad_hot_cue: 0x00,
            pad_beat_jump: 0x20,
            pad_beat_loop: 0x60,
        }
    }
}

const BEAT_JUMPS: [f64; 8] = [-1.0, 1.0, -2.0, 2.0, -4.0, 4.0, -8.0, 8.0];
const BEAT_LOOPS: [f64; 8] = [0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0];

/// Things that belong to the UI thread (library browsing).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UiAction {
    Browse(i32),
    BrowsePress,
    Back,
    Load(usize),
}

pub enum Action {
    Engine(Cmd),
    Ui(UiAction),
}

impl std::fmt::Debug for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Action::Engine(c) => write!(f, "{c:?}"),
            Action::Ui(u) => write!(f, "{u:?}"),
        }
    }
}

/// An absolute control that can lose sync with its on-screen value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ctl {
    Tempo(usize),
    Trim(usize),
    Hi(usize),
    Mid(usize),
    Low(usize),
    Filter(usize),
    Fader(usize),
    Crossfader,
    HeadMix,
    HeadLevel,
}

impl Ctl {
    /// Current engine value in the hardware's 0..1 scale.
    fn current(self, v: &EngineView, invert_tempo: bool) -> f32 {
        let m = &v.mixer;
        match self {
            Ctl::Tempo(d) => {
                let f = v.decks[d].tempo_fader as f32;
                if invert_tempo { (1.0 - f) / 2.0 } else { (f + 1.0) / 2.0 }
            }
            Ctl::Trim(c) => m.ch[c].trim,
            Ctl::Hi(c) => m.ch[c].hi,
            Ctl::Mid(c) => m.ch[c].mid,
            Ctl::Low(c) => m.ch[c].low,
            Ctl::Filter(c) => m.ch[c].filter,
            Ctl::Fader(c) => m.ch[c].fader,
            Ctl::Crossfader => m.crossfader,
            Ctl::HeadMix => m.head_mix,
            Ctl::HeadLevel => m.head_level,
        }
    }
}

/// Controls changed on screen since the hardware last matched them. The UI adds to this;
/// the MIDI thread ignores those hardware controls until they reach the on-screen value.
pub type PickupLocks = Arc<Mutex<HashSet<Ctl>>>;

/// Soft takeover so nothing jumps when hardware and screen disagree.
pub struct Pickup {
    hw: HashMap<Ctl, f32>,
    locks: PickupLocks,
}

const PICKUP_EPS: f32 = 0.03;

impl Pickup {
    fn allow(&mut self, ctl: Ctl, value: f32, current: f32) -> bool {
        let prev = self.hw.insert(ctl, value);
        let mut locks = self.locks.lock();
        if !locks.contains(&ctl) {
            return true;
        }
        let near = (value - current).abs() < PICKUP_EPS;
        let crossed = prev.is_some_and(|p| (p - current).signum() != (value - current).signum());
        if near || crossed {
            locks.remove(&ctl);
            true
        } else {
            false
        }
    }
}

/// Decoder: remembers 14-bit MSBs and soft-takeover state.
pub struct Mapper {
    pub map: MidiMap,
    msb: [[u8; 128]; 16],
    pickup: Pickup,
}

impl Mapper {
    pub fn new(map: MidiMap, locks: PickupLocks) -> Self {
        Self { map, msb: [[0; 128]; 16], pickup: Pickup { hw: HashMap::new(), locks } }
    }
}

impl Default for Mapper {
    fn default() -> Self {
        Self::new(MidiMap::default(), PickupLocks::default())
    }
}

fn rel(v: u8) -> f64 {
    v as f64 - 64.0
}

impl Mapper {
    /// 14-bit value 0..=1 for a CC pair; called for both the MSB and LSB message.
    fn hires(&mut self, ch: u8, cc: u8, value: u8, is_lsb: bool) -> f32 {
        let msb_cc = if is_lsb { cc - 0x20 } else { cc };
        if !is_lsb {
            self.msb[ch as usize][msb_cc as usize] = value;
            return (value as u32 * 128) as f32 / 16383.0;
        }
        let m = self.msb[ch as usize][msb_cc as usize] as u32;
        ((m << 7) | value as u32) as f32 / 16383.0
    }

    /// Like [`Self::decode`], applying soft takeover against the engine's current values.
    pub fn decode_with(&mut self, msg: &[u8], view: Option<&EngineView>) -> Option<Action> {
        let (action, ctl) = self.decode_inner(msg)?;
        if let (Some(ctl), Some(v), Action::Engine(cmd)) = (ctl, view, &action) {
            let value = match cmd {
                Cmd::TempoFader(_, f) => {
                    let f = *f as f32;
                    if self.map.invert_tempo { (1.0 - f) / 2.0 } else { (f + 1.0) / 2.0 }
                }
                Cmd::Mix(p) => match *p {
                    MixParam::Trim(_, x)
                    | MixParam::Hi(_, x)
                    | MixParam::Mid(_, x)
                    | MixParam::Low(_, x)
                    | MixParam::Filter(_, x)
                    | MixParam::Fader(_, x)
                    | MixParam::Crossfader(x)
                    | MixParam::HeadMix(x)
                    | MixParam::HeadLevel(x) => x,
                    _ => return Some(action),
                },
                _ => return Some(action),
            };
            let current = ctl.current(v, self.map.invert_tempo);
            if !self.pickup.allow(ctl, value, current) {
                return None;
            }
        }
        Some(action)
    }

    pub fn decode(&mut self, msg: &[u8]) -> Option<Action> {
        self.decode_inner(msg).map(|(a, _)| a)
    }

    fn decode_inner(&mut self, msg: &[u8]) -> Option<(Action, Option<Ctl>)> {
        if msg.len() < 3 {
            return None;
        }
        let (status, d1, d2) = (msg[0], msg[1], msg[2]);
        let kind = status & 0xF0;
        let ch = status & 0x0F;
        let pressed = kind == 0x90 && d2 > 0;
        let is_note = kind == 0x90 || kind == 0x80;
        let m = self.map.clone();
        let e = |c: Cmd| Some((Action::Engine(c), None));
        let ec = |c: Cmd, ctl: Ctl| Some((Action::Engine(c), Some(ctl)));
        let u = |a: UiAction| Some((Action::Ui(a), None));

        // Performance pads.
        if is_note && (m.pad_status..m.pad_status.saturating_add(8)).contains(&status) {
            let idx = status - m.pad_status;
            let deck = (idx / 2) as usize % DECKS;
            let shift = idx % 2 == 1;
            let pad = (d1 & 0x07) as usize;
            let group = d1 & 0xF8;
            return if group == m.pad_hot_cue {
                if shift {
                    if pressed { e(Cmd::HotCueDelete(deck, pad)) } else { None }
                } else {
                    e(Cmd::HotCue(deck, pad, pressed))
                }
            } else if group == m.pad_beat_jump {
                if pressed { e(Cmd::BeatJump(deck, BEAT_JUMPS[pad])) } else { None }
            } else if group == m.pad_beat_loop {
                if pressed { e(Cmd::AutoLoop(deck, BEAT_LOOPS[pad])) } else { None }
            } else {
                None
            };
        }

        // Global section.
        if ch == m.global_channel {
            if is_note {
                if !pressed {
                    return None;
                }
                return match d1 {
                    n if n == m.browse_press => u(UiAction::BrowsePress),
                    n if n == m.shift_browse_press => u(UiAction::Back),
                    n if (m.load_base..m.load_base + 4).contains(&n) => u(UiAction::Load((n - m.load_base) as usize % DECKS)),
                    _ => None,
                };
            }
            if kind == 0xB0 {
                let lsb = d1 >= 0x20 && d1 < 0x40;
                let base = if lsb { d1 - 0x20 } else { d1 };
                if d1 == m.cc_browse {
                    return u(UiAction::Browse(if d2 < 64 { d2 as i32 } else { d2 as i32 - 128 }));
                }
                return match base {
                    c if c == m.cc_crossfader => {
                        let v = self.hires(ch, d1, d2, lsb);
                        ec(Cmd::Mix(MixParam::Crossfader(v)), Ctl::Crossfader)
                    }
                    c if c == m.cc_head_mix => {
                        let v = self.hires(ch, d1, d2, lsb);
                        ec(Cmd::Mix(MixParam::HeadMix(v)), Ctl::HeadMix)
                    }
                    c if c == m.cc_head_level => {
                        let v = self.hires(ch, d1, d2, lsb);
                        ec(Cmd::Mix(MixParam::HeadLevel(v)), Ctl::HeadLevel)
                    }
                    c if (m.cc_color_fx_base..m.cc_color_fx_base + 2).contains(&c) => {
                        let v = self.hires(ch, d1, d2, lsb);
                        let chan = (c - m.cc_color_fx_base) as usize;
                        ec(Cmd::Mix(MixParam::Filter(chan, v)), Ctl::Filter(chan))
                    }
                    _ => None,
                };
            }
            return None;
        }

        // Deck / mixer channels 1-4.
        if ch > 3 {
            return None;
        }
        let deck = ch as usize % DECKS;
        let mixer_ch = (ch < 2).then_some(ch as usize);

        if is_note {
            let on = |c: Cmd| if pressed { e(c) } else { None };
            return match d1 {
                n if n == m.play => on(Cmd::Play(deck)),
                n if n == m.shift_play => e(Cmd::Reverse(deck, pressed)),
                n if n == m.cue => e(Cmd::Cue(deck, pressed)),
                n if n == m.shift_cue => on(Cmd::ToStart(deck)),
                n if n == m.sync => on(Cmd::SyncToggle(deck)),
                n if n == m.shift_sync => on(Cmd::TempoRangeCycle(deck)),
                n if n == m.jog_touch || n == m.shift_jog_touch => e(Cmd::JogTouch(deck, pressed)),
                n if n == m.loop_in => on(Cmd::LoopIn(deck)),
                n if n == m.loop_out => on(Cmd::LoopOut(deck)),
                n if n == m.reloop => on(Cmd::ReloopExit(deck)),
                n if n == m.shift_loop_in => on(Cmd::AutoLoop(deck, 4.0)),
                n if n == m.shift_loop_out => on(Cmd::LoopScale(deck, 0.5)),
                n if n == m.shift_reloop => on(Cmd::LoopScale(deck, 2.0)),
                n if n == m.pfl => match (pressed, mixer_ch) {
                    (true, Some(c)) => e(Cmd::Mix(MixParam::PflToggle(c))),
                    _ => None,
                },
                _ => None,
            };
        }
        if kind != 0xB0 {
            return None;
        }
        // Scratch ticks scale so one platter turn is one 33⅓ rpm record turn.
        let tick = 720.0 / m.jog_ticks_per_rev.max(1.0);
        match d1 {
            c if c == m.cc_jog_scratch => e(Cmd::JogScratch(deck, rel(d2) * tick)),
            c if c == m.cc_jog_bend || c == m.cc_jog_bend_top => e(Cmd::JogBend(deck, rel(d2) * tick)),
            c if m.cc_jog_search.contains(&c) => e(Cmd::JogSearch(deck, rel(d2) * tick)),
            c if c == m.cc_tempo || c == m.cc_tempo + 0x20 => {
                let v = self.hires(ch, c, d2, c >= 0x20) as f64;
                let f = if m.invert_tempo { 1.0 - 2.0 * v } else { 2.0 * v - 1.0 };
                ec(Cmd::TempoFader(deck, f), Ctl::Tempo(deck))
            }
            c => {
                let mc = mixer_ch?;
                let is_lsb = (0x20..0x40).contains(&c);
                let base = if is_lsb { c - 0x20 } else { c };
                let (param, ctl): (fn(usize, f32) -> MixParam, Ctl) = match base {
                    b if b == m.cc_trim => (MixParam::Trim, Ctl::Trim(mc)),
                    b if b == m.cc_hi => (MixParam::Hi, Ctl::Hi(mc)),
                    b if b == m.cc_mid => (MixParam::Mid, Ctl::Mid(mc)),
                    b if b == m.cc_low => (MixParam::Low, Ctl::Low(mc)),
                    b if b == m.cc_fader => (MixParam::Fader, Ctl::Fader(mc)),
                    _ => return None,
                };
                let v = self.hires(ch, c, d2, is_lsb);
                ec(Cmd::Mix(param(mc, v)), ctl)
            }
        }
    }
}

fn port_matches(name: &str, wanted: Option<&str>, hints: &[String]) -> bool {
    let lname = name.to_lowercase();
    match wanted {
        Some(w) => lname.contains(&w.to_lowercase()),
        None => hints.iter().any(|h| lname.contains(&h.to_lowercase())),
    }
}

pub fn list_ports() {
    match MidiInput::new("platter-list") {
        Ok(mi) => {
            println!("MIDI inputs:");
            for p in mi.ports() {
                println!("  {}", mi.port_name(&p).unwrap_or_default());
            }
        }
        Err(e) => println!("MIDI unavailable: {e}"),
    }
}

/// Connection status for the UI: the connected port's name, if any.
pub type MidiStatus = Arc<Mutex<Option<String>>>;

struct Link {
    name: String,
    _input: MidiInputConnection<()>,
    output: Option<MidiOutputConnection>,
    leds: HashMap<(u8, u8), u8>,
}

fn try_connect(
    wanted: Option<&str>,
    map: &MidiMap,
    locks: &PickupLocks,
    engine_tx: &Sender<Cmd>,
    ui_tx: &Sender<UiAction>,
    repaint: &Arc<dyn Fn() + Send + Sync>,
    view: &SharedView,
) -> Option<Link> {
    let mut input = MidiInput::new("platter").ok()?;
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
            "platter-in",
            move |_, msg, _| {
                let snapshot = vw.try_lock().map(|g| *g);
                match mapper.decode_with(msg, snapshot.as_ref()) {
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
    let output = MidiOutput::new("platter-out").ok().and_then(|out| {
        let p = out
            .ports()
            .into_iter()
            .find(|p| out.port_name(p).map(|n| port_matches(&n, wanted, &map.port_hints)).unwrap_or(false))?;
        out.connect(&p, "platter-leds").ok()
    });
    Some(Link { name, _input: conn, output, leds: HashMap::new() })
}

fn port_present(name: &str) -> bool {
    MidiInput::new("platter-probe")
        .map(|mi| mi.ports().iter().any(|p| mi.port_name(p).is_ok_and(|n| n == name)))
        .unwrap_or(false)
}

fn led_targets(v: &EngineView, map: &MidiMap, blink: bool) -> Vec<(u8, u8, u8)> {
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
    let mut input = match MidiInput::new("platter-monitor") {
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
        "platter-monitor",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::CueRouting;

    fn dec(m: &mut Mapper, b: [u8; 3]) -> String {
        format!("{:?}", m.decode(&b))
    }

    #[test]
    fn decodes_core_controls() {
        let mut m = Mapper::default();
        assert_eq!(dec(&mut m, [0x90, 0x0B, 0x7F]), "Some(Play(0))");
        assert_eq!(dec(&mut m, [0x91, 0x0C, 0x00]), "Some(Cue(1, false))");
        assert_eq!(dec(&mut m, [0x92, 0x0B, 0x7F]), "Some(Play(0))"); // deck 3 folds onto deck 1
        assert_eq!(dec(&mut m, [0xB1, 0x22, 0x41]), "Some(JogScratch(1, 1.0))");
        assert_eq!(dec(&mut m, [0xB0, 0x21, 0x3E]), "Some(JogBend(0, -2.0))");
        assert_eq!(dec(&mut m, [0x97, 0x02, 0x7F]), "Some(HotCue(0, 2, true))");
        assert_eq!(dec(&mut m, [0x9A, 0x03, 0x7F]), "Some(HotCueDelete(1, 3))");
        assert_eq!(dec(&mut m, [0x96, 0x47, 0x7F]), "Some(Load(1))");
        assert_eq!(dec(&mut m, [0x96, 0x42, 0x7F]), "Some(Back)");
        assert_eq!(dec(&mut m, [0xB6, 0x40, 0x7F]), "Some(Browse(-1))");
        assert_eq!(dec(&mut m, [0x90, 0x54, 0x7F]), "Some(Mix(PflToggle(0)))");
        assert_eq!(dec(&mut m, [0x90, 0x47, 0x7F]), "Some(Reverse(0, true))");
        assert_eq!(dec(&mut m, [0xB0, 0x13, 0x7F]), format!("Some(Mix(Fader(0, {:?})))", 127.0 * 128.0 / 16383.0f32));
        assert_eq!(dec(&mut m, [0xB0, 0x33, 0x7F]), "Some(Mix(Fader(0, 1.0)))");
        assert_eq!(dec(&mut m, [0xB6, 0x17, 0x40]), format!("Some(Mix(Filter(0, {:?})))", 64.0 * 128.0 / 16383.0f32));
        m.decode(&[0xB1, 0x00, 0x40]);
        assert_eq!(dec(&mut m, [0xB1, 0x20, 0x00]), format!("Some(TempoFader(1, {:?}))", 2.0 * (8192.0f32 / 16383.0) as f64 - 1.0));
        assert_eq!(dec(&mut m, [0xB2, 0x13, 0x10]), "None");
    }

    #[test]
    fn overrides_change_the_mapping() {
        let map = MidiMap { play: 0x20, invert_tempo: true, ..Default::default() };
        let mut m = Mapper::new(map, PickupLocks::default());
        assert_eq!(dec(&mut m, [0x90, 0x20, 0x7F]), "Some(Play(0))");
        assert_eq!(dec(&mut m, [0x90, 0x0B, 0x7F]), "None");
    }

    #[test]
    fn soft_takeover_after_on_screen_change() {
        let locks = PickupLocks::default();
        let mut m = Mapper::new(MidiMap::default(), locks.clone());
        let mut v = EngineView::new(48000, 2, CueRouting::MasterOnly);
        v.mixer.ch[0].fader = 1.0;
        assert!(m.decode_with(&[0xB0, 0x13, 0x7F], Some(&v)).is_some(), "unlocked: passes");
        assert!(m.decode_with(&[0xB0, 0x33, 0x7F], Some(&v)).is_some());
        // The fader is dragged down to 0.3 on screen; the UI locks it.
        v.mixer.ch[0].fader = 0.3;
        locks.lock().insert(Ctl::Fader(0));
        assert!(m.decode_with(&[0xB0, 0x13, 0x7C], Some(&v)).is_none(), "would jump: ignored");
        assert!(m.decode_with(&[0xB0, 0x33, 0x00], Some(&v)).is_none());
        // Hardware brought down past 0.3: picks up, and stays picked up.
        assert!(m.decode_with(&[0xB0, 0x13, 0x20], Some(&v)).is_some(), "crossed: picks up");
        assert!(m.decode_with(&[0xB0, 0x33, 0x00], Some(&v)).is_some());
        assert!(locks.lock().is_empty());
    }
}
