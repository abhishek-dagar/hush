//! The page's font and image keys (spec 0002, Resources).
//!
//! A key is registered with the browser before the first list that uses it,
//! and freed one epoch after the last list that used it was replaced, so
//! the browser never draws a list whose resources are already gone.

use std::collections::BTreeMap;

use display_list::{DisplayList, Epoch, FontKey, ImageFormat, ImageKey, ItemKind, PageId};
use ipc::{SharedBytes, ToBrowser};

/// One key's use so far.
#[derive(Debug, Clone, Copy, Default)]
struct Entry {
    /// The epoch of the last sent list that named the key.
    last_used: Option<Epoch>,
    /// Its owner no longer needs it.
    released: bool,
}

impl Entry {
    /// Whether a released key can go once the list at `epoch` is sent: the
    /// list that last used it has been replaced, and one more epoch passed,
    /// so even a browser still showing the previous list never needs it.
    fn can_free(self, epoch: Epoch) -> bool {
        self.released
            && self
                .last_used
                .is_none_or(|last| epoch.0 >= last.0.saturating_add(2))
    }
}

/// The page's resource table: hands out keys, and queues the `Add*`
/// messages to send before a list and the `Delete*` messages to send after.
#[derive(Debug)]
pub struct ResourceTable {
    page: PageId,
    next_image: u32,
    fonts: BTreeMap<FontKey, Entry>,
    images: BTreeMap<ImageKey, Entry>,
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
            images: BTreeMap::new(),
            adds: Vec::new(),
            deletes: Vec::new(),
        }
    }

    /// Registers font `data` (face `index` of the file, at `variations`)
    /// under `key`, which the page's font context gave it (spec 0004, child
    /// 0004).
    pub fn add_font(&mut self, key: FontKey, data: &[u8], index: u32, variations: Vec<(u32, f32)>) {
        self.fonts.insert(key, Entry::default());
        self.adds.push(ToBrowser::AddFont {
            page: self.page,
            key,
            data: SharedBytes::from_bytes(data),
            index,
            variations,
        });
    }

    /// Registers an image and returns its key. The browser checks its size
    /// and data length, and refuses it if they are off.
    pub fn add_image(&mut self, size: (u32, u32), format: ImageFormat, pixels: &[u8]) -> ImageKey {
        let key = ImageKey(self.next_image);
        self.next_image = self.next_image.saturating_add(1);
        self.images.insert(key, Entry::default());
        self.adds.push(ToBrowser::AddImage {
            page: self.page,
            key,
            size,
            format,
            pixels: SharedBytes::from_bytes(pixels),
        });
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
        if let Some(entry) = self.images.get_mut(&key) {
            entry.released = true;
        }
    }

    /// Records the keys `list` names, then queues a delete for each
    /// released key that is now safe to free.
    pub fn list_painted(&mut self, list: &DisplayList) {
        for item in &list.items {
            match &item.kind {
                ItemKind::Text { font, .. } => {
                    if let Some(entry) = self.fonts.get_mut(font) {
                        entry.last_used = Some(list.epoch);
                    }
                }
                ItemKind::Image { key, .. } => {
                    if let Some(entry) = self.images.get_mut(key) {
                        entry.last_used = Some(list.epoch);
                    }
                }
                _ => {}
            }
        }
        let page = self.page;
        let epoch = list.epoch;
        self.fonts.retain(|&key, entry| {
            let keep = !entry.can_free(epoch);
            if !keep {
                self.deletes.push(ToBrowser::DeleteFont { page, key });
            }
            keep
        });
        self.images.retain(|&key, entry| {
            let keep = !entry.can_free(epoch);
            if !keep {
                self.deletes.push(ToBrowser::DeleteImage { page, key });
            }
            keep
        });
    }

    /// The `AddFont` and `AddImage` messages to send before the next list.
    pub fn take_adds(&mut self) -> Vec<ToBrowser> {
        std::mem::take(&mut self.adds)
    }

    /// The `DeleteFont` and `DeleteImage` messages to send after the list.
    pub fn take_deletes(&mut self) -> Vec<ToBrowser> {
        std::mem::take(&mut self.deletes)
    }
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
        table.add_font(font, b"font", 0, Vec::new());
        assert!(matches!(
            table.take_adds().as_slice(),
            [ToBrowser::AddFont { key, .. }] if *key == font
        ));

        table.list_painted(&list(1, Some(font)));
        table.release_font(font);
        table.list_painted(&list(2, None));
        assert!(
            table.take_deletes().is_empty(),
            "list 1 may still be on screen"
        );
        table.list_painted(&list(3, None));
        assert_eq!(
            table.take_deletes(),
            [ToBrowser::DeleteFont {
                page: PageId(1),
                key: font
            }]
        );
        table.list_painted(&list(4, None));
        assert!(table.take_deletes().is_empty(), "deleted once");
    }

    #[test]
    fn a_released_key_still_in_use_stays() {
        let mut table = ResourceTable::new(PageId(1));
        let font = FontKey(3);
        table.add_font(font, b"font", 0, Vec::new());
        table.release_font(font);
        for epoch in 1..5 {
            table.list_painted(&list(epoch, Some(font)));
        }
        assert!(table.take_deletes().is_empty());
        let image = table.add_image((1, 1), ImageFormat::Rgba8, &[0; 4]);
        table.release_image(image);
        table.list_painted(&list(5, None));
        assert!(matches!(
            table.take_deletes().as_slice(),
            [ToBrowser::DeleteImage { key, .. }] if *key == image
        ));
    }
}
