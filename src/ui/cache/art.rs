//! Cover art: from a rekordbox stick's artwork files or embedded in audio files, decoded on a
//! worker thread into small textures and cached.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crossbeam_channel::{unbounded, Receiver, Sender};
use egui::{ColorImage, TextureHandle, TextureOptions};

/// Covers are decoded to this many pixels square: crisp at 2× for the deck header.
pub const SIZE: u32 = 96;
const MAX_TEXTURES: usize = 400;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ArtKey {
    /// An image file (rekordbox's `PIONEER/Artwork/…/a1.jpg`).
    File(PathBuf),
    /// The cover embedded in an audio file's tags.
    Embedded(PathBuf),
}

pub struct ArtCache {
    textures: HashMap<ArtKey, Option<TextureHandle>>,
    pending: HashSet<ArtKey>,
    order: Vec<ArtKey>,
    req: Sender<ArtKey>,
    done: Receiver<(ArtKey, Option<ColorImage>)>,
    #[cfg(test)]
    done_tx: Sender<(ArtKey, Option<ColorImage>)>,
}

impl ArtCache {
    pub fn new(repaint: impl Fn() + Send + 'static) -> ArtCache {
        let (req, rx) = unbounded::<ArtKey>();
        let (tx, done) = unbounded();
        #[cfg(test)]
        let done_tx = tx.clone();
        std::thread::Builder::new()
            .name("artwork".into())
            .spawn(move || {
                for key in rx {
                    let img = decode(&key);
                    if tx.send((key, img)).is_err() {
                        break;
                    }
                    repaint();
                }
            })
            .ok();
        ArtCache {
            textures: HashMap::new(),
            pending: HashSet::new(),
            order: Vec::new(),
            req,
            done,
            #[cfg(test)]
            done_tx,
        }
    }

    /// Hand over a decoded cover for `key`, as if the worker had read it. The screenshot
    /// test's WAV files can't carry artwork.
    #[cfg(test)]
    pub fn provide(&mut self, key: ArtKey, img: ColorImage) {
        self.pending.insert(key.clone());
        let _ = self.done_tx.send((key, Some(img)));
    }

    /// Upload finished decodes. Call once per frame.
    pub fn poll(&mut self, ctx: &egui::Context) {
        while let Ok((key, img)) = self.done.try_recv() {
            self.pending.remove(&key);
            let tex = img.map(|i| ctx.load_texture(format!("art{:?}", key), i, TextureOptions::LINEAR));
            self.textures.insert(key.clone(), tex);
            self.order.push(key);
            if self.order.len() > MAX_TEXTURES {
                let old = self.order.remove(0);
                self.textures.remove(&old);
            }
        }
    }

    /// The texture if ready; otherwise queue it (once) and return None.
    pub fn get(&mut self, key: &ArtKey) -> Option<&TextureHandle> {
        if !self.textures.contains_key(key) && self.pending.insert(key.clone()) {
            let _ = self.req.send(key.clone());
        }
        self.textures.get(key).and_then(|t| t.as_ref())
    }
}

fn decode(key: &ArtKey) -> Option<ColorImage> {
    let bytes = match key {
        ArtKey::File(p) => std::fs::read(p).ok()?,
        ArtKey::Embedded(p) => embedded(p)?,
    };
    let img = image::load_from_memory(&bytes).ok()?;
    let img = img.resize_to_fill(SIZE, SIZE, image::imageops::FilterType::Triangle).to_rgba8();
    Some(ColorImage::from_rgba_unmultiplied([SIZE as usize, SIZE as usize], img.as_raw()))
}

/// The front cover (or any picture) from an audio file's tags.
fn embedded(path: &Path) -> Option<Vec<u8>> {
    use symphonia::core::meta::StandardVisualKey;
    let mut format = crate::track::open_format(path).ok()?;
    let mut md = format.metadata();
    let rev = md.skip_to_latest()?;
    let visuals = &rev.media.visuals;
    let v = visuals.iter().find(|v| v.usage == Some(StandardVisualKey::FrontCover)).or(visuals.first())?;
    Some(v.data.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_a_file_and_skips_missing_ones() {
        let dir = std::env::temp_dir().join(format!("platter-art-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.png");
        image::RgbaImage::from_pixel(200, 100, image::Rgba([255, 0, 0, 255])).save(&p).unwrap();
        let img = decode(&ArtKey::File(p)).unwrap();
        assert_eq!(img.size, [96, 96]);
        assert_eq!(img.pixels[0], egui::Color32::from_rgb(255, 0, 0));
        assert!(decode(&ArtKey::File(dir.join("missing.jpg"))).is_none());
        assert!(decode(&ArtKey::Embedded(dir.join("missing.mp3"))).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}
