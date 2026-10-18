//! Turning MIDI messages into deck, mixer and screen actions, with soft takeover for knobs and
//! faders that were last moved on screen.

use std::collections::HashMap;
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

/// Decoder: remembers 14-bit MSBs and soft-takeover state.
pub struct Mapper {
    pub map: MidiMap,
    msb: [[u8; 128]; 16],
    pickup: Pickup,
}

impl Default for Mapper {
    fn default() -> Self {
        Self::new(MidiMap::default(), PickupLocks::default())
    }
}

pub(crate) fn rel(v: u8) -> f64 {
    v as f64 - 64.0
}

impl Mapper {
    pub fn new(map: MidiMap, locks: PickupLocks) -> Self {
        Self { map, msb: [[0; 128]; 16], pickup: Pickup { hw: HashMap::new(), locks } }
    }
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
                    | MixParam::HeadLevel(x)
                    | MixParam::FxLevel(x) => x,
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

        // Beat FX section.
        if ch == m.fx_channel {
            use crate::engine::fx::FxTarget;
            if is_note {
                if !pressed {
                    return None;
                }
                let mx = |p: MixParam| e(Cmd::Mix(p));
                return match d1 {
                    n if n == m.fx_select => mx(MixParam::FxNext(1)),
                    n if n == m.fx_beat_left => mx(MixParam::FxBeat(-1)),
                    n if n == m.fx_beat_right => mx(MixParam::FxBeat(1)),
                    n if n == m.fx_ch1 => mx(MixParam::FxTarget(FxTarget::Ch(0))),
                    n if n == m.fx_ch2 => mx(MixParam::FxTarget(FxTarget::Ch(1))),
                    n if n == m.fx_master => mx(MixParam::FxTarget(FxTarget::Master)),
                    n if n == m.fx_on => mx(MixParam::FxToggle),
                    _ => None,
                };
            }
            if kind == 0xB0 && (d1 == m.cc_fx_level || d1 == m.cc_fx_level + 0x20) {
                let v = self.hires(ch, d1, d2, d1 != m.cc_fx_level);
                return ec(Cmd::Mix(MixParam::FxLevel(v)), Ctl::FxLevel);
            }
            return None;
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
