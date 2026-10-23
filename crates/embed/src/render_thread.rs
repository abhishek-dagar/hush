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
use std::time::Instant;

use display_list::{DisplayList, PageId, PointF, ScrollId, ScrollOffset};
use ipc::{ContentSender, NavigationId, ToBrowser, ToContent};
use navigation::EntryId;
use render::{Chrome, PageSlot, RenderError, Renderer};
use tracing::{debug, error, warn};
use winit::event_loop::EventLoopProxy;
use winit::window::{Window, WindowId};

use crate::EmbedError;
use crate::browser::UserEvent;
use crate::page_view::PageView;
use crate::scrolling::{self, KeyScroll};

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
    /// A user scroll by `dx`, `dy` CSS px at `at`, in page CSS px.
    Scroll { at: PointF, dx: f32, dy: f32 },
    /// A key scroll of the root viewport (AC-30).
    ScrollKey(KeyScroll),
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
            let wake = self
                .scrolled_at
                .map(|at| scrolling::indicator_wake(at, now));
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
            RenderMsg::Scroll { at, dx, dy } => {
                let moved = self.page.scroll_by(at, dx, dy);
                self.user_scrolled(moved);
            }
            RenderMsg::ScrollKey(key) => {
                let moved = self
                    .page
                    .scroll_viewport(|offset, frame, content| key.target(offset, frame, content));
                self.user_scrolled(moved);
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
        let indicator = self.scrolled_at.and_then(|at| {
            let (frame, content, offset) = self.page.viewport()?;
            scrolling::indicator(frame, content, offset, at, now)
        });
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
        self.renderer.render(
            self.page.list(),
            self.scale,
            chrome.as_ref(),
            indicator.as_ref(),
        )
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
