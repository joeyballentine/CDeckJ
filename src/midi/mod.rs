//! Controller I/O. The default mapping targets the AlphaTheta (Pioneer DJ) DDJ-GRV6, whose MIDI
//! layout follows the DDJ-400/FLX family: decks on MIDI channels 1-4, browser and global mixer
//! controls on channel 7, performance pads on channels 8-15.
//!
//! Every number can be overridden in the `[midi]` table of `config.toml`; run
//! `cdeckj --midi-monitor` to see what your unit sends.

mod device;
mod map;
mod mapper;

pub use device::*;
pub use map::*;
pub use mapper::*;

use std::collections::HashSet;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::engine::{Cmd, EngineView};

/// Things that belong to the UI thread (library browsing).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UiAction {
    Browse(i32),
    BrowsePress,
    Back,
    Load(usize),
    /// Start recording, or (pressed twice) stop.
    Record,
    /// Next (+1) or previous (−1) screen view.
    CycleView(i32),
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
    FxLevel,
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
            Ctl::FxLevel => m.fx.level,
        }
    }
}

/// Controls changed on screen since the hardware last matched them. The UI adds to this;
/// the MIDI thread ignores those hardware controls until they reach the on-screen value.
pub type PickupLocks = Arc<Mutex<HashSet<Ctl>>>;

/// Connection status for the UI: the connected port's name, if any.
pub type MidiStatus = Arc<Mutex<Option<String>>>;

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
        // GRV6 browse knob tilt: BACK and VIEW.
        assert_eq!(dec(&mut m, [0x96, 0x65, 0x7F]), "Some(Back)");
        assert_eq!(dec(&mut m, [0x96, 0x66, 0x7F]), "Some(Back)");
        assert_eq!(dec(&mut m, [0x96, 0x65, 0x00]), "None", "release does nothing");
        assert_eq!(dec(&mut m, [0x96, 0x7A, 0x7F]), "Some(CycleView(1))");
        assert_eq!(dec(&mut m, [0x96, 0x67, 0x7F]), "Some(CycleView(-1))");
        assert_eq!(dec(&mut m, [0xB6, 0x40, 0x7F]), "Some(Browse(-1))");
        assert_eq!(dec(&mut m, [0x90, 0x54, 0x7F]), "Some(Mix(PflToggle(0)))");
        assert_eq!(dec(&mut m, [0x90, 0x47, 0x7F]), "Some(Reverse(0, true))");
        assert_eq!(dec(&mut m, [0x94, 0x47, 0x7F]), "Some(Mix(FxToggle))");
        // Level meters go out as CC 0x02 on each deck's channel.
        let mut v = EngineView::new(48000, 2, crate::engine::CueRouting::MasterOnly);
        v.decks[0].peak = 1.0;
        v.decks[1].peak = 0.0;
        let leds = led_targets(&v, &MidiMap::default(), false);
        assert!(leds.contains(&(0xB0, 0x02, 127)));
        assert!(leds.contains(&(0xB1, 0x02, 0)));
        assert!((40..90).contains(&meter_value(0.1)), "-20 dB lights about half: {}", meter_value(0.1));
        assert_eq!(dec(&mut m, [0x94, 0x4B, 0x7F]), "Some(Mix(FxBeat(1)))");
        assert_eq!(dec(&mut m, [0x94, 0x11, 0x7F]), "Some(Mix(FxTarget(Ch(1))))");
        // GRV6 FX type knob: one note per position from 0x20.
        assert_eq!(dec(&mut m, [0x94, 0x20, 0x7F]), "Some(Mix(FxKind(Delay)))");
        assert_eq!(dec(&mut m, [0x94, 0x25, 0x7F]), "Some(Mix(FxKind(Reverb)))");
        assert_eq!(dec(&mut m, [0x94, 0x2B, 0x7F]), "Some(Mix(FxKind(Roll)))");
        assert_eq!(dec(&mut m, [0x94, 0x2E, 0x7F]), "None");
        // SHIFT + LOOP OUT halves the loop (0x77 on the GRV6).
        assert_eq!(dec(&mut m, [0x90, 0x77, 0x7F]), "Some(LoopScale(0, 0.5))");
        assert_eq!(dec(&mut m, [0xB4, 0x22, 0x7F]), format!("Some(Mix(FxLevel({:?})))", 127.0 / 16383.0f32));
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
