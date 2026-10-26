//! The page's font and image keys (spec 0002, Resources).
//!
//! A key is registered with the browser before the first list that uses it,
//! and freed one epoch after the last list that used it was replaced, so
//! the browser never draws a list whose resources are already gone.
//!
//! Fonts are named, not copied, when they come from a system file, and a web
//! font file's bytes are sent once however many faces use them (spec 0032).
//! An image's pixels wait here until the next list shows how large it is
//! drawn, so a needlessly large upload can be downscaled (spec 0032, AC-8).

use std::collections::{BTreeMap, HashMap, HashSet};

use display_list::{DisplayList, Epoch, FontKey, ImageFormat, ImageKey, ItemKind, PageId};
use ipc::{FontSource, SharedBytes, ToBrowser};
use text::FaceSource;

/// The most pixels an image may have before it is worth downscaling (spec
/// 0032, AC-8).
const DOWNSCALE_PIXELS: u64 = 4_000_000;

/// One key's use so far.
#[derive(Debug, Clone, Copy, Default)]
struct Entry {
    /// The epoch of the last sent list that named the key.
    last_used: Option<Epoch>,
    /// Its owner no longer needs it.
    released: bool,
}

impl Entry {
    /// Whether a released key can go once the list at `epoch` is sent. A key
    /// some list used waits until one more epoch has passed since that list
    /// was replaced, so even a browser still showing the previous list never
    /// needs it. A key no list ever used cannot be on screen at all, so it
    /// can go as soon as the list being sent does not name it.
    fn can_free(self, epoch: Epoch, named_by_current: bool) -> bool {
        if !self.released {
            return false;
        }
        match self.last_used {
            Some(last) => epoch.0 >= last.0.saturating_add(2),
            None => !named_by_current,
        }
    }
}

/// One image's bytes and where it was drawn.
#[derive(Debug)]
struct ImageRecord {
    /// Release bookkeeping.
    entry: Entry,
    /// The image's decoded size, in pixels.
    natural: (u32, u32),
    /// How the pixels are laid out.
    format: ImageFormat,
    /// The decoded pixels, before their first upload.
    pixels: Option<Vec<u8>>,
    /// The encoded bytes, kept only when the upload was downscaled, so a
    /// larger layout can decode it again (spec 0032, AC-8).
    encoded: Option<Vec<u8>>,
    /// The largest size it was drawn at, in device px.
    drawn: (u32, u32),
    /// The size last uploaded, in device px.
    uploaded: Option<(u32, u32)>,
}

/// The page's resource table: hands out keys, and queues the `Add*`
/// messages to send before a list and the `Delete*` messages to send after.
#[derive(Debug)]
pub struct ResourceTable {
    page: PageId,
    next_image: u32,
    fonts: BTreeMap<FontKey, Entry>,
    /// The first key sent for each web font blob, so a later face of the
    /// same file names its bytes instead of sending them again (spec 0032,
    /// AC-4).
    font_blobs: HashMap<u64, FontKey>,
    images: BTreeMap<ImageKey, ImageRecord>,
    adds: Vec<ToBrowser>,
    deletes: Vec<ToBrowser>,
}

impl ResourceTable {
    /// An empty table for `page`.
    pub fn new(page: PageId) -> Self {
        Self {
            page,
            next_image: 0,
            fonts: BTreeMap::new(),
            font_blobs: HashMap::new(),
            images: BTreeMap::new(),
            adds: Vec::new(),
            deletes: Vec::new(),
        }
    }

    /// Registers font `data` (face `index` of the file, at `variations`)
    /// under `key`, which the page's font context gave it (spec 0004, child
    /// 0004). A system face is named by its file, and a web face's later
    /// instances name the first key's bytes (spec 0032, AC-1, AC-4).
    pub fn add_font(
        &mut self,
        key: FontKey,
        source: FaceSource,
        data: &[u8],
        index: u32,
        variations: Vec<(u32, f32)>,
    ) {
        let source = match source {
            FaceSource::System(path) => match path.to_str() {
                Some(path) => FontSource::System {
                    path: path.to_owned(),
                },
                // A path that is not UTF 8 cannot cross as a string; send
                // the bytes instead (spec 0032, FontSource).
                None => FontSource::Bytes(SharedBytes::from_bytes(data)),
            },
            FaceSource::Web(blob) => match self.font_blobs.get(&blob) {
                Some(&first) => FontSource::SameBytesAs(first),
                None => {
                    self.font_blobs.insert(blob, key);
                    FontSource::Bytes(SharedBytes::from_bytes(data))
                }
            },
        };
        self.fonts.insert(key, Entry::default());
        self.adds.push(ToBrowser::AddFont {
            page: self.page,
            key,
            source,
            index,
            variations,
        });
    }

    /// Registers an image and returns its key. `pixels` are its decoded
    /// RGBA8 rows; `encoded` are the bytes they came from, kept only if the
    /// upload is downscaled (spec 0032, AC-8). The browser checks the size
    /// and data length, and refuses it if they are off.
    pub fn add_image(
        &mut self,
        size: (u32, u32),
        format: ImageFormat,
        pixels: Vec<u8>,
        encoded: Vec<u8>,
    ) -> ImageKey {
        let key = ImageKey(self.next_image);
        self.next_image = self.next_image.saturating_add(1);
        self.images.insert(
            key,
            ImageRecord {
                entry: Entry::default(),
                natural: size,
                format,
                pixels: Some(pixels),
                encoded: Some(encoded),
                drawn: (0, 0),
                uploaded: None,
            },
        );
        key
    }

    /// The font's owner is done with it; it is freed once no list on
    /// screen can still name it.
    pub fn release_font(&mut self, key: FontKey) {
        if let Some(entry) = self.fonts.get_mut(&key) {
            entry.released = true;
        }
    }

    /// As [`ResourceTable::release_font`], for an image.
    pub fn release_image(&mut self, key: ImageKey) {
        if let Some(record) = self.images.get_mut(&key) {
            record.entry.released = true;
        }
    }

    /// Records the keys `list` names at `scale` device pixels per CSS px,
    /// queues a delete for each released key that is now safe to free, and
    /// re-uploads an image this list draws larger than the last one did.
    pub fn list_painted(&mut self, list: &DisplayList, scale: f32) {
        // Free first, from the uses recorded for earlier lists: a key is
        // one epoch past the list that last used it (see [`Entry::can_free`]),
        // so this list's own uses must not count yet. The keys this list
        // names are gathered up front, so a key released before its first
        // paint is still kept for the list that uses it.
        let page = self.page;
        let epoch = list.epoch;
        let mut named_fonts = HashSet::new();
        let mut named_images = HashSet::new();
        for item in &list.items {
            match &item.kind {
                ItemKind::Text { font, .. } => {
                    named_fonts.insert(*font);
                }
                ItemKind::Image { key, .. } => {
                    named_images.insert(*key);
                }
                _ => {}
            }
        }
        let mut freed_fonts = Vec::new();
        self.fonts.retain(|&key, entry| {
            let keep = !entry.can_free(epoch, named_fonts.contains(&key));
            if !keep {
                self.deletes.push(ToBrowser::DeleteFont { page, key });
                freed_fonts.push(key);
            }
            keep
        });
        if !freed_fonts.is_empty() {
            self.font_blobs
                .retain(|_, first| !freed_fonts.contains(first));
        }
        self.images.retain(|&key, record| {
            let keep = !record.entry.can_free(epoch, named_images.contains(&key));
            if !keep {
                self.deletes.push(ToBrowser::DeleteImage { page, key });
            }
            keep
        });
        for item in &list.items {
            match &item.kind {
                ItemKind::Text { font, .. } => {
                    if let Some(entry) = self.fonts.get_mut(font) {
                        entry.last_used = Some(epoch);
                    }
                }
                ItemKind::Image { key, rect } => {
                    if let Some(record) = self.images.get_mut(key) {
                        record.entry.last_used = Some(epoch);
                        let drawn = drawn_size(rect.size.width, rect.size.height, scale);
                        record.drawn = (record.drawn.0.max(drawn.0), record.drawn.1.max(drawn.1));
                    }
                }
                _ => {}
            }
        }
        let grown: Vec<ImageKey> = self
            .images
            .iter()
            .filter(|(_, record)| record.uploaded.is_some() && record.encoded.is_some())
            .filter(|(_, record)| {
                let target = upload_size(record, record.drawn);
                record
                    .uploaded
                    .is_some_and(|sent| target.0 > sent.0 || target.1 > sent.1)
            })
            .map(|(&key, _)| key)
            .collect();
        for key in grown {
            self.upload(key, true);
        }
    }

    /// The `AddFont` and `AddImage` messages to send before the next list.
    /// Pending image pixels go out now, at the size the last list drew them.
    pub fn take_adds(&mut self) -> Vec<ToBrowser> {
        let pending: Vec<ImageKey> = self
            .images
            .iter()
            .filter(|(_, record)| record.pixels.is_some())
            .map(|(&key, _)| key)
            .collect();
        for key in pending {
            self.upload(key, false);
        }
        std::mem::take(&mut self.adds)
    }

    /// The `DeleteFont` and `DeleteImage` messages to send after the list.
    pub fn take_deletes(&mut self) -> Vec<ToBrowser> {
        std::mem::take(&mut self.deletes)
    }

    /// Sends one image, downscaling it when every draw is small enough
    /// (spec 0032, AC-8). `reupload` decodes the kept encoded bytes again,
    /// because a later list draws it larger than the first upload.
    fn upload(&mut self, key: ImageKey, reupload: bool) {
        let Some(record) = self.images.get_mut(&key) else {
            return;
        };
        let natural = record.natural;
        let target = upload_size(record, record.drawn);
        let downscaled = target != natural;
        let pixels = if reupload {
            let Some(encoded) = record.encoded.as_deref() else {
                return;
            };
            let Ok(decoded) = image::decode(encoded, &image::Limits::default()) else {
                tracing::warn!(key = key.0, "kept image bytes did not decode again");
                record.encoded = None;
                return;
            };
            decoded.pixels
        } else {
            let Some(pixels) = record.pixels.take() else {
                return;
            };
            pixels
        };
        let bytes = if downscaled {
            image::downscale(&pixels, natural.0, natural.1, target.0, target.1)
        } else {
            pixels
        };
        if !downscaled {
            record.encoded = None;
        }
        record.uploaded = Some(target);
        let format = record.format;
        self.adds.push(ToBrowser::AddImage {
            page: self.page,
            key,
            size: target,
            format,
            pixels: SharedBytes::from_bytes(&bytes),
        });
    }
}

/// The size (device px) an image should upload at: the largest size it is
/// drawn at, but only when that is small enough to be worth downscaling;
/// otherwise its natural pixels (spec 0032, AC-8).
fn upload_size(record: &ImageRecord, drawn: (u32, u32)) -> (u32, u32) {
    let natural = record.natural;
    // Nothing drew it yet: send its own pixels rather than guess a size.
    if drawn.0 == 0 || drawn.1 == 0 {
        return natural;
    }
    let drawn = (drawn.0.clamp(1, natural.0), drawn.1.clamp(1, natural.1));
    if should_downscale(natural, drawn) {
        drawn
    } else {
        natural
    }
}

/// Whether an image of `natural` pixels, drawn at `drawn` device pixels,
/// should upload downscaled: more than [`DOWNSCALE_PIXELS`] natural pixels,
/// and at most half the natural side on both axes.
fn should_downscale(natural: (u32, u32), drawn: (u32, u32)) -> bool {
    let pixels = u64::from(natural.0) * u64::from(natural.1);
    pixels > DOWNSCALE_PIXELS
        && u64::from(drawn.0) * 2 <= u64::from(natural.0)
        && u64::from(drawn.1) * 2 <= u64::from(natural.1)
}

/// A drawn rectangle in device pixels, rounded up and at least one.
fn drawn_size(width: f32, height: f32, scale: f32) -> (u32, u32) {
    let side = |value: f32| {
        if value.is_finite() && value > 0.0 {
            (value * scale).ceil().max(1.0) as u32
        } else {
            1
        }
    };
    (side(width), side(height))
}

#[cfg(test)]
mod tests {
    use display_list::{ColorF, DisplayItem, SizeF, SpatialId};

    use super::*;

    fn list(epoch: u64, font: Option<FontKey>) -> DisplayList {
        let mut list = DisplayList::new(PageId(1), Epoch(epoch), SizeF::default());
        if let Some(font) = font {
            list.items.push(DisplayItem {
                spatial: SpatialId::ROOT,
                clip: None,
                kind: ItemKind::Text {
                    font,
                    size: 12.0,
                    color: ColorF::default(),
                    glyphs: Vec::new(),
                },
            });
        }
        list
    }

    #[test]
    fn a_key_is_freed_one_epoch_after_its_last_list_was_replaced() {
        let mut table = ResourceTable::new(PageId(1));
        let font = FontKey(0);
        table.add_font(font, FaceSource::Web(7), b"font", 0, Vec::new());
        assert!(matches!(
            table.take_adds().as_slice(),
            [ToBrowser::AddFont { key, .. }] if *key == font
        ));
        table.release_font(font);
        table.list_painted(&list(1, Some(font)), 1.0);
        assert!(table.take_deletes().is_empty());
        table.list_painted(&list(3, Some(font)), 1.0);
        assert!(matches!(
            table.take_deletes().as_slice(),
            [ToBrowser::DeleteFont { key, .. }] if *key == font
        ));
    }

    #[test]
    fn a_released_key_still_in_use_stays() {
        let mut table = ResourceTable::new(PageId(1));
        let font = FontKey(0);
        table.add_font(font, FaceSource::Web(7), b"font", 0, Vec::new());
        let _ = table.take_adds();
        table.release_font(font);
        table.list_painted(&list(1, Some(font)), 1.0);
        table.list_painted(&list(2, Some(font)), 1.0);
        assert!(table.take_deletes().is_empty());
    }

    #[test]
    fn a_released_key_no_list_ever_used_is_freed_at_once() {
        // A web font registered and dropped before its first paint is not on
        // any list the browser can show, so it must not stay forever: the
        // entry has no `last_used` to wait one epoch from (spec 0032).
        let mut table = ResourceTable::new(PageId(1));
        let font = FontKey(0);
        table.add_font(font, FaceSource::Web(7), b"font", 0, Vec::new());
        let _ = table.take_adds();
        table.release_font(font);
        let key = table.add_image((2, 2), ImageFormat::Rgba8, vec![1; 16], vec![2; 4]);
        let _ = table.take_adds();
        table.release_image(key);
        table.list_painted(&list(1, None), 1.0);
        let deletes = table.take_deletes();
        assert!(matches!(
            deletes.as_slice(),
            [ToBrowser::DeleteFont { key: f, .. }, ToBrowser::DeleteImage { key: k, .. }]
                if *f == font && *k == key
        ));
    }

    #[test]
    fn a_released_key_the_next_list_names_is_kept_for_it() {
        // Released before the first list that names it: that list is about
        // to be sent, so the key goes one epoch later, not now.
        let mut table = ResourceTable::new(PageId(1));
        let font = FontKey(0);
        table.add_font(font, FaceSource::Web(7), b"font", 0, Vec::new());
        let _ = table.take_adds();
        table.release_font(font);
        table.list_painted(&list(1, Some(font)), 1.0);
        assert!(table.take_deletes().is_empty());
        table.list_painted(&list(3, None), 1.0);
        assert!(matches!(
            table.take_deletes().as_slice(),
            [ToBrowser::DeleteFont { key, .. }] if *key == font
        ));
    }

    #[test]
    fn a_web_fonts_later_faces_name_the_first_key() {
        let mut table = ResourceTable::new(PageId(1));
        table.add_font(FontKey(0), FaceSource::Web(9), b"one", 0, Vec::new());
        table.add_font(FontKey(1), FaceSource::Web(9), b"one", 0, Vec::new());
        table.add_font(FontKey(2), FaceSource::Web(10), b"two", 0, Vec::new());
        let adds = table.take_adds();
        assert!(matches!(
            adds.as_slice(),
            [
                ToBrowser::AddFont {
                    source: FontSource::Bytes(_),
                    ..
                },
                ToBrowser::AddFont {
                    source: FontSource::SameBytesAs(first),
                    ..
                },
                ToBrowser::AddFont {
                    source: FontSource::Bytes(_),
                    ..
                },
            ] if *first == FontKey(0)
        ));
    }

    #[test]
    fn only_small_drawings_of_large_images_downscale() {
        // 4000 × 3000 is 12 million pixels: over the limit.
        assert!(should_downscale((4000, 3000), (1600, 1200)));
        // Exactly half on both sides still counts.
        assert!(should_downscale((4000, 3000), (2000, 1500)));
        // One side over half does not.
        assert!(!should_downscale((4000, 3000), (2001, 1200)));
        // A small image is never downscaled.
        assert!(!should_downscale((1000, 1000), (100, 100)));
    }

    #[test]
    fn image_pixels_wait_until_the_next_list() {
        let mut table = ResourceTable::new(PageId(1));
        let key = table.add_image((2, 2), ImageFormat::Rgba8, vec![1; 16], vec![2; 4]);
        assert!(table.adds.is_empty());
        let adds = table.take_adds();
        assert!(matches!(
            adds.as_slice(),
            [ToBrowser::AddImage { key: k, size: (2, 2), .. }] if *k == key
        ));
    }
}
