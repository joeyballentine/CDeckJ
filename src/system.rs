//! Steam Deck integration: the built-in controls as a DJ controller, and keeping the screen on.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use gilrs::{Axis, Button, EventType, Gilrs};
use parking_lot::Mutex;

use crate::engine::{Cmd, MixParam, SharedView};
use crate::midi::UiAction;

pub type GamepadStatus = Arc<Mutex<Option<String>>>;

/// Map a gamepad (the Deck's own controls in Steam's "Gamepad" layout) onto the decks:
///
/// | Control | Action |
/// |---|---|
/// | LB / RB | PLAY deck 1 / 2 |
/// | LT / RT (hold) | CUE deck 1 / 2 |
/// | View + LB / RB | SYNC deck 1 / 2 |
/// | D-pad ↑ ↓ | browse; ← back, → open |
/// | A / Y | open / back |
/// | X / B | LOAD deck 1 / 2 |
/// | Left / right stick ← → | nudge deck 1 / 2 |
/// | View + left stick ← → | crossfader |
/// | Menu (≡) | record; press twice to stop |
/// | Left / right stick click | previous / next screen view |
pub fn spawn_gamepad(cmd: Sender<Cmd>, ui: Sender<UiAction>, repaint: Arc<dyn Fn() + Send + Sync>, view: SharedView) -> GamepadStatus {
    let status: GamepadStatus = Arc::new(Mutex::new(None));
    let st = status.clone();
    thread::Builder::new()
        .name("gamepad".into())
        .spawn(move || {
            let mut g = match Gilrs::new() {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("gamepad: {e}");
                    return;
                }
            };
            let refresh_name = |g: &Gilrs, st: &GamepadStatus| {
                *st.lock() = g.gamepads().next().map(|(_, p)| p.name().to_string());
            };
            refresh_name(&g, &st);
            let mut shift = false;
            let mut sticks = [0f32; 2];
            let mut held_dir: Option<(i32, Instant)> = None;
            let mut last_repeat = Instant::now();
            let send = |c: Cmd| {
                let _ = cmd.try_send(c);
                repaint();
            };
            let ui_send = |a: UiAction| {
                let _ = ui.try_send(a);
                repaint();
            };
            loop {
                while let Some(ev) = g.next_event() {
                    match ev.event {
                        EventType::Connected | EventType::Disconnected => {
                            refresh_name(&g, &st);
                            repaint();
                        }
                        EventType::ButtonPressed(b, _) => match b {
                            Button::Select => shift = true,
                            Button::LeftTrigger if shift => send(Cmd::SyncToggle(0)),
                            Button::RightTrigger if shift => send(Cmd::SyncToggle(1)),
                            Button::LeftTrigger => send(Cmd::Play(0)),
                            Button::RightTrigger => send(Cmd::Play(1)),
                            Button::LeftTrigger2 => send(Cmd::Cue(0, true)),
                            Button::RightTrigger2 => send(Cmd::Cue(1, true)),
                            Button::DPadUp => {
                                ui_send(UiAction::Browse(-1));
                                held_dir = Some((-1, Instant::now()));
                            }
                            Button::DPadDown => {
                                ui_send(UiAction::Browse(1));
                                held_dir = Some((1, Instant::now()));
                            }
                            Button::DPadLeft | Button::North => ui_send(UiAction::Back),
                            Button::DPadRight | Button::South => ui_send(UiAction::BrowsePress),
                            Button::West => ui_send(UiAction::Load(0)),
                            Button::East => ui_send(UiAction::Load(1)),
                            Button::Start => ui_send(UiAction::Record),
                            Button::LeftThumb => ui_send(UiAction::CycleView(-1)),
                            Button::RightThumb => ui_send(UiAction::CycleView(1)),
                            _ => {}
                        },
                        EventType::ButtonReleased(b, _) => match b {
                            Button::Select => shift = false,
                            Button::LeftTrigger2 => send(Cmd::Cue(0, false)),
                            Button::RightTrigger2 => send(Cmd::Cue(1, false)),
                            Button::DPadUp | Button::DPadDown => held_dir = None,
                            _ => {}
                        },
                        EventType::AxisChanged(Axis::LeftStickX, v, _) => sticks[0] = v,
                        EventType::AxisChanged(Axis::RightStickX, v, _) => sticks[1] = v,
                        _ => {}
                    }
                }
                // Held D-pad repeats like a keyboard.
                if let Some((dir, since)) = held_dir {
                    if since.elapsed() > Duration::from_millis(350) && last_repeat.elapsed() > Duration::from_millis(70) {
                        last_repeat = Instant::now();
                        ui_send(UiAction::Browse(dir));
                    }
                }
                // Sticks: nudge (or crossfader with View held), with a dead zone.
                for (d, &x) in sticks.iter().enumerate() {
                    if x.abs() < 0.2 {
                        continue;
                    }
                    let amt = (x.abs() - 0.2) / 0.8 * x.signum();
                    if shift && d == 0 {
                        let cur = view.lock().mixer.crossfader;
                        send(Cmd::Mix(MixParam::Crossfader((cur + amt * 0.03).clamp(0.0, 1.0))));
                    } else {
                        send(Cmd::JogBend(d, amt as f64 * 1.5));
                    }
                }
                thread::sleep(Duration::from_millis(10));
            }
        })
        .ok();
    status
}

/// Keeps the screen on and the machine awake while alive.
pub struct KeepAwake {
    /// The ScreenSaver inhibit lasts as long as this connection.
    _session: Option<zbus::blocking::Connection>,
    /// logind releases the sleep/idle block when this descriptor closes.
    _logind: Option<zbus::zvariant::OwnedFd>,
}

/// Ask the desktop not to dim/lock (freedesktop ScreenSaver inhibit, held for as long as
/// the D-Bus connection lives) and logind not to idle-sleep (held by a file descriptor).
/// Both are best effort. (Talking to logind directly rather than running `systemd-inhibit`
/// also keeps the binary free of newer-glibc process-spawning symbols.)
pub fn keep_awake() -> KeepAwake {
    let session = zbus::blocking::Connection::session().ok().and_then(|c| {
        c.call_method(
            Some("org.freedesktop.ScreenSaver"),
            "/org/freedesktop/ScreenSaver",
            Some("org.freedesktop.ScreenSaver"),
            "Inhibit",
            &("CDeckJ", "DJ set in progress"),
        )
        .ok()
        .map(|_| c)
    });
    let logind = zbus::blocking::Connection::system().ok().and_then(|c| {
        let reply = c
            .call_method(
                Some("org.freedesktop.login1"),
                "/org/freedesktop/login1",
                Some("org.freedesktop.login1.Manager"),
                "Inhibit",
                &("idle:sleep", "CDeckJ", "DJ set in progress", "block"),
            )
            .ok()?;
        reply.body().deserialize::<zbus::zvariant::OwnedFd>().ok()
    });
    if session.is_none() && logind.is_none() {
        eprintln!("keep-awake: no screensaver or logind service; the screen may dim");
    }
    KeepAwake { _session: session, _logind: logind }
}
