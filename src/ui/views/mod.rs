//! Switching between full-screen views, like a CDJ's screen buttons: PERFORM (everything),
//! WAVE (big waveforms), BROWSE (full library), TAG (tag list) and SETTINGS.

use crate::ui::App;

pub(crate) mod browse;
mod display;
pub(crate) mod install;
mod perform;
pub(crate) mod settings;
mod wave;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Perform,
    Wave,
    Browse,
    Tag,
    Settings,
}

pub const VIEWS: [View; 5] = [View::Perform, View::Wave, View::Browse, View::Tag, View::Settings];

impl View {
    pub fn name(self) -> &'static str {
        match self {
            View::Perform => "PERFORM",
            View::Wave => "WAVE",
            View::Browse => "BROWSE",
            View::Tag => "TAG",
            View::Settings => "SETTINGS",
        }
    }

    pub fn icon(self) -> &'static str {
        use crate::ui::icons as i;
        match self {
            View::Perform => i::PERFORM,
            View::Wave => i::WAVE,
            View::Browse => i::BROWSE,
            View::Tag => i::TAG,
            View::Settings => i::SETTINGS,
        }
    }

    pub fn tip(self) -> &'static str {
        match self {
            View::Perform => "Decks, mixer and library together  ( F1 )",
            View::Wave => "Big waveforms with phase meter and cue countdown  ( F2 )",
            View::Browse => "The whole library, with search, filter and track info  ( F3 )",
            View::Tag => "Your tag list for this set  ( F4 )",
            View::Settings => "Player, mixer, audio and controller settings  ( F5 )",
        }
    }

    /// Views you play from (loading a track returns to the last one of these).
    pub fn is_play_view(self) -> bool {
        matches!(self, View::Perform | View::Wave)
    }
}

impl App {
    pub(crate) fn set_view(&mut self, v: View) {
        if v == self.view_mode {
            return;
        }
        if self.view_mode.is_play_view() {
            self.last_play_view = self.view_mode;
        }
        match v {
            View::Tag => self.lib.jump_to(crate::library::Node::Tag),
            View::Browse if self.lib.node() == crate::library::Node::Tag => self.lib.jump_to(crate::library::Node::Root),
            _ => {}
        }
        if v == View::Settings {
            self.settings_opened();
        }
        self.keyboard_open = false;
        self.view_mode = v;
        self.scroll_to_selected = true;
    }

    pub(crate) fn cycle_view(&mut self, dir: i32) {
        let i = VIEWS.iter().position(|&v| v == self.view_mode).unwrap_or(0) as i32;
        self.set_view(VIEWS[(i + dir).rem_euclid(VIEWS.len() as i32) as usize]);
    }

    /// After loading from a browse view, go back to playing (like a CDJ).
    pub(crate) fn after_load(&mut self) {
        if !self.view_mode.is_play_view() {
            let back = self.last_play_view;
            self.set_view(back);
        }
    }

}
