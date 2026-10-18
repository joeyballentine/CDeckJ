//! The on-screen keyboard for searching without a physical one (Game Mode).

use crate::ui::prelude::*;
use crate::ui::App;

impl App {
    /// QWERTY keyboard for searching without a physical keyboard (Game Mode).
    pub(crate) fn on_screen_keyboard(&mut self, ui: &mut Ui, r: Rect) {
        let p = ui.painter().clone();
        panel(&p, r);
        let rows: [&str; 4] = ["1234567890", "QWERTYUIOP", "ASDFGHJKL", "ZXCVBNM"];
        let inner = r.shrink(8.0);
        let gap = 5.0;
        let kh = (inner.height() - 4.0 * gap) / 5.0;
        let kw = (inner.width() - 9.0 * gap) / 10.0;
        let mut typed: Option<String> = None;
        let mut back = false;
        for (ri, row) in rows.iter().enumerate() {
            let n = row.chars().count() as f32;
            let off = (10.0 - n) * (kw + gap) / 2.0;
            for (ci, ch) in row.chars().enumerate() {
                let kr = Rect::from_min_size(pos2(inner.min.x + off + ci as f32 * (kw + gap), inner.min.y + ri as f32 * (kh + gap)), vec2(kw, kh));
                if button(ui, &mut self.touches, kr, Id::NULL.with(("key", ri, ci)), &ch.to_string(), None, semi(17.0)).clicked {
                    typed = Some(ch.to_ascii_lowercase().to_string());
                }
            }
        }
        // Bottom row: CLEAR, SPACE, DEL, DONE.
        let y = inner.min.y + 4.0 * (kh + gap);
        let del = format!("{}  DEL", icons::BACKSPACE);
        let specs: [(&str, f32); 4] = [("CLEAR", 2.0), ("SPACE", 4.0), (del.as_str(), 2.0), ("DONE", 2.0)];
        let mut x = inner.min.x;
        for (i, (label, units)) in specs.iter().enumerate() {
            let w = units * kw + (units - 1.0) * gap;
            let kr = Rect::from_min_size(pos2(x, y), vec2(w, kh));
            let lit = (*label == "DONE").then_some(t::ACCENT);
            if button(ui, &mut self.touches, kr, Id::NULL.with(("keyx", i)), label, lit, semi(15.0)).clicked {
                match i {
                    0 => {
                        self.search.clear();
                        typed = Some(String::new());
                    }
                    1 => typed = Some(" ".into()),
                    2 => back = true,
                    _ => self.keyboard_open = false,
                }
            }
            x += w + gap;
        }
        if back {
            self.search.pop();
            typed = Some(String::new());
        }
        if let Some(s) = typed {
            self.search.push_str(&s);
            let f = self.search.clone();
            self.lib.set_filter(&f);
            self.scroll_to_selected = true;
        }
        let _ = Key::A;
    }
}
