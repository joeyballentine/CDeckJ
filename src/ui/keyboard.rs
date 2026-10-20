//! Keyboard shortcuts (see the README's Keyboard section).

use crate::ui::prelude::*;
use egui::{Event, ViewportCommand};
use crate::library::Row;
use crate::ui::App;

impl App {
    pub(crate) fn keyboard(&mut self, ui: &Ui) {
        if !ui.ctx().text_edit_focused() && ui.input(|i| i.key_pressed(Key::Slash) || (i.modifiers.command && i.key_pressed(Key::F))) {
            self.focus_search = true;
            return;
        }
        if ui.ctx().text_edit_focused() {
            return;
        }
        // A focused control owns the arrows, Enter and Space.
        let widget_focus = ui.ctx().memory(|m| m.focused().is_some());
        // Ctrl+Q quits (press twice); Ctrl-chords never reach the deck shortcuts.
        if ui.input(|i| i.modifiers.command && i.key_pressed(Key::Q)) {
            self.exit_press(ui.ctx());
        }
        let events: Vec<(Key, bool, bool)> = ui.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    Event::Key { key, pressed, repeat, modifiers, .. } if !modifiers.command => Some((*key, *pressed, *repeat)),
                    _ => None,
                })
                .collect()
        });
        for (key, pressed, repeat) in events {
            if widget_focus && matches!(key, Key::ArrowUp | Key::ArrowDown | Key::ArrowLeft | Key::ArrowRight | Key::Enter | Key::Space) {
                continue;
            }
            if pressed && repeat {
                match key {
                    Key::ArrowUp => self.browse(-1),
                    Key::ArrowDown => self.browse(1),
                    _ => {}
                }
                continue;
            }
            if pressed {
                if self.key_held.contains(&key) {
                    continue;
                }
                self.key_held.push(key);
            } else {
                self.key_held.retain(|k| *k != key);
            }
            let hot = |k: Key| -> Option<(usize, usize)> {
                Some(match k {
                    Key::Num1 => (0, 0),
                    Key::Num2 => (0, 1),
                    Key::Num3 => (0, 2),
                    Key::Num4 => (0, 3),
                    Key::Num7 => (1, 0),
                    Key::Num8 => (1, 1),
                    Key::Num9 => (1, 2),
                    Key::Num0 => (1, 3),
                    _ => return None,
                })
            };
            if let Some((d, i)) = hot(key) {
                self.send(Cmd::HotCue(d, i, pressed));
                continue;
            }
            match (key, pressed) {
                (Key::X, p) => self.send(Cmd::Cue(0, p)),
                (Key::N, p) => self.send(Cmd::Cue(1, p)),
                (Key::Z, true) => self.send(Cmd::Play(0)),
                (Key::M, true) => self.send(Cmd::Play(1)),
                (Key::A, true) => self.send(Cmd::SyncToggle(0)),
                (Key::K, true) => self.send(Cmd::SyncToggle(1)),
                (Key::S, true) => self.send(Cmd::MasterTempoToggle(0)),
                (Key::J, true) => self.send(Cmd::MasterTempoToggle(1)),
                (Key::Q, true) => self.load_selected(0),
                (Key::P, true) => self.load_selected(1),
                (Key::G, true) => self.send(Cmd::QuantizeToggle),
                (Key::R, true) => self.record_toggle(),
                (Key::F1, true) => self.set_view(View::Perform),
                (Key::F2, true) => self.set_view(View::Wave),
                (Key::F3, true) => self.set_view(View::Browse),
                (Key::F4, true) => self.set_view(View::Tag),
                (Key::F5, true) => self.set_view(View::Settings),
                (Key::T, true) => {
                    if let Some(Row::Track(t)) = self.lib.selected_row().cloned() {
                        self.lib.toggle_tag(&t);
                    }
                }
                (Key::F, true) => self.send(Cmd::Mix(MixParam::FxToggle)),
                (Key::W, true) => self.config.waveform_color = crate::track::wave::next_mode(self.config.waveform_color),
                // Same as the on-screen −/+: minus shows more time (zoom out).
                (Key::Minus, true) => self.set_zoom(1),
                (Key::Equals | Key::Plus, true) => self.set_zoom(-1),
                (Key::ArrowUp, true) => self.browse(-1),
                (Key::ArrowDown, true) => self.browse(1),
                (Key::PageUp, true) => self.browse(-10),
                (Key::PageDown, true) => self.browse(10),
                (Key::Enter | Key::ArrowRight, true) => self.browse_enter(),
                // Esc closes a BROWSE popup or the on-screen keyboard before it goes back.
                (Key::Escape, true) if self.browse_popup.is_some() => self.browse_popup = None,
                (Key::Escape, true) if self.keyboard_open => self.keyboard_open = false,
                (Key::Backspace | Key::ArrowLeft | Key::Escape, true) => self.browse_back(),
                (Key::F11, true) => {
                    self.fullscreen = !self.fullscreen;
                    ui.ctx().send_viewport_cmd(ViewportCommand::Fullscreen(self.fullscreen));
                }
                _ => {}
            }
        }
    }
}
