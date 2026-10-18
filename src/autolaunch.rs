//! Start CDeckJ when the DJ controller is plugged in.
//!
//! `cdeckj --install-autolaunch` installs a systemd *user* service (no root needed) that runs
//! `cdeckj --watch`. The watcher looks for the controller every two seconds in
//! `/proc/asound/cards` and the USB device list, and when one appears (and CDeckJ isn't
//! already running) it starts CDeckJ: through Steam when CDeckJ has been added as a
//! non-Steam game, which works in both Game Mode and Desktop Mode, otherwise directly.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const SERVICE: &str = "cdeckj-watch.service";
/// The service name before the rename (removed when installing).
const OLD_SERVICE: &str = "platter-watch.service";

/// Names of attached sound cards and USB devices, lowercased.
fn device_names(proc_cards: &Path, usb_devices: &Path) -> Vec<String> {
    let mut names = Vec::new();
    if let Ok(s) = std::fs::read_to_string(proc_cards) {
        names.extend(s.lines().map(|l| l.to_lowercase()));
    }
    if let Ok(rd) = std::fs::read_dir(usb_devices) {
        for e in rd.flatten() {
            for f in ["product", "manufacturer"] {
                if let Ok(s) = std::fs::read_to_string(e.path().join(f)) {
                    names.push(s.trim().to_lowercase());
                }
            }
        }
    }
    names
}

/// Whether any attached device matches one of the controller name hints.
pub fn controller_present_in(hints: &[String], proc_cards: &Path, usb_devices: &Path) -> bool {
    let names = device_names(proc_cards, usb_devices);
    hints.iter().map(|h| h.to_lowercase().replace([' ', '-'], "")).filter(|h| !h.is_empty()).any(|h| {
        names.iter().any(|n| n.replace([' ', '-'], "").contains(&h))
    })
}

fn controller_present(hints: &[String]) -> bool {
    controller_present_in(hints, Path::new("/proc/asound/cards"), Path::new("/sys/bus/usb/devices"))
}

/// Another CDeckJ (not a watcher) is running.
fn platter_running() -> bool {
    let me = std::process::id();
    let Ok(rd) = std::fs::read_dir("/proc") else { return false };
    rd.flatten().any(|e| {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { return false };
        if pid == me {
            return false;
        }
        let cmd = std::fs::read(e.path().join("cmdline")).unwrap_or_default();
        let mut args = cmd.split(|&b| b == 0).filter(|a| !a.is_empty());
        let exe = args.next().map(|a| String::from_utf8_lossy(a).into_owned()).unwrap_or_default();
        let is_platter = Path::new(&exe).file_name().is_some_and(|n| n == "cdeckj" || n == "platter");
        is_platter && !args.any(|a| a == b"--watch")
    })
}

// ---------------------------------------------------------------------------------------------
// Steam shortcut lookup (binary VDF in userdata/*/config/shortcuts.vdf)
// ---------------------------------------------------------------------------------------------

/// The app id of a non-Steam shortcut whose target is `exe` (or any `cdeckj`).
pub fn shortcut_appid(vdf: &[u8], exe: &Path) -> Option<u32> {
    let exe_s = exe.to_string_lossy().to_lowercase();
    let mut i = 0;
    let mut appid: Option<u32> = None;
    let mut target: Option<String> = None;
    let mut best: Option<(u8, u32)> = None;
    let cstr = |i: &mut usize| -> Option<String> {
        let end = vdf.get(*i..)?.iter().position(|&b| b == 0)? + *i;
        let s = String::from_utf8_lossy(&vdf[*i..end]).into_owned();
        *i = end + 1;
        Some(s)
    };
    let mut consider = |appid: Option<u32>, target: &Option<String>| {
        if let (Some(id), Some(t)) = (appid, target) {
            let t = t.trim_matches('"').to_lowercase();
            let score = if t == exe_s { 2 } else if Path::new(&t).file_name().is_some_and(|n| n == "cdeckj" || n == "platter") { 1 } else { 0 };
            if score > 0 && best.is_none_or(|(s, _)| score > s) {
                best = Some((score, id));
            }
        }
    };
    while i < vdf.len() {
        let ty = vdf[i];
        i += 1;
        match ty {
            0x00 => {
                // A nested map: a new shortcut entry starts.
                let _ = cstr(&mut i)?;
                consider(appid.take(), &target);
                target = None;
            }
            0x01 => {
                let key = cstr(&mut i)?.to_lowercase();
                let val = cstr(&mut i)?;
                if key == "exe" {
                    target = Some(val);
                }
            }
            0x02 => {
                let key = cstr(&mut i)?.to_lowercase();
                let v = u32::from_le_bytes(vdf.get(i..i + 4)?.try_into().ok()?);
                i += 4;
                if key == "appid" {
                    appid = Some(v);
                }
            }
            0x08 => consider(appid.take(), &target),
            _ => return best.map(|b| b.1),
        }
    }
    consider(appid, &target);
    best.map(|b| b.1)
}

/// Steam's 64-bit game id for a non-Steam shortcut, as used by `steam://rungameid/`.
pub fn rungameid(appid: u32) -> u64 {
    ((appid as u64) << 32) | 0x0200_0000
}

/// Every Steam user that has CDeckJ as a non-Steam game: (their `config` folder, app id).
pub fn shortcuts(exe: &Path) -> Vec<(PathBuf, u32)> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return Vec::new() };
    // ~/.steam/steam is usually a link to ~/.local/share/Steam: count each folder once.
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for root in [home.join(".local/share/Steam/userdata"), home.join(".steam/steam/userdata")] {
        let Ok(rd) = std::fs::read_dir(root) else { continue };
        for user in rd.flatten() {
            let config = user.path().join("config");
            let key = config.canonicalize().unwrap_or_else(|_| config.clone());
            if !seen.insert(key) {
                continue;
            }
            if let Ok(d) = std::fs::read(config.join("shortcuts.vdf")) {
                if let Some(id) = shortcut_appid(&d, exe) {
                    out.push((config, id));
                }
            }
        }
    }
    out
}

fn find_shortcut(exe: &Path) -> Option<u32> {
    shortcuts(exe).first().map(|s| s.1)
}

fn launch(exe: &Path) {
    if let Some(id) = find_shortcut(exe) {
        let url = format!("steam://rungameid/{}", rungameid(id));
        eprintln!("watch: controller found, starting CDeckJ through Steam ({url})");
        if Command::new("steam").arg(&url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().is_ok() {
            return;
        }
    }
    // Not added to Steam: start it straight into the desktop session.
    eprintln!("watch: controller found, starting {}", exe.display());
    let mut c = Command::new(exe);
    c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        c.env("DISPLAY", ":0");
        if let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR") {
            if Path::new(&rt).join("wayland-0").exists() {
                c.env("WAYLAND_DISPLAY", "wayland-0");
            }
        }
    }
    if let Err(e) = c.spawn() {
        eprintln!("watch: couldn't start CDeckJ: {e}");
    }
}

/// `cdeckj --watch`: start CDeckJ each time the controller is plugged in.
pub fn watch(hints: &[String]) -> ! {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("cdeckj"));
    eprintln!("watch: waiting for a controller matching {hints:?}");
    // Already plugged in when the watcher starts (e.g. at login): launch too.
    let mut was_present = false;
    loop {
        let present = controller_present(hints);
        if present && !was_present && !platter_running() {
            launch(&exe);
        }
        was_present = present;
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn unit_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("systemd/user").join(SERVICE))
}

fn unit_text(exe: &Path) -> String {
    format!(
        "[Unit]\n\
         Description=Start CDeckJ when a DJ controller is plugged in\n\
         \n\
         [Service]\n\
         ExecStart=\"{}\" --watch\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        exe.display()
    )
}

fn systemctl(args: &[&str]) -> Result<(), String> {
    let out = Command::new("systemctl").arg("--user").args(args).output().map_err(|e| format!("couldn't run systemctl: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "couldn't set up the background service (systemctl --user {}): {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Whether the auto-launch service is installed.
pub fn is_installed() -> bool {
    unit_path().is_some_and(|p| p.exists())
}

/// `cdeckj --install-autolaunch`
/// Remove the auto-launch service from before the rename to CDeckJ, if it's there.
fn remove_old_service() {
    let _ = systemctl(&["disable", "--now", OLD_SERVICE]);
    if let Some(p) = unit_path().and_then(|p| p.parent().map(|d| d.join(OLD_SERVICE))) {
        let _ = std::fs::remove_file(p);
    }
}

pub fn install() -> Result<String, String> {
    remove_old_service();
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let path = unit_path().ok_or("no home directory")?;
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(&path, unit_text(&exe)).map_err(|e| format!("writing {}: {e}", path.display()))?;
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", SERVICE])?;
    // Restart rather than start, so a reinstall or an updated binary takes over.
    systemctl(&["restart", SERVICE])?;
    let desktop = crate::desktop::install(&exe).map(|m| format!("{m}\n")).unwrap_or_else(|e| format!("(Couldn't add the menu entry: {e})\n"));
    let steam = if find_shortcut(&exe).is_some() {
        "CDeckJ is in Steam, so it will open in Game Mode and Desktop Mode.".to_string()
    } else {
        "CDeckJ isn't in Steam yet: it will open in Desktop Mode only. Add it with \"Add a Non-Steam Game\" for Game Mode too (no need to reinstall this).".to_string()
    };
    Ok(format!("Auto-launch is on: CDeckJ starts when your controller is plugged in.\n{desktop}{steam}\nIf you move the cdeckj file, run install.sh (or --install-autolaunch) again. Turn it off with --uninstall-autolaunch."))
}

/// `cdeckj --uninstall-autolaunch`
pub fn uninstall() -> Result<String, String> {
    remove_old_service();
    let _ = systemctl(&["disable", "--now", SERVICE]);
    if let Some(p) = unit_path() {
        let _ = std::fs::remove_file(p);
    }
    let _ = systemctl(&["daemon-reload"]);
    Ok("Auto-launch is off.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vdf(entries: &[(u32, &str)]) -> Vec<u8> {
        let mut v = vec![0x00];
        v.extend_from_slice(b"shortcuts\0");
        for (i, (id, exe)) in entries.iter().enumerate() {
            v.push(0x00);
            v.extend_from_slice(format!("{i}\0").as_bytes());
            v.push(0x02);
            v.extend_from_slice(b"appid\0");
            v.extend_from_slice(&id.to_le_bytes());
            v.push(0x01);
            v.extend_from_slice(b"AppName\0Something\0");
            v.push(0x01);
            v.extend_from_slice(format!("Exe\0\"{exe}\"\0").as_bytes());
            v.push(0x00);
            v.extend_from_slice(b"tags\0");
            v.push(0x08);
            v.push(0x08);
        }
        v.extend_from_slice(&[0x08, 0x08]);
        v
    }

    #[test]
    fn finds_the_platter_shortcut() {
        let d = vdf(&[(111, "/usr/bin/firefox"), (3_000_000_123, "/home/deck/Applications/cdeckj")]);
        assert_eq!(shortcut_appid(&d, Path::new("/home/deck/Applications/cdeckj")), Some(3_000_000_123));
        // A copy elsewhere still matches by name.
        assert_eq!(shortcut_appid(&d, Path::new("/tmp/x/cdeckj")), Some(3_000_000_123));
        assert_eq!(shortcut_appid(&vdf(&[(1, "/usr/bin/vlc")]), Path::new("/a/cdeckj")), None);
        assert_eq!(shortcut_appid(&[0x00, 0x01], Path::new("/a/cdeckj")), None);
        assert_eq!(rungameid(3_000_000_123), (3_000_000_123u64 << 32) | 0x0200_0000);
    }

    #[test]
    fn detects_the_controller_by_card_or_usb_name() {
        let dir = std::env::temp_dir().join(format!("platter-al-{}", std::process::id()));
        let usb = dir.join("usb");
        std::fs::create_dir_all(usb.join("1-1")).unwrap();
        let cards = dir.join("cards");
        std::fs::write(&cards, " 0 [Generic        ]: HDA-Intel - HD-Audio Generic\n").unwrap();
        let hints = vec!["GRV6".to_string(), "DDJ".to_string()];
        assert!(!controller_present_in(&hints, &cards, &usb));
        std::fs::write(usb.join("1-1/product"), "DDJ-GRV6\n").unwrap();
        assert!(controller_present_in(&hints, &cards, &usb));
        std::fs::remove_file(usb.join("1-1/product")).unwrap();
        std::fs::write(&cards, " 1 [DDJGRV6        ]: USB-Audio - DDJ-GRV6\n").unwrap();
        assert!(controller_present_in(&hints, &cards, &usb));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unit_runs_this_binary_in_watch_mode() {
        let u = unit_text(Path::new("/home/deck/Applications/cdeckj"));
        assert!(u.contains("ExecStart=\"/home/deck/Applications/cdeckj\" --watch"));
        assert!(u.contains("WantedBy=default.target"));
    }
}
