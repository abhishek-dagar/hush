//! Messages and channels between the browser process and the content process.
//!
//! This is the only crate that names `ipc-channel` types (spec 0001, IPC boundary).
//! Everything that crosses the process boundary is a plain `serde` type defined here.

#![forbid(unsafe_code)]

use std::fmt::Display;
use std::ops::Deref;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use display_list::{Epoch, FontKey, ImageFormat, ImageKey, PageId, ScrollId, SizeF};
use ipc_channel::ipc::{self, IpcOneShotServer, IpcReceiver, IpcSender, IpcSharedMemory};
use serde::{Deserialize, Serialize};

/// Bytes in shared memory, sent without copying them through the channel
/// (spec 0002, Transport).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SharedBytes(IpcSharedMemory);

impl SharedBytes {
    /// Copies `bytes` into a new shared memory region.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self(IpcSharedMemory::from_bytes(bytes))
    }
}

impl Deref for SharedBytes {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.0
    }
}

impl AsRef<[u8]> for SharedBytes {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// Where a font's bytes come from (spec 0032, AC-1 to AC-5).
///
/// The content process is untrusted, so the browser checks every case: a
/// `System` path must lie under an allowed font root, and `SameBytesAs`
/// must name a key the same page already registered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FontSource {
    /// A system font file the browser opens itself, by its path. The
    /// browser maps or reads the file once per canonical path, so every
    /// face of it shares the bytes (spec 0032, AC-1, AC-2).
    System {
        /// The file's path, UTF 8. A path that is not UTF 8 is sent as
        /// [`FontSource::Bytes`] instead.
        path: String,
    },
    /// The font file's bytes, in shared memory.
    Bytes(SharedBytes),
    /// The bytes already sent for `key` on the same page (spec 0032,
    /// AC-4): another face or variation of one web font file.
    SameBytesAs(FontKey),
}

/// One category in the memory report (spec 0032, AC-10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryCategory {
    /// The process's physical footprint, in bytes.
    Footprint,
    /// File backed font bytes the browser maps, in bytes.
    FontMapped,
    /// Font bytes the browser holds on its heap, in bytes.
    FontHeap,
    /// Image textures on the GPU, estimated from their sizes.
    ImagesGpu,
    /// The glyph atlas texture, estimated from its side.
    AtlasGpu,
    /// The offscreen layer textures, estimated from their sizes.
    LayersGpu,
    /// The window's swap chain, estimated from its size and latency.
    SurfaceGpu,
    /// The script engine's heap, in bytes.
    JsHeap,
    /// The number of DOM nodes (a count, not bytes).
    DomNodes,
}

impl MemoryCategory {
    /// The name the memory report's JSON uses.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Footprint => "footprint",
            Self::FontMapped => "font_mapped",
            Self::FontHeap => "font_heap",
            Self::ImagesGpu => "images_gpu",
            Self::AtlasGpu => "atlas_gpu",
            Self::LayersGpu => "layers_gpu",
            Self::SurfaceGpu => "surface_gpu",
            Self::JsHeap => "js_heap",
            Self::DomNodes => "dom_nodes",
        }
    }
}

/// Bytes that fit in a message are sent inline; bigger ones go through
/// shared memory (spec 0004, Messages added to `ipc`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum IpcBytes {
    /// Up to [`IpcBytes::INLINE_MAX`] bytes, inside the message.
    Inline(Vec<u8>),
    /// Anything bigger, in a shared memory region.
    Shared(SharedBytes),
}

impl IpcBytes {
    /// The most bytes sent inline: one coalesced fetch chunk.
    pub const INLINE_MAX: usize = 64 * 1024;

    /// Wraps `bytes`, inline when they are small enough.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        if bytes.len() <= Self::INLINE_MAX {
            Self::Inline(bytes.to_vec())
        } else {
            Self::Shared(SharedBytes::from_bytes(bytes))
        }
    }
}

impl Deref for IpcBytes {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match self {
            Self::Inline(bytes) => bytes,
            Self::Shared(bytes) => bytes,
        }
    }
}

/// One load of a page: the first load, a reload or a live reload. Assigned
/// by the browser, increasing per page (spec 0004, AC-11).
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct NavigationId(pub u64);

impl NavigationId {
    /// The navigation after this one.
    pub fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// One fetch within a navigation. Assigned by the page, unique within one
/// navigation; [`FetchId::DOCUMENT`] is the document itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FetchId(pub u32);

impl FetchId {
    /// The document's own body.
    pub const DOCUMENT: Self = Self(0);
}

/// Why a fetch failed (spec 0004, child 0001, reason table).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum FetchError {
    /// The input is not a valid URL.
    #[error("invalid URL")]
    InvalidUrl,
    /// The scheme is not one this load may use.
    #[error("unsupported scheme")]
    UnsupportedScheme,
    /// The host name did not resolve.
    #[error("DNS lookup failed")]
    DnsFailed,
    /// No connection could be made.
    #[error("connection failed")]
    ConnectFailed,
    /// The TLS handshake or the certificate check failed.
    #[error("TLS failed: {0}")]
    TlsFailed(String),
    /// A limit on time ran out.
    #[error("timed out")]
    TimedOut,
    /// More redirects than the limit.
    #[error("too many redirects")]
    TooManyRedirects,
    /// The body is over its size limit.
    #[error("too large")]
    TooLarge,
    /// The file does not exist.
    #[error("file not found")]
    FileNotFound,
    /// The page may not read that file.
    #[error("file access denied")]
    FileAccessDenied,
    /// An `http` subresource of an `https` page.
    #[error("mixed content blocked")]
    MixedContentBlocked,
    /// The response is a type this browser cannot show.
    #[error("unsupported content type {0}")]
    UnsupportedContentType(String),
    /// The content process crashed.
    #[error("content process crashed")]
    ContentCrashed,
    /// A newer navigation or the page cancelled it.
    #[error("cancelled")]
    Cancelled,
}

/// The page's session history as scripts see it (spec 0008, Interface
/// surface): `history.length` and where the current entry sits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryInfo {
    /// How many entries there are.
    pub length: u32,
    /// The current entry's index, from 0.
    pub index: u32,
}

/// What a commit carries beyond the document itself (spec 0008, Interface
/// surface). The browser decides it when the navigation begins; the
/// network passes it through untouched into `CommitNavigation`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct CommitParams {
    /// The root scroll offset to restore, in CSS px, when going back or
    /// forward to an entry that saved one (AC-23).
    pub restore_scroll: Option<(f32, f32)>,
    /// The session history once this navigation commits.
    pub history: HistoryInfo,
}

/// Who asked a page started load to happen (spec 0008, Interface surface).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PageNavigationOrigin {
    /// The user activated a link (AC-17).
    Link,
    /// A script used `location` (AC-27).
    Script,
}

/// What a page started load does to the session history (spec 0008,
/// Interface surface).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PageNavigationAction {
    /// A new entry.
    Push,
    /// The current entry is replaced.
    Replace,
    /// The current URL loads again.
    Reload,
}

/// Where a same document move scrolls the page (spec 0008, Interface
/// surface, `SameDocument`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ScrollTarget {
    /// The element the URL's fragment names, else the top (AC-20).
    Fragment,
    /// A root scroll offset in CSS px, saved when the entry was left
    /// (AC-23).
    Offset(f32, f32),
}

/// The most URL text one `LinkHover` carries, in bytes (spec 0008,
/// Interface surface).
pub const MAX_LINK_HOVER_BYTES: usize = 4 * 1024;

/// The most text one `ClipboardWrite` carries (spec 0008, Interface
/// surface).
pub const MAX_CLIPBOARD_WRITE_BYTES: usize = 1024 * 1024;

/// The longest URL a page may ask to load, in bytes (spec 0008, AC-18).
pub const MAX_PAGE_URL_BYTES: usize = 2 * 1024 * 1024;

/// How the content process treats a committed document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DocumentKind {
    /// HTML, parsed as such.
    Document,
    /// Plain text, shown preformatted.
    PlainText,
    /// The built in error page for a load that failed. The body that follows
    /// is the filled in error page, parsed as HTML.
    Error {
        /// Why the load failed.
        reason: FetchError,
        /// What the user asked for, as typed.
        input: String,
    },
}

/// What a subresource is for, which picks its `Accept` header and its size
/// limit (spec 0004, Value sourcing).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Destination {
    /// A `<link rel=stylesheet>`.
    Style,
    /// A classic `<script src>`.
    Script,
    /// An `<img>` or a `background-image`.
    Image,
    /// The page's icon.
    Favicon,
    /// An `@font-face` source (spec 0006, AC-11).
    Font,
}

/// A pointer button (spec 0006, Value sourcing, Pointer button).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PointerButton {
    /// The main button, usually the left one.
    Primary,
    /// The context menu button, usually the right one.
    Secondary,
    /// The wheel button.
    Middle,
}

/// The modifier keys held with a key.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Modifiers {
    /// Shift.
    pub shift: bool,
    /// Control.
    pub ctrl: bool,
    /// Alt, or Option on macOS.
    pub alt: bool,
    /// Command on macOS, the Windows key elsewhere.
    pub meta: bool,
}

/// A key the page did not use, so the browser scrolls a frame with it
/// (spec 0009, child 0002, AC-9; moved here from `embed::scrolling`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum KeyScroll {
    /// One line up (40 CSS px).
    LineUp,
    /// One line down (40 CSS px).
    LineDown,
    /// One line left (40 CSS px).
    LineLeft,
    /// One line right (40 CSS px).
    LineRight,
    /// One viewport-sized page up (minus 40 px overlap).
    PageUp,
    /// One viewport-sized page down (minus 40 px overlap).
    PageDown,
    /// The top of the scrollable content.
    Home,
    /// The bottom of the scrollable content.
    End,
}

/// A key, by what it means rather than where it sits (spec 0006,
/// Interface surface).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Key {
    /// Tab.
    Tab,
    /// Enter or Return.
    Enter,
    /// The space bar.
    Space,
    /// Escape.
    Escape,
    /// Up arrow.
    ArrowUp,
    /// Down arrow.
    ArrowDown,
    /// Left arrow.
    ArrowLeft,
    /// Right arrow.
    ArrowRight,
    /// Home.
    Home,
    /// End.
    End,
    /// Page Up.
    PageUp,
    /// Page Down.
    PageDown,
    /// Backspace: deletes before the caret in a text input (spec 0008,
    /// AC-13). It never navigates (AC-29).
    Backspace,
    /// Delete: deletes after the caret.
    Delete,
    /// A key that types this text.
    Character(String),
    /// Any other key, modifiers alone excepted: the browser keeps those.
    Other,
}

/// Which way focus moves through the Tab order.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum FocusDirection {
    /// Tab: to the next element.
    Forward,
    /// Shift+Tab: to the previous element.
    Backward,
}

/// The pointer shape a page asks for (spec 0006, AC-18).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum CursorKind {
    /// The arrow.
    #[default]
    Default,
    /// The hand, over links and buttons that ask for it.
    Pointer,
    /// The text beam.
    Text,
}

/// One input event for a page, already in page CSS px (spec 0006,
/// Interface surface). The browser runs its own shortcuts first and
/// forwards only what is left.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum InputEvent {
    /// The pointer moved to `x`, `y` in page CSS px, before scrolling. The
    /// page keeps only the latest move and hit tests it at the next frame.
    PointerMove {
        /// Horizontal position from the page's left edge.
        x: f32,
        /// Vertical position from the page's top edge.
        y: f32,
    },
    /// The pointer left the page: nothing is hovered or pressed any more.
    PointerLeave,
    /// A pointer button went down at `x`, `y` (page CSS px).
    PointerDown {
        /// Horizontal position from the page's left edge.
        x: f32,
        /// Vertical position from the page's top edge.
        y: f32,
        /// Which button.
        button: PointerButton,
    },
    /// A pointer button went up at `x`, `y`, which may be off the page
    /// when the press began on it.
    PointerUp {
        /// Horizontal position from the page's left edge.
        x: f32,
        /// Vertical position from the page's top edge.
        y: f32,
        /// Which button.
        button: PointerButton,
    },
    /// A key went down or up.
    Key {
        /// Which key.
        key: Key,
        /// The modifiers held with it.
        modifiers: Modifiers,
        /// Down when true, up when false.
        pressed: bool,
        /// A repeat from holding the key down.
        repeat: bool,
    },
    /// Focus comes into the page from the browser, at the start of the Tab
    /// order (`Forward`) or its end (`Backward`).
    FocusEnter {
        /// Which end to start from.
        direction: FocusDirection,
    },
    /// The window gained or lost OS focus.
    WindowFocus {
        /// Whether it has focus now.
        focused: bool,
    },
    /// Text the user typed, from the OS text input (winit's
    /// `KeyEvent.text`, dead keys and layouts applied), with no Cmd or
    /// Ctrl held (spec 0008, AC-13).
    Text {
        /// What to insert.
        text: String,
    },
    /// The platform's select all key (Cmd+A, Ctrl+A) (spec 0008, AC-15).
    SelectAll,
    /// The platform's copy key: answer with `ClipboardWrite` under
    /// `request` if there is a selection to copy.
    Copy {
        /// The browser's id for this copy, used once.
        request: u32,
    },
    /// The platform's cut key: like `Copy`, and the selection goes.
    Cut {
        /// The browser's id for this cut, used once.
        request: u32,
    },
    /// The platform's paste key, with the clipboard's text, read by the
    /// browser right then (spec 0008, Key invariants).
    Paste {
        /// The clipboard text, as the OS gave it.
        text: String,
    },
}

/// A message the browser process sends to the content process.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ToContent {
    /// Finish current work and exit.
    Shutdown,
    /// A tab opened: create its page.
    CreatePage {
        /// The id the browser gave the page.
        page: PageId,
        /// The viewport in CSS px.
        viewport: SizeF,
        /// Device pixels per CSS px.
        scale: f32,
    },
    /// The window or zoom changed.
    Resize {
        /// Which page.
        page: PageId,
        /// The new viewport in CSS px.
        viewport: SizeF,
        /// Device pixels per CSS px.
        scale: f32,
    },
    /// Time to produce a frame, sent only while the page asked for frames.
    BeginFrame {
        /// Which page.
        page: PageId,
        /// Milliseconds on a monotonic clock.
        frame_time: f64,
    },
    /// The render side moved a scroll frame (a user scroll). The page keeps
    /// the offset but not a new generation (spec 0002, Scrolling).
    ScrollChanged {
        /// Which page.
        page: PageId,
        /// Which scroll frame.
        scroll_id: ScrollId,
        /// The new horizontal offset in CSS px.
        x: f32,
        /// The new vertical offset in CSS px.
        y: f32,
    },
    /// Pointer or keyboard input for the page (spec 0006, AC-12). An
    /// unknown page drops it.
    Input {
        /// Which page.
        page: PageId,
        /// What happened.
        event: InputEvent,
    },
    /// A tab closed: drop its page.
    ClosePage {
        /// Which page.
        page: PageId,
    },
    /// A new navigation started: stop work for the old document and expect
    /// a commit for this one (spec 0004, Navigation lifecycle).
    Navigate {
        /// Which page.
        page: PageId,
        /// The new navigation.
        navigation: NavigationId,
    },
    /// The browser fetched a document and checked it against policy, or the
    /// load failed: create the new document. Its body follows as
    /// [`ToContent::FetchChunk`] messages under [`FetchId::DOCUMENT`].
    CommitNavigation {
        /// Which page.
        page: PageId,
        /// The navigation this document belongs to.
        navigation: NavigationId,
        /// The document's final URL, after redirects.
        url: String,
        /// The `Content-Type` the document came with, without parameters.
        content_type: String,
        /// The charset from the `Content-Type` header, if any.
        charset: Option<String>,
        /// How to treat the body.
        kind: DocumentKind,
        /// The root scroll offset to restore, for a move back or forward
        /// (spec 0008, AC-23).
        restore_scroll: Option<(f32, f32)>,
        /// The session history once this document commits (spec 0008).
        history: HistoryInfo,
    },
    /// The session history changed without a new document: an entry was
    /// evicted or replaced (spec 0008, Interface surface).
    HistoryChanged {
        /// Which page.
        page: PageId,
        /// The navigation the page is on; a stale one is dropped.
        navigation: NavigationId,
        /// The history now.
        history: HistoryInfo,
    },
    /// The browser moved inside the current document (spec 0008, AC-20,
    /// AC-21): no reload, only a new URL and a scroll. It keeps the
    /// current navigation, so fetches in flight go on.
    SameDocument {
        /// Which page.
        page: PageId,
        /// The page's current navigation; a stale one is dropped.
        navigation: NavigationId,
        /// The document's new URL, with its new fragment.
        url: String,
        /// Where to scroll.
        scroll: ScrollTarget,
        /// The history now.
        history: HistoryInfo,
    },
    /// Stop after commit (spec 0008, AC-26): the page cancels every fetch
    /// still running, keeps what it has, and fires `load`.
    Stop {
        /// Which page.
        page: PageId,
        /// The navigation to stop; a stale one is dropped.
        navigation: NavigationId,
    },
    /// A subresource's response headers arrived; its body follows as
    /// [`ToContent::FetchChunk`] messages. The document's headers are inside
    /// [`ToContent::CommitNavigation`] instead.
    FetchResponse {
        /// Which page.
        page: PageId,
        /// Which navigation.
        navigation: NavigationId,
        /// Which fetch.
        fetch: FetchId,
        /// The HTTP status; 200 for a file.
        status: u16,
        /// The response headers, names lowercased.
        headers: Vec<(String, String)>,
        /// The URL after redirects.
        final_url: String,
    },
    /// Body bytes arrived.
    FetchChunk {
        /// Which page.
        page: PageId,
        /// Which navigation.
        navigation: NavigationId,
        /// Which fetch.
        fetch: FetchId,
        /// The bytes, already decompressed.
        data: IpcBytes,
    },
    /// The body ended.
    FetchDone {
        /// Which page.
        page: PageId,
        /// Which navigation.
        navigation: NavigationId,
        /// Which fetch.
        fetch: FetchId,
    },
    /// The fetch failed, including a body cut short after it began.
    FetchFailed {
        /// Which page.
        page: PageId,
        /// Which navigation.
        navigation: NavigationId,
        /// Which fetch.
        fetch: FetchId,
        /// Why.
        reason: FetchError,
    },
    /// Ask the content process for its perf marks and footprint (spec 0007,
    /// IPC messages). Perf mode only: content ignores this and logs a
    /// warning when it was not launched with `--perf`.
    PerfSample,
    /// Ask the content process for its memory report counters (spec 0032,
    /// AC-10). Only sent when the browser runs with `--memory-report`.
    ReportMemory,
}

/// A message the content process sends to the browser process.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ToBrowser {
    /// The content process is connected and ready for work.
    Ready {
        /// The content process's OS process id.
        pid: u32,
    },
    /// The page has work to draw: please send `BeginFrame`.
    NeedsFrame {
        /// Which page.
        page: PageId,
    },
    /// A new display list, in answer to a `BeginFrame`.
    DisplayList {
        /// Which page.
        page: PageId,
        /// The navigation whose document it paints; the browser drops a
        /// list from an older one (spec 0004, AC-11).
        navigation: NavigationId,
        /// The list's epoch, also inside the encoded list.
        epoch: Epoch,
        /// The `postcard` encoded `DisplayList`.
        data: SharedBytes,
    },
    /// Nothing changed since the last frame: stop sending `BeginFrame`.
    FrameSkipped {
        /// Which page.
        page: PageId,
    },
    /// Registers a font, before the first list that uses its key.
    AddFont {
        /// Which page.
        page: PageId,
        /// The key lists will name it by.
        key: FontKey,
        /// Where the font file's bytes come from (spec 0032, AC-1).
        source: FontSource,
        /// Which face in a collection file; 0 for a single font file.
        index: u32,
        /// Variation axis settings for a variable face, as (OpenType tag,
        /// user space value), such as `wght` 700 for bold (spec 0004,
        /// AC-26). Empty for a static face.
        variations: Vec<(u32, f32)>,
    },
    /// Registers an image, before the first list that uses its key.
    AddImage {
        /// Which page.
        page: PageId,
        /// The key lists will name it by.
        key: ImageKey,
        /// Width and height in pixels.
        size: (u32, u32),
        /// How the pixels are laid out.
        format: ImageFormat,
        /// The pixels, rows top to bottom with no padding.
        pixels: SharedBytes,
    },
    /// The page needs a subresource. The browser checks it against the
    /// page's document before loading anything (spec 0004, AC-8, AC-9) and
    /// answers with `FetchResponse` and the body, or `FetchFailed`.
    Fetch {
        /// Which page.
        page: PageId,
        /// The navigation that wants it.
        navigation: NavigationId,
        /// The page's id for this fetch, never [`FetchId::DOCUMENT`].
        fetch: FetchId,
        /// The absolute URL, as the page resolved it.
        url: String,
        /// What it is for.
        destination: Destination,
    },
    /// The element that wanted a fetch went away: stop loading it.
    CancelFetch {
        /// Which page.
        page: PageId,
        /// Which navigation.
        navigation: NavigationId,
        /// Which fetch.
        fetch: FetchId,
    },
    /// Frees a font key, after the last list that used it.
    DeleteFont {
        /// Which page.
        page: PageId,
        /// The freed key.
        key: FontKey,
    },
    /// Frees an image key, after the last list that used it.
    DeleteImage {
        /// Which page.
        page: PageId,
        /// The freed key.
        key: ImageKey,
    },
    /// The pointer shape to show while the pointer is over the page (spec
    /// 0006, AC-18), sent when it changes.
    SetCursor {
        /// Which page.
        page: PageId,
        /// The shape.
        cursor: CursorKind,
    },
    /// The page did not use a scroll key, so the browser scrolls the target
    /// frame, or the viewport when `target` is `None`, with a smooth scroll
    /// (spec 0009, child 0002, AC-9).
    ScrollKey {
        /// Which page.
        page: PageId,
        /// Which key.
        key: KeyScroll,
        /// Which frame to scroll, or `None` for the viewport.
        target: Option<ScrollId>,
    },
    /// Tab or Shift+Tab went past the end of the page's Tab order; the page
    /// cleared its focus. The browser decides where focus goes next (spec
    /// 0006, AC-13).
    FocusLeft {
        /// Which page.
        page: PageId,
        /// Which way focus was moving.
        direction: FocusDirection,
    },
    /// The window title to show (spec 0004, AC-36): the document's
    /// `<title>`, or its URL when it has none.
    TitleChanged {
        /// Which page.
        page: PageId,
        /// At most [`MAX_TITLE_CHARS`] characters.
        title: String,
    },
    /// The page asks to load a new document: a link activated or a script
    /// used `location` (spec 0008, AC-17, AC-27). The browser checks it
    /// against the document `net` recorded (AC-18) and decides.
    NavigateRequest {
        /// Which page.
        page: PageId,
        /// The navigation whose document asks; a stale one is dropped.
        navigation: NavigationId,
        /// The absolute URL, as the page resolved it, at most
        /// [`MAX_PAGE_URL_BYTES`]. Untrusted.
        url: String,
        /// Who asked.
        origin: PageNavigationOrigin,
        /// What it does to the history.
        action: PageNavigationAction,
    },
    /// A script asks to move through the session history:
    /// `history.back()`, `forward()` or `go(delta)` (spec 0008, AC-27).
    HistoryGo {
        /// Which page.
        page: PageId,
        /// The navigation whose document asks; a stale one is dropped.
        navigation: NavigationId,
        /// How many entries to move; 0 reloads.
        delta: i32,
    },
    /// The answer to a `Copy` or `Cut`: the text to put on the clipboard.
    /// The browser writes it only for a request it sent within the last
    /// second, each id once (spec 0008, Key invariants).
    ClipboardWrite {
        /// Which page.
        page: PageId,
        /// The `Copy` or `Cut` it answers.
        request: u32,
        /// At most [`MAX_CLIPBOARD_WRITE_BYTES`].
        text: String,
    },
    /// The link under the pointer, or with keyboard focus, changed (spec
    /// 0008, AC-19): its resolved URL, at most [`MAX_LINK_HOVER_BYTES`],
    /// or `None` when no link is hovered or focused any more. Untrusted:
    /// the browser shows it, nothing more.
    LinkHover {
        /// Which page.
        page: PageId,
        /// The link's absolute URL.
        url: Option<String>,
    },
    /// The document's `load` event fired: loading is over (spec 0008,
    /// AC-25).
    LoadComplete {
        /// Which page.
        page: PageId,
        /// The navigation whose document loaded.
        navigation: NavigationId,
    },
    /// The page's favicon decoded (spec 0004, AC-37).
    FaviconChanged {
        /// Which page.
        page: PageId,
        /// Width in pixels, at most 256.
        width: u32,
        /// Height in pixels, at most 256.
        height: u32,
        /// Straight alpha RGBA8, rows top to bottom with no padding.
        rgba: SharedBytes,
    },
    /// The content process's answer to [`ToContent::PerfSample`] (spec 0007,
    /// IPC messages): its marks and its own footprint. Perf mode only; the
    /// content process sends it only when it was launched with `--perf`.
    PerfSample {
        /// Navigation start to `load`, in µs, or `None` when no navigation
        /// has been recorded.
        nav_to_load_us: Option<u64>,
        /// Wall time spent inside the script engine, in µs; 0 when untimed.
        script_us: u64,
        /// The content process's physical footprint in bytes, or `None`
        /// when this OS has no reader (spec 0007, Value sourcing).
        footprint_bytes: Option<u64>,
    },
    /// The content process's memory report counters, in answer to
    /// [`ToContent::ReportMemory`] (spec 0032, AC-10).
    MemoryReport {
        /// One counter per category the content process fills.
        counters: Vec<(MemoryCategory, u64)>,
    },
}

/// The longest title `TitleChanged` carries, in characters (spec 0004,
/// IPC messages).
pub const MAX_TITLE_CHARS: usize = 1024;

/// What the content process hands the browser during the bootstrap handshake:
/// the sender the browser talks through, and the receiver it listens on.
#[derive(Serialize, Deserialize)]
struct Handshake {
    to_content: IpcSender<ToContent>,
    from_content: IpcReceiver<ToBrowser>,
}

/// Errors raised while setting up or using a channel.
#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    /// The underlying channel failed.
    #[error("ipc transport failed while trying to {context}: {message}")]
    Transport {
        /// What the caller was trying to do.
        context: &'static str,
        /// The transport's own error message.
        message: String,
    },
    /// The content process did not connect in time.
    #[error("the content process did not connect within {0:?}")]
    HandshakeTimeout(Duration),
    /// The other process closed the channel.
    #[error("the other process closed the channel")]
    Disconnected,
}

fn transport<E: Display>(context: &'static str) -> impl FnOnce(E) -> IpcError {
    move |err| IpcError::Transport {
        context,
        message: err.to_string(),
    }
}

/// The browser side of a not yet connected content process.
///
/// Create it, pass [`BootstrapServer::name`] to the content process on its
/// command line, then call [`BootstrapServer::accept`].
pub struct BootstrapServer {
    server: IpcOneShotServer<Handshake>,
    name: String,
}

impl BootstrapServer {
    /// Opens a one shot server for one content process to connect to.
    pub fn new() -> Result<Self, IpcError> {
        let (server, name) =
            IpcOneShotServer::new().map_err(transport("open the bootstrap server"))?;
        Ok(Self { server, name })
    }

    /// The one shot server name the content process connects to.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Waits for the content process to connect, giving up after `timeout`.
    pub fn accept(self, timeout: Duration) -> Result<ContentChannel, IpcError> {
        // `accept` blocks with no timeout of its own, so it runs on a helper
        // thread. If the child never connects, that thread is left parked
        // until the process exits, which is acceptable for a startup failure.
        let (done_tx, done_rx) = mpsc::channel();
        thread::Builder::new()
            .name("ipc-bootstrap".into())
            .spawn(move || {
                let result = self.server.accept().map(|(_, handshake)| handshake);
                let _ = done_tx.send(result);
            })
            .map_err(transport("spawn the bootstrap thread"))?;

        let handshake = done_rx
            .recv_timeout(timeout)
            .map_err(|_| IpcError::HandshakeTimeout(timeout))?
            .map_err(transport("accept the content process"))?;

        Ok(ContentChannel {
            sender: handshake.to_content,
            receiver: handshake.from_content,
        })
    }
}

/// The browser process's connection to the content process.
pub struct ContentChannel {
    sender: IpcSender<ToContent>,
    receiver: IpcReceiver<ToBrowser>,
}

impl ContentChannel {
    /// Sends a message to the content process.
    pub fn send(&self, message: ToContent) -> Result<(), IpcError> {
        self.sender
            .send(message)
            .map_err(transport("send to content"))
    }

    /// Waits up to `timeout` for the next message from the content process.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<ToBrowser, IpcError> {
        self.receiver
            .try_recv_timeout(timeout)
            .map_err(transport("receive from content"))
    }

    /// Splits the channel so another thread can wait for messages while this
    /// one keeps sending.
    pub fn split(self) -> (ContentSender, ContentReceiver) {
        (
            ContentSender {
                sender: self.sender,
            },
            ContentReceiver {
                receiver: self.receiver,
            },
        )
    }
}

/// The sending half of a [`ContentChannel`]. Clones send into the same
/// channel, so the network thread can stream bodies straight to the page.
#[derive(Clone)]
pub struct ContentSender {
    sender: IpcSender<ToContent>,
}

impl ContentSender {
    /// Sends a message to the content process.
    pub fn send(&self, message: ToContent) -> Result<(), IpcError> {
        self.sender
            .send(message)
            .map_err(transport("send to content"))
    }
}

/// The receiving half of a [`ContentChannel`].
pub struct ContentReceiver {
    receiver: IpcReceiver<ToBrowser>,
}

impl ContentReceiver {
    /// Blocks until the content process sends a message. Returns
    /// [`IpcError::Disconnected`] once the content process is gone.
    pub fn recv(&self) -> Result<ToBrowser, IpcError> {
        self.receiver.recv().map_err(|_| IpcError::Disconnected)
    }
}

/// The content process's connection to the browser process.
pub struct BrowserChannel {
    sender: IpcSender<ToBrowser>,
    receiver: IpcReceiver<ToContent>,
}

impl BrowserChannel {
    /// Connects to the browser's bootstrap server named on the command line.
    pub fn connect(server_name: &str) -> Result<Self, IpcError> {
        let bootstrap: IpcSender<Handshake> = IpcSender::connect(server_name.to_owned())
            .map_err(transport("connect to the browser"))?;
        let (to_content, from_browser) =
            ipc::channel().map_err(transport("create the inbound channel"))?;
        let (to_browser, from_content) =
            ipc::channel().map_err(transport("create the outbound channel"))?;

        bootstrap
            .send(Handshake {
                to_content,
                from_content,
            })
            .map_err(transport("send the handshake"))?;

        Ok(Self {
            sender: to_browser,
            receiver: from_browser,
        })
    }

    /// Sends a message to the browser process.
    pub fn send(&self, message: ToBrowser) -> Result<(), IpcError> {
        self.sender
            .send(message)
            .map_err(transport("send to browser"))
    }

    /// Blocks until the browser sends a message. Returns
    /// [`IpcError::Disconnected`] once the browser process is gone.
    pub fn recv(&self) -> Result<ToContent, IpcError> {
        self.receiver.recv().map_err(|_| IpcError::Disconnected)
    }

    /// Waits up to `timeout` for a message, so the page can wake for its
    /// timers (spec 0004, child 0006, Event loop). `Ok(None)` when the time
    /// ran out first.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Option<ToContent>, IpcError> {
        match self.receiver.try_recv_timeout(timeout) {
            Ok(message) => Ok(Some(message)),
            Err(ipc_channel::TryRecvError::Empty) => Ok(None),
            Err(ipc_channel::TryRecvError::IpcError(_)) => Err(IpcError::Disconnected),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AC-11: the perf messages survive a real channel round trip, the
    /// same `ipc-channel` path production uses (spec 0007, IPC messages).
    #[test]
    fn perf_sample_messages_round_trip() {
        // The ask: no fields, no surprises.
        let (tx, rx) = ipc::channel::<ToContent>().unwrap();
        tx.send(ToContent::PerfSample).unwrap();
        assert_eq!(rx.recv().unwrap(), ToContent::PerfSample);

        // The answer: every field arrives as it left.
        let (tx, rx) = ipc::channel::<ToBrowser>().unwrap();
        let message = ToBrowser::PerfSample {
            nav_to_load_us: Some(1_234_567),
            script_us: 890,
            footprint_bytes: Some(12_345_678),
        };
        tx.send(message.clone()).unwrap();
        assert_eq!(rx.recv().unwrap(), message);
    }

    /// AC-10: the memory report ask and answer survive the same channel.
    #[test]
    fn memory_report_messages_round_trip() {
        let (tx, rx) = ipc::channel::<ToContent>().unwrap();
        tx.send(ToContent::ReportMemory).unwrap();
        assert_eq!(rx.recv().unwrap(), ToContent::ReportMemory);

        let (tx, rx) = ipc::channel::<ToBrowser>().unwrap();
        let message = ToBrowser::MemoryReport {
            counters: vec![
                (MemoryCategory::Footprint, 1),
                (MemoryCategory::JsHeap, 2),
                (MemoryCategory::DomNodes, 3),
            ],
        };
        tx.send(message.clone()).unwrap();
        assert_eq!(rx.recv().unwrap(), message);
    }

    /// AC-1, AC-4, AC-5: every `AddFont` source, including the shared
    /// region a web font crosses in, arrives as it left.
    #[test]
    fn add_font_sources_round_trip() {
        let (tx, rx) = ipc::channel::<ToBrowser>().unwrap();
        for source in [
            FontSource::System {
                path: "/System/Library/Fonts/SFNS.ttf".to_owned(),
            },
            FontSource::Bytes(SharedBytes::from_bytes(b"font bytes")),
            FontSource::SameBytesAs(FontKey(2)),
        ] {
            let message = ToBrowser::AddFont {
                page: PageId(1),
                key: FontKey(1),
                source,
                index: 0,
                variations: vec![(0x7767_6874, 700.0)],
            };
            tx.send(message.clone()).unwrap();
            assert_eq!(rx.recv().unwrap(), message);
        }
    }
}
