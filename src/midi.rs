//! Controller I/O. The default mapping targets the AlphaTheta (Pioneer DJ) DDJ-GRV6, whose MIDI
//! layout follows the DDJ-400/FLX family: decks on MIDI channels 1-4, browser and global mixer
//! controls on channel 7, performance pads on channels 8-15.
//!
//! All numbers live in this file; run `platter --midi-monitor` to see what your unit sends.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use midir::{Ignore, MidiInput, MidiInputConnection, MidiOutput};

use crate::engine::{Cmd, MixParam, SharedView, DECKS};

/// Flip if the tempo fader runs backwards on your unit.
pub const INVERT_TEMPO_FADER: bool = false;
/// Port-name fragments used to auto-detect the controller.
pub const PORT_HINTS: &[&str] = &["GRV6", "DDJ", "AlphaTheta", "Pioneer"];

// Deck channel (0x90 + n notes, 0xB0 + n CCs), n = 0..3.
const N_PLAY: u8 = 0x0B;
const N_SHIFT_PLAY: u8 = 0x47;
const N_CUE: u8 = 0x0C;
const N_SHIFT_CUE: u8 = 0x48;
const N_SYNC: u8 = 0x58;
const N_SHIFT_SYNC: u8 = 0x5C;
const N_SHIFT: u8 = 0x3F;
const N_JOG_TOUCH: u8 = 0x36;
const N_SHIFT_JOG_TOUCH: u8 = 0x67;
const N_LOOP_IN: u8 = 0x10;
const N_LOOP_OUT: u8 = 0x11;
const N_RELOOP: u8 = 0x4D;
const N_SHIFT_LOOP_IN: u8 = 0x4C;
const N_SHIFT_LOOP_OUT: u8 = 0x4E;
const N_SHIFT_RELOOP: u8 = 0x50;
const N_PFL: u8 = 0x54;

const CC_TEMPO: u8 = 0x00; // + LSB 0x20
const CC_TRIM: u8 = 0x04;
const CC_HI: u8 = 0x07;
const CC_MID: u8 = 0x0B;
const CC_LOW: u8 = 0x0F;
const CC_FADER: u8 = 0x13;
const CC_JOG_BEND: u8 = 0x21; // side of platter
const CC_JOG_SCRATCH: u8 = 0x22; // top, vinyl mode
const CC_JOG_BEND_TOP: u8 = 0x23; // top, vinyl mode off
const CC_JOG_SEARCH: [u8; 2] = [0x26, 0x29]; // SHIFT + jog

// Global channel 7 (0x96 / 0xB6).
const GLOBAL_CH: u8 = 6;
const N_BROWSE_PRESS: u8 = 0x41;
const N_LOAD_BASE: u8 = 0x46; // +deck
const CC_BROWSE: u8 = 0x40;
const CC_CROSSFADER: u8 = 0x1F;
const CC_HEAD_MIX: u8 = 0x0C;
const CC_HEAD_LEVEL: u8 = 0x0D;
const CC_COLOR_FX_BASE: u8 = 0x17; // +channel

// Pads: status 0x97 + 2·deck (+1 with SHIFT).
const PAD_STATUS_BASE: u8 = 0x97;
const PAD_HOT_CUE: u8 = 0x00;
const PAD_BEAT_JUMP: u8 = 0x20;
const PAD_BEAT_LOOP: u8 = 0x60;
const BEAT_JUMPS: [f64; 8] = [-1.0, 1.0, -2.0, 2.0, -4.0, 4.0, -8.0, 8.0];
const BEAT_LOOPS: [f64; 8] = [0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0];

/// Things that belong to the UI thread (library browsing).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UiAction {
    Browse(i32),
    BrowsePress,
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

/// Stateless-ish decoder: remembers 14-bit MSBs and nothing else.
pub struct Mapper {
    msb: [[u8; 128]; 16],
}

impl Default for Mapper {
    fn default() -> Self {
        Self { msb: [[0; 128]; 16] }
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

    pub fn decode(&mut self, msg: &[u8]) -> Option<Action> {
        if msg.len() < 3 {
            return None;
        }
        let (status, d1, d2) = (msg[0], msg[1], msg[2]);
        let kind = status & 0xF0;
        let ch = status & 0x0F;
        let pressed = kind == 0x90 && d2 > 0;
        let is_note = kind == 0x90 || kind == 0x80;
        use Action::{Engine as E, Ui as U};

        // Performance pads.
        if is_note && (PAD_STATUS_BASE..PAD_STATUS_BASE + 8).contains(&status) {
            let idx = status - PAD_STATUS_BASE;
            let deck = (idx / 2) as usize % DECKS;
            let shift = idx % 2 == 1;
            let pad = (d1 & 0x07) as usize;
            return match d1 & 0xF8 {
                PAD_HOT_CUE if shift => pressed.then(|| E(Cmd::HotCueDelete(deck, pad))),
                PAD_HOT_CUE => Some(E(Cmd::HotCue(deck, pad, pressed))),
                PAD_BEAT_JUMP => pressed.then(|| E(Cmd::BeatJump(deck, BEAT_JUMPS[pad]))),
                PAD_BEAT_LOOP => pressed.then(|| E(Cmd::AutoLoop(deck, BEAT_LOOPS[pad]))),
                _ => None,
            };
        }

        // Global section.
        if ch == GLOBAL_CH {
            if is_note {
                if !pressed {
                    return None;
                }
                return match d1 {
                    N_BROWSE_PRESS => Some(U(UiAction::BrowsePress)),
                    n if (N_LOAD_BASE..N_LOAD_BASE + 4).contains(&n) => {
                        Some(U(UiAction::Load((n - N_LOAD_BASE) as usize % DECKS)))
                    }
                    _ => None,
                };
            }
            if kind == 0xB0 {
                return match d1 {
                    CC_BROWSE => Some(U(UiAction::Browse(if d2 < 64 { d2 as i32 } else { d2 as i32 - 128 }))),
                    CC_CROSSFADER | 0x3F => {
                        let v = self.hires(ch, d1, d2, d1 >= 0x20);
                        Some(E(Cmd::Mix(MixParam::Crossfader(v))))
                    }
                    CC_HEAD_MIX | 0x2C => {
                        let v = self.hires(ch, d1, d2, d1 >= 0x20);
                        Some(E(Cmd::Mix(MixParam::HeadMix(v))))
                    }
                    CC_HEAD_LEVEL | 0x2D => {
                        let v = self.hires(ch, d1, d2, d1 >= 0x20);
                        Some(E(Cmd::Mix(MixParam::HeadLevel(v))))
                    }
                    c if (CC_COLOR_FX_BASE..CC_COLOR_FX_BASE + 2).contains(&c)
                        || (CC_COLOR_FX_BASE + 0x20..CC_COLOR_FX_BASE + 0x22).contains(&c) =>
                    {
                        let is_lsb = c >= 0x20;
                        let v = self.hires(ch, c, d2, is_lsb);
                        let chan = ((c & 0x1F) - CC_COLOR_FX_BASE) as usize;
                        Some(E(Cmd::Mix(MixParam::Filter(chan, v))))
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
            return match d1 {
                N_PLAY | N_SHIFT_PLAY => pressed.then(|| E(Cmd::Play(deck))),
                N_CUE => Some(E(Cmd::Cue(deck, pressed))),
                N_SHIFT_CUE => pressed.then(|| E(Cmd::ToStart(deck))),
                N_SYNC => pressed.then(|| E(Cmd::SyncToggle(deck))),
                N_SHIFT_SYNC => pressed.then(|| E(Cmd::TempoRangeCycle(deck))),
                N_JOG_TOUCH | N_SHIFT_JOG_TOUCH => Some(E(Cmd::JogTouch(deck, pressed))),
                N_LOOP_IN => pressed.then(|| E(Cmd::LoopIn(deck))),
                N_LOOP_OUT => pressed.then(|| E(Cmd::LoopOut(deck))),
                N_RELOOP => pressed.then(|| E(Cmd::ReloopExit(deck))),
                N_SHIFT_LOOP_IN => pressed.then(|| E(Cmd::AutoLoop(deck, 4.0))),
                N_SHIFT_LOOP_OUT => pressed.then(|| E(Cmd::LoopScale(deck, 0.5))),
                N_SHIFT_RELOOP => pressed.then(|| E(Cmd::LoopScale(deck, 2.0))),
                N_PFL => match (pressed, mixer_ch) {
                    (true, Some(c)) => Some(E(Cmd::Mix(MixParam::PflToggle(c)))),
                    _ => None,
                },
                N_SHIFT => None,
                _ => None,
            };
        }
        if kind != 0xB0 {
            return None;
        }
        match d1 {
            CC_JOG_SCRATCH => Some(E(Cmd::JogScratch(deck, rel(d2)))),
            CC_JOG_BEND | CC_JOG_BEND_TOP => Some(E(Cmd::JogBend(deck, rel(d2)))),
            c if CC_JOG_SEARCH.contains(&c) => Some(E(Cmd::JogSearch(deck, rel(d2)))),
            c if c == CC_TEMPO || c == CC_TEMPO + 0x20 => {
                let v = self.hires(ch, c, d2, c >= 0x20) as f64;
                let f = if INVERT_TEMPO_FADER { 1.0 - 2.0 * v } else { 2.0 * v - 1.0 };
                Some(E(Cmd::TempoFader(deck, f)))
            }
            c => {
                let mc = mixer_ch?;
                let is_lsb = c >= 0x20;
                let base = if is_lsb { c - 0x20 } else { c };
                let param: fn(usize, f32) -> MixParam = match base {
                    CC_TRIM => MixParam::Trim,
                    CC_HI => MixParam::Hi,
                    CC_MID => MixParam::Mid,
                    CC_LOW => MixParam::Low,
                    CC_FADER => MixParam::Fader,
                    _ => return None,
                };
                let v = self.hires(ch, c, d2, is_lsb);
                Some(E(Cmd::Mix(param(mc, v))))
            }
        }
    }
}

pub struct MidiHandles {
    _input: Option<MidiInputConnection<()>>,
    pub port_name: Option<String>,
}

fn port_matches(name: &str, wanted: Option<&str>) -> bool {
    let lname = name.to_lowercase();
    match wanted {
        Some(w) => lname.contains(&w.to_lowercase()),
        None => PORT_HINTS.iter().any(|h| lname.contains(&h.to_lowercase())),
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

/// Connect to the controller. Engine commands go straight to the audio thread;
/// browser actions go to the UI (with a repaint nudge).
pub fn connect(
    wanted: Option<&str>,
    engine_tx: Sender<Cmd>,
    ui_tx: Sender<UiAction>,
    repaint: Arc<dyn Fn() + Send + Sync>,
    view: SharedView,
) -> MidiHandles {
    let mut input = match MidiInput::new("platter") {
        Ok(m) => m,
        Err(e) => {
            eprintln!("midi: {e}");
            return MidiHandles { _input: None, port_name: None };
        }
    };
    input.ignore(Ignore::All);
    let port = input
        .ports()
        .into_iter()
        .find(|p| input.port_name(p).map(|n| port_matches(&n, wanted)).unwrap_or(false));
    let Some(port) = port else {
        return MidiHandles { _input: None, port_name: None };
    };
    let name = input.port_name(&port).unwrap_or_default();
    let mut mapper = Mapper::default();
    let conn = input.connect(
        &port,
        "platter-in",
        move |_, msg, _| match mapper.decode(msg) {
            Some(Action::Engine(c)) => {
                let _ = engine_tx.try_send(c);
            }
            Some(Action::Ui(u)) => {
                let _ = ui_tx.try_send(u);
                repaint();
            }
            None => {}
        },
        (),
    );
    let conn = match conn {
        Ok(c) => c,
        Err(e) => {
            eprintln!("midi: could not open {name}: {e}");
            return MidiHandles { _input: None, port_name: None };
        }
    };
    start_leds(wanted, view);
    MidiHandles { _input: Some(conn), port_name: Some(name) }
}

/// Drive button/pad LEDs from the engine snapshot (only sending changes).
fn start_leds(wanted: Option<&str>, view: SharedView) {
    let Ok(out) = MidiOutput::new("platter-out") else { return };
    let Some(port) = out
        .ports()
        .into_iter()
        .find(|p| out.port_name(p).map(|n| port_matches(&n, wanted)).unwrap_or(false))
    else {
        return;
    };
    let Ok(mut conn) = out.connect(&port, "platter-leds") else { return };
    thread::Builder::new()
        .name("leds".into())
        .spawn(move || {
            let mut last = std::collections::HashMap::<(u8, u8), u8>::new();
            let t0 = Instant::now();
            loop {
                let v = *view.lock();
                let blink = (t0.elapsed().as_millis() / 250) % 2 == 0;
                let mut want: Vec<(u8, u8, u8)> = Vec::with_capacity(64);
                let on = |b: bool| if b { 0x7F } else { 0x00 };
                for d in 0..DECKS {
                    let dv = &v.decks[d];
                    let paused_loaded = dv.loaded && !dv.playing;
                    let play = if dv.playing { true } else { paused_loaded && blink };
                    let cue = if dv.playing || dv.at_cue { dv.loaded } else { paused_loaded && blink };
                    for st in [0x90 + d as u8, 0x92 + d as u8] {
                        want.push((st, N_PLAY, on(play)));
                        want.push((st, N_CUE, on(cue)));
                        want.push((st, N_SYNC, on(dv.synced)));
                        want.push((st, N_LOOP_IN, on(dv.loop_active)));
                        want.push((st, N_LOOP_OUT, on(dv.loop_active)));
                        want.push((st, N_RELOOP, on(dv.loop_active)));
                    }
                    want.push((0x90 + d as u8, N_PFL, on(v.mixer.ch[d].pfl)));
                    for s in [0, 1] {
                        let st = PAD_STATUS_BASE + 2 * d as u8 + s;
                        for i in 0..8u8 {
                            want.push((st, PAD_HOT_CUE + i, on(dv.hot[i as usize].is_some())));
                        }
                    }
                }
                for (st, note, val) in want {
                    if last.get(&(st, note)) != Some(&val) {
                        if conn.send(&[st, note, val]).is_err() {
                            return;
                        }
                        last.insert((st, note), val);
                    }
                }
                thread::sleep(Duration::from_millis(30));
            }
        })
        .ok();
}

/// `--midi-monitor`: print raw messages and what they map to, until Ctrl+C.
pub fn monitor(wanted: Option<&str>) {
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
        .find(|p| input.port_name(p).map(|n| port_matches(&n, wanted)).unwrap_or(false))
        .or(ports.first());
    let Some(port) = port.cloned() else {
        eprintln!("no MIDI input ports found");
        return;
    };
    println!("listening on {} (Ctrl+C to quit)", input.port_name(&port).unwrap_or_default());
    let mut mapper = Mapper::default();
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
        assert_eq!(dec(&mut m, [0xB6, 0x40, 0x7F]), "Some(Browse(-1))");
        assert_eq!(dec(&mut m, [0x90, 0x54, 0x7F]), "Some(Mix(PflToggle(0)))");
        // 14-bit fader: MSB then LSB.
        assert_eq!(dec(&mut m, [0xB0, 0x13, 0x7F]), format!("Some(Mix(Fader(0, {:?})))", 127.0 * 128.0 / 16383.0f32));
        assert_eq!(dec(&mut m, [0xB0, 0x33, 0x7F]), "Some(Mix(Fader(0, 1.0)))");
        // Tempo centre.
        m.decode(&[0xB1, 0x00, 0x40]);
        assert_eq!(dec(&mut m, [0xB1, 0x20, 0x00]), format!("Some(TempoFader(1, {:?}))", 2.0 * (8192.0f32 / 16383.0) as f64 - 1.0));
        // Mixer channels 3/4 are ignored.
        assert_eq!(dec(&mut m, [0xB2, 0x13, 0x10]), "None");
    }
}
