//! The app icon, the desktop menu entry and Steam library artwork.
//!
//! Art lives in `assets/icon` (drawn by `tools/make-icons.py`) and is built into the binary, so
//! a single `cdeckj` file can install its own icon, menu entry and Steam artwork.

use std::path::{Path, PathBuf};

/// Icon sizes for the freedesktop icon theme (hicolor), with their PNGs.
const ICONS: [(u32, &[u8]); 7] = [
    (16, include_bytes!("../assets/icon/cdeckj-16.png")),
    (32, include_bytes!("../assets/icon/cdeckj-32.png")),
    (48, include_bytes!("../assets/icon/cdeckj-48.png")),
    (64, include_bytes!("../assets/icon/cdeckj-64.png")),
    (128, include_bytes!("../assets/icon/cdeckj-128.png")),
    (256, include_bytes!("../assets/icon/cdeckj-256.png")),
    (512, include_bytes!("../assets/icon/cdeckj-512.png")),
];
const SVG: &[u8] = include_bytes!("../assets/icon/cdeckj.svg");

/// Steam's custom artwork for a non-Steam shortcut, by file-name suffix after the app id:
/// portrait capsule, wide capsule, hero banner, logo.
const STEAM_ART: [(&str, &[u8]); 4] = [
    ("p.png", include_bytes!("../assets/icon/steam-capsule.png")),
    (".png", include_bytes!("../assets/icon/steam-wide.png")),
    ("_hero.png", include_bytes!("../assets/icon/steam-hero.png")),
    ("_logo.png", include_bytes!("../assets/icon/steam-logo.png")),
];

/// The window icon.
pub fn window_icon() -> egui::IconData {
    let png = ICONS[5].1;
    match image::load_from_memory_with_format(png, image::ImageFormat::Png) {
        Ok(img) => {
            let rgba = img.into_rgba8();
            let (width, height) = rgba.dimensions();
            egui::IconData { rgba: rgba.into_raw(), width, height }
        }
        Err(_) => egui::IconData::default(),
    }
}

fn data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
}

pub fn entry_text(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=CDeckJ\n\
         GenericName=DJ Player\n\
         Comment=Play rekordbox USB sticks with a DDJ controller\n\
         Exec=\"{}\"\n\
         Icon=cdeckj\n\
         Terminal=false\n\
         Categories=AudioVideo;Audio;Player;\n\
         Keywords=DJ;rekordbox;CDJ;mixer;\n\
         StartupWMClass=cdeckj\n",
        exe.display()
    )
}

fn write_if_changed(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if std::fs::read(path).is_ok_and(|old| old == data) {
        return Ok(());
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(path, data)
}

/// Install the icon and a menu entry (Desktop Mode's launcher, and the list Steam shows in
/// "Add a Non-Steam Game"), then give any Steam shortcut its artwork.
pub fn install(exe: &Path) -> Result<String, String> {
    let share = data_home().ok_or("no home directory")?;
    let icons = share.join("icons/hicolor");
    for (size, png) in ICONS {
        write_if_changed(&icons.join(format!("{size}x{size}/apps/cdeckj.png")), png).map_err(|e| e.to_string())?;
    }
    write_if_changed(&icons.join("scalable/apps/cdeckj.svg"), SVG).map_err(|e| e.to_string())?;
    let entry = share.join("applications/cdeckj.desktop");
    write_if_changed(&entry, entry_text(exe).as_bytes()).map_err(|e| e.to_string())?;
    let art = steam_art(exe);
    let mut msg = "CDeckJ is in the applications menu, with its icon.".to_string();
    if art > 0 {
        msg.push_str(" Added Steam library artwork (restart Steam to see it).");
    }
    Ok(msg)
}

/// Give CDeckJ's Steam shortcut its library artwork, keeping any art already set. Returns how
/// many files were written.
pub fn steam_art(exe: &Path) -> usize {
    let mut n = 0;
    for (config, appid) in crate::autolaunch::shortcuts(exe) {
        let grid = config.join("grid");
        for (suffix, png) in STEAM_ART {
            let path = grid.join(format!("{appid}{suffix}"));
            if path.exists() {
                continue;
            }
            if std::fs::create_dir_all(&grid).is_ok() && std::fs::write(&path, png).is_ok() {
                n += 1;
            }
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_icon_is_the_rounded_tile() {
        let i = window_icon();
        assert_eq!((i.width, i.height), (256, 256));
        let px = |x: usize, y: usize| &i.rgba[(y * 256 + x) * 4..(y * 256 + x) * 4 + 4];
        assert_eq!(px(0, 0)[3], 0, "rounded corners are transparent");
        assert_eq!(px(128, 128)[3], 255, "centre opaque");
    }

    #[test]
    fn entry_runs_this_binary_with_the_icon() {
        let t = entry_text(Path::new("/home/deck/Applications/cdeckj"));
        assert!(t.contains("Exec=\"/home/deck/Applications/cdeckj\""));
        assert!(t.contains("Icon=cdeckj"));
        assert!(t.contains("StartupWMClass=cdeckj"));
    }

    #[test]
    fn steam_art_keeps_existing_files() {
        let dir = std::env::temp_dir().join(format!("cdeckj-art-{}", std::process::id()));
        let grid = dir.join("grid");
        std::fs::create_dir_all(&grid).unwrap();
        std::fs::write(grid.join("7p.png"), b"mine").unwrap();
        // Same writing logic as steam_art, on a known folder.
        for (suffix, png) in STEAM_ART {
            let p = grid.join(format!("7{suffix}"));
            if !p.exists() {
                std::fs::write(&p, png).unwrap();
            }
        }
        assert_eq!(std::fs::read(grid.join("7p.png")).unwrap(), b"mine");
        assert!(grid.join("7_hero.png").exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
