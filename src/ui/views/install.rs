//! The install screen: shown instead of everything else when CDeckJ is opened from the
//! download (or from anywhere but the installed copy).

use std::path::PathBuf;

use crossbeam_channel::Receiver;

use crate::platform::install::{self, Offer};
use crate::ui::prelude::*;
use crate::ui::App;

pub struct Prompt {
    offer: Offer,
    target: PathBuf,
    job: Option<Receiver<Result<(), String>>>,
    error: Option<String>,
}

impl Prompt {
    pub fn new(offer: Offer, target: PathBuf) -> Prompt {
        Prompt { offer, target, job: None, error: None }
    }
}

impl App {
    /// Close the audio stream, hand the sound card back, stop recording and save, as on exit.
    pub(crate) fn shutdown(&mut self) {
        // Close the stream before handing the card back its old profile.
        self.audio = None;
        crate::audio::cardprofile::restore_saved();
        if let Some(f) = self.recorder.stop() {
            eprintln!("recording: saved {}", f.parts.first().map(|p| p.path.display().to_string()).unwrap_or_default());
        }
        let _ = self.config.save();
        self.store.lock().flush(true);
    }

    /// Shut down and start `exe` (the installed copy) in this process's place. Returns the
    /// error if it couldn't start.
    pub(crate) fn restart_into(&mut self, exe: &std::path::Path) -> String {
        self.shutdown();
        install::restart(exe, &["--installed"])
    }

    /// Draw the install screen over `full`. Returns false once it's dismissed.
    pub(crate) fn install_screen(&mut self, ui: &mut Ui, full: Rect) -> bool {
        let Some(prompt) = self.install.as_mut() else { return false };
        if let Some(rx) = &prompt.job {
            match rx.try_recv() {
                Ok(Ok(())) => {
                    prompt.job = None;
                    let target = prompt.target.clone();
                    let err = self.restart_into(&target);
                    if let Some(p) = self.install.as_mut() {
                        p.error = Some(err);
                    }
                }
                Ok(Err(e)) => {
                    prompt.job = None;
                    prompt.error = Some(e);
                }
                Err(_) => ui.ctx().request_repaint_after(std::time::Duration::from_millis(100)),
            }
        }
        let Some(prompt) = self.install.as_ref() else { return false };
        let p = ui.painter().clone();
        let r = Rect::from_center_size(full.center(), vec2(640.0, 260.0));
        card(&p, r);
        let inner = r.shrink2(vec2(32.0, 28.0));
        let (title, action, body) = match prompt.offer {
            Offer::Install => (
                "INSTALL CDECKJ",
                "INSTALL",
                format!("CDeckJ goes in {}, in the applications menu, and opens when you plug in the controller.", prompt.target.display()),
            ),
            Offer::Update => (
                "UPDATE CDECKJ",
                "UPDATE",
                format!("CDeckJ is installed in {}. Replace it with this version?", prompt.target.display()),
            ),
        };
        p.text(inner.min, Align2::LEFT_TOP, title, semi(26.0), t::INK);
        let g = p.layout(body, cond(18.0), t::INK_MUTED, inner.width());
        let body_h = g.size().y;
        p.galley(pos2(inner.min.x, inner.min.y + 50.0), g, t::INK_MUTED);
        let busy = prompt.job.is_some();
        let note = if busy {
            Some(("Installing…".to_string(), t::INK_MUTED))
        } else {
            prompt.error.clone().map(|e| (e, t::DANGER))
        };
        if let Some((n, col)) = note {
            let g = p.layout(n, cond(16.0), col, inner.width());
            p.galley(pos2(inner.min.x, inner.min.y + 64.0 + body_h), g, col);
        }
        let bw = 200.0;
        let go = Rect::from_min_size(pos2(inner.max.x - bw, inner.max.y - 48.0), vec2(bw, 48.0));
        let later = Rect::from_min_size(pos2(go.min.x - bw - 12.0, go.min.y), vec2(bw, 48.0));
        let clicked_go = button(ui, &mut self.touches, go, Id::NULL.with("inst_go"), action, Some(t::ACCENT), semi(18.0)).clicked;
        let clicked_later = button(ui, &mut self.touches, later, Id::NULL.with("inst_later"), "NOT NOW", None, semi(18.0)).clicked;
        if busy {
            return true;
        }
        if clicked_later {
            self.install = None;
            return false;
        }
        if clicked_go {
            let target = prompt.target.clone();
            let (tx, rx) = crossbeam_channel::bounded(1);
            std::thread::spawn(move || {
                let r = std::env::current_exe().map_err(|e| e.to_string()).and_then(|exe| install::install(&exe, &target));
                let _ = tx.send(r);
            });
            if let Some(p) = self.install.as_mut() {
                p.error = None;
                p.job = Some(rx);
            }
        }
        true
    }
}
