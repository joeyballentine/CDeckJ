//! PERFORM's full layout, for playing by touch: waveforms, both decks, mixer, Beat FX and
//! the library. With a controller connected PERFORM shows the display layout instead
//! (views/display.rs).

use crate::ui::prelude::*;
use crate::ui::App;

impl App {
    /// The all-in-one view: waveforms, decks, mixer, beat FX, library.
    pub(crate) fn perform_view(&mut self, ui: &mut Ui, full: Rect, v: &EngineView) {
        let wave_h = (full.height() * 0.15).clamp(92.0, 170.0);
        let fx_h = 44.0;
        let main_h = 396.0f32.min((full.height() - wave_h - fx_h - 140.0).max(330.0));
        let mixer_w = 340.0;

        let waves = Rect::from_min_size(full.min + vec2(6.0, 6.0), vec2(full.width() - 6.0, wave_h - 6.0));
        let main = Rect::from_min_size(pos2(full.min.x, waves.max.y), vec2(full.width(), main_h));
        let fx_bar = Rect::from_min_size(pos2(full.min.x, main.max.y), vec2(full.width(), fx_h));
        let browser = Rect::from_min_max(pos2(full.min.x, fx_bar.max.y), full.max);
        let deck_w = (full.width() - mixer_w) / 2.0;
        let deck_rects = [
            Rect::from_min_size(main.min, vec2(deck_w, main_h)),
            Rect::from_min_size(pos2(main.min.x + deck_w + mixer_w, main.min.y), vec2(deck_w, main_h)),
        ];
        let mixer = Rect::from_min_size(pos2(main.min.x + deck_w, main.min.y), vec2(mixer_w, main_h));

        for d in 0..DECKS {
            let h = wave_h / 2.0;
            let r = Rect::from_min_size(pos2(waves.min.x, waves.min.y + d as f32 * h), vec2(waves.width(), h));
            self.waveform(ui, r.shrink2(vec2(0.0, 1.0)), d, &v.decks[d]);
        }
        self.zoom_buttons(ui, waves, false);
        for d in 0..DECKS {
            self.deck_panel(ui, deck_rects[d].shrink(6.0), d, &v.decks[d]);
        }
        self.mixer_panel(ui, mixer.shrink2(vec2(0.0, 6.0)), v);
        self.fx_bar(ui, fx_bar.shrink2(vec2(6.0, 2.0)), v);
        self.browser(ui, browser.shrink(6.0), false);

    }
}
