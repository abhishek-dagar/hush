//! The browser side of one page: the last good display list, its scroll
//! offsets, its resource keys and the frame cadence (spec 0002, Rendering
//! update, Scrolling, Security model), and which navigation it is on
//! (spec 0004, AC-11, AC-12).

use display_list::{
    DisplayList, Epoch, FontKey, ImageFormat, ImageKey, MAX_ENCODED_LEN, PageId, PointF, RectF,
    ResourceKeys, ScrollId, ScrollOffset, ScrollState, SizeF, SpatialKind,
};
use ipc::NavigationId;
use tracing::{debug, error};

/// What the browser knows about one page in the content process.
pub(crate) struct PageView {
    id: PageId,
    /// The last good list; kept on screen until a newer valid one arrives.
    /// Its scroll offsets are the ones the user sees.
    list: Option<DisplayList>,
    /// The render side's scroll offsets (AC-13).
    scroll: ScrollState,
    /// The fonts and images the page registered (AC-20).
    keys: ResourceKeys,
    /// The page asked for frames and has not said `FrameSkipped` since.
    wants_frames: bool,
    /// A `BeginFrame` was sent and not answered yet. At most one is in flight.
    frame_in_flight: bool,
    /// The page's current navigation; lists from any other are dropped.
    navigation: NavigationId,
    /// The navigation the list on screen belongs to.
    shown: NavigationId,
    /// Fonts a previous content process registered, still drawn by the list
    /// on screen until the new process paints.
    orphan_fonts: Vec<FontKey>,
    /// Orphan fonts the new process did not register again, for the
    /// renderer to free.
    fonts_to_free: Vec<FontKey>,
}

impl PageView {
    pub(crate) fn new(id: PageId) -> Self {
        Self {
            id,
            list: None,
            scroll: ScrollState::default(),
            keys: ResourceKeys::default(),
            wants_frames: false,
            frame_in_flight: false,
            navigation: NavigationId(0),
            shown: NavigationId(0),
            orphan_fonts: Vec::new(),
            fonts_to_free: Vec::new(),
        }
    }

    pub(crate) fn id(&self) -> PageId {
        self.id
    }

    pub(crate) fn list(&self) -> Option<&DisplayList> {
        self.list.as_ref()
    }

    /// A new navigation started. The list on screen stays (paint holding,
    /// AC-12), but only lists from `navigation` replace it from now on.
    pub(crate) fn navigate(&mut self, navigation: NavigationId) {
        self.navigation = navigation;
    }

    /// Whether the list on screen is the current navigation's.
    pub(crate) fn painted(&self) -> bool {
        self.list.is_some() && self.shown == self.navigation
    }

    /// Paint holding ran out: show white until the new page paints (AC-12).
    pub(crate) fn clear(&mut self) {
        self.list = None;
    }

    /// The content process was relaunched (AC-14). Its keys, epochs and
    /// frame cadence start over; the list on screen stays until it paints,
    /// and the old process's fonts with it.
    pub(crate) fn restart(&mut self) {
        let keys = std::mem::take(&mut self.keys);
        self.orphan_fonts.extend(keys.font_keys());
        self.wants_frames = false;
        self.frame_in_flight = false;
    }

    /// Fonts the renderer can free now: a previous process's, once the new
    /// one painted without registering them again.
    pub(crate) fn take_fonts_to_free(&mut self) -> Vec<FontKey> {
        std::mem::take(&mut self.fonts_to_free)
    }

    /// The page sent `NeedsFrame`.
    pub(crate) fn needs_frame(&mut self) {
        self.wants_frames = true;
    }

    /// Whether to send `BeginFrame` now. Marks it in flight if so.
    pub(crate) fn take_begin_frame(&mut self) -> bool {
        if self.wants_frames && !self.frame_in_flight {
            self.frame_in_flight = true;
            return true;
        }
        false
    }

    /// The page answered with `FrameSkipped`: stop ticking until `NeedsFrame`.
    pub(crate) fn frame_skipped(&mut self) {
        self.frame_in_flight = false;
        self.wants_frames = false;
    }

    /// The page answered with a list. Keeps it only if it is for the current
    /// navigation, decodes, belongs to this page, is newer than the one on
    /// screen, and passes every check in [`display_list::validate`];
    /// otherwise logs and keeps the last good frame (AC-20). The first list
    /// of a navigation replaces the old one whatever its epoch (a relaunched
    /// process counts from the start again) and starts with fresh scroll
    /// offsets. A kept list takes the render side's scroll offsets where the
    /// page did not scroll by script (AC-13). Returns whether the list on
    /// screen changed.
    pub(crate) fn display_list(
        &mut self,
        navigation: NavigationId,
        epoch: Epoch,
        data: &[u8],
    ) -> bool {
        self.frame_in_flight = false;
        if navigation != self.navigation {
            debug!(?navigation, "display list from another navigation dropped");
            return false;
        }
        if data.len() > MAX_ENCODED_LEN {
            error!(
                bytes = data.len(),
                "display list over the size limit, dropped"
            );
            return false;
        }
        let mut list: DisplayList = match postcard::from_bytes(data) {
            Ok(list) => list,
            Err(err) => {
                error!(%err, "display list did not decode, dropped");
                return false;
            }
        };
        if list.epoch != epoch {
            error!(epoch = ?list.epoch, "display list for the wrong epoch, dropped");
            return false;
        }
        if let Err(err) = display_list::validate(&list, self.id, &self.keys) {
            error!(%err, "malformed display list, dropped");
            return false;
        }
        let same_navigation = self.shown == navigation;
        if same_navigation
            && self
                .list
                .as_ref()
                .is_some_and(|shown| list.epoch <= shown.epoch)
        {
            debug!(epoch = ?list.epoch, "stale display list ignored");
            return false;
        }
        if !same_navigation {
            self.scroll = ScrollState::default();
            self.shown = navigation;
            let keys = &self.keys;
            self.fonts_to_free.extend(
                self.orphan_fonts
                    .drain(..)
                    .filter(|&key| !keys.has_font(key)),
            );
        }
        self.scroll.apply(&mut list);
        self.list = Some(list);
        true
    }

    /// A user scroll by `dx`, `dy` CSS px at `at`, in page CSS px. Moves the
    /// list on screen at once and returns what to tell the page with
    /// `ScrollChanged`, or `None` if nothing under the point could move.
    pub(crate) fn scroll_by(
        &mut self,
        at: PointF,
        dx: f32,
        dy: f32,
    ) -> Option<(ScrollId, ScrollOffset)> {
        let list = self.list.as_mut()?;
        self.scroll.scroll_by(list, at, dx, dy)
    }

    /// A key scroll of the root viewport (AC-30): `to` picks the offset
    /// from the viewport's offset, scrollport and content size. Moves the
    /// list on screen at once, as [`PageView::scroll_by`] does.
    pub(crate) fn scroll_viewport(
        &mut self,
        to: impl FnOnce(ScrollOffset, RectF, SizeF) -> (f32, f32),
    ) -> Option<(ScrollId, ScrollOffset)> {
        let list = self.list.as_mut()?;
        self.scroll.scroll_viewport(list, to)
    }

    /// The root viewport on screen: its scrollport, content size and
    /// offset.
    pub(crate) fn viewport(&self) -> Option<(RectF, SizeF, ScrollOffset)> {
        let list = self.list.as_ref()?;
        let frame = list.viewport_frame()?;
        match list.spatial.get(frame.0 as usize)?.kind {
            SpatialKind::ScrollFrame {
                frame_rect,
                content_size,
                offset,
                ..
            } => Some((frame_rect, content_size, offset)),
            _ => None,
        }
    }

    /// The page registered a font.
    pub(crate) fn add_font(&mut self, key: FontKey) {
        self.keys.add_font(key);
    }

    /// The page registered an image. One over the size limits, or whose
    /// data does not match its size, is refused and logged, so lists naming
    /// it are dropped too. Returns whether it was kept.
    pub(crate) fn add_image(
        &mut self,
        key: ImageKey,
        size: (u32, u32),
        format: ImageFormat,
        data_len: usize,
    ) -> bool {
        match self.keys.add_image(key, size, format, data_len) {
            Ok(()) => true,
            Err(err) => {
                error!(%err, "image refused");
                false
            }
        }
    }

    /// The page freed a font.
    pub(crate) fn delete_font(&mut self, key: FontKey) {
        self.keys.delete_font(key);
    }

    /// The page freed an image.
    pub(crate) fn delete_image(&mut self, key: ImageKey) {
        self.keys.delete_image(key);
    }
}

#[cfg(test)]
mod tests {
    use display_list::{ColorF, DisplayItem, ItemKind, RectF, SizeF, SpatialId, SpatialKind};

    use super::*;

    fn encode(list: &DisplayList) -> Vec<u8> {
        postcard::to_allocvec(list).unwrap()
    }

    fn encoded(page: u32, epoch: u64) -> Vec<u8> {
        encode(&DisplayList::new(
            PageId(page),
            Epoch(epoch),
            SizeF::default(),
        ))
    }

    #[test]
    fn one_begin_frame_in_flight_until_skipped() {
        let mut view = PageView::new(PageId(1));
        assert!(!view.take_begin_frame(), "no frames until asked");
        view.needs_frame();
        assert!(view.take_begin_frame());
        assert!(!view.take_begin_frame(), "one in flight at most");

        assert!(view.display_list(NavigationId(0), Epoch(1), &encoded(1, 1)));
        assert!(view.take_begin_frame(), "keeps ticking after a list");
        view.frame_skipped();
        assert!(!view.take_begin_frame(), "stops after FrameSkipped");
    }

    #[test]
    fn only_valid_newer_lists_replace_the_frame() {
        let mut view = PageView::new(PageId(1));
        assert!(view.display_list(NavigationId(0), Epoch(2), &encoded(1, 2)));
        assert!(
            !view.display_list(NavigationId(0), Epoch(2), &encoded(1, 2)),
            "same epoch"
        );
        assert!(
            !view.display_list(NavigationId(0), Epoch(1), &encoded(1, 1)),
            "older epoch"
        );
        assert!(
            !view.display_list(NavigationId(0), Epoch(3), &encoded(9, 3)),
            "another page"
        );
        assert!(
            !view.display_list(NavigationId(0), Epoch(4), &encoded(1, 3)),
            "epoch mismatch"
        );
        assert!(
            !view.display_list(NavigationId(0), Epoch(5), &[0xff; 7]),
            "garbage"
        );
        assert_eq!(
            view.list().map(|l| l.epoch),
            Some(Epoch(2)),
            "last good frame kept"
        );
    }

    #[test]
    fn the_old_frame_stays_until_the_new_navigation_paints() {
        // AC-11, AC-12
        let mut view = PageView::new(PageId(1));
        view.navigate(NavigationId(1));
        assert!(view.display_list(NavigationId(1), Epoch(5), &encoded(1, 5)));
        assert!(view.painted());

        view.navigate(NavigationId(2));
        assert!(!view.painted(), "holding the old frame");
        assert_eq!(view.list().map(|l| l.epoch), Some(Epoch(5)));
        assert!(
            !view.display_list(NavigationId(1), Epoch(6), &encoded(1, 6)),
            "the old document's late frame"
        );
        assert_eq!(view.list().map(|l| l.epoch), Some(Epoch(5)));

        // The new navigation's first list wins even with a lower epoch.
        assert!(view.display_list(NavigationId(2), Epoch(1), &encoded(1, 1)));
        assert!(view.painted());
        assert!(!view.display_list(NavigationId(2), Epoch(1), &encoded(1, 1)));

        view.navigate(NavigationId(3));
        view.clear();
        assert!(view.list().is_none(), "paint holding ran out: white");
        assert!(!view.painted());
    }

    #[test]
    fn a_relaunched_process_frees_the_old_fonts_it_did_not_bring_back() {
        // AC-14
        let mut view = PageView::new(PageId(1));
        view.navigate(NavigationId(1));
        view.add_font(FontKey(1));
        view.add_font(FontKey(2));
        assert!(view.display_list(NavigationId(1), Epoch(3), &encoded(1, 3)));

        view.needs_frame();
        assert!(view.take_begin_frame());
        view.restart();
        assert!(
            !view.take_begin_frame(),
            "the new process asks for frames itself"
        );
        view.navigate(NavigationId(2));
        view.add_font(FontKey(1));
        assert!(
            view.take_fonts_to_free().is_empty(),
            "the old list still draws"
        );
        assert!(view.display_list(NavigationId(2), Epoch(1), &encoded(1, 1)));
        assert_eq!(view.take_fonts_to_free(), vec![FontKey(2)]);
    }

    #[test]
    fn hostile_lists_are_dropped_and_the_last_good_frame_stays() {
        // AC-20
        let mut view = PageView::new(PageId(1));
        assert!(view.display_list(NavigationId(0), Epoch(1), &encoded(1, 1)));

        let item = |spatial, kind| DisplayItem {
            spatial: SpatialId(spatial),
            clip: None,
            kind,
        };
        let rect = ItemKind::Rect {
            rect: RectF::default(),
            color: ColorF::default(),
            radii: None,
        };
        let image = ItemKind::Image {
            key: ImageKey(5),
            rect: RectF::default(),
        };
        let mut out_of_range = DisplayList::new(PageId(1), Epoch(2), SizeF::default());
        out_of_range.items.push(item(7, rect));
        assert!(!view.display_list(NavigationId(0), Epoch(2), &encode(&out_of_range)));

        let mut unknown_image = DisplayList::new(PageId(1), Epoch(3), SizeF::default());
        unknown_image.items.push(item(0, image.clone()));
        assert!(!view.display_list(NavigationId(0), Epoch(3), &encode(&unknown_image)));

        // An oversized buffer is refused before decoding.
        let oversized = vec![0u8; MAX_ENCODED_LEN + 1];
        assert!(!view.display_list(NavigationId(0), Epoch(4), &oversized));
        assert_eq!(view.list().map(|l| l.epoch), Some(Epoch(1)));

        // Once the image is registered, the same list is accepted.
        assert!(view.add_image(ImageKey(5), (1, 1), ImageFormat::Rgba8, 4));
        assert!(view.display_list(NavigationId(0), Epoch(3), &encode(&unknown_image)));
        // A refused image never registers.
        assert!(!view.add_image(ImageKey(6), (20_000, 1), ImageFormat::Rgba8, 80_000));
        let mut refused = DisplayList::new(PageId(1), Epoch(5), SizeF::default());
        refused.items.push(item(
            0,
            ItemKind::Image {
                key: ImageKey(6),
                rect: RectF::default(),
            },
        ));
        assert!(!view.display_list(NavigationId(0), Epoch(5), &encode(&refused)));
    }

    #[test]
    fn a_user_scroll_moves_the_list_on_screen_and_survives_the_next_list() {
        // AC-13
        let scroller = |epoch: u64, y: f32, generation: u32| {
            let mut list = DisplayList::new(PageId(1), Epoch(epoch), SizeF::default());
            list.push_spatial(
                SpatialId::ROOT,
                SpatialKind::ScrollFrame {
                    scroll_id: ScrollId::new(0, 3, 1),
                    frame_rect: RectF {
                        origin: PointF::default(),
                        size: SizeF {
                            width: 100.0,
                            height: 100.0,
                        },
                    },
                    content_size: SizeF {
                        width: 100.0,
                        height: 400.0,
                    },
                    offset: ScrollOffset {
                        x: 0.0,
                        y,
                        generation,
                    },
                },
            );
            encode(&list)
        };
        let shown_y = |view: &PageView| match view.list().unwrap().spatial[1].kind {
            SpatialKind::ScrollFrame { offset, .. } => offset.y,
            _ => f32::NAN,
        };
        let mut view = PageView::new(PageId(1));
        assert!(view.display_list(NavigationId(0), Epoch(1), &scroller(1, 0.0, 0)));
        let moved = view.scroll_by(PointF { x: 5.0, y: 5.0 }, 0.0, 30.0);
        assert_eq!(moved.map(|(_, o)| o.y), Some(30.0));
        assert_eq!(shown_y(&view), 30.0);
        assert!(view.display_list(NavigationId(0), Epoch(2), &scroller(2, 0.0, 0)));
        assert_eq!(shown_y(&view), 30.0, "the page's stale offset loses");
    }
}
