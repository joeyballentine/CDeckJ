//! Multi-touch for custom controls. egui turns only the first finger into a pointer, so two
//! hands on the screen (hold CUE + tap PLAY, ride two faders) need per-finger tracking.
//! Each finger is claimed by the control it landed on and stays with it until lifted.

use std::collections::HashMap;

use egui::{Event, Id, Pos2, Rect, Response, TouchPhase, Vec2};

#[derive(Clone, Copy, Debug)]
struct Finger {
    pos: Pos2,
    prev: Pos2,
    start: Pos2,
    start_time: f64,
    claimed: Option<Id>,
    started_now: bool,
    ended: bool,
}

/// What one control saw this frame, from either touch or mouse.
#[derive(Clone, Copy, Debug, Default)]
pub struct Ctl {
    pub down: bool,
    pub pressed: bool,
    pub released: bool,
    pub clicked: bool,
    pub double: bool,
    pub delta: Vec2,
    pub pos: Option<Pos2>,
    pub hovered: bool,
}

#[derive(Default)]
pub struct Touches {
    fingers: HashMap<u64, Finger>,
    last_touch: f64,
    now: f64,
    prev_down: HashMap<Id, bool>,
    taps: HashMap<Id, f64>,
}

const TAP_SLOP: f32 = 14.0;
const TAP_TIME: f64 = 0.6;
const DOUBLE_TAP: f64 = 0.35;

impl Touches {
    /// Feed this frame's input events.
    pub fn begin_frame(&mut self, events: &[Event], now: f64) {
        self.now = now;
        self.fingers.retain(|_, f| !f.ended);
        for f in self.fingers.values_mut() {
            f.prev = f.pos;
            f.started_now = false;
        }
        for e in events {
            if let Event::Touch { id, phase, pos, .. } = e {
                self.last_touch = now;
                match phase {
                    TouchPhase::Start => {
                        self.fingers.insert(
                            id.0,
                            Finger { pos: *pos, prev: *pos, start: *pos, start_time: now, claimed: None, started_now: true, ended: false },
                        );
                    }
                    TouchPhase::Move => {
                        if let Some(f) = self.fingers.get_mut(&id.0) {
                            f.pos = *pos;
                        }
                    }
                    TouchPhase::End | TouchPhase::Cancel => {
                        if let Some(f) = self.fingers.get_mut(&id.0) {
                            f.pos = *pos;
                            f.ended = true;
                        }
                    }
                }
            }
        }
    }

    /// Touch is in use (a finger is down, or was very recently): ignore emulated pointer input.
    pub fn touch_mode(&self) -> bool {
        !self.fingers.is_empty() || self.now - self.last_touch < 1.0
    }

    /// Interaction state of a control occupying `rect`. `resp` supplies mouse/keyboard input
    /// when no touch screen is in use.
    pub fn ctl(&mut self, rect: Rect, id: Id, resp: &Response) -> Ctl {
        let prev_down = self.prev_down.get(&id).copied().unwrap_or(false);
        let mut c = Ctl { hovered: resp.hovered(), ..Default::default() };
        if self.touch_mode() {
            for f in self.fingers.values_mut() {
                if f.claimed.is_none() && rect.contains(f.start) {
                    f.claimed = Some(id);
                }
            }
            for f in self.fingers.values().filter(|f| f.claimed == Some(id)) {
                if !f.ended {
                    c.down = true;
                    c.pos = Some(f.pos);
                } else if c.pos.is_none() {
                    c.pos = Some(f.pos);
                }
                c.pressed |= f.started_now;
                c.delta += f.pos - f.prev;
                if f.ended && (f.pos - f.start).length() < TAP_SLOP && self.now - f.start_time < TAP_TIME {
                    c.clicked = true;
                }
            }
            c.released = prev_down && !c.down;
            c.hovered = c.down;
        } else {
            c.down = resp.is_pointer_button_down_on();
            c.pressed = c.down && !prev_down;
            c.released = !c.down && prev_down;
            c.clicked = resp.clicked();
            c.delta = resp.drag_delta();
            c.pos = resp.interact_pointer_pos();
        }
        if c.clicked {
            let last = self.taps.insert(id, self.now);
            c.double = last.is_some_and(|t| self.now - t < DOUBLE_TAP) || resp.double_clicked();
            if c.double {
                self.taps.remove(&id);
            }
        }
        self.prev_down.insert(id, c.down);
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, TouchDeviceId, TouchId};

    fn touch(id: u64, phase: TouchPhase, x: f32, y: f32) -> Event {
        Event::Touch { device_id: TouchDeviceId(0), id: TouchId(id), phase, pos: pos2(x, y), force: None }
    }

    fn resp(ctx: &egui::Context) -> Response {
        let mut out = None;
        let _ = ctx.run_ui(Default::default(), |ui| {
            out = Some(ui.interact(Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(1.0, 1.0)), Id::NULL.with("dummy"), egui::Sense::hover()));
        });
        out.unwrap()
    }

    #[test]
    fn two_fingers_hold_two_controls() {
        let ctx = egui::Context::default();
        let r = resp(&ctx);
        let cue = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(50.0, 50.0));
        let play = Rect::from_min_size(pos2(0.0, 100.0), egui::vec2(50.0, 50.0));
        let (cue_id, play_id) = (Id::NULL.with("cue"), Id::NULL.with("play"));
        let mut t = Touches::default();

        t.begin_frame(&[touch(1, TouchPhase::Start, 10.0, 10.0)], 0.0);
        let c = t.ctl(cue, cue_id, &r);
        assert!(c.down && c.pressed);
        assert!(!t.ctl(play, play_id, &r).down);

        // Second finger on PLAY while CUE is held.
        t.begin_frame(&[touch(2, TouchPhase::Start, 10.0, 110.0)], 0.1);
        assert!(t.ctl(cue, cue_id, &r).down);
        let p = t.ctl(play, play_id, &r);
        assert!(p.down && p.pressed);

        // Lift CUE, then tap ends on PLAY.
        t.begin_frame(&[touch(1, TouchPhase::End, 10.0, 10.0)], 0.2);
        let c = t.ctl(cue, cue_id, &r);
        assert!(!c.down && c.released);
        t.begin_frame(&[touch(2, TouchPhase::End, 11.0, 111.0)], 0.25);
        let p = t.ctl(play, play_id, &r);
        assert!(p.clicked && p.released);
    }

    #[test]
    fn a_finger_stays_with_its_control_and_reports_drag() {
        let ctx = egui::Context::default();
        let r = resp(&ctx);
        let fader = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(30.0, 200.0));
        let other = Rect::from_min_size(pos2(100.0, 0.0), egui::vec2(30.0, 200.0));
        let id = Id::NULL.with("fader");
        let mut t = Touches::default();
        t.begin_frame(&[touch(7, TouchPhase::Start, 10.0, 150.0)], 0.0);
        t.ctl(fader, id, &r);
        // Finger slides out of the fader's rect onto another control: still the fader's.
        t.begin_frame(&[touch(7, TouchPhase::Move, 110.0, 40.0)], 0.1);
        assert!(!t.ctl(other, Id::NULL.with("other"), &r).down);
        let c = t.ctl(fader, id, &r);
        assert!(c.down);
        assert_eq!(c.delta, egui::vec2(100.0, -110.0));
        assert!(!c.clicked);
    }

    #[test]
    fn double_tap() {
        let ctx = egui::Context::default();
        let r = resp(&ctx);
        let k = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(40.0, 40.0));
        let id = Id::NULL.with("knob");
        let mut t = Touches::default();
        let mut tap = |t: &mut Touches, n: u64, at: f64| {
            t.begin_frame(&[touch(n, TouchPhase::Start, 5.0, 5.0)], at);
            t.ctl(k, id, &r);
            t.begin_frame(&[touch(n, TouchPhase::End, 5.0, 5.0)], at + 0.05);
            t.ctl(k, id, &r)
        };
        assert!(!tap(&mut t, 1, 0.0).double);
        assert!(tap(&mut t, 2, 0.2).double);
    }
}
