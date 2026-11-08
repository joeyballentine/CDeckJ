//! Installing, updating and removing CDeckJ from inside the app. Installing copies the running
//! executable to `~/Applications/cdeckj` (or the file CDeckJ's Steam shortcut already runs),
//! then sets up auto-launch, the applications-menu entry and Steam artwork.

use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::{autolaunch, desktop};

pub const LATEST_URL: &str = "https://github.com/joeyballentine/CDeckJ/releases/latest/download/cdeckj-steamdeck-x86_64.tar.gz";

/// What the start screen offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offer {
    Install,
    /// Another copy is installed: replace it with this one.
    Update,
}

/// The file CDeckJ's Steam shortcut runs, from `shortcuts.vdf`: the first quoted path ending
/// in `/cdeckj`.
pub fn shortcut_target_in(vdf: &[u8]) -> Option<PathBuf> {
    let needle = b"/cdeckj\"";
    let end = vdf.windows(needle.len()).position(|w| w == needle)? + needle.len() - 1;
    let start = vdf[..end].iter().rposition(|&b| b == b'"')? + 1;
    Some(PathBuf::from(String::from_utf8_lossy(&vdf[start..end]).into_owned()))
}

/// Where the installed copy lives: the file CDeckJ's Steam shortcut runs, if its folder is
/// still there, so the shortcut keeps working; else `~/Applications/cdeckj`.
pub fn target() -> Option<PathBuf> {
    let steam = autolaunch::shortcut_configs().into_iter().find_map(|c| std::fs::read(c.join("shortcuts.vdf")).ok().and_then(|d| shortcut_target_in(&d)));
    if let Some(t) = steam.filter(|t| t.parent().is_some_and(Path::is_dir)) {
        return Some(t);
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Applications/cdeckj"))
}

fn same_file(a: &Path, b: &Path) -> bool {
    matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b)
}

/// What to offer when CDeckJ runs from `exe`: nothing when it is the installed copy. Only
/// release builds ask, so `cargo run` doesn't.
pub fn offer(exe: &Path, target: &Path) -> Option<Offer> {
    if option_env!("CDECKJ_BUILD").is_none() || same_file(exe, target) {
        return None;
    }
    Some(if target.exists() { Offer::Update } else { Offer::Install })
}

/// Copy `exe` to `target` and set up auto-launch, the menu entry and Steam artwork. The copy
/// goes through a temporary file and a rename, so a copy that's running keeps running.
pub fn install(exe: &Path, target: &Path) -> Result<(), String> {
    let dir = target.parent().ok_or("no folder to install into")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    if !same_file(exe, target) {
        let tmp = dir.join(".cdeckj.new");
        std::fs::copy(exe, &tmp).map_err(|e| format!("copying to {}: {e}", dir.display()))?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, target).map_err(|e| format!("installing {}: {e}", target.display()))?;
    }
    // Left by the old install script: its menu action would run an updater that no longer works.
    let _ = std::fs::remove_file(dir.join("cdeckj-install.sh"));
    let _ = std::fs::remove_file(dir.join("cdeckj.png"));
    if let Err(e) = autolaunch::install_for(target) {
        eprintln!("install: {e}");
        desktop::install(target)?;
    }
    Ok(())
}

/// What to tell the user after installing.
pub fn installed_note(target: &Path) -> String {
    if autolaunch::in_steam(target) {
        "CDeckJ is installed. It opens when you plug in the controller, in Game Mode and Desktop Mode.".into()
    } else {
        "CDeckJ is installed. It's in the applications menu and opens when you plug in the controller. For Game Mode, add it in Steam: Games > Add a Non-Steam Game to My Library.".into()
    }
}

/// Download the latest release and install it over `target`.
pub fn update(target: &Path) -> Result<(), String> {
    let tmp = std::env::temp_dir().join(format!("cdeckj-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let result = (|| {
        let tar = tmp.join("cdeckj.tar.gz");
        let run = |c: &mut Command, what: &str| -> Result<(), String> {
            let out = c.output().map_err(|e| format!("couldn't run {what}: {e}"))?;
            if out.status.success() { Ok(()) } else { Err(format!("{what} failed: {}", String::from_utf8_lossy(&out.stderr).trim())) }
        };
        run(Command::new("curl").args(["-fsSL", "--retry", "3", "-o"]).arg(&tar).arg(LATEST_URL), "the download (are you online?)")?;
        run(Command::new("tar").arg("-xzf").arg(&tar).arg("-C").arg(&tmp), "unpacking the download")?;
        let exe = tmp.join("cdeckj-steamdeck/cdeckj");
        if !exe.is_file() {
            return Err("the download doesn't contain cdeckj".into());
        }
        install(&exe, target)
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

/// Remove auto-launch, the menu entry and the installed copy. Settings, saved data and
/// recordings stay.
pub fn uninstall(target: &Path) -> Result<String, String> {
    autolaunch::uninstall()?;
    desktop::uninstall()?;
    if target.exists() {
        std::fs::remove_file(target).map_err(|e| format!("removing {}: {e}", target.display()))?;
    }
    let mut msg = "CDeckJ is uninstalled. Your settings, cues, history and recordings are kept.".to_string();
    if autolaunch::in_steam(target) {
        msg.push_str(" Remove it from your Steam library too.");
    }
    Ok(msg)
}

/// Replace this process with `exe`, passing this one's arguments and `extra`.
pub fn restart(exe: &Path, extra: &[&str]) -> String {
    let args: Vec<String> = std::env::args().skip(1).filter(|a| a != "--installed").collect();
    let e = Command::new(exe).args(args).args(extra).exec();
    format!("couldn't start {}: {e}", exe.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_steam_shortcut_target() {
        let vdf = b"\x00shortcuts\x00\x000\x00\x02appid\x00\x01\x02\x03\x04\x01AppName\x00CDeckJ\x00\x01Exe\x00\"/home/deck/Apps/cdeckj\"\x00\x08\x08";
        assert_eq!(shortcut_target_in(vdf), Some(PathBuf::from("/home/deck/Apps/cdeckj")));
        assert_eq!(shortcut_target_in(b"\x01Exe\x00\"/usr/bin/steam\"\x00"), None);
    }
}
