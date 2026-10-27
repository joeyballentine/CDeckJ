//! Controller I/O. Each controller gets a profile (`profiles/*.toml`, picked by MIDI port
//! name) that binds CDeckJ's controls to its MIDI messages. `[controller.bind]` in
//! `config.toml` and MIDI learn in SETTINGS add to or replace those bindings; run
//! `cdeckj --midi-monitor` to see what your unit sends.

mod device;
mod learn;
mod map;
mod mapper;

pub use device::*;
pub use learn::*;
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

/// The connected controller.
#[derive(Clone, Debug)]
pub struct Connected {
    pub port: String,
    pub profile: Arc<Resolved>,
    /// A built-in profile matched the port.
    pub known: bool,
}

/// Connection status for the UI.
pub type MidiStatus = Arc<Mutex<Option<Connected>>>;

/// `[controller]` from the settings, with a counter the UI bumps after each change.
pub type MidiUser = Arc<Mutex<(u64, ControllerConfig)>>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::CueRouting;

    fn profile(name: &str, user: &ControllerConfig) -> Resolved {
        let profiles = builtin();
        let p = profiles.iter().find(|p| p.name == name).unwrap();
        let (r, errors) = resolve(&profiles, Some(p), user);
        assert!(errors.is_empty(), "{errors:?}");
        r
    }

    fn grv6() -> Mapper {
        Mapper::new(Arc::new(profile("AlphaTheta DDJ-GRV6", &ControllerConfig::default())), PickupLocks::default())
    }

    fn dec(m: &mut Mapper, b: [u8; 3]) -> String {
        format!("{:?}", m.decode(&b))
    }

    #[test]
    fn decodes_core_controls() {
        let mut m = grv6();
        assert_eq!(dec(&mut m, [0x90, 0x0B, 0x7F]), "Some(Play(0))");
        assert_eq!(dec(&mut m, [0x91, 0x0C, 0x00]), "Some(Cue(1, false))");
        assert_eq!(dec(&mut m, [0x80, 0x0C, 0x40]), "Some(Cue(0, false))", "note off");
        assert_eq!(dec(&mut m, [0xB1, 0x22, 0x41]), "Some(JogScratch(1, 1.0))");
        assert_eq!(dec(&mut m, [0xB0, 0x21, 0x3E]), "Some(JogBend(0, -2.0))");
        assert_eq!(dec(&mut m, [0x97, 0x02, 0x7F]), "Some(HotCue(0, 2, true))");
        assert_eq!(dec(&mut m, [0x9A, 0x03, 0x7F]), "Some(HotCueDelete(1, 3))");
        assert_eq!(dec(&mut m, [0x99, 0x21, 0x7F]), "Some(BeatJump(1, 1.0))");
        assert_eq!(dec(&mut m, [0x97, 0x64, 0x7F]), "Some(AutoLoop(0, 4.0))");
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
        assert_eq!(dec(&mut m, [0x90, 0x60, 0x7F]), "Some(TempoRangeCycle(0))");
        assert_eq!(dec(&mut m, [0x94, 0x47, 0x7F]), "Some(Mix(FxToggle))");
        // Level meters go out as CC 0x02 on each deck's channel.
        let mut v = EngineView::new(48000, 2, CueRouting::MasterOnly);
        v.decks[0].peak = 1.0;
        v.decks[1].peak = 0.0;
        let leds = led_targets(&v, &m.profile, false);
        assert!(leds.contains(&(0xB0, 0x02, 127)));
        assert!(leds.contains(&(0xB1, 0x02, 0)));
        assert!(leds.contains(&(0x90, 0x0B, 0)), "PLAY LED");
        assert!(leds.contains(&(0x99, 0x00, 0)), "hot cue pad LED");
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
        assert_eq!(dec(&mut m, [0x90, 0x4E, 0x7F]), "None");
        assert_eq!(dec(&mut m, [0xB4, 0x22, 0x7F]), format!("Some(Mix(FxLevel({:?})))", 127.0 / 16383.0f32));
        assert_eq!(dec(&mut m, [0xB0, 0x13, 0x7F]), format!("Some(Mix(Fader(0, {:?})))", 127.0 * 128.0 / 16383.0f32));
        assert_eq!(dec(&mut m, [0xB0, 0x33, 0x7F]), "Some(Mix(Fader(0, 1.0)))");
        assert_eq!(dec(&mut m, [0xB6, 0x17, 0x40]), format!("Some(Mix(Filter(0, {:?})))", 64.0 * 128.0 / 16383.0f32));
        m.decode(&[0xB1, 0x00, 0x40]);
        assert_eq!(dec(&mut m, [0xB1, 0x20, 0x00]), format!("Some(TempoFader(1, {:?}))", 2.0 * (8192.0f32 / 16383.0) as f64 - 1.0));
        assert_eq!(dec(&mut m, [0xB2, 0x13, 0x10]), "None", "no deck 3");
    }

    #[test]
    fn pioneer_family_profiles() {
        let profiles = builtin();
        let pick = |port: &str| profile_for_port(&profiles, port).map(|p| p.name.as_str());
        assert_eq!(pick("DDJ-GRV6:DDJ-GRV6 MIDI 1 20:0"), Some("AlphaTheta DDJ-GRV6"));
        assert_eq!(pick("DDJ-FLX4:DDJ-FLX4 MIDI 1 24:0"), Some("Pioneer DDJ-FLX4"));
        assert_eq!(pick("DDJ-400 MIDI 1"), Some("Pioneer DDJ-400"));
        assert_eq!(pick("DDJ-FLX2:DDJ-FLX2 MIDI 1"), Some("Pioneer DDJ-400"), "other DDJs get the shared layout");
        assert_eq!(pick("Midi Through:Midi Through Port-0 14:0"), None);
        let mut flx4 = Mapper::new(Arc::new(profile("Pioneer DDJ-FLX4", &ControllerConfig::default())), PickupLocks::default());
        assert_eq!(dec(&mut flx4, [0x90, 0x0E, 0x7F]), "Some(Reverse(0, true))");
        assert_eq!(dec(&mut flx4, [0x90, 0x47, 0x7F]), "Some(Reverse(0, true))");
        assert_eq!(dec(&mut flx4, [0x90, 0x4E, 0x7F]), "Some(LoopScale(0, 0.5))");
        assert_eq!(dec(&mut flx4, [0x94, 0x20, 0x7F]), "None", "no FX type knob");
    }

    #[test]
    fn user_bindings_replace_and_free_messages() {
        let mut user = ControllerConfig::default();
        user.bind.insert("deck1.play".into(), Msgs::One("90 20".into()));
        // 0x0C (CUE) moves to SYNC; CUE loses it.
        user.bind.insert("deck1.sync".into(), Msgs::One("90 0C".into()));
        user.bind.insert("deck2.play".into(), Msgs::One(String::new()));
        let r = profile("AlphaTheta DDJ-GRV6", &user);
        assert_eq!(r.user.len(), 3);
        let mut m = Mapper::new(Arc::new(r), PickupLocks::default());
        assert_eq!(dec(&mut m, [0x90, 0x20, 0x7F]), "Some(Play(0))");
        assert_eq!(dec(&mut m, [0x90, 0x0B, 0x7F]), "None");
        assert_eq!(dec(&mut m, [0x90, 0x0C, 0x7F]), "Some(SyncToggle(0))");
        assert_eq!(dec(&mut m, [0x90, 0x58, 0x7F]), "None");
        assert_eq!(dec(&mut m, [0x91, 0x0B, 0x7F]), "None", "unbound");
        let user = ControllerConfig { invert_tempo: Some(true), ..Default::default() };
        let mut m = Mapper::new(Arc::new(profile("AlphaTheta DDJ-GRV6", &user)), PickupLocks::default());
        m.decode(&[0xB0, 0x00, 0x00]);
        assert_eq!(dec(&mut m, [0xB0, 0x20, 0x00]), "Some(TempoFader(0, 1.0))");
    }

    #[test]
    fn software_shift_and_jog_touch() {
        let mut r = Resolved::empty("test");
        for (c, m) in [
            (Control::Shift(0), "90 20"),
            (Control::Play(0), "90 00"),
            (Control::Reverse(0), "shift 90 00"),
            (Control::Cue(0), "90 01"),
            (Control::JogTouch(0), "90 06"),
            (Control::Jog(0), "B0 06"),
            (Control::JogSearch(0), "shift B0 06"),
            (Control::Browse, "B0 1A"),
        ] {
            r.bind(c, &[m.parse().unwrap()]);
        }
        let mut m = Mapper::new(Arc::new(r), PickupLocks::default());
        // Two's complement encoders.
        assert_eq!(dec(&mut m, [0xB0, 0x06, 0x02]), "Some(JogBend(0, 2.0))");
        assert_eq!(dec(&mut m, [0xB0, 0x1A, 0x7F]), "Some(Browse(-1))");
        assert_eq!(dec(&mut m, [0x90, 0x06, 0x7F]), "Some(JogTouch(0, true))");
        assert_eq!(dec(&mut m, [0xB0, 0x06, 0x7E]), "Some(JogScratch(0, -2.0))");
        assert_eq!(dec(&mut m, [0x90, 0x06, 0x00]), "Some(JogTouch(0, false))");
        assert_eq!(dec(&mut m, [0x90, 0x00, 0x7F]), "Some(Play(0))");
        assert_eq!(dec(&mut m, [0x90, 0x20, 0x7F]), "None", "SHIFT down");
        assert_eq!(dec(&mut m, [0x90, 0x00, 0x7F]), "Some(Reverse(0, true))");
        assert_eq!(dec(&mut m, [0xB0, 0x06, 0x41]), "Some(JogSearch(0, 1.0))", "offset encoder");
        // No SHIFT binding: falls through to the plain one.
        assert_eq!(dec(&mut m, [0x90, 0x01, 0x7F]), "Some(Cue(0, true))");
        assert_eq!(dec(&mut m, [0x90, 0x20, 0x00]), "None", "SHIFT up");
        // The release goes to the control the press went to.
        assert_eq!(dec(&mut m, [0x90, 0x00, 0x00]), "Some(Reverse(0, false))");
        assert_eq!(dec(&mut m, [0x90, 0x01, 0x00]), "Some(Cue(0, false))");
        let leds = led_targets(&EngineView::new(48000, 2, CueRouting::MasterOnly), &m.profile, false);
        assert!(leds.contains(&(0x90, 0x00, 0)) && leds.len() == 2, "PLAY and CUE light, the SHIFT layer doesn't: {leds:?}");
    }

    #[test]
    fn leds_elsewhere_and_keepalive() {
        let r = profile("Reloop Beatmix MK2", &ControllerConfig::default());
        let mut v = EngineView::new(48000, 2, CueRouting::MasterOnly);
        v.decks[0].playing = true;
        v.decks[0].loaded = true;
        let leds = led_targets(&v, &r, false);
        assert!(leds.contains(&(0x91, 0x23, 0x7F)), "PLAY lights on 0x23: {leds:?}");
        assert!(!leds.iter().any(|&(s, n, _)| s == 0x91 && n == 0x24), "not on the PLAY button's note");
        let mut m = Mapper::new(Arc::new(r), PickupLocks::default());
        assert_eq!(dec(&mut m, [0x91, 0x24, 0x7F]), "Some(Play(0))", "the button is unchanged");
        assert_eq!(profile("Roland DJ-505", &ControllerConfig::default()).keepalive, vec![vec![0xBF, 0x64, 0x00]]);
    }

    #[test]
    fn every_profile_finds_its_port() {
        let profiles = builtin();
        for p in &profiles {
            for port in &p.ports {
                let name = format!("{port}:{port} MIDI 1 20:0");
                let got = profile_for_port(&profiles, &name).map(|q| q.name.as_str());
                assert_eq!(got, Some(p.name.as_str()), "{name}");
            }
        }
    }

    #[test]
    fn pitch_bend_and_seven_bit() {
        let mut r = Resolved::empty("test");
        r.bind(Control::Tempo(1), &["E1".parse().unwrap()]);
        r.bind(Control::Fader(0), &["B0 07".parse().unwrap()]);
        let mut m = Mapper::new(Arc::new(r), PickupLocks::default());
        assert_eq!(dec(&mut m, [0xE1, 0x7F, 0x7F]), "Some(TempoFader(1, 1.0))");
        assert_eq!(dec(&mut m, [0xB0, 0x07, 0x7F]), "Some(Mix(Fader(0, 1.0)))");
    }

    #[test]
    fn messages_parse_and_print() {
        for s in ["90 0B", "B6 17 37", "E3", "shift 91 0B", "BF 7F"] {
            assert_eq!(s.parse::<Bound>().unwrap().to_string(), s);
        }
        assert_eq!("80 0b".parse::<Bound>().unwrap().to_string(), "90 0B");
        for bad in ["", "90", "90 0B 0C", "C0 01", "90 80", "shift", "zz 01"] {
            assert!(bad.parse::<Bound>().is_err(), "{bad}");
        }
    }

    #[test]
    fn builtin_profiles_are_valid() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/midi/profiles");
        let mut files: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        files.sort();
        let mut listed: Vec<String> = BUILTIN.iter().map(|(f, _)| f.to_string()).collect();
        listed.sort();
        assert_eq!(files, listed, "every file in profiles/ is listed in BUILTIN");
        let profiles = builtin();
        assert_eq!(profiles.len(), BUILTIN.len(), "every profile parses");
        for p in &profiles {
            assert!(!p.name.is_empty() && !p.ports.is_empty(), "{}: name and ports", p.name);
            assert_eq!(profiles.iter().filter(|q| q.name == p.name).count(), 1, "{}: unique name", p.name);
            let (r, errors) = resolve(&profiles, Some(p), &ControllerConfig::default());
            assert!(errors.is_empty(), "{}: {errors:?}", p.name);
            // Each message drives one control (per SHIFT layer).
            let mut seen = HashSet::new();
            for (c, b) in &r.bindings {
                for k in b.msg.keys() {
                    assert!(seen.insert((k, b.shift)), "{}: {b} bound twice (again for {})", p.name, c.name());
                }
            }
            // A profile's own table shouldn't take messages from another of its own controls.
            let mut own = HashSet::new();
            for (name, msgs) in &p.bind {
                for s in msgs.list() {
                    let b: Bound = s.parse().unwrap();
                    for k in b.msg.keys() {
                        assert!(own.insert((k, b.shift)), "{}: {s} is listed twice (again for {name})", p.name);
                    }
                }
            }
        }
    }

    #[test]
    fn overrides_parse_from_settings() {
        let c: ControllerConfig = toml::from_str("profile = \"Pioneer DDJ-400\"\n[bind]\n\"deck1.play\" = \"90 20\"\n\"browser.back\" = [\"96 42\", \"96 65\"]\n").unwrap();
        assert_eq!(c.bind.len(), 2);
        let profiles = builtin();
        let (r, errors) = resolve_for(&profiles, None, &c);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(r.name, "Pioneer DDJ-400");
        assert_eq!(r.messages(Control::Back).len(), 2);
        let bad = ControllerConfig { bind: [("deck9.play".into(), Msgs::One("90 00".into()))].into(), ..Default::default() };
        assert_eq!(resolve_for(&profiles, None, &bad).1.len(), 1);
    }

    #[test]
    fn picks_ports() {
        let profiles = builtin();
        let names: Vec<String> = ["Midi Through:Midi Through Port-0 14:0", "Keystation 49:Keystation 49 MIDI 1 20:0", "DDJ-FLX4:DDJ-FLX4 MIDI 1 24:0"].map(String::from).to_vec();
        let (n, p) = pick_port(&names, None, &profiles).unwrap();
        assert!(n.starts_with("DDJ-FLX4") && p.is_some());
        let (n, p) = pick_port(&names, Some("keystation"), &profiles).unwrap();
        assert!(n.starts_with("Keystation") && p.is_none());
        let (n, _) = pick_port(&names[..2], None, &profiles).unwrap();
        assert!(n.starts_with("Keystation"), "an unknown controller beats nothing");
        assert!(pick_port(&names[..1], None, &profiles).is_none());
    }

    #[test]
    fn soft_takeover_after_on_screen_change() {
        let locks = PickupLocks::default();
        let mut m = Mapper::new(grv6().profile, locks.clone());
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
