//! MIDI learn: turning the messages caught after picking a control into its binding.

use std::time::{Duration, Instant};

use super::*;

/// How long to keep listening after the first message, to catch a 14-bit control's LSB.
pub const SETTLE: Duration = Duration::from_millis(150);

/// The binding for `control` from what was caught, once `SETTLE` has passed since the first
/// message that fits it. SHIFT buttons' own messages are skipped.
pub fn learned(control: Control, caught: &[Learned], profile: &Resolved, now: Instant) -> Option<Bound> {
    let shift_keys: Vec<(u8, u8)> =
        profile.bindings.iter().filter(|(c, _)| matches!(c, Control::Shift(_))).flat_map(|(_, b)| b.msg.keys()).collect();
    let kind = control.kind();
    let fits = |l: &&Learned| {
        let [st, d1, d2] = l.msg;
        let key = match st & 0xF0 {
            0x90 | 0x80 => (0x90 | (st & 0x0F), d1),
            k => (k | (st & 0x0F), d1),
        };
        if !matches!(control, Control::Shift(_)) && shift_keys.contains(&key) {
            return false;
        }
        match (kind, st & 0xF0) {
            (Kind::Button, 0x90 | 0xB0) => d2 > 0,
            (Kind::Relative, 0xB0) => true,
            (Kind::Absolute, 0xB0 | 0xE0) => true,
            _ => false,
        }
    };
    // Buttons are usually notes; a CC counts only when no note arrives, so a knob nudged
    // while reaching for the button isn't taken.
    let note = |l: &&Learned| fits(l) && l.msg[0] & 0xF0 == 0x90;
    let first = if kind == Kind::Button { caught.iter().find(note).or_else(|| caught.iter().find(fits)) } else { caught.iter().find(fits) }?;
    if now.duration_since(first.at) < SETTLE {
        return None;
    }
    let [st, d1, _] = first.msg;
    let ch = st & 0x0F;
    let has_cc = |n: u8| caught.iter().any(|l| l.msg[0] == st && l.msg[1] == n);
    let msg = match st & 0xF0 {
        0x90 => Msg::Note(ch, d1),
        0xE0 => Msg::PitchBend(ch),
        _ if kind == Kind::Absolute && d1 < 0x20 && has_cc(d1 + 0x20) => Msg::Cc14(ch, d1, d1 + 0x20),
        _ if kind == Kind::Absolute && (0x20..0x40).contains(&d1) && has_cc(d1 - 0x20) => Msg::Cc14(ch, d1 - 0x20, d1),
        _ => Msg::Cc(ch, d1),
    };
    Some(Bound { msg, shift: first.shift && !matches!(control, Control::Shift(_)) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(t0: Instant, ms: u64, msg: [u8; 3], shift: bool) -> Learned {
        Learned { msg, shift, at: t0 + Duration::from_millis(ms) }
    }

    #[test]
    fn learns_buttons_faders_and_shift() {
        let t0 = Instant::now();
        let p = Resolved::empty("test");
        let later = t0 + Duration::from_millis(500);
        // A button: the release and anything before the press are skipped.
        let caught = [at(t0, 0, [0xB0, 0x13, 0x10], false), at(t0, 5, [0x91, 0x0B, 0x7F], false), at(t0, 90, [0x91, 0x0B, 0x00], false)];
        assert_eq!(learned(Control::Play(1), &caught, &p, later).unwrap().to_string(), "91 0B");
        assert_eq!(learned(Control::Play(1), &caught, &p, t0 + Duration::from_millis(50)), None, "still settling");
        // A 14-bit fader: MSB then LSB.
        let caught = [at(t0, 0, [0xB0, 0x13, 0x10], false), at(t0, 1, [0xB0, 0x33, 0x05], false)];
        assert_eq!(learned(Control::Fader(0), &caught, &p, later).unwrap().to_string(), "B0 13 33");
        // A 7-bit knob.
        let caught = [at(t0, 0, [0xB2, 0x07, 0x40], false)];
        assert_eq!(learned(Control::Hi(0), &caught, &p, later).unwrap().to_string(), "B2 07");
        // With a software SHIFT held, and the SHIFT button's own message skipped.
        let mut p = Resolved::empty("test");
        p.bind(Control::Shift(0), &["90 20".parse().unwrap()]);
        let caught = [at(t0, 0, [0x90, 0x20, 0x7F], true), at(t0, 300, [0x90, 0x0B, 0x7F], true)];
        assert_eq!(learned(Control::Reverse(0), &caught, &p, t0 + Duration::from_millis(600)).unwrap().to_string(), "shift 90 0B");
        assert_eq!(learned(Control::Shift(1), &caught, &p, later).unwrap().to_string(), "90 20");
    }
}
