//! Turning MIDI messages into deck, mixer and screen actions, with soft takeover for knobs and
//! faders that were last moved on screen.

use std::collections::HashMap;
use std::sync::Arc;

use crate::engine::fx::FxTarget;
use crate::engine::{Cmd, EngineView, MixParam, DECKS};

use super::*;

pub(crate) const BEAT_JUMPS: [f64; 8] = [-1.0, 1.0, -2.0, 2.0, -4.0, 4.0, -8.0, 8.0];

pub(crate) const BEAT_LOOPS: [f64; 8] = [0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0];

/// Soft takeover so nothing jumps when hardware and screen disagree.
pub struct Pickup {
    hw: HashMap<Ctl, f32>,
    locks: PickupLocks,
}

pub(crate) const PICKUP_EPS: f32 = 0.03;

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

/// Encoder ticks from a relative value. Controllers send either 64 ± n or two's complement
/// (n, or 128 − n backwards); the reading with the smaller magnitude is the right one for
/// anything under 32 ticks per message.
pub(crate) fn rel(v: u8) -> f64 {
    let offset = v as f64 - 64.0;
    let twos = if v < 64 { v as f64 } else { v as f64 - 128.0 };
    if twos.abs() < offset.abs() { twos } else { offset }
}

/// Decoder: remembers 14-bit MSBs, held buttons, SHIFT and jog touch, and soft-takeover state.
pub struct Mapper {
    pub profile: Arc<Resolved>,
    table: Table,
    msb: [[u8; 128]; 16],
    pickup: Pickup,
    shift: [bool; DECKS],
    touched: [bool; DECKS],
    /// Buttons down, so the release goes to the control the press went to.
    held: HashMap<(u8, u8), Control>,
}

impl Mapper {
    pub fn new(profile: Arc<Resolved>, locks: PickupLocks) -> Self {
        let table = Table::new(&profile);
        Self {
            profile,
            table,
            msb: [[0; 128]; 16],
            pickup: Pickup { hw: HashMap::new(), locks },
            shift: [false; DECKS],
            touched: [false; DECKS],
            held: HashMap::new(),
        }
    }

    pub fn set_profile(&mut self, profile: Arc<Resolved>) {
        self.table = Table::new(&profile);
        self.profile = profile;
        self.held.clear();
    }

    /// A software SHIFT button is down.
    pub fn shifted(&self) -> bool {
        self.shift.iter().any(|&s| s)
    }

    /// Like [`Self::decode`], applying soft takeover against the engine's current values.
    pub fn decode_with(&mut self, msg: &[u8], view: Option<&EngineView>) -> Option<Action> {
        let (action, pick) = self.decode_inner(msg)?;
        if let (Some((ctl, value)), Some(v)) = (pick, view) {
            let current = ctl.current(v, self.profile.invert_tempo);
            if !self.pickup.allow(ctl, value, current) {
                return None;
            }
        }
        Some(action)
    }

    pub fn decode(&mut self, msg: &[u8]) -> Option<Action> {
        self.decode_inner(msg).map(|(a, _)| a)
    }

    /// The action, and for knobs and faders the soft-takeover control with its 0..1 value.
    fn decode_inner(&mut self, msg: &[u8]) -> Option<(Action, Option<(Ctl, f32)>)> {
        let (&status, rest) = msg.split_first()?;
        let kind = status & 0xF0;
        let ch = status & 0x0F;
        let (d1, d2) = match (kind, rest) {
            (0xE0, &[lsb, msb, ..]) => (lsb, msb),
            (_, &[a, b, ..]) => (a, b),
            _ => return None,
        };
        let key = match kind {
            0x80 | 0x90 => (0x90 | ch, d1),
            0xB0 => (0xB0 | ch, d1),
            0xE0 => (0xE0 | ch, 0),
            _ => return None,
        };
        let is_note = kind == 0x80 || kind == 0x90;
        let pressed = match kind {
            0x90 | 0xB0 => d2 > 0,
            _ => false,
        };

        // A release goes wherever its press went, even if SHIFT changed in between.
        let held = if is_note || kind == 0xB0 {
            if pressed { None } else { self.held.remove(&key) }
        } else {
            None
        };
        let (control, bound) = match held {
            Some(c) => (c, self.table.by_key.get(&key).and_then(|v| v.iter().find(|(x, _)| *x == c)).map(|(_, b)| *b)?),
            None => {
                let entries = self.table.by_key.get(&key)?;
                let shift = self.shifted();
                let pick = entries.iter().find(|(_, b)| b.shift == shift).or_else(|| if shift { entries.iter().find(|(_, b)| !b.shift) } else { None });
                *pick?
            }
        };

        let e = |c: Cmd| Some((Action::Engine(c), None));
        let u = |a: UiAction| if pressed { Some((Action::Ui(a), None)) } else { None };
        let on = |c: Cmd| if pressed { e(c) } else { None };
        let mx = |p: MixParam| on(Cmd::Mix(p));

        match control.kind() {
            Kind::Output => None,
            Kind::Button => {
                if pressed {
                    self.held.insert(key, control);
                }
                use Control::*;
                match control {
                    Play(d) => on(Cmd::Play(d)),
                    Cue(d) => e(Cmd::Cue(d, pressed)),
                    Sync(d) => on(Cmd::SyncToggle(d)),
                    TempoRange(d) => on(Cmd::TempoRangeCycle(d)),
                    MasterTempo(d) => on(Cmd::MasterTempoToggle(d)),
                    Reverse(d) => e(Cmd::Reverse(d, pressed)),
                    ToStart(d) => on(Cmd::ToStart(d)),
                    Shift(d) => {
                        self.shift[d] = pressed;
                        None
                    }
                    JogTouch(d) => {
                        self.touched[d] = pressed;
                        e(Cmd::JogTouch(d, pressed))
                    }
                    LoopIn(d) => on(Cmd::LoopIn(d)),
                    LoopOut(d) => on(Cmd::LoopOut(d)),
                    Reloop(d) => on(Cmd::ReloopExit(d)),
                    Loop4(d) => on(Cmd::AutoLoop(d, 4.0)),
                    LoopHalf(d) => on(Cmd::LoopScale(d, 0.5)),
                    LoopDouble(d) => on(Cmd::LoopScale(d, 2.0)),
                    Load(d) => u(UiAction::Load(d)),
                    HotCue(d, i) => e(Cmd::HotCue(d, i, pressed)),
                    HotCueDelete(d, i) => on(Cmd::HotCueDelete(d, i)),
                    BeatJump(d, i) => on(Cmd::BeatJump(d, BEAT_JUMPS[i])),
                    BeatLoop(d, i) => on(Cmd::AutoLoop(d, BEAT_LOOPS[i])),
                    Pfl(c) => mx(MixParam::PflToggle(c)),
                    BrowsePress => u(UiAction::BrowsePress),
                    Back => u(UiAction::Back),
                    // VIEW: next screen, like a CDJ's screen buttons.
                    ViewNext => u(UiAction::CycleView(1)),
                    ViewPrev => u(UiAction::CycleView(-1)),
                    FxNext => mx(MixParam::FxNext(1)),
                    FxKind(k) => mx(MixParam::FxKind(k)),
                    FxBeatDown => mx(MixParam::FxBeat(-1)),
                    FxBeatUp => mx(MixParam::FxBeat(1)),
                    FxCh(c) => mx(MixParam::FxTarget(FxTarget::Ch(c))),
                    FxMaster => mx(MixParam::FxTarget(FxTarget::Master)),
                    FxOn => mx(MixParam::FxToggle),
                    _ => None,
                }
            }
            Kind::Relative => {
                if is_note {
                    return None;
                }
                let ticks = rel(d2);
                // Scratch ticks scale so one platter turn is one 33⅓ rpm record turn.
                let tick = 720.0 / self.profile.jog_ticks_per_rev.max(1.0);
                use Control::*;
                match control {
                    Jog(d) if self.touched[d] => e(Cmd::JogScratch(d, ticks * tick)),
                    Jog(d) | JogBend(d) => e(Cmd::JogBend(d, ticks * tick)),
                    JogScratch(d) => e(Cmd::JogScratch(d, ticks * tick)),
                    JogSearch(d) => e(Cmd::JogSearch(d, ticks * tick)),
                    Browse => Some((Action::Ui(UiAction::Browse(ticks as i32)), None)),
                    _ => None,
                }
            }
            Kind::Absolute => {
                let v = match bound.msg {
                    Msg::Cc(..) => d2 as f32 / 127.0,
                    Msg::Cc14(_, m, l) => {
                        if d1 == m {
                            self.msb[ch as usize][m as usize] = d2;
                            (d2 as u32 * 128) as f32 / 16383.0
                        } else {
                            debug_assert_eq!(d1, l);
                            let hi = self.msb[ch as usize][m as usize] as u32;
                            ((hi << 7) | d2 as u32) as f32 / 16383.0
                        }
                    }
                    Msg::PitchBend(_) => (((d2 as u32) << 7) | d1 as u32) as f32 / 16383.0,
                    Msg::Note(..) => return None,
                };
                let mix = |p: MixParam, ctl: Ctl| Some((Action::Engine(Cmd::Mix(p)), Some((ctl, v))));
                use Control::*;
                match control {
                    Tempo(d) => {
                        let f = if self.profile.invert_tempo { 1.0 - 2.0 * v as f64 } else { 2.0 * v as f64 - 1.0 };
                        Some((Action::Engine(Cmd::TempoFader(d, f)), Some((Ctl::Tempo(d), v))))
                    }
                    Trim(c) => mix(MixParam::Trim(c, v), Ctl::Trim(c)),
                    Hi(c) => mix(MixParam::Hi(c, v), Ctl::Hi(c)),
                    Mid(c) => mix(MixParam::Mid(c, v), Ctl::Mid(c)),
                    Low(c) => mix(MixParam::Low(c, v), Ctl::Low(c)),
                    Filter(c) => mix(MixParam::Filter(c, v), Ctl::Filter(c)),
                    Fader(c) => mix(MixParam::Fader(c, v), Ctl::Fader(c)),
                    Crossfader => mix(MixParam::Crossfader(v), Ctl::Crossfader),
                    HeadMix => mix(MixParam::HeadMix(v), Ctl::HeadMix),
                    HeadLevel => mix(MixParam::HeadLevel(v), Ctl::HeadLevel),
                    FxLevel => mix(MixParam::FxLevel(v), Ctl::FxLevel),
                    _ => None,
                }
            }
        }
    }
}
