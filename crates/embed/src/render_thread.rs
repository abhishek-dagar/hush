//! The render thread (spec 0004, child 0005, Render thread, AC-28).
//!
//! It owns the `Renderer` (and with it the glyph atlas and the image
//! textures), the page's last good display list with its scroll offsets,
//! and the frame clock. The main thread only runs the window's event loop
//! and sends it [`RenderMsg`]; the IPC router sends it the page's drawing
//! messages directly, so drawing and scrolling never wait on the main
//! thread or on the page. While nothing changes it blocks on its channel
//! and uses no CPU; with vsync, each present paces the next `BeginFrame`.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use display_list::{
    ColorF, DisplayList, PageId, PointF, RectF, ScrollId, ScrollOffset, SizeF, SpatialId,
    SpatialKind,
};
use ipc::{ContentSender, NavigationId, ToBrowser, ToContent};
use navigation::EntryId;
use render::{Chrome, PageSlot, RenderError, Renderer, ScrollIndicator};
use scrolling::{Axis, Bars, Gesture, GesturePhase, SmoothScroll};
use tracing::{debug, error, warn};
use winit::event_loop::EventLoopProxy;
use winit::window::{Window, WindowId};

use crate::EmbedError;
use crate::browser::UserEvent;
use crate::page_view::PageView;
use crate::scrolling;
use ipc::KeyScroll;

/// What the main thread and the IPC router tell the render thread.
pub(crate) enum RenderMsg {
    /// The window's drawing size in physical pixels, and its device pixels
    /// per CSS px.
    Window { width: u32, height: u32, scale: f32 },
    /// A new rendering of the chrome document (spec 0003, milestone 3).
    #[cfg_attr(
        not(feature = "chrome"),
        allow(dead_code, reason = "only the browser app has a chrome document")
    )]
    Chrome(Box<ChromeDraw>),
    /// The OS asked for the window to be drawn again.
    Redraw,
    /// A user scroll by `dx`, `dy` CSS px at `at`, in page CSS px, in the
    /// gesture `phase` (spec 0009, child 0002, AC-7).
    Scroll {
        at: PointF,
        dx: f32,
        dy: f32,
        phase: GesturePhase,
    },
    /// A key scroll of `target`, or the viewport when `None` (AC-9): the
    /// page did not use the key, so the browser moves the frame it names.
    ScrollKey {
        key: KeyScroll,
        target: Option<ScrollId>,
    },
    /// The pointer moved to `at` (page CSS px), or `None` when it left the
    /// page, for bar hover and drag (spec 0009, child 0002, AC-8).
    Pointer { at: Option<PointF> },
    /// A press on a scroll bar at `at`: thumb drag or track page (AC-8).
    BarPress { at: PointF },
    /// The bar press ended (AC-8).
    BarRelease,
    /// A navigation of the page started; lists from older ones are dropped
    /// (AC-11).
    NavigationStarted { navigation: NavigationId },
    /// The page is leaving history entry `entry`: answer with where the
    /// root viewport is scrolled on screen (spec 0008, AC-23).
    SaveScroll { entry: EntryId },
    /// Content process `process` is up, reachable through `sender`. Sent
    /// before its router starts, so it arrives before anything it sends.
    ContentStarted { process: u32, sender: ContentSender },
    /// Content process `process` closed its channel.
    ContentGone { process: u32 },
    /// A drawing message from content process `process`, from its router.
    Content { process: u32, message: ToBrowser },
    /// Free the GPU resources and end the thread.
    Shutdown,
}

/// The chrome document's drawing: built in process from the embedded
/// assets, so it is trusted and not validated like a content list.
pub(crate) struct ChromeDraw {
    /// Its list, in chrome px.
    pub(crate) list: DisplayList,
    /// `AddFont` and `AddImage` for its page, applied before the list.
    pub(crate) adds: Vec<ToBrowser>,
    /// `DeleteFont` and `DeleteImage`, applied after.
    pub(crate) deletes: Vec<ToBrowser>,
    /// Device pixels per chrome px.
    pub(crate) scale: f32,
    /// Where the page goes, in physical pixels; `None` shows no page.
    pub(crate) slot: Option<PageSlot>,
    /// The overlay layer, drawn after the page (spec 0008, Decision).
    pub(crate) overlay: Option<DisplayList>,
}

/// What the render thread tells the main thread.
#[derive(Debug)]
pub(crate) enum RenderEvent {
    /// The first list of `navigation` is on screen (AC-12).
    Painted { navigation: NavigationId },
    /// Where the root viewport was scrolled as entry `entry` was left, in
    /// CSS px (spec 0008, AC-23).
    ScrollSaved { entry: EntryId, offset: (f32, f32) },
    /// The geometry of each overlay bar, so the main thread can hit test
    /// presses against them before forwarding them to the page
    /// (spec 0009, child 0002, AC-8).
    Bars { hits: Vec<scrolling::BarHit> },
    /// Drawing into `window` failed for good: the surface was lost and
    /// could not be made again.
    Failed {
        /// The window whose render thread ended.
        window: WindowId,
        /// Why.
        error: RenderError,
    },
}

/// Starts the render thread for `page` in `window` (a window with only a
/// chrome document passes the chrome's page id), drawing with
/// `renderer`, taking its messages from `receiver`.
pub(crate) fn spawn(
    renderer: Renderer,
    window: Arc<Window>,
    page: PageId,
    receiver: Receiver<RenderMsg>,
    proxy: EventLoopProxy<UserEvent>,
) -> Result<JoinHandle<()>, EmbedError> {
    let size = window.inner_size();
    let state = RenderLoop {
        renderer,
        scale: window.scale_factor() as f32,
        size: (size.width, size.height),
        window,
        page: PageView::new(page),
        proxy,
        chrome: None,
        process: 0,
        content: None,
        started: Instant::now(),
        scrolled: HashMap::new(),
        gesture: None,
        smooth: None,
        drag: None,
        hovered_bar: None,
        bars: Bars::default(),
        scrolled_at: None,
        dirty: true,
    };
    thread::Builder::new()
        .name("render".into())
        .spawn(move || state.run(&receiver))
        .map_err(EmbedError::SpawnRender)
}

struct RenderLoop {
    renderer: Renderer,
    /// Kept to make the surface again if it is lost.
    window: Arc<Window>,
    page: PageView,
    proxy: EventLoopProxy<UserEvent>,
    size: (u32, u32),
    scale: f32,
    /// The chrome document's last drawing, drawn around the page.
    chrome: Option<ChromeDraw>,
    /// The live content process's launch number, and the way to it.
    process: u32,
    content: Option<ContentSender>,
    /// The zero point of `BeginFrame.frame_time`.
    started: Instant,
    /// Where user scrolls left each scroll frame since the last present;
    /// the page hears once per frame (spec 0002, Scrolling).
    scrolled: HashMap<ScrollId, ScrollOffset>,
    /// The gesture in progress: which frame it latched to, and when its last
    /// event was, so a quiet gap starts a new gesture (spec 0009, child
    /// 0002, AC-7).
    gesture: Option<Gesture>,
    /// A smooth scroll in progress, if any (spec 0009, child 0002, AC-9).
    smooth: Option<SmoothScroll>,
    /// A thumb drag in progress: which bar, which axis, the offset and
    /// pointer where it began.
    drag: Option<(ScrollId, Axis, PointF, PointF)>,
    /// Which bar the pointer currently hovers, so it widens and stays shown.
    hovered_bar: Option<(ScrollId, Axis)>,
    /// The latest bar geometry, reported to the main thread so it can hit
    /// test presses before forwarding them to the page (spec 0009, child
    /// 0002, AC-8).
    bars: Bars,
    /// When the root viewport last moved, while the scroll indicator shows.
    scrolled_at: Option<Instant>,
    /// Something on screen changed since the last present.
    dirty: bool,
}

impl RenderLoop {
    /// Waits for a message, or for the scroll indicator's next frame, takes
    /// every waiting message, then draws once if anything changed. The
    /// thread ends when the main thread drops its sender or says
    /// `Shutdown`.
    fn run(mut self, receiver: &Receiver<RenderMsg>) {
        loop {
            let now = Instant::now();
            let mut wake = self
                .scrolled_at
                .map(|at| scrolling::indicator_wake(at, now));
            // A smooth scroll keeps redrawing until it settles.
            if self.smooth.is_some() {
                let soon = now + Duration::from_millis(16);
                wake = Some(wake.map_or(soon, |w| w.min(soon)));
            }
            let first = match wake {
                None => match receiver.recv() {
                    Ok(message) => Some(message),
                    Err(_) => return,
                },
                Some(at) => match receiver.recv_timeout(at.saturating_duration_since(now)) {
                    Ok(message) => Some(message),
                    Err(RecvTimeoutError::Timeout) => {
                        // The indicator fades.
                        self.dirty = true;
                        None
                    }
                    Err(RecvTimeoutError::Disconnected) => return,
                },
            };
            let mut go_on = first.is_none_or(|message| self.handle(message));
            while go_on && let Ok(message) = receiver.try_recv() {
                go_on = self.handle(message);
            }
            if !go_on {
                return;
            }
            if self.dirty && !self.draw() {
                return;
            }
        }
    }

    /// Applies one message. False ends the thread.
    fn handle(&mut self, message: RenderMsg) -> bool {
        match message {
            RenderMsg::Window {
                width,
                height,
                scale,
            } => {
                if (width, height) != self.size {
                    self.renderer.resize(width, height);
                    self.size = (width, height);
                }
                self.scale = scale;
                self.dirty = true;
            }
            RenderMsg::Chrome(draw) => self.chrome_draw(*draw),
            RenderMsg::Redraw => self.dirty = true,
            RenderMsg::Scroll { at, dx, dy, phase } => {
                // Wheel and touchpad gestures: latch to one frame so the
                // rest of the gesture stays on it, then chain outward when
                // it cannot move (spec 0009, child 0002, AC-7).
                let now = Instant::now();
                let moved = match phase {
                    GesturePhase::Start | GesturePhase::None => {
                        let moved = match phase {
                            GesturePhase::Start => self.page.scroll_by(at, dx, dy),
                            // A mouse wheel has no phases: keep the gesture
                            // alive while events arrive within the quiet gap,
                            // otherwise start a fresh one on whichever frame
                            // is under the pointer (spec 0009, child 0002,
                            // Value sourcing, Gesture phase).
                            GesturePhase::None => {
                                let same = self.gesture.is_some_and(|g| {
                                    now.saturating_duration_since(g.last_event)
                                        < scrolling::GESTURE_GAP
                                });
                                if same {
                                    if let Some(frame) = self.gesture.and_then(|g| g.frame) {
                                        self.scroll_frame_by_id(frame, |offset, _, _| {
                                            (offset.x + dx, offset.y + dy)
                                        })
                                    } else {
                                        self.page.scroll_by(at, dx, dy)
                                    }
                                } else {
                                    self.page.scroll_by(at, dx, dy)
                                }
                            }
                            _ => None,
                        };
                        if let Some((id, _)) = moved {
                            self.gesture = Some(Gesture {
                                frame: Some(id),
                                last_event: now,
                            });
                        } else if phase == GesturePhase::None {
                            self.gesture = None;
                        }
                        moved
                    }
                    GesturePhase::Move => match self.gesture.and_then(|g| g.frame) {
                        // Keep scrolling the frame the gesture latched to.
                        Some(frame) => {
                            self.gesture = self.gesture.map(|g| Gesture {
                                last_event: now,
                                ..g
                            });
                            self.scroll_frame_by_id(frame, |offset, _, _| {
                                (offset.x + dx, offset.y + dy)
                            })
                        }
                        None => None,
                    },
                    GesturePhase::End => {
                        let Some(frame) = self.gesture.and_then(|g| g.frame) else {
                            self.gesture = None;
                            return true;
                        };
                        let moved = self.scroll_frame_by_id(frame, |offset, _, _| {
                            (offset.x + dx, offset.y + dy)
                        });
                        self.gesture = None;
                        moved
                    }
                };
                self.user_scrolled(moved);
            }
            RenderMsg::ScrollKey { key, target } => {
                // A key scroll animates smoothly instead of jumping
                // (spec 0009, child 0002, AC-9).
                let now = Instant::now();
                self.smooth = None;
                let frame = match target {
                    Some(id) => id,
                    None => match self.page.viewport_scroll_id() {
                        Some(id) => id,
                        None => return true,
                    },
                };
                let Some(index) = self.page.scroll_frame_index(frame) else {
                    return true;
                };
                let Some(list) = self.page.list_mut() else {
                    return true;
                };
                let SpatialKind::ScrollFrame {
                    frame_rect,
                    content_size,
                    offset,
                    ..
                } = &list.spatial[index].kind
                else {
                    return true;
                };
                let (tx, ty) =
                    scrolling::key_scroll_target(key, *offset, *frame_rect, *content_size);
                if (tx, ty) == (offset.x, offset.y) {
                    return true;
                }
                self.smooth = Some(SmoothScroll {
                    frame,
                    from: PointF {
                        x: offset.x,
                        y: offset.y,
                    },
                    to: PointF { x: tx, y: ty },
                    start: now,
                });
                self.dirty = true;
            }
            RenderMsg::Pointer { at } => {
                // The pointer moved over the page: if a thumb drag is in
                // progress, scroll with it; otherwise update which bar, if
                // any, it hovers so that bar widens and stays shown. When the
                // pointer leaves the page, end any drag and clear the hover.
                match at {
                    Some(at) => {
                        if let Some((scroll_id, axis, start_offset, start_pointer)) = self.drag {
                            self.drag_thumb(scroll_id, axis, start_offset, start_pointer, at);
                        } else {
                            self.hover_bar(at);
                        }
                    }
                    None => {
                        self.drag = None;
                        self.hovered_bar = None;
                    }
                }
                self.dirty = true;
            }
            RenderMsg::BarPress { at } => {
                // A press on a bar thumb starts a drag; on the track it
                // scrolls a page. Presses on bars never reach the page.
                if self.press_bar(at) {
                    self.dirty = true;
                }
            }
            RenderMsg::BarRelease => {
                self.drag = None;
                self.dirty = true;
            }
            RenderMsg::NavigationStarted { navigation } => self.page.navigate(navigation),
            RenderMsg::SaveScroll { entry } => {
                if let Some((_, _, offset)) = self.page.viewport() {
                    let offset = (offset.x, offset.y);
                    let _ = self
                        .proxy
                        .send_event(UserEvent::Render(RenderEvent::ScrollSaved {
                            entry,
                            offset,
                        }));
                }
            }
            RenderMsg::ContentStarted { process, sender } => {
                self.process = process;
                self.content = Some(sender);
                self.page.restart();
            }
            RenderMsg::ContentGone { process } if process == self.process => {
                self.content = None;
                // The window keeps the last drawn frame (spec 0002).
                self.page.frame_skipped();
            }
            RenderMsg::ContentGone { .. } => {}
            RenderMsg::Content { process, message } if process == self.process => {
                self.content_message(message);
            }
            RenderMsg::Content { process, .. } => {
                debug!(process, "message from an old content process dropped");
            }
            RenderMsg::Shutdown => return false,
        }
        true
    }

    /// Takes a new chrome drawing: its new resources first, then the list,
    /// then the resources it no longer uses.
    fn chrome_draw(&mut self, mut draw: ChromeDraw) {
        for message in draw.adds.drain(..).chain(std::mem::take(&mut draw.deletes)) {
            match message {
                ToBrowser::AddFont {
                    page,
                    key,
                    data,
                    index,
                    variations,
                } => self.renderer.add_font(page, key, &data, index, &variations),
                ToBrowser::AddImage {
                    page,
                    key,
                    size,
                    format,
                    pixels,
                } => self.renderer.add_image(page, key, size, format, &pixels),
                ToBrowser::DeleteFont { page, key } => self.renderer.delete_font(page, key),
                ToBrowser::DeleteImage { page, key } => self.renderer.delete_image(page, key),
                message => warn!(?message, "unexpected chrome resource ignored"),
            }
        }
        self.chrome = Some(draw);
        self.dirty = true;
    }

    /// A drawing message from the live content process. Resources for an
    /// unknown page fall through to the warning (spec 0002, Security model).
    fn content_message(&mut self, message: ToBrowser) {
        let id = self.page.id();
        match message {
            ToBrowser::NeedsFrame { page } if page == id => {
                self.page.needs_frame();
                self.dirty = true;
            }
            ToBrowser::DisplayList {
                page,
                navigation,
                epoch,
                data,
            } if page == id => {
                let was_painted = self.page.painted();
                let before = self.page.viewport().map(|(_, _, o)| (o.x, o.y));
                self.page.display_list(navigation, epoch, &data);
                let after = self.page.viewport().map(|(_, _, o)| (o.x, o.y));
                // A script scroll shows the indicator too.
                if was_painted && before != after {
                    self.scrolled_at = Some(Instant::now());
                }
                if !was_painted && self.page.painted() {
                    let _ = self
                        .proxy
                        .send_event(UserEvent::Render(RenderEvent::Painted { navigation }));
                }
                for key in self.page.take_fonts_to_free() {
                    self.renderer.delete_font(id, key);
                }
                // Draw even if the list was dropped, so the next
                // `BeginFrame` goes out.
                self.dirty = true;
            }
            ToBrowser::FrameSkipped { page } if page == id => self.page.frame_skipped(),
            ToBrowser::AddFont {
                page,
                key,
                data,
                index,
                variations,
            } if page == id => {
                self.page.add_font(key);
                self.renderer.add_font(page, key, &data, index, &variations);
            }
            ToBrowser::AddImage {
                page,
                key,
                size,
                format,
                pixels,
            } if page == id => {
                if self.page.add_image(key, size, format, pixels.len()) {
                    self.renderer.add_image(page, key, size, format, &pixels);
                }
            }
            ToBrowser::DeleteFont { page, key } if page == id => {
                self.page.delete_font(key);
                self.renderer.delete_font(page, key);
            }
            ToBrowser::DeleteImage { page, key } if page == id => {
                self.page.delete_image(key);
                self.renderer.delete_image(page, key);
            }
            message => warn!(
                ?message,
                "unexpected message from the content process ignored"
            ),
        }
    }

    /// A user scroll moved `moved` (or nothing): the page hears after the
    /// next present, and the indicator shows.
    fn user_scrolled(&mut self, moved: Option<(ScrollId, ScrollOffset)>) {
        if let Some((scroll_id, offset)) = moved {
            self.scrolled.insert(scroll_id, offset);
            self.scrolled_at = Some(Instant::now());
            self.dirty = true;
        }
    }

    /// Moves the scroll frame `id` by `to`, clamped to its range, the way
    /// `ScrollState::scroll_frame` does but found by id rather than index.
    fn scroll_frame_by_id(
        &mut self,
        id: ScrollId,
        to: impl FnOnce(ScrollOffset, RectF, SizeF) -> (f32, f32),
    ) -> Option<(ScrollId, ScrollOffset)> {
        let list = self.page.list_mut()?;
        let index = list.spatial.iter().position(|node| {
            matches!(
                node.kind,
                SpatialKind::ScrollFrame { scroll_id: sid, .. } if sid == id
            )
        })?;
        self.page.scroll_frame_by_index(index, to)
    }

    /// Which bar, if any, the pointer at `at` (page CSS px) hovers: within
    /// `HOVER_BAND` of a bar's track. Returns the bar and whether it is over
    /// the thumb (for dragging) or the track (for a page scroll).
    fn hover_bar(&mut self, at: PointF) {
        // Recompute the current bar geometry from the list on screen.
        let hits = self.current_bars();
        let mut hovered: Option<(ScrollId, Axis)> = None;
        'bars: for hit in &hits {
            let band = |rect: RectF| RectF {
                origin: PointF {
                    x: rect.origin.x - scrolling::HOVER_BAND,
                    y: rect.origin.y - scrolling::HOVER_BAND,
                },
                size: SizeF {
                    width: rect.size.width + 2.0 * scrolling::HOVER_BAND,
                    height: rect.size.height + 2.0 * scrolling::HOVER_BAND,
                },
            };
            let in_track = |rect: RectF| {
                at.x >= band(rect).origin.x
                    && at.x < band(rect).origin.x + band(rect).size.width
                    && at.y >= band(rect).origin.y
                    && at.y < band(rect).origin.y + band(rect).size.height
            };
            if in_track(hit.track) {
                hovered = Some((hit.scroll_id, hit.axis));
                break 'bars;
            }
        }
        // Keep the previously hovered bar while the pointer is still within
        // its band, so it does not flicker on the boundary.
        let changed = self.hovered_bar != hovered;
        self.hovered_bar = hovered;
        if changed {
            self.bars = Bars { hits };
            let _ = self.proxy.send_event(UserEvent::Render(RenderEvent::Bars {
                hits: self.bars.hits.clone(),
            }));
        }
    }

    /// Drag a thumb: the pointer moves the thumb in proportion to the
    /// content, so `delta_pointer × content_size / track_length` scrolls
    /// the frame, clamped to its range (spec 0009, child 0002, AC-8).
    fn drag_thumb(
        &mut self,
        scroll_id: ScrollId,
        axis: Axis,
        start_offset: PointF,
        start_pointer: PointF,
        at: PointF,
    ) {
        let Some(list) = self.page.list_mut() else {
            return;
        };
        let Some(index) = list.spatial.iter().position(|node| {
            matches!(
                node.kind,
                SpatialKind::ScrollFrame { scroll_id: sid, .. } if sid == scroll_id
            )
        }) else {
            return;
        };
        let SpatialKind::ScrollFrame {
            frame_rect,
            content_size,
            offset,
            ..
        } = &mut list.spatial[index].kind
        else {
            return;
        };
        let delta = match axis {
            Axis::Y => at.y - start_pointer.y,
            Axis::X => at.x - start_pointer.x,
        };
        let (track_length, overhang, current) = match axis {
            Axis::Y => {
                let track = frame_rect.size.height - 2.0 * scrolling::INDICATOR_INSET;
                (
                    track,
                    (content_size.height - frame_rect.size.height).max(0.0),
                    start_offset.y,
                )
            }
            Axis::X => {
                let track = frame_rect.size.width - 2.0 * scrolling::INDICATOR_INSET;
                (
                    track,
                    (content_size.width - frame_rect.size.width).max(0.0),
                    start_offset.x,
                )
            }
        };
        if track_length <= 0.0 {
            return;
        }
        let target = current + delta * (overhang / track_length);
        let target = target.clamp(0.0, overhang);
        let (x, y) = match axis {
            Axis::Y => (offset.x, target),
            Axis::X => (target, offset.y),
        };
        *offset = ScrollOffset { x, y, ..*offset };
        self.scrolled.insert(scroll_id, *offset);
        self.scrolled_at = Some(Instant::now());
    }

    /// The current bar geometry for every scrollable frame on screen, in
    /// page root space (CSS px).
    fn current_bars(&self) -> Vec<scrolling::BarHit> {
        let Some(list) = self.page.list() else {
            return Vec::new();
        };
        let mut hits = Vec::new();
        for (index, node) in list.spatial.iter().enumerate() {
            let SpatialKind::ScrollFrame {
                scroll_id,
                frame_rect,
                content_size,
                offset,
                user_scrollable,
                ..
            } = &node.kind
            else {
                continue;
            };
            // Convert the frame rect to root space. The scrollbar track is
            // anchored at the scrollport's top-left corner, which stays fixed
            // as the frame scrolls (spec 0009, child 0002, AC-8).
            let origin =
                scrolling::scroll_port_origin_in_root(list, SpatialId(index as u32), *offset);
            let Some(origin) = origin else {
                continue;
            };
            let frame = RectF {
                origin,
                size: frame_rect.size,
            };
            for (axis, scrollable) in [Axis::X, Axis::Y]
                .into_iter()
                .zip([user_scrollable[0], user_scrollable[1]])
            {
                if !scrollable {
                    continue;
                }
                if let Some((track, thumb)) = scrolling::bar_geometry(
                    frame,
                    *content_size,
                    *offset,
                    axis,
                    scrolling::INDICATOR_WIDTH,
                ) {
                    hits.push(scrolling::BarHit {
                        scroll_id: *scroll_id,
                        axis,
                        track,
                        thumb,
                    });
                }
            }
        }
        hits
    }

    /// The offset of one scroll frame, in page root space (CSS px), or
    /// `None` when it is not on screen.
    fn frame_offset(&self, scroll_id: ScrollId) -> Option<PointF> {
        let list = self.page.list()?;
        let (index, _) = list.scroll_frames().find(|(_, sid)| *sid == scroll_id)?;
        match list.spatial[index.0 as usize].kind {
            SpatialKind::ScrollFrame { offset, .. } => Some(PointF {
                x: offset.x,
                y: offset.y,
            }),
            _ => None,
        }
    }

    /// A press at `at` (page CSS px): on a thumb it starts a drag, on a
    /// track it scrolls one page toward the pointer. Bar presses never
    /// reach the page.
    fn press_bar(&mut self, at: PointF) -> bool {
        let hits = self.current_bars();
        for hit in &hits {
            let contains = |rect: RectF| {
                at.x >= rect.origin.x
                    && at.x < rect.origin.x + rect.size.width
                    && at.y >= rect.origin.y
                    && at.y < rect.origin.y + rect.size.height
            };
            if contains(hit.thumb) {
                let start_offset = self.frame_offset(hit.scroll_id).unwrap_or_default();
                self.drag = Some((hit.scroll_id, hit.axis, start_offset, at));
                return true;
            }
            if contains(hit.track) {
                // Press the track: scroll one page toward the pointer.
                let Some(list) = self.page.list_mut() else {
                    return false;
                };
                let Some(index) = list.spatial.iter().position(|node| {
                    matches!(
                        node.kind,
                        SpatialKind::ScrollFrame { scroll_id: sid, .. } if sid == hit.scroll_id
                    )
                }) else {
                    return false;
                };
                let SpatialKind::ScrollFrame {
                    frame_rect,
                    content_size,
                    offset: current_offset,
                    ..
                } = &list.spatial[index].kind
                else {
                    return false;
                };
                let page = match hit.axis {
                    Axis::Y => {
                        (frame_rect.size.height - scrolling::PAGE_OVERLAP).max(scrolling::LINE)
                    }
                    Axis::X => {
                        (frame_rect.size.width - scrolling::PAGE_OVERLAP).max(scrolling::LINE)
                    }
                };
                let (target_x, target_y) = match hit.axis {
                    Axis::Y => {
                        let target = if at.y < hit.thumb.origin.y {
                            current_offset.y - page
                        } else {
                            current_offset.y + page
                        };
                        (
                            current_offset.x,
                            target.clamp(
                                0.0,
                                (content_size.height - frame_rect.size.height).max(0.0),
                            ),
                        )
                    }
                    Axis::X => {
                        let target = if at.x < hit.thumb.origin.x {
                            current_offset.x - page
                        } else {
                            current_offset.x + page
                        };
                        (
                            target
                                .clamp(0.0, (content_size.width - frame_rect.size.width).max(0.0)),
                            current_offset.y,
                        )
                    }
                };
                let moved = self.scroll_frame_by_id(hit.scroll_id, |_, _, _| (target_x, target_y));
                if let Some((_, offset)) = moved {
                    self.scrolled.insert(hit.scroll_id, offset);
                    self.scrolled_at = Some(Instant::now());
                }
                return true;
            }
        }
        false
    }

    /// Draws and presents, then tells the page where user scrolls ended and
    /// sends `BeginFrame` if it wants frames and none is in flight. A lost
    /// surface is made again once; false (after telling the main thread)
    /// when that fails too.
    fn draw(&mut self) -> bool {
        self.dirty = false;
        let drawn = match self.render() {
            Err(RenderError::SurfaceLost) => {
                warn!("the drawing surface was lost; making it again");
                self.renderer
                    .recreate_surface(Arc::clone(&self.window))
                    .and_then(|()| self.render())
            }
            drawn => drawn,
        };
        if let Err(error) = drawn {
            error!(%error, "drawing failed");
            let window = self.window.id();
            let _ = self
                .proxy
                .send_event(UserEvent::Render(RenderEvent::Failed { window, error }));
            return false;
        }
        let page = self.page.id();
        let scrolled: Vec<_> = self.scrolled.drain().collect();
        for (scroll_id, offset) in scrolled {
            self.send(ToContent::ScrollChanged {
                page,
                scroll_id,
                x: offset.x,
                y: offset.y,
            });
        }
        if self.page.take_begin_frame() {
            let frame_time = self.started.elapsed().as_secs_f64() * 1000.0;
            self.send(ToContent::BeginFrame { page, frame_time });
        }
        true
    }

    fn render(&mut self) -> Result<(), RenderError> {
        let now = Instant::now();
        // Advance any smooth scroll one frame at a time (spec 0009, child
        // 0002, AC-9).
        if let Some(smooth) = self.smooth {
            match scrolling::smooth_step(&smooth, now) {
                Some((next, done)) => {
                    if let Some((id, offset)) =
                        self.scroll_frame_by_id(smooth.frame, |_, _, _| (next.x, next.y))
                        && done
                    {
                        self.scrolled.insert(id, offset);
                    }
                    if done {
                        self.smooth = None;
                    }
                }
                None => {
                    self.smooth = None;
                }
            }
            self.dirty = true;
        }
        // One overlay bar per scrollable frame, in page root space (CSS px).
        let (hits, indicators): (Vec<scrolling::BarHit>, Vec<ScrollIndicator>) =
            self.page.list().map_or((Vec::new(), Vec::new()), |list| {
                let mut hits = Vec::new();
                let mut out = Vec::new();
                for (index, node) in list.spatial.iter().enumerate() {
                    let SpatialKind::ScrollFrame {
                        scroll_id,
                        frame_rect,
                        content_size,
                        offset,
                        user_scrollable,
                        ..
                    } = &node.kind
                    else {
                        continue;
                    };
                    let origin = scrolling::scroll_port_origin_in_root(
                        list,
                        SpatialId(index as u32),
                        *offset,
                    );
                    let Some(origin) = origin else {
                        continue;
                    };
                    let frame = RectF {
                        origin,
                        size: frame_rect.size,
                    };
                    for (axis, scrollable) in [Axis::X, Axis::Y]
                        .into_iter()
                        .zip([user_scrollable[0], user_scrollable[1]])
                    {
                        if !scrollable {
                            continue;
                        }
                        if let Some((track, thumb)) = scrolling::bar_geometry(
                            frame,
                            *content_size,
                            *offset,
                            axis,
                            scrolling::INDICATOR_WIDTH,
                        ) {
                            hits.push(scrolling::BarHit {
                                scroll_id: *scroll_id,
                                axis,
                                track,
                                thumb,
                            });
                        }
                        let hovered = self.hovered_bar == Some((*scroll_id, axis));
                        let width = if hovered {
                            scrolling::HOVER_WIDTH
                        } else {
                            scrolling::INDICATOR_WIDTH
                        };
                        let Some((_, thumb)) =
                            scrolling::bar_geometry(frame, *content_size, *offset, axis, width)
                        else {
                            continue;
                        };
                        let alpha = if hovered {
                            1.0
                        } else {
                            self.scrolled_at.map_or(0.0, |at| {
                                scrolling::indicator_alpha(at, now).unwrap_or(0.0)
                            })
                        };
                        if alpha <= 0.0 {
                            continue;
                        }
                        out.push(ScrollIndicator {
                            rect: thumb,
                            color: ColorF {
                                r: 0.0,
                                g: 0.0,
                                b: 0.0,
                                a: scrolling::INDICATOR_ALPHA * alpha,
                            },
                        });
                    }
                }
                (hits, out)
            });
        // Report changed bar geometry to the main thread so it can hit test
        // presses against the bars before forwarding them to the page.
        self.bars = Bars { hits };
        let _ = self.proxy.send_event(UserEvent::Render(RenderEvent::Bars {
            hits: self.bars.hits.clone(),
        }));
        // The frame after the fade ends draws without it; then it stops.
        if self
            .scrolled_at
            .is_some_and(|at| scrolling::indicator_gone(at, now))
        {
            self.scrolled_at = None;
        }
        let chrome = self.chrome.as_ref().map(|draw| Chrome {
            list: &draw.list,
            scale: draw.scale,
            slot: draw.slot,
            overlay: draw.overlay.as_ref(),
        });
        let indicators = if indicators.is_empty() {
            None
        } else {
            Some(indicators.as_slice())
        };
        self.renderer
            .render(self.page.list(), self.scale, chrome.as_ref(), indicators)
    }

    /// Sends to the live content process. A failure is logged, not fatal:
    /// the window keeps showing the last good frame (spec 0002).
    fn send(&self, message: ToContent) {
        let Some(content) = &self.content else {
            return;
        };
        if let Err(err) = content.send(message) {
            warn!(%err, "could not reach the content process");
        }
    }
}
