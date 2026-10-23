//! The browser side checks on every list and resource message the content
//! process sends (spec 0002, Security model, AC-20).
//!
//! The content process runs untrusted page code, so nothing it sends is
//! trusted: a list that fails any check is dropped whole, and the renderer
//! never indexes with a value these checks did not see.

use std::collections::HashSet;
use std::fmt;

use crate::{
    DisplayList, FontKey, ImageFormat, ImageKey, ItemKind, MAX_IMAGE_SIDE, PageId, ScrollId,
    SpatialKind,
};

/// Why a list or a resource message was rejected.
///
/// Written by hand rather than with `thiserror`, because this crate depends
/// on nothing but `serde` (spec 0002, Crates and who depends on whom).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListError {
    /// The list belongs to another page.
    WrongPage(PageId),
    /// The first spatial node is missing, has a parent, or is not `Root`.
    BadRoot,
    /// A spatial node other than the first is a `Root`, has no parent, or
    /// its parent is not an earlier node.
    SpatialParent(usize),
    /// A clip's parent is not an earlier clip.
    ClipParent(usize),
    /// A clip names a spatial node that does not exist.
    ClipSpatial(usize),
    /// An item names a spatial node that does not exist.
    ItemSpatial(usize),
    /// An item names a clip that does not exist.
    ItemClip(usize),
    /// An item names a font that was never registered.
    UnknownFont(FontKey),
    /// An item names an image that was never registered.
    UnknownImage(ImageKey),
    /// Two scroll frames share an id.
    DuplicateScrollId(ScrollId),
    /// A stacking context pop with no push, or a push left open.
    UnbalancedStackingContext(usize),
    /// An image is wider or taller than [`MAX_IMAGE_SIDE`].
    ImageTooLarge {
        /// Its width in pixels.
        width: u32,
        /// Its height in pixels.
        height: u32,
    },
    /// An image's pixel data does not match its size and format.
    ImageDataLength {
        /// How many bytes the size and format need.
        expected: usize,
        /// How many arrived.
        actual: usize,
    },
}

impl fmt::Display for ListError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongPage(page) => write!(f, "list for another page ({})", page.0),
            Self::BadRoot => f.write_str("the first spatial node is not a parentless root"),
            Self::SpatialParent(i) => write!(f, "spatial node {i} has a bad parent"),
            Self::ClipParent(i) => write!(f, "clip {i} has a bad parent"),
            Self::ClipSpatial(i) => write!(f, "clip {i} names a missing spatial node"),
            Self::ItemSpatial(i) => write!(f, "item {i} names a missing spatial node"),
            Self::ItemClip(i) => write!(f, "item {i} names a missing clip"),
            Self::UnknownFont(key) => write!(f, "unregistered font key {}", key.0),
            Self::UnknownImage(key) => write!(f, "unregistered image key {}", key.0),
            Self::DuplicateScrollId(id) => write!(f, "scroll id {id:?} appears twice"),
            Self::UnbalancedStackingContext(i) => {
                write!(f, "stacking contexts unbalanced at item {i}")
            }
            Self::ImageTooLarge { width, height } => {
                write!(
                    f,
                    "image {width}x{height} is over {MAX_IMAGE_SIDE}px a side"
                )
            }
            Self::ImageDataLength { expected, actual } => {
                write!(f, "image data is {actual} bytes, expected {expected}")
            }
        }
    }
}

impl std::error::Error for ListError {}

/// The font and image keys one page registered, kept by the browser side.
#[derive(Debug, Default)]
pub struct ResourceKeys {
    fonts: HashSet<FontKey>,
    images: HashSet<ImageKey>,
}

impl ResourceKeys {
    /// Registers a font.
    pub fn add_font(&mut self, key: FontKey) {
        self.fonts.insert(key);
    }

    /// Registers an image after checking its size and data length
    /// (spec 0002, Security model).
    pub fn add_image(
        &mut self,
        key: ImageKey,
        (width, height): (u32, u32),
        format: ImageFormat,
        data_len: usize,
    ) -> Result<(), ListError> {
        if width > MAX_IMAGE_SIDE || height > MAX_IMAGE_SIDE {
            return Err(ListError::ImageTooLarge { width, height });
        }
        // Both sides are at most 16,384, so this cannot overflow a usize.
        let expected = width as usize * height as usize * format.bytes_per_pixel();
        if data_len != expected {
            return Err(ListError::ImageDataLength {
                expected,
                actual: data_len,
            });
        }
        self.images.insert(key);
        Ok(())
    }

    /// Frees a font key.
    pub fn delete_font(&mut self, key: FontKey) {
        self.fonts.remove(&key);
    }

    /// Frees an image key.
    pub fn delete_image(&mut self, key: ImageKey) {
        self.images.remove(&key);
    }

    /// Whether `key` is registered.
    pub fn has_font(&self, key: FontKey) -> bool {
        self.fonts.contains(&key)
    }

    /// Every registered font key, in no order.
    pub fn font_keys(&self) -> impl Iterator<Item = FontKey> + '_ {
        self.fonts.iter().copied()
    }

    /// Whether `key` is registered.
    pub fn has_image(&self, key: ImageKey) -> bool {
        self.images.contains(&key)
    }
}

/// Checks a decoded `list` meant for `page` against the keys that page
/// registered. Every index must point at something that exists (parents at
/// earlier entries, so the trees have no cycles), every key must be
/// registered, scroll ids must be unique, and stacking contexts must
/// balance. Stops at the first violation.
///
/// Not checked yet: whether a `ScrollId` belongs to this page. A `ScrollId`
/// does not carry its page, so that check waits on the decision about its
/// shape (spec 0002, AC-20).
pub fn validate(list: &DisplayList, page: PageId, keys: &ResourceKeys) -> Result<(), ListError> {
    if list.page != page {
        return Err(ListError::WrongPage(list.page));
    }
    match list.spatial.first() {
        Some(root) if root.parent.is_none() && root.kind == SpatialKind::Root => {}
        _ => return Err(ListError::BadRoot),
    }
    let mut scroll_ids = HashSet::new();
    for (index, node) in list.spatial.iter().enumerate().skip(1) {
        let parent_ok = node.parent.is_some_and(|p| (p.0 as usize) < index);
        if !parent_ok || node.kind == SpatialKind::Root {
            return Err(ListError::SpatialParent(index));
        }
        if let SpatialKind::ScrollFrame { scroll_id, .. } = node.kind
            && !scroll_ids.insert(scroll_id)
        {
            return Err(ListError::DuplicateScrollId(scroll_id));
        }
    }

    let spatial_count = list.spatial.len();
    for (index, clip) in list.clips.iter().enumerate() {
        if clip.parent.is_some_and(|p| p.0 as usize >= index) {
            return Err(ListError::ClipParent(index));
        }
        if clip.spatial.0 as usize >= spatial_count {
            return Err(ListError::ClipSpatial(index));
        }
    }

    let mut depth: usize = 0;
    for (index, item) in list.items.iter().enumerate() {
        if item.spatial.0 as usize >= spatial_count {
            return Err(ListError::ItemSpatial(index));
        }
        if item.clip.is_some_and(|c| c.0 as usize >= list.clips.len()) {
            return Err(ListError::ItemClip(index));
        }
        match &item.kind {
            ItemKind::Text { font, .. } if !keys.has_font(*font) => {
                return Err(ListError::UnknownFont(*font));
            }
            ItemKind::Image { key, .. } if !keys.has_image(*key) => {
                return Err(ListError::UnknownImage(*key));
            }
            ItemKind::PushStackingContext { .. } => depth += 1,
            ItemKind::PopStackingContext => {
                depth = depth
                    .checked_sub(1)
                    .ok_or(ListError::UnbalancedStackingContext(index))?;
            }
            _ => {}
        }
    }
    if depth != 0 {
        return Err(ListError::UnbalancedStackingContext(list.items.len()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BlendMode, ClipId, ClipNode, ColorF, DisplayItem, Epoch, RectF, ScrollOffset, SizeF,
        SpatialId, SpatialNode,
    };

    const PAGE: PageId = PageId(1);

    fn item(spatial: u32, clip: Option<u32>, kind: ItemKind) -> DisplayItem {
        DisplayItem {
            spatial: SpatialId(spatial),
            clip: clip.map(ClipId),
            kind,
        }
    }

    fn rect() -> ItemKind {
        ItemKind::Rect {
            rect: RectF::default(),
            color: ColorF::default(),
            radii: None,
        }
    }

    fn scroll_frame(id: u64) -> SpatialKind {
        SpatialKind::ScrollFrame {
            scroll_id: ScrollId(id),
            frame_rect: RectF::default(),
            content_size: SizeF::default(),
            offset: ScrollOffset::default(),
        }
    }

    /// A list using every kind of tree link once, and every resource kind.
    fn good() -> (DisplayList, ResourceKeys) {
        let mut list = DisplayList::new(PAGE, Epoch(1), SizeF::default());
        let frame = list.push_spatial(SpatialId::ROOT, scroll_frame(1));
        let outer = list.push_clip(ClipNode {
            parent: None,
            spatial: SpatialId::ROOT,
            rect: RectF::default(),
            radii: None,
        });
        list.push_clip(ClipNode {
            parent: Some(outer),
            spatial: frame,
            rect: RectF::default(),
            radii: Some([4.0; 8]),
        });
        list.items = vec![
            item(
                0,
                None,
                ItemKind::PushStackingContext {
                    opacity: 0.5,
                    blend_mode: BlendMode::Normal,
                },
            ),
            item(1, Some(1), rect()),
            item(
                1,
                Some(1),
                ItemKind::Text {
                    font: FontKey(3),
                    size: 16.0,
                    color: ColorF::default(),
                    glyphs: Vec::new(),
                },
            ),
            item(
                0,
                Some(0),
                ItemKind::Image {
                    key: ImageKey(4),
                    rect: RectF::default(),
                },
            ),
            item(0, None, ItemKind::PopStackingContext),
        ];
        let mut keys = ResourceKeys::default();
        keys.add_font(FontKey(3));
        keys.add_image(ImageKey(4), (2, 2), ImageFormat::Rgba8, 16)
            .unwrap();
        (list, keys)
    }

    fn rejects(change: impl FnOnce(&mut DisplayList), expected: ListError) {
        let (mut list, keys) = good();
        change(&mut list);
        assert_eq!(validate(&list, PAGE, &keys), Err(expected));
    }

    #[test]
    fn a_well_formed_list_passes() {
        let (list, keys) = good();
        assert_eq!(validate(&list, PAGE, &keys), Ok(()));
    }

    #[test]
    fn out_of_range_indexes_are_rejected() {
        // AC-20
        rejects(
            |l| l.items[1].spatial = SpatialId(2),
            ListError::ItemSpatial(1),
        );
        rejects(
            |l| l.items[1].clip = Some(ClipId(2)),
            ListError::ItemClip(1),
        );
        rejects(
            |l| l.clips[1].spatial = SpatialId(9),
            ListError::ClipSpatial(1),
        );
        rejects(
            |l| l.clips[0].parent = Some(ClipId(1)),
            ListError::ClipParent(0),
        );
        rejects(
            |l| l.clips[1].parent = Some(ClipId(1)),
            ListError::ClipParent(1),
        );
        rejects(
            |l| l.spatial[1].parent = Some(SpatialId(1)),
            ListError::SpatialParent(1),
        );
        rejects(|l| l.spatial[1].parent = None, ListError::SpatialParent(1));
        rejects(|l| l.spatial.clear(), ListError::BadRoot);
        rejects(
            |l| {
                l.spatial[0] = SpatialNode {
                    parent: None,
                    kind: scroll_frame(9),
                }
            },
            ListError::BadRoot,
        );
    }

    #[test]
    fn unknown_keys_and_duplicate_scroll_ids_are_rejected() {
        // AC-20
        let (list, mut keys) = good();
        keys.delete_font(FontKey(3));
        assert_eq!(
            validate(&list, PAGE, &keys),
            Err(ListError::UnknownFont(FontKey(3)))
        );
        keys.add_font(FontKey(3));
        keys.delete_image(ImageKey(4));
        assert_eq!(
            validate(&list, PAGE, &keys),
            Err(ListError::UnknownImage(ImageKey(4)))
        );
        rejects(
            |l| {
                l.push_spatial(SpatialId::ROOT, scroll_frame(1));
            },
            ListError::DuplicateScrollId(ScrollId(1)),
        );
        rejects(|l| l.page = PageId(2), ListError::WrongPage(PageId(2)));
    }

    #[test]
    fn stacking_contexts_must_balance() {
        rejects(
            |l| l.items.truncate(4),
            ListError::UnbalancedStackingContext(4),
        );
        rejects(
            |l| {
                l.items
                    .insert(0, item(0, None, ItemKind::PopStackingContext))
            },
            ListError::UnbalancedStackingContext(0),
        );
    }

    #[test]
    fn images_over_the_limits_are_refused() {
        let mut keys = ResourceKeys::default();
        assert_eq!(
            keys.add_image(ImageKey(1), (16_385, 1), ImageFormat::Rgba8, 65_540),
            Err(ListError::ImageTooLarge {
                width: 16_385,
                height: 1
            })
        );
        assert_eq!(
            keys.add_image(ImageKey(1), (2, 2), ImageFormat::Rgba8, 15),
            Err(ListError::ImageDataLength {
                expected: 16,
                actual: 15
            })
        );
        assert!(!keys.has_image(ImageKey(1)));
    }
}
