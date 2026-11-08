//! The install screen: shown instead of everything else when CDeckJ is opened from the
//! download (or from anywhere but the installed copy).

use std::path::PathBuf;

use crossbeam_channel::Receiver;

use crate::platform::install::{self, Offer};
use crate::ui::prelude::*;
use crate::ui::{App, Banner, BannerKind};

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

/// The check for a newer release, and an update in progress.
#[derive(Default)]
pub struct Updates {
    check: Option<Receiver<Result<bool, String>>>,
    /// Whether a newer release is out; None until a check succeeds.
    pub available: Option<bool>,
    /// Where the update installs to, and its result.
    running: Option<(PathBuf, Receiver<Result<(), String>>)>,
}

impl Updates {
    pub fn running(&self) -> bool {
        self.running.is_some()
    }
}

impl App {
    /// Ask GitHub, off the UI thread, whether a newer release is out. Release builds only.
    pub(crate) fn check_for_update(&mut self) {
        if install::build_commit().is_none() || self.updates.check.is_some() {
            return;
        }
        let (tx, rx) = crossbeam_channel::bounded(1);
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(install::update_available());
            ctx.request_repaint();
        });
        self.updates.check = Some(rx);
    }

    /// Download the latest release over the installed copy, then restart into it.
    pub(crate) fn start_update(&mut self) {
        let Some(target) = install::target() else {
            self.banner = Some(Banner::notice("Can't update: no home folder to install into."));
            return;
        };
        let (tx, rx) = crossbeam_channel::bounded(1);
        let ctx = self.ctx.clone();
        let t = target.clone();
        std::thread::spawn(move || {
            let _ = tx.send(install::update(&t));
            ctx.request_repaint();
        });
        self.updates.running = Some((target, rx));
        self.banner = Some(Banner::notice("Downloading the latest release…"));
    }

    /// Pick up the results of the update check and of an update.
    pub(crate) fn update_tick(&mut self) {
        if let Some(Ok(res)) = self.updates.check.as_ref().map(|rx| rx.try_recv()) {
            self.updates.check = None;
            match res {
                Ok(available) => {
                    self.updates.available = Some(available);
                    if available && self.banner.is_none() {
                        self.banner = Some(Banner { text: "A new version of CDeckJ is available.".into(), kind: BannerKind::UpdateAvailable });
                    }
                }
                // Usually offline. SETTINGS checks again when opened, and UPDATE still works.
                Err(e) => eprintln!("update check: {e}"),
            }
        }
        if let Some(Ok(res)) = self.updates.running.as_ref().map(|(_, rx)| rx.try_recv()) {
            let Some((target, _)) = self.updates.running.take() else { return };
            let msg = match res {
                Ok(()) => self.restart_into(&target, Offer::Update),
                Err(e) => format!("Update failed: {e}"),
            };
            self.banner = Some(Banner::notice(msg));
        }
    }

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

    /// Shut down and start `exe` (the installed copy) in this process's place, telling it what
    /// was `done`. Returns the error if it couldn't start.
    pub(crate) fn restart_into(&mut self, exe: &std::path::Path, done: Offer) -> String {
        self.shutdown();
        install::restart(exe, &[done.flag()])
    }

    /// Draw the install screen over `full`. Returns false once it's dismissed.
    pub(crate) fn install_screen(&mut self, ui: &mut Ui, full: Rect) -> bool {
        let Some(prompt) = self.install.as_mut() else { return false };
        if let Some(rx) = &prompt.job {
            match rx.try_recv() {
                Ok(Ok(())) => {
                    prompt.job = None;
                    let (target, done) = (prompt.target.clone(), prompt.offer);
                    let err = self.restart_into(&target, done);
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
        p.text(inner.min, Align2::LEFT_TOP, title, t::title(), t::INK);
        let g = p.layout(body, t::body(), t::INK_MUTED, inner.width());
        let body_h = g.size().y;
        p.galley(pos2(inner.min.x, inner.min.y + 50.0), g, t::INK_MUTED);
        let busy = prompt.job.is_some();
        let note = if busy {
            Some(("Installing…".to_string(), t::INK_MUTED))
        } else {
            prompt.error.clone().map(|e| (e, t::DANGER))
        };
        if let Some((n, col)) = note {
            let g = p.layout(n, t::small(), col, inner.width());
            p.galley(pos2(inner.min.x, inner.min.y + 64.0 + body_h), g, col);
        }
        let bw = 200.0;
        let go = Rect::from_min_size(pos2(inner.max.x - bw, inner.max.y - 48.0), vec2(bw, 48.0));
        let later = Rect::from_min_size(pos2(go.min.x - bw - 12.0, go.min.y), vec2(bw, 48.0));
        let clicked_go = button(ui, &mut self.touches, go, Id::NULL.with("inst_go"), action, Some(t::ACCENT), t::button_font()).clicked;
        let clicked_later = button(ui, &mut self.touches, later, Id::NULL.with("inst_later"), "NOT NOW", None, t::button_font()).clicked;
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
