//! Which MIDI message is which control.
//!
//! A controller profile is a TOML table (see `profiles/`) that binds control names to MIDI
//! messages:
//!
//! ```toml
//! name = "Pioneer DDJ-400"
//! ports = ["DDJ-400"]          # MIDI port name substrings that select this profile
//! extends = "Pioneer DDJ-FLX4" # optional: start from another profile's bindings
//! jog_ticks_per_rev = 720      # jog wheel resolution
//! invert_tempo = false         # tempo fader: true when the top sends the highest value
//! init = ["B0 7F 7F"]          # optional: raw messages sent on connect
//! [bind]
//! "deck1.play" = "90 0B"                # note 0x0B on MIDI channel 1
//! "deck1.fader" = "B0 13 33"            # 14-bit CC: MSB 0x13, LSB 0x33
//! "deck1.tempo" = "E0"                  # pitch bend on channel 1
//! "browser.back" = ["96 42", "96 65"]   # several messages
//! "deck1.reverse" = "shift 90 0B"       # only while a `deckN.shift` button is held
//! ```
//!
//! Messages are written as `--midi-monitor` prints them: status byte, then data byte(s), in hex.
//! `[controller.bind]` in the settings file uses the same format and is applied last: binding a
//! control replaces that control's messages and frees the messages from any other control.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::engine::fx::{FxKind, FX_KINDS};
use crate::engine::DECKS;

pub const PADS: usize = 8;

/// Everything a controller can drive. Deck and mixer channel indices are 0-based.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Control {
    Play(usize),
    Cue(usize),
    Sync(usize),
    TempoRange(usize),
    MasterTempo(usize),
    Reverse(usize),
    ToStart(usize),
    /// A SHIFT button handled in software: while held, `shift` bindings take over.
    Shift(usize),
    JogTouch(usize),
    /// Jog turn that scratches while the platter is touched and bends otherwise.
    Jog(usize),
    JogScratch(usize),
    JogBend(usize),
    JogSearch(usize),
    Tempo(usize),
    LoopIn(usize),
    LoopOut(usize),
    Reloop(usize),
    Loop4(usize),
    LoopHalf(usize),
    LoopDouble(usize),
    Load(usize),
    HotCue(usize, usize),
    HotCueDelete(usize, usize),
    BeatJump(usize, usize),
    BeatLoop(usize, usize),
    /// Output only: the channel level, sent as a CC.
    Meter(usize),
    Trim(usize),
    Hi(usize),
    Mid(usize),
    Low(usize),
    Filter(usize),
    Fader(usize),
    Pfl(usize),
    Crossfader,
    HeadMix,
    HeadLevel,
    Browse,
    BrowsePress,
    Back,
    ViewNext,
    ViewPrev,
    FxNext,
    FxKind(FxKind),
    FxBeatDown,
    FxBeatUp,
    FxCh(usize),
    FxMaster,
    FxOn,
    FxLevel,
}

/// How a control reads its messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Button,
    /// Knob or fader position, 0..1.
    Absolute,
    /// Encoder ticks (jog wheels, browse knob).
    Relative,
    Output,
}

fn fx_key(k: FxKind) -> &'static str {
    match k {
        FxKind::Delay => "delay",
        FxKind::Echo => "echo",
        FxKind::PingPong => "ping_pong",
        FxKind::Reverb => "reverb",
        FxKind::Flanger => "flanger",
        FxKind::Phaser => "phaser",
        FxKind::Trans => "trans",
        FxKind::Roll => "roll",
    }
}

impl Control {
    /// Every control, in the order the controller settings list them.
    pub fn all() -> Vec<Control> {
        use Control::*;
        let mut v = Vec::new();
        for d in 0..DECKS {
            v.extend([
                Play(d), Cue(d), Sync(d), TempoRange(d), MasterTempo(d), Reverse(d), ToStart(d), Shift(d),
                Load(d), Tempo(d), JogTouch(d), Jog(d), JogScratch(d), JogBend(d), JogSearch(d),
                LoopIn(d), LoopOut(d), Reloop(d), Loop4(d), LoopHalf(d), LoopDouble(d),
            ]);
            v.extend((0..PADS).map(|i| HotCue(d, i)));
            v.extend((0..PADS).map(|i| HotCueDelete(d, i)));
            v.extend((0..PADS).map(|i| BeatJump(d, i)));
            v.extend((0..PADS).map(|i| BeatLoop(d, i)));
            v.push(Meter(d));
        }
        for c in 0..DECKS {
            v.extend([Trim(c), Hi(c), Mid(c), Low(c), Filter(c), Fader(c), Pfl(c)]);
        }
        v.extend([Crossfader, HeadMix, HeadLevel, Browse, BrowsePress, Back, ViewNext, ViewPrev]);
        v.push(FxNext);
        v.extend(FX_KINDS.iter().map(|&k| FxKind(k)));
        v.extend([FxBeatDown, FxBeatUp]);
        v.extend((0..DECKS).map(FxCh));
        v.extend([FxMaster, FxOn, FxLevel]);
        v
    }

    /// The name used in profiles and the settings file, e.g. `deck1.play`.
    pub fn name(self) -> String {
        use Control::*;
        let deck = |d: usize, s: &str| format!("deck{}.{s}", d + 1);
        let ch = |c: usize, s: &str| format!("ch{}.{s}", c + 1);
        match self {
            Play(d) => deck(d, "play"),
            Cue(d) => deck(d, "cue"),
            Sync(d) => deck(d, "sync"),
            TempoRange(d) => deck(d, "tempo_range"),
            MasterTempo(d) => deck(d, "master_tempo"),
            Reverse(d) => deck(d, "reverse"),
            ToStart(d) => deck(d, "to_start"),
            Shift(d) => deck(d, "shift"),
            JogTouch(d) => deck(d, "jog_touch"),
            Jog(d) => deck(d, "jog"),
            JogScratch(d) => deck(d, "jog_scratch"),
            JogBend(d) => deck(d, "jog_bend"),
            JogSearch(d) => deck(d, "jog_search"),
            Tempo(d) => deck(d, "tempo"),
            LoopIn(d) => deck(d, "loop_in"),
            LoopOut(d) => deck(d, "loop_out"),
            Reloop(d) => deck(d, "reloop"),
            Loop4(d) => deck(d, "loop_4"),
            LoopHalf(d) => deck(d, "loop_half"),
            LoopDouble(d) => deck(d, "loop_double"),
            Load(d) => deck(d, "load"),
            HotCue(d, i) => deck(d, &format!("hotcue{}", i + 1)),
            HotCueDelete(d, i) => deck(d, &format!("hotcue{}_delete", i + 1)),
            BeatJump(d, i) => deck(d, &format!("beatjump{}", i + 1)),
            BeatLoop(d, i) => deck(d, &format!("beatloop{}", i + 1)),
            Meter(d) => deck(d, "meter"),
            Trim(c) => ch(c, "trim"),
            Hi(c) => ch(c, "hi"),
            Mid(c) => ch(c, "mid"),
            Low(c) => ch(c, "low"),
            Filter(c) => ch(c, "filter"),
            Fader(c) => ch(c, "fader"),
            Pfl(c) => ch(c, "pfl"),
            Crossfader => "mixer.crossfader".into(),
            HeadMix => "mixer.head_mix".into(),
            HeadLevel => "mixer.head_level".into(),
            Browse => "browser.turn".into(),
            BrowsePress => "browser.press".into(),
            Back => "browser.back".into(),
            ViewNext => "browser.view_next".into(),
            ViewPrev => "browser.view_prev".into(),
            FxNext => "fx.next".into(),
            FxKind(k) => format!("fx.{}", fx_key(k)),
            FxBeatDown => "fx.beat_down".into(),
            FxBeatUp => "fx.beat_up".into(),
            FxCh(c) => format!("fx.ch{}", c + 1),
            FxMaster => "fx.master".into(),
            FxOn => "fx.on".into(),
            FxLevel => "fx.level".into(),
        }
    }

    /// Section heading in the controller settings.
    pub fn group(self) -> String {
        let n = self.name();
        let (g, _) = n.split_once('.').unwrap_or(("", ""));
        match g {
            g if g.starts_with("deck") => format!("DECK {}", &g[4..]),
            g if g.starts_with("ch") || g == "mixer" => "MIXER".into(),
            "browser" => "BROWSER".into(),
            _ => "BEAT FX".into(),
        }
    }

    /// Short label in the controller settings, e.g. "HOT CUE 3" or "CH 1 HI".
    pub fn label(self) -> String {
        use Control::*;
        match self {
            HotCue(_, i) => format!("HOT CUE {}", i + 1),
            HotCueDelete(_, i) => format!("DELETE HOT CUE {}", i + 1),
            BeatJump(_, i) => {
                let b = super::BEAT_JUMPS[i];
                format!("JUMP {}{} BEAT{}", if b < 0.0 { "BACK " } else { "" }, b.abs(), if b.abs() == 1.0 { "" } else { "S" })
            }
            BeatLoop(_, i) => match super::BEAT_LOOPS[i] {
                b if b < 1.0 => format!("1/{}-BEAT LOOP", (1.0 / b).round()),
                b => format!("{b}-BEAT LOOP"),
            },
            Loop4(_) => "4-BEAT LOOP".into(),
            LoopHalf(_) => "HALVE LOOP".into(),
            LoopDouble(_) => "DOUBLE LOOP".into(),
            Jog(_) => "JOG (SCRATCH WHEN TOUCHED)".into(),
            ToStart(_) => "BACK TO START".into(),
            FxKind(k) => k.name().into(),
            FxNext => "FX SELECT".into(),
            FxBeatDown => "BEAT LEFT".into(),
            FxBeatUp => "BEAT RIGHT".into(),
            FxCh(c) => format!("FX ON CH {}", c + 1),
            FxMaster => "FX ON MASTER".into(),
            FxOn => "FX ON/OFF".into(),
            Browse => "BROWSE (TURN)".into(),
            BrowsePress => "BROWSE (PRESS)".into(),
            ViewNext => "NEXT VIEW".into(),
            ViewPrev => "PREVIOUS VIEW".into(),
            _ => {
                let n = self.name();
                let (g, c) = n.split_once('.').unwrap_or(("", &n));
                let c = c.replace('_', " ").to_uppercase();
                match g.strip_prefix("ch") {
                    Some(k) => format!("CH {k} {c}"),
                    None => c,
                }
            }
        }
    }

    pub fn parse(name: &str) -> Option<Control> {
        Control::all().into_iter().find(|c| c.name() == name)
    }

    pub fn kind(self) -> Kind {
        use Control::*;
        match self {
            Tempo(_) | Trim(_) | Hi(_) | Mid(_) | Low(_) | Filter(_) | Fader(_) | Crossfader | HeadMix | HeadLevel
            | FxLevel => Kind::Absolute,
            Jog(_) | JogScratch(_) | JogBend(_) | JogSearch(_) | Browse => Kind::Relative,
            Meter(_) => Kind::Output,
            _ => Kind::Button,
        }
    }
}

/// One MIDI message shape. Channels are 0-based.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Msg {
    Note(u8, u8),
    Cc(u8, u8),
    /// MSB and LSB controller numbers.
    Cc14(u8, u8, u8),
    PitchBend(u8),
}

/// A message, optionally only while a software SHIFT is held.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Bound {
    pub msg: Msg,
    pub shift: bool,
}

impl Msg {
    /// The (status, data 1) pairs this message arrives as.
    pub fn keys(self) -> Vec<(u8, u8)> {
        match self {
            Msg::Note(ch, n) => vec![(0x90 | ch, n)],
            Msg::Cc(ch, c) => vec![(0xB0 | ch, c)],
            Msg::Cc14(ch, m, l) => vec![(0xB0 | ch, m), (0xB0 | ch, l)],
            Msg::PitchBend(ch) => vec![(0xE0 | ch, 0)],
        }
    }
}

impl fmt::Display for Bound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.shift {
            write!(f, "shift ")?;
        }
        match self.msg {
            Msg::Note(ch, n) => write!(f, "{:02X} {n:02X}", 0x90 | ch),
            Msg::Cc(ch, c) => write!(f, "{:02X} {c:02X}", 0xB0 | ch),
            Msg::Cc14(ch, m, l) => write!(f, "{:02X} {m:02X} {l:02X}", 0xB0 | ch),
            Msg::PitchBend(ch) => write!(f, "{:02X}", 0xE0 | ch),
        }
    }
}

impl std::str::FromStr for Bound {
    type Err = String;

    fn from_str(s: &str) -> Result<Bound, String> {
        let mut words: Vec<&str> = s.split_whitespace().collect();
        let shift = words.first() == Some(&"shift");
        if shift {
            words.remove(0);
        }
        let bytes: Vec<u8> = words
            .iter()
            .map(|w| u8::from_str_radix(w.trim_start_matches("0x"), 16).map_err(|_| format!("\"{s}\": {w} isn't a hex byte")))
            .collect::<Result<_, _>>()?;
        let bad = || format!("\"{s}\" isn't a note (90 0B), CC (B0 13), 14-bit CC (B0 13 33) or pitch bend (E0)");
        let (&status, data) = bytes.split_first().ok_or_else(bad)?;
        if data.iter().any(|&b| b > 0x7F) {
            return Err(bad());
        }
        let ch = status & 0x0F;
        let msg = match (status & 0xF0, data) {
            (0x80 | 0x90, &[n]) => Msg::Note(ch, n),
            (0xB0, &[c]) => Msg::Cc(ch, c),
            (0xB0, &[m, l]) => Msg::Cc14(ch, m, l),
            (0xE0, &[]) => Msg::PitchBend(ch),
            _ => return Err(bad()),
        };
        Ok(Bound { msg, shift })
    }
}

/// One message or a list, as written in TOML.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Msgs {
    One(String),
    Many(Vec<String>),
}

impl Msgs {
    pub fn list(&self) -> Vec<&str> {
        match self {
            Msgs::One(s) if s.trim().is_empty() => Vec::new(),
            Msgs::One(s) => vec![s.as_str()],
            Msgs::Many(v) => v.iter().map(String::as_str).collect(),
        }
    }
}

/// A controller profile as written in `profiles/*.toml`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Profile {
    pub name: String,
    pub ports: Vec<String>,
    pub extends: Option<String>,
    pub jog_ticks_per_rev: Option<f64>,
    pub invert_tempo: Option<bool>,
    pub led_on: Option<u8>,
    pub led_off: Option<u8>,
    pub init: Vec<String>,
    /// Raw messages sent every [`KEEPALIVE`] while connected.
    pub keepalive: Vec<String>,
    pub bind: BTreeMap<String, Msgs>,
    /// LEDs that answer on other messages than their buttons send.
    pub leds: BTreeMap<String, Msgs>,
}

/// How often `keepalive` messages go out (the Roland DJ-505 gives up after about 1.5 s).
pub const KEEPALIVE: std::time::Duration = std::time::Duration::from_millis(400);

macro_rules! profiles {
    ($($f:literal),* $(,)?) => { &[$(($f, include_str!(concat!("profiles/", $f)))),*] };
}

/// Built-in profiles: (file name, TOML).
pub const BUILTIN: &[(&str, &str)] = profiles![
    "behringer-cmd-studio-4a.toml",
    "denon-mc4000.toml",
    "denon-mc7000.toml",
    "hercules-djcontrol-inpulse-200.toml",
    "hercules-djcontrol-inpulse-300.toml",
    "hercules-djcontrol-inpulse-500.toml",
    "hercules-djcontrol-mix.toml",
    "hercules-djcontrol-starlight.toml",
    "numark-dj2go2-touch.toml",
    "numark-mixstream-pro.toml",
    "numark-mixtrack-platinum-fx.toml",
    "numark-mixtrack-platinum.toml",
    "numark-mixtrack-pro-3.toml",
    "numark-mixtrack-pro-fx.toml",
    "numark-party-mix.toml",
    "pioneer-ddj-200.toml",
    "pioneer-ddj-400.toml",
    "pioneer-ddj-flx4.toml",
    "pioneer-ddj-grv6.toml",
    "pioneer-ddj-sb.toml",
    "pioneer-ddj-sb2.toml",
    "pioneer-ddj-sb3.toml",
    "pioneer-ddj-sx.toml",
    "reloop-beatmix-mk2.toml",
    "reloop-mixage.toml",
    "reloop-terminal-mix.toml",
    "roland-dj-505.toml",
];

pub fn builtin() -> Vec<Profile> {
    BUILTIN
        .iter()
        .filter_map(|(f, s)| match toml::from_str::<Profile>(s) {
            Ok(p) => Some(p),
            Err(e) => {
                eprintln!("midi: built-in profile {f}: {e}");
                None
            }
        })
        .collect()
}

/// Lowercase with spaces, dashes and underscores removed, for matching device names.
pub fn squash(s: &str) -> String {
    s.to_lowercase().replace([' ', '-', '_'], "")
}

/// The built-in profile for a MIDI port: the one with the longest matching port substring.
pub fn profile_for_port<'a>(profiles: &'a [Profile], port: &str) -> Option<&'a Profile> {
    let p = squash(port);
    profiles
        .iter()
        .filter_map(|pr| pr.ports.iter().map(|h| squash(h)).filter(|h| !h.is_empty() && p.contains(h.as_str())).map(|h| h.len()).max().map(|n| (n, pr)))
        .max_by_key(|(n, _)| *n)
        .map(|(_, pr)| pr)
}

/// Device name substrings of every built-in controller (for autolaunch and the sound card).
pub fn device_hints(profiles: &[Profile], extra: Option<&str>) -> Vec<String> {
    let mut v: Vec<String> = profiles.iter().flat_map(|p| p.ports.iter().cloned()).collect();
    v.extend(extra.map(String::from));
    v.sort();
    v.dedup();
    v
}

/// `[controller]` in the settings file.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ControllerConfig {
    /// Use this profile whatever the port is called.
    pub profile: Option<String>,
    pub jog_ticks_per_rev: Option<f64>,
    pub invert_tempo: Option<bool>,
    /// Your own bindings, applied over the profile's. An empty string unbinds a control.
    pub bind: BTreeMap<String, Msgs>,
}

/// A profile with its `extends` chain and the user's bindings applied.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    pub name: String,
    pub jog_ticks_per_rev: f64,
    pub invert_tempo: bool,
    pub led_on: u8,
    pub led_off: u8,
    pub init: Vec<Vec<u8>>,
    pub keepalive: Vec<Vec<u8>>,
    pub bindings: Vec<(Control, Bound)>,
    /// LED messages for controls whose LED isn't on their button's message.
    pub leds: Vec<(Control, Bound)>,
    /// Controls bound by `[controller.bind]` rather than the profile.
    pub user: Vec<Control>,
}

impl Resolved {
    pub fn empty(name: &str) -> Resolved {
        Resolved {
            name: name.into(),
            jog_ticks_per_rev: 720.0,
            invert_tempo: false,
            led_on: 0x7F,
            led_off: 0x00,
            init: Vec::new(),
            keepalive: Vec::new(),
            bindings: Vec::new(),
            leds: Vec::new(),
            user: Vec::new(),
        }
    }

    /// Bind `control` to `msgs`, replacing its old messages and taking these from any other
    /// control.
    pub fn bind(&mut self, control: Control, msgs: &[Bound]) {
        let taken: Vec<(u8, u8, bool)> = msgs.iter().flat_map(|b| b.msg.keys().into_iter().map(|(s, d)| (s, d, b.shift))).collect();
        self.bindings.retain(|(c, b)| *c != control && !b.msg.keys().iter().any(|&(s, d)| taken.contains(&(s, d, b.shift))));
        self.bindings.extend(msgs.iter().map(|&b| (control, b)));
    }

    /// Apply a `[bind]` table; returns one message per entry that couldn't be read.
    fn apply(&mut self, table: &BTreeMap<String, Msgs>, user: bool) -> Vec<String> {
        let mut errors = Vec::new();
        for (name, msgs) in table {
            let Some(control) = Control::parse(name) else {
                errors.push(format!("unknown control \"{name}\""));
                continue;
            };
            let parsed: Result<Vec<Bound>, String> = msgs.list().iter().map(|s| s.parse()).collect();
            match parsed {
                Ok(b) => {
                    self.bind(control, &b);
                    if user && !self.user.contains(&control) {
                        self.user.push(control);
                    }
                }
                Err(e) => errors.push(format!("{name}: {e}")),
            }
        }
        errors
    }

    pub fn messages(&self, control: Control) -> Vec<Bound> {
        self.bindings.iter().filter(|(c, _)| *c == control).map(|(_, b)| *b).collect()
    }
}

fn parse_raw(s: &str) -> Result<Vec<u8>, String> {
    s.split_whitespace().map(|w| u8::from_str_radix(w.trim_start_matches("0x"), 16).map_err(|_| format!("\"{s}\" isn't hex bytes"))).collect()
}

/// Build the bindings for `profile` (None: no profile, only the user's bindings).
/// Returns the result and any problems found.
pub fn resolve(profiles: &[Profile], profile: Option<&Profile>, user: &ControllerConfig) -> (Resolved, Vec<String>) {
    let mut chain: Vec<&Profile> = Vec::new();
    let mut errors = Vec::new();
    let mut next = profile;
    while let Some(p) = next {
        if chain.iter().any(|q| q.name == p.name) {
            errors.push(format!("{}: extends loops back on itself", p.name));
            break;
        }
        chain.push(p);
        next = p.extends.as_deref().and_then(|e| {
            let found = profiles.iter().find(|q| q.name == e);
            if found.is_none() {
                errors.push(format!("{}: extends unknown profile \"{e}\"", p.name));
            }
            found
        });
    }
    let mut r = Resolved::empty(profile.map(|p| p.name.as_str()).unwrap_or("No profile"));
    for p in chain.iter().rev() {
        r.jog_ticks_per_rev = p.jog_ticks_per_rev.unwrap_or(r.jog_ticks_per_rev);
        r.invert_tempo = p.invert_tempo.unwrap_or(r.invert_tempo);
        r.led_on = p.led_on.unwrap_or(r.led_on);
        r.led_off = p.led_off.unwrap_or(r.led_off);
        for (from, to) in [(&p.init, &mut r.init), (&p.keepalive, &mut r.keepalive)] {
            if !from.is_empty() {
                to.clear();
                for s in from {
                    match parse_raw(s) {
                        Ok(b) => to.push(b),
                        Err(e) => errors.push(format!("{}: {e}", p.name)),
                    }
                }
            }
        }
        errors.extend(r.apply(&p.bind, false).into_iter().map(|e| format!("{}: {e}", p.name)));
        for (name, msgs) in &p.leds {
            let Some(control) = Control::parse(name) else {
                errors.push(format!("{}: [leds] unknown control \"{name}\"", p.name));
                continue;
            };
            match msgs.list().iter().map(|s| s.parse()).collect::<Result<Vec<Bound>, String>>() {
                Ok(b) => {
                    r.leds.retain(|(c, _)| *c != control);
                    r.leds.extend(b.into_iter().map(|b| (control, b)));
                }
                Err(e) => errors.push(format!("{}: [leds] {name}: {e}", p.name)),
            }
        }
    }
    r.jog_ticks_per_rev = user.jog_ticks_per_rev.unwrap_or(r.jog_ticks_per_rev);
    r.invert_tempo = user.invert_tempo.unwrap_or(r.invert_tempo);
    errors.extend(r.apply(&user.bind, true).into_iter().map(|e| format!("[controller.bind] {e}")));
    (r, errors)
}

/// Lookup table from incoming (status, data 1) to the bindings it can trigger.
#[derive(Clone, Debug, Default)]
pub struct Table {
    pub by_key: HashMap<(u8, u8), Vec<(Control, Bound)>>,
}

impl Table {
    pub fn new(r: &Resolved) -> Table {
        let mut by_key: HashMap<(u8, u8), Vec<(Control, Bound)>> = HashMap::new();
        for &(c, b) in &r.bindings {
            if c.kind() == Kind::Output {
                continue;
            }
            for k in b.msg.keys() {
                by_key.entry(k).or_default().push((c, b));
            }
        }
        Table { by_key }
    }
}
