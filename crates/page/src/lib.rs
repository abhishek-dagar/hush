//! A page: one tab's documents on the script thread, and the rendering update
//! that turns them into display lists (spec 0002, Rendering update).
//!
//! This is the only crate that runs the whole pipeline: parsing, style, box
//! building, layout, paint and encoding. `embed` creates pages and wires
//! them to IPC; the browser commits each navigation's document to the page
//! and streams its body in (spec 0004, Navigation lifecycle).

#![forbid(unsafe_code)]

mod anchors;
mod dom_sink;
mod editor;
mod fonts;
mod head;
mod images;
mod input;
mod links;
mod resources;
pub mod sample;
mod script_host;
mod scripts;
mod styles;

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::time::Instant;

use base::{Au, Point, Size};
use css::{Device, Stylesheet};
use display_list::{
    ColorF, DisplayList, Epoch, MAX_ENCODED_LEN, PageId, PointF, RectF, ScrollId, ScrollOffset,
    SizeF,
};
use dom::{Document, FrameChange, FrameId, NodeId, NodeRef};
use html::{ParseStep, Parser, ParserOptions};
use ipc::{Destination, DocumentKind, FetchError, FetchId, NavigationId, SharedBytes};
use layout::{BoxTree, FragmentTree};
pub use script::ReadyState;
use script::{EventTarget, ScriptEngine, ScriptHost};
pub use text::FontContext;
use tracing::{debug, debug_span, info, warn};

use crate::head::Head;
pub use crate::input::InputOutcome;
use crate::input::InputState;
use crate::script_host::PageHost;
use crate::scripts::{Prepared, Runnable, Scripts};

pub use css::ua_stylesheet;
pub use dom_sink::DomSink;
pub use editor::{EditState, MAX_PASTE_BYTES, PAGE_SELECTION};
pub use fonts::{BLOCK_PERIOD, FontRequest, MAX_FONT_FACES, WebFonts};
pub use images::{ImageRequest, Images};
pub use resources::ResourceTable;
pub use styles::{RENDER_BLOCKING_CAP, StyleRequest, StyleStats, Styles};

/// Why a frame could not be sent. The page keeps its last good frame on
/// screen and tries again on the next `BeginFrame`.
#[derive(Debug, thiserror::Error)]
pub enum PageError {
    /// `postcard` could not encode the list.
    #[error("could not encode the display list")]
    Encode(#[from] postcard::Error),
    /// The encoded list is over the size limit.
    #[error("the display list is {0} bytes, over the {MAX_ENCODED_LEN} byte limit")]
    TooLarge(usize),
}

/// Hands out the page's subresource fetch ids: unique within one
/// navigation, from 1, since 0 is the document (spec 0004, Value sourcing).
/// Style sheets and images share it.
#[derive(Debug)]
pub struct FetchIds(u32);

impl Default for FetchIds {
    fn default() -> Self {
        Self(1)
    }
}

impl FetchIds {
    /// The next id.
    pub fn next_id(&mut self) -> FetchId {
        let id = FetchId(self.0);
        self.0 = self.0.saturating_add(1);
        id
    }
}

/// A committed document's script engine and the host it calls back into
/// (spec 0004, child 0006, Runtime per page).
struct Script {
    engine: Box<dyn ScriptEngine>,
    host: Rc<RefCell<PageHost>>,
}

/// How far the committed document's lifecycle got (spec 0004, Navigation
/// lifecycle).
#[derive(Debug, Default)]
struct Lifecycle {
    /// The parser finished.
    parsed: bool,
    /// `defer` scripts ran and `DOMContentLoaded` fired.
    content_loaded: bool,
    /// Every subresource finished and `load` fired.
    loaded: bool,
    /// `load` fired and the browser was not told yet (spec 0008, AC-25).
    load_unreported: bool,
}

/// One document and its layout state.
///
/// The box tree lives here, beside the document, because `layout` depends on
/// `dom` and so `Document` cannot hold it (spec 0002, Crates and who depends
/// on whom).
pub struct Frame {
    /// The document.
    pub document: Document,
    /// The document's boxes.
    pub boxes: BoxTree,
    /// The iframe element showing this frame, `None` for the main frame.
    pub parent: Option<NodeRef>,
    /// The viewport the document lays out in.
    pub viewport: Size,
    /// The last layout result, which hit testing reads. `None` until the
    /// first layout.
    pub fragments: Option<FragmentTree>,
    /// Whether the style pass computes this document's styles with the
    /// cascade. Committed documents do; hand built ones (tests, the sample
    /// page) and iframes' initial about:blank assign styles directly.
    pub computes_styles: bool,
}

/// One tab's page: the main frame plus a frame per connected iframe, all on
/// the same script thread (spec 0002, Frames, AC-16).
pub struct Page {
    id: PageId,
    main_frame: Frame,
    /// Iframe documents by id. Ids only grow, so a torn down frame's id never
    /// names a later frame.
    frames: BTreeMap<FrameId, Frame>,
    next_frame: u32,
    scale: f32,
    epoch: Epoch,
    /// The viewport or scale changed since the last frame.
    needs_update: bool,
    /// No boxes were built yet.
    needs_first_build: bool,
    /// `NeedsFrame` was sent and no `BeginFrame` has answered it yet.
    frame_requested: bool,
    /// Each scroll container's offset and script scroll generation
    /// (spec 0002, Scrolling). Paint reads it; `ScrollChanged` and script
    /// scrolls write it.
    scroll: HashMap<ScrollId, ScrollOffset>,
    /// Font and image keys.
    resources: ResourceTable,
    /// The page's fonts, which text is shaped with.
    fonts: FontContext,
    /// The committed document's `@font-face` faces (spec 0006, AC-10).
    /// Only the main frame lays out, so it is the only frame with a set.
    web_fonts: WebFonts,
    /// A web face changed state: the next frame lays out all the text again
    /// (spec 0006, Relayout on font load).
    relayout_text: bool,
    /// The navigation the page is on, from the last `Navigate`. Messages
    /// for any other are stale and dropped (spec 0004, AC-11).
    navigation: Option<NavigationId>,
    /// The navigation whose document the page shows, from the last
    /// `CommitNavigation`. Its lists carry it, so the browser can tell the
    /// old document's frames from the new one's first paint (AC-11, AC-12).
    committed: NavigationId,
    /// The parser for the document while its body streams in.
    parser: Option<Parser<NodeId>>,
    /// The user agent sheet, parsed once per content process.
    ua: Rc<Stylesheet>,
    /// The committed document's sheets and rules; `None` before a commit.
    styles: Option<Styles>,
    /// The committed document's images; `None` before a commit.
    images: Option<Images>,
    /// The committed navigation's subresource fetch ids.
    fetch_ids: FetchIds,
    /// The committed document's script engine; `None` before a commit,
    /// after a new navigation starts, or when the engine could not start.
    script: Option<Script>,
    /// The committed document's script elements.
    scripts: Scripts,
    /// The committed document's lifecycle.
    lifecycle: Lifecycle,
    /// The committed document's title and favicon.
    head: Head,
    /// What the last style pass did.
    last_style: StyleStats,
    /// The pointer and the hover state it drives (spec 0006).
    input: InputState,
    /// Committed documents get a script engine. Off for the browser's
    /// chrome documents, which run no script (spec 0003, key invariants).
    scripting: bool,
    /// Each text input's caret and selection (spec 0008, AC-13).
    edits: editor::Edits,
    /// The input whose caret moved since the last frame, which scrolls
    /// to keep it in view (AC-14).
    caret_moved: Option<NodeId>,
    /// What selected text sits on (AC-14).
    selection_color: ColorF,
    /// A cap in bytes on every text input's value: the address's (AC-10).
    text_cap: Option<usize>,
    /// The link URL last reported for the status bubble (spec 0008,
    /// AC-19).
    link_hover: Option<String>,
    /// The overlay layer's roots, by scroll id (spec 0008, Decision): the
    /// chrome's `data-layer="overlay"` elements. Empty for pages.
    overlay_roots: Vec<ScrollId>,
    /// The last frame's overlay list, while there are overlay roots.
    overlay: Option<DisplayList>,
    /// The root scroll still owed to a fragment or a restored offset
    /// (spec 0008, AC-20, AC-23).
    pending_scroll: Option<anchors::PendingScroll>,
    /// The session history as scripts see it (spec 0008, AC-27).
    history: ipc::HistoryInfo,
}

impl Page {
    /// A page showing `document` in a `viewport` of CSS px, at `scale`
    /// device pixels per CSS px. It needs a first frame.
    pub fn new(id: PageId, document: Document, viewport: SizeF, scale: f32) -> Self {
        Self {
            id,
            main_frame: Frame {
                document,
                boxes: BoxTree::default(),
                parent: None,
                viewport: to_au(viewport),
                fragments: None,
                computes_styles: false,
            },
            frames: BTreeMap::new(),
            next_frame: FrameId::MAIN.0 + 1,
            scale,
            epoch: Epoch::default(),
            needs_update: true,
            needs_first_build: true,
            frame_requested: false,
            scroll: HashMap::new(),
            resources: ResourceTable::new(id),
            fonts: FontContext::new(),
            web_fonts: WebFonts::new(),
            relayout_text: false,
            navigation: None,
            committed: NavigationId(0),
            parser: None,
            ua: Rc::new(Stylesheet::empty(css::Origin::Ua)),
            styles: None,
            images: None,
            fetch_ids: FetchIds::default(),
            script: None,
            scripts: Scripts::default(),
            lifecycle: Lifecycle::default(),
            head: Head::default(),
            last_style: StyleStats::default(),
            input: InputState::default(),
            scripting: true,
            edits: editor::Edits::default(),
            caret_moved: None,
            selection_color: editor::PAGE_SELECTION,
            text_cap: None,
            link_hover: None,
            overlay_roots: Vec::new(),
            overlay: None,
            pending_scroll: None,
            history: ipc::HistoryInfo::default(),
        }
    }

    /// A page with an empty document, waiting for its first navigation to
    /// commit (spec 0004, `CreatePage` no longer implies a document).
    /// Committed documents are styled with `ua`, the user agent sheet the
    /// content process parsed once ([`ua_stylesheet`]).
    pub fn blank(id: PageId, viewport: SizeF, scale: f32, ua: Rc<Stylesheet>) -> Self {
        let mut page = Self::new(id, Document::new("about:blank"), viewport, scale);
        page.ua = ua;
        page
    }

    /// Turns scripting off (or back on) for the documents committed from
    /// now on: no script engine starts, and the parser treats `noscript`
    /// as markup. The browser's chrome documents run with it off
    /// (spec 0003, key invariants; spec 0006, AC-21).
    pub fn set_scripting(&mut self, scripting: bool) {
        self.scripting = scripting;
    }

    /// What selected text in text inputs sits on: the chrome passes its
    /// `--selection` token; pages keep the UA color (spec 0008, AC-14).
    pub fn set_selection_color(&mut self, color: ColorF) {
        self.selection_color = color;
    }

    /// Paints what the stacking contexts rooted at `roots` paint into an
    /// overlay list of its own (spec 0008, Decision): the chrome's
    /// `data-layer="overlay"` elements. Each must be positioned with a
    /// `z-index`, so it roots a stacking context.
    pub fn set_overlay_roots(&mut self, roots: &[NodeId]) {
        self.overlay_roots = roots
            .iter()
            .map(|&node| paint::scroll_id(FrameId::MAIN.0, node))
            .collect();
        self.needs_update = true;
    }

    /// The overlay list painted with the last list [`Page::begin_frame`]
    /// returned, drawn after the page; `None` without overlay roots.
    pub fn overlay_list(&self) -> Option<&DisplayList> {
        self.overlay.as_ref()
    }

    /// Caps every text input's value at `bytes` (spec 0008, AC-10): the
    /// chrome's address takes 8 KB.
    pub fn set_text_cap(&mut self, bytes: Option<usize>) {
        self.text_cap = bytes;
    }

    /// A new navigation started (`Navigate`): the old document stops
    /// loading, and only messages for `navigation` count from now on.
    pub fn navigate(&mut self, navigation: NavigationId) {
        self.navigation = Some(navigation);
        self.parser = None;
        // Its timers and events stop with it; the document stays on
        // screen until the next one commits.
        self.script = None;
    }

    /// Stop after commit (`Stop`, spec 0008, AC-26): the parser ends with
    /// what arrived, so no more of the document loads. The browser fails
    /// every fetch still running, and `load` follows once they are in.
    /// A stale navigation is ignored.
    pub fn stop(&mut self, navigation: NavigationId) {
        if !self.is_current(navigation) || self.committed != navigation {
            debug!(?navigation, "stale stop dropped");
            return;
        }
        if let Some(parser) = &mut self.parser {
            info!("stopped; keeping what arrived");
            let step = parser.end(&mut DomSink::new(&mut self.main_frame.document));
            self.pump(step);
        }
    }

    /// Whether `navigation` is the page's current one.
    fn is_current(&self, navigation: NavigationId) -> bool {
        self.navigation == Some(navigation)
    }

    /// The browser committed a document (`CommitNavigation`): it replaces
    /// the old one, empty, and its body arrives through
    /// [`Page::fetch_chunk`]. Returns false for a stale navigation.
    pub fn commit_navigation(
        &mut self,
        navigation: NavigationId,
        url: &str,
        charset: Option<String>,
        kind: &DocumentKind,
    ) -> bool {
        if !self.is_current(navigation) {
            debug!(?navigation, "stale commit dropped");
            return false;
        }
        match kind {
            DocumentKind::Document => info!(url, "document committed"),
            DocumentKind::PlainText => info!(url, "plain text committed"),
            DocumentKind::Error { reason, input } => {
                info!(%reason, input, "error page committed");
            }
        }
        self.committed = navigation;
        let frame = &mut self.main_frame;
        frame.document = Document::new(url);
        frame.boxes = BoxTree::default();
        frame.fragments = None;
        frame.computes_styles = true;
        let device = Device {
            width: frame.viewport.width.to_f32_px(),
            height: frame.viewport.height.to_f32_px(),
        };
        self.styles = Some(Styles::new(Rc::clone(&self.ua), device, Instant::now()));
        if let Some(old) = self.images.replace(Images::new()) {
            old.release_all(&mut self.resources);
        }
        self.web_fonts.document_replaced();
        for key in self.fonts.forget_web_fonts() {
            self.resources.release_font(key);
        }
        self.fetch_ids = FetchIds::default();
        self.scripts = Scripts::default();
        self.lifecycle = Lifecycle::default();
        self.head = Head::default();
        self.script = if self.scripting { new_script() } else { None };
        // The old document's iframes go with it.
        self.frames.clear();
        self.scroll.clear();
        self.input.document_replaced();
        self.edits.clear();
        self.caret_moved = None;
        // A new document starts at the top, or at its fragment (AC-23).
        self.pending_scroll = url::Url::parse(url)
            .ok()
            .and_then(|url| url.fragment().map(|_| anchors::PendingScroll::Fragment));
        self.needs_first_build = true;
        self.needs_update = true;
        self.parser = Some(Parser::new(ParserOptions {
            encoding_hint: charset,
            scripting: self.scripting,
            plain_text: *kind == DocumentKind::PlainText,
        }));
        true
    }

    /// Body bytes arrived. The document's go straight into the parser, so
    /// the page can show what it has before the rest arrives (AC-5).
    pub fn fetch_chunk(&mut self, navigation: NavigationId, fetch: FetchId, data: &[u8]) {
        if !self.is_current(navigation) {
            return;
        }
        if fetch != FetchId::DOCUMENT {
            match &mut self.images {
                _ if self.head.owns(fetch) => self.head.fetch_chunk(data),
                _ if self.web_fonts.owns(fetch) => self.web_fonts.fetch_chunk(fetch, data),
                Some(images) if images.owns(fetch) => images.fetch_chunk(fetch, data),
                _ if self.scripts.owns(fetch) => self.scripts.fetch_chunk(fetch, data),
                _ => {
                    if let Some(styles) = &mut self.styles {
                        styles.fetch_chunk(fetch, data);
                    }
                }
            }
            return;
        }
        let Some(parser) = &mut self.parser else {
            return;
        };
        let document = &mut self.main_frame.document;
        let step = parser.feed(&mut DomSink::new(document), data);
        document.set_encoding(parser.encoding());
        self.pump(step);
    }

    /// A subresource's headers arrived (`FetchResponse`).
    pub fn fetch_response(
        &mut self,
        navigation: NavigationId,
        fetch: FetchId,
        status: u16,
        headers: &[(String, String)],
        final_url: &str,
    ) {
        if !self.is_current(navigation) {
            return;
        }
        match &mut self.images {
            _ if self.head.owns(fetch) => self.head.fetch_response(status),
            _ if self.web_fonts.owns(fetch) => self.web_fonts.fetch_response(fetch, status),
            Some(images) if images.owns(fetch) => images.fetch_response(fetch, status),
            _ if self.scripts.owns(fetch) => self.scripts.fetch_response(fetch, status, headers),
            _ => {
                if let Some(styles) = &mut self.styles {
                    styles.fetch_response(fetch, status, headers, final_url);
                }
            }
        }
    }

    /// A body ended: the document finishes parsing, a style sheet is
    /// parsed and restyles the page, an image is decoded and shown.
    pub fn fetch_done(&mut self, navigation: NavigationId, fetch: FetchId) {
        if !self.is_current(navigation) {
            return;
        }
        if self.head.owns(fetch) {
            self.head.fetch_done(self.id);
            return;
        }
        if self.web_fonts.owns(fetch) {
            let changed = self
                .web_fonts
                .fetch_done(fetch, &mut self.fetch_ids, Instant::now());
            self.fonts_changed(changed);
            return;
        }
        if let Some(images) = &mut self.images
            && images.owns(fetch)
        {
            images.fetch_done(&mut self.main_frame.document, fetch, &mut self.resources);
            // A background may show it too.
            self.needs_update = true;
            return;
        }
        if self.scripts.owns(fetch) {
            // It runs when the task ends (`advance`).
            self.scripts.fetch_done(fetch);
            return;
        }
        if fetch != FetchId::DOCUMENT {
            if let Some(styles) = &mut self.styles {
                styles.fetch_done(&mut self.main_frame.document, fetch);
            }
            return;
        }
        if let Some(parser) = &mut self.parser {
            let step = parser.end(&mut DomSink::new(&mut self.main_frame.document));
            self.pump(step);
        }
    }

    /// A fetch failed. A document cut short keeps what arrived; a style
    /// sheet is skipped and stops holding the first paint; an image shows
    /// its `alt` text.
    pub fn fetch_failed(&mut self, navigation: NavigationId, fetch: FetchId, reason: &FetchError) {
        if !self.is_current(navigation) {
            return;
        }
        warn!(%reason, fetch = fetch.0, "fetch failed");
        if self.head.owns(fetch) {
            self.head.fetch_failed();
            return;
        }
        if self.web_fonts.owns(fetch) {
            let changed = self
                .web_fonts
                .fetch_failed(fetch, &mut self.fetch_ids, Instant::now());
            self.fonts_changed(changed);
            return;
        }
        if let Some(images) = &mut self.images
            && images.owns(fetch)
        {
            images.fetch_failed(&mut self.main_frame.document, fetch);
            return;
        }
        if self.scripts.owns(fetch) {
            self.scripts.fetch_failed(fetch);
            return;
        }
        if fetch != FetchId::DOCUMENT {
            if let Some(styles) = &mut self.styles {
                styles.fetch_failed(fetch);
            }
            return;
        }
        self.fetch_done(navigation, fetch);
    }

    /// A web face changed state (or not): when it did, the next frame lays
    /// out the document's text again.
    fn fonts_changed(&mut self, changed: bool) {
        if changed {
            self.relayout_text = true;
            self.needs_update = true;
        }
    }

    /// The `Fetch` and `CancelFetch` messages to send for the page's style
    /// sheets, images, scripts, favicon and fonts, under the committed
    /// navigation.
    pub fn take_fetch_messages(&mut self) -> Vec<ipc::ToBrowser> {
        let (page, navigation) = (self.id, self.committed);
        let fetch = |fetch, url, destination| ipc::ToBrowser::Fetch {
            page,
            navigation,
            fetch,
            url,
            destination,
        };
        let cancel = |fetch| ipc::ToBrowser::CancelFetch {
            page,
            navigation,
            fetch,
        };
        let styles = self
            .styles
            .as_mut()
            .map(Styles::take_requests)
            .unwrap_or_default()
            .into_iter()
            .map(|request| match request {
                StyleRequest::Fetch { fetch: id, url } => fetch(id, url, Destination::Style),
                StyleRequest::Cancel { fetch: id } => cancel(id),
            });
        let images = self
            .images
            .as_mut()
            .map(Images::take_requests)
            .unwrap_or_default()
            .into_iter()
            .map(|request| match request {
                ImageRequest::Fetch { fetch: id, url } => fetch(id, url, Destination::Image),
                ImageRequest::Cancel { fetch: id } => cancel(id),
            });
        let scripts = self
            .scripts
            .take_requests()
            .into_iter()
            .map(|(id, url)| fetch(id, url, Destination::Script));
        let fonts = self
            .web_fonts
            .take_requests()
            .into_iter()
            .map(|request| match request {
                FontRequest::Fetch { fetch: id, url } => fetch(id, url, Destination::Font),
                FontRequest::Cancel { fetch: id } => cancel(id),
            });
        let favicon = self
            .lifecycle
            .parsed
            .then(|| {
                self.head
                    .request_favicon(&self.main_frame.document, &mut self.fetch_ids)
            })
            .flatten()
            .map(|(id, url)| fetch(id, url, Destination::Favicon));
        styles
            .chain(images)
            .chain(scripts)
            .chain(favicon)
            .chain(fonts)
            .collect()
    }

    /// The `TitleChanged` and `FaviconChanged` messages to send (AC-36,
    /// AC-37). Only a committed document has a title.
    pub fn take_head_messages(&mut self) -> Vec<ipc::ToBrowser> {
        let document = &self.main_frame.document;
        if self.styles.is_some() && (!self.head.title_sent() || document.needs_frame()) {
            self.head
                .refresh_title(self.id, document, self.lifecycle.parsed);
        }
        self.head.take_messages()
    }

    /// `LoadComplete`, once, after the committed document's `load` event
    /// fired (spec 0008, AC-25).
    pub fn take_load_message(&mut self) -> Option<ipc::ToBrowser> {
        std::mem::take(&mut self.lifecycle.load_unreported).then_some(
            ipc::ToBrowser::LoadComplete {
                page: self.id,
                navigation: self.committed,
            },
        )
    }

    /// The committed document's sheets and rules, if a document committed.
    pub fn styles(&self) -> Option<&Styles> {
        self.styles.as_ref()
    }

    /// What the last frame's style pass did (spec 0004, AC-21).
    pub fn last_style_stats(&self) -> StyleStats {
        self.last_style
    }

    /// Shapes the page's text with `fonts` from now on, such as a
    /// [`FontContext::with_fonts`] context that makes text dumps the same on
    /// every machine. Call it before the first frame.
    pub fn use_fonts(&mut self, fonts: FontContext) {
        self.fonts = fonts;
    }

    /// The page's id.
    pub fn id(&self) -> PageId {
        self.id
    }

    /// The main frame.
    pub fn main_frame(&self) -> &Frame {
        &self.main_frame
    }

    /// The main frame, to change its document.
    pub fn main_frame_mut(&mut self) -> &mut Frame {
        &mut self.main_frame
    }

    /// The frame `id`, if it is still alive.
    pub fn frame(&self, id: FrameId) -> Option<&Frame> {
        if id == FrameId::MAIN {
            return Some(&self.main_frame);
        }
        self.frames.get(&id)
    }

    /// The frame `id` to change, if it is still alive.
    pub fn frame_mut(&mut self, id: FrameId) -> Option<&mut Frame> {
        if id == FrameId::MAIN {
            return Some(&mut self.main_frame);
        }
        self.frames.get_mut(&id)
    }

    /// Every frame's id, the main frame first.
    pub fn frame_ids(&self) -> impl Iterator<Item = FrameId> + '_ {
        std::iter::once(FrameId::MAIN).chain(self.frames.keys().copied())
    }

    /// Device pixels per CSS px.
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// The epoch of the last list sent.
    pub fn epoch(&self) -> Epoch {
        self.epoch
    }

    /// The navigation whose document the page shows: `NavigationId(0)`
    /// until the first commit. Display lists carry it (AC-11).
    pub fn navigation(&self) -> NavigationId {
        self.committed
    }

    /// Scrolls the text input whose caret moved sideways so the caret is
    /// inside its content box (spec 0008, AC-14), after layout placed the
    /// text.
    fn keep_caret_visible(&mut self) {
        let Some(id) = self.caret_moved.take() else {
            return;
        };
        if self.editing() != Some(id) {
            return;
        }
        let Some(edit) = self.edit_paint() else {
            return;
        };
        let Some(caret) = edit.caret else {
            return;
        };
        let frame = &self.main_frame;
        let Some((field, texts)) = frame.fragments.as_ref().and_then(|t| field_texts(t, id)) else {
            return;
        };
        let Some(caret_x) = texts.iter().find_map(|(at, text)| {
            let start = text.text_start as usize;
            let holds = (start..start + text.text.len()).contains(&caret)
                || (caret == start + text.text.len() && caret == edit.len)
                || edit.len == 0;
            holds
                .then(|| paint::offset_x(text, if edit.len == 0 { 0 } else { caret }))
                .flatten()
                .map(|x| at.x + x)
        }) else {
            return;
        };
        let border = frame
            .boxes
            .get(field.box_id)
            .map(|b| layout::border_widths(&b.style))
            .unwrap_or_default();
        let left = border.left + field.padding.left;
        let right = field.size.width - border.right - field.padding.right;
        let node = NodeRef {
            frame: FrameId::MAIN,
            node: id,
        };
        let scroll_id = paint::scroll_id(FrameId::MAIN.0, id);
        let old = self.scroll_offset(scroll_id).x;
        let one_px = Au::from_px(1.0);
        let mut scroll = Au::from_px(old);
        if caret_x - scroll < left {
            scroll = caret_x - left;
        } else if caret_x + one_px - scroll > right {
            scroll = caret_x + one_px - right;
        }
        let scroll = scroll.to_f32_px().max(0.0);
        if (scroll - old).abs() > f32::EPSILON {
            self.scroll_to(node, scroll, 0.0);
        }
    }

    /// The grapheme boundary of the text input `id`'s value nearest to `x`,
    /// measured from its border box's left edge as the pointer sees it
    /// (spec 0008, AC-13). `None` before layout, for an empty value, or a
    /// password, whose bullets do not line up with its bytes.
    pub(crate) fn caret_at(&self, id: NodeId, x: Au) -> Option<usize> {
        let doc = &self.main_frame.document;
        let value = doc.input_value(id)?;
        if value.is_empty() || editor::limits(doc, id, None).password {
            return None;
        }
        let (_, texts) = self
            .main_frame
            .fragments
            .as_ref()
            .and_then(|tree| field_texts(tree, id))?;
        let scroll = self.scroll_offset(paint::scroll_id(FrameId::MAIN.0, id)).x;
        let target = x + Au::from_px(scroll);
        text::grapheme_boundaries(&value)
            .into_iter()
            .filter_map(|boundary| {
                texts.iter().find_map(|(at, text)| {
                    paint::offset_x(text, boundary).map(|dx| (boundary, at.x + dx))
                })
            })
            .min_by_key(|&(_, at)| (at - target).0.unsigned_abs())
            .map(|(boundary, _)| boundary)
    }

    /// The focused text input's caret and selection, for paint (spec 0008,
    /// AC-14), in the bytes of the text it shows: a password shows one
    /// bullet per character. The caret hides while the window is in the
    /// background.
    fn edit_paint(&self) -> Option<paint::EditPaint> {
        let id = self.editing()?;
        let (value, state) = self.edit_state()?;
        let doc = &self.main_frame.document;
        let password = editor::limits(doc, id, None).password;
        let shown = |offset: usize| {
            if password {
                value[..offset].chars().count() * '•'.len_utf8()
            } else {
                offset
            }
        };
        let (start, end) = state.selection();
        Some(paint::EditPaint {
            field: paint::scroll_id(FrameId::MAIN.0, id),
            caret: self.input.window_focused().then(|| shown(state.caret)),
            selection: (shown(start), shown(end)),
            len: shown(value.len()),
            highlight: self.selection_color,
        })
    }

    /// The page's font and image keys.
    pub fn resources_mut(&mut self) -> &mut ResourceTable {
        &mut self.resources
    }

    /// The scroll offset paint will use for `scroll_id`.
    pub fn scroll_offset(&self, scroll_id: ScrollId) -> ScrollOffset {
        self.scroll.get(&scroll_id).copied().unwrap_or_default()
    }

    /// The render side moved a scroll frame (`ScrollChanged`): keep its
    /// offset, but not a new generation, so the next list does not undo a
    /// user scroll that happens meanwhile (AC-13). Nothing needs repainting.
    pub fn scroll_changed(&mut self, scroll_id: ScrollId, x: f32, y: f32) {
        self.user_scrolled(scroll_id);
        let entry = self.scroll.entry(scroll_id).or_default();
        entry.x = x;
        entry.y = y;
    }

    /// A script scroll of the scroll container `node` to `x`, `y` CSS px
    /// (`scrollTop`, `scrollTo`, anchor navigation): sets the offset and
    /// bumps the generation, so the render side applies it over its own
    /// (AC-13). Offsets below zero become zero; the render side keeps them
    /// inside the scroll range. Only the main frame paints so far.
    pub fn scroll_to(&mut self, node: NodeRef, x: f32, y: f32) {
        if node.frame != FrameId::MAIN {
            return;
        }
        let scroll_id = paint::scroll_id(node.frame.0, node.node);
        let entry = self.scroll.entry(scroll_id).or_default();
        *entry = ScrollOffset {
            x: x.max(0.0),
            y: y.max(0.0),
            generation: entry.generation.wrapping_add(1),
        };
        self.needs_update = true;
    }

    /// Every element's border box in the main frame, in page CSS px with
    /// no scroll applied: the first box fragment of each element, in paint
    /// tree order. Empty before the first layout. The browser's chrome
    /// reads its regions from it (spec 0003, frame anatomy).
    pub fn element_boxes(&self) -> Vec<(NodeId, RectF)> {
        let Some(tree) = self.main_frame.fragments.as_ref() else {
            return Vec::new();
        };
        let mut seen = HashSet::new();
        let mut found = Vec::new();
        let mut stack: Vec<(Point, Rc<layout::Fragment>)> = tree
            .root
            .iter()
            .map(|(at, fragment)| (*at, Rc::clone(fragment)))
            .collect();
        while let Some((at, fragment)) = stack.pop() {
            if let layout::Fragment::Box(b) = &*fragment
                && let Some(node) = b.node
                && seen.insert(node)
            {
                found.push((
                    node,
                    RectF {
                        origin: PointF {
                            x: at.x.to_f32_px(),
                            y: at.y.to_f32_px(),
                        },
                        size: SizeF {
                            width: b.size.width.to_f32_px(),
                            height: b.size.height.to_f32_px(),
                        },
                    },
                ));
            }
            for (offset, child) in fragment.children().iter().rev() {
                stack.push((at.offset_by(*offset), Rc::clone(child)));
            }
        }
        found
    }

    /// The node under `point` (main frame CSS px) and the point in its
    /// border box, honoring the current scroll offsets (AC-14). `None`
    /// outside the page, over no box, or before the first layout.
    pub fn hit_test(&self, point: PointF) -> Option<(NodeRef, PointF)> {
        let hit = self.hit(point)?;
        Some((
            NodeRef {
                frame: FrameId::MAIN,
                node: hit.node,
            },
            PointF {
                x: hit.point.x.to_f32_px(),
                y: hit.point.y.to_f32_px(),
            },
        ))
    }

    /// The main frame's hit test result under `point` (main frame CSS px).
    fn hit(&self, point: PointF) -> Option<layout::HitTestResult> {
        let frame = &self.main_frame;
        let fragments = frame.fragments.as_ref()?;
        let point = Point::new(Au::from_px(point.x), Au::from_px(point.y));
        let offset = |node| {
            let offset = self.scroll_offset(paint::scroll_id(FrameId::MAIN.0, node));
            Point::new(Au::from_px(offset.x), Au::from_px(offset.y))
        };
        layout::hit_test(fragments, &frame.boxes, point, offset)
    }

    /// Applies a new viewport and scale. Only a real change needs a frame.
    pub fn resize(&mut self, viewport: SizeF, scale: f32) {
        let viewport = to_au(viewport);
        if viewport != self.main_frame.viewport || scale != self.scale {
            self.main_frame.viewport = viewport;
            self.scale = scale;
            self.needs_update = true;
            if let Some(styles) = &mut self.styles {
                styles.set_viewport(
                    &mut self.main_frame.document,
                    viewport.width,
                    viewport.height,
                );
            }
        }
    }

    /// Ends a task: creates and tears down iframe frames for the iframes that
    /// joined or left their documents, then lets each document clear its
    /// mutation queue and sweep (spec 0002, Pinning and sweep).
    pub fn end_task(&mut self) {
        loop {
            let changes: Vec<(FrameId, FrameChange)> = self
                .frame_ids()
                .collect::<Vec<_>>()
                .into_iter()
                .flat_map(|id| {
                    let changes = self
                        .frame_mut(id)
                        .map(|frame| frame.document.take_frame_changes())
                        .unwrap_or_default();
                    changes.into_iter().map(move |change| (id, change))
                })
                .collect();
            if changes.is_empty() {
                break;
            }
            for (frame, change) in changes {
                match change {
                    FrameChange::Connected(node) => self.attach_frame(NodeRef { frame, node }),
                    FrameChange::Disconnected(child) => self.tear_down_frame(child),
                }
            }
        }
        self.refresh_styles();
        self.refresh_images();
        self.advance();
        // Collected wrappers unpin their nodes before the sweep looks.
        self.with_script(|engine| engine.sweep_wrappers());
        self.main_frame.document.end_task();
        for frame in self.frames.values_mut() {
            frame.document.end_task();
        }
    }

    /// Picks up `<style>` and `<link>` changes from the task that just ran,
    /// so linked sheets start loading at once.
    fn refresh_styles(&mut self) {
        let parsing = self.parser.is_some();
        let document = &mut self.main_frame.document;
        if let Some(styles) = &mut self.styles
            && document.take_style_sources_changed()
        {
            styles.refresh(document, parsing, &mut self.fetch_ids);
        }
    }

    /// Picks up `<img>` changes from the task that just ran, so new images
    /// start loading at once.
    fn refresh_images(&mut self) {
        let document = &mut self.main_frame.document;
        if let Some(images) = &mut self.images
            && document.take_image_sources_changed()
        {
            images.refresh_elements(document, &mut self.fetch_ids, &mut self.resources);
        }
    }

    /// Lends the main document to the script host, runs `f` with the engine,
    /// and takes the document back. `None` without an engine.
    fn with_script<R>(&mut self, f: impl FnOnce(&mut dyn ScriptEngine) -> R) -> Option<R> {
        let script = self.script.as_mut()?;
        std::mem::swap(
            &mut self.main_frame.document,
            &mut script.host.borrow_mut().document,
        );
        let result = f(&mut *script.engine);
        std::mem::swap(
            &mut self.main_frame.document,
            &mut script.host.borrow_mut().document,
        );
        Some(result)
    }

    /// Runs a classic script, then the microtask checkpoint. Errors were
    /// logged by the engine; the page goes on (spec 0004, Failure handling).
    fn run_script(&mut self, script: &Runnable) {
        let _span = debug_span!("script", url = script.url).entered();
        self.with_script(|engine| {
            let _ = engine.eval_classic(&script.source, &script.url);
            let _ = engine.run_microtasks();
        });
    }

    /// Fires a lifecycle event, then the microtask checkpoint (AC-33).
    fn fire(&mut self, target: EventTarget, event: &str) {
        self.with_script(|engine| {
            let _ = engine.fire_event(target, event);
            let _ = engine.run_microtasks();
        });
    }

    fn set_ready_state(&mut self, state: ReadyState) {
        if let Some(script) = &self.script {
            script.host.borrow_mut().ready_state = state;
        }
    }

    /// Drives the parser on from `step`: each script it pauses at is
    /// prepared, then run or waited for (AC-31).
    fn pump(&mut self, mut step: ParseStep<NodeId>) {
        loop {
            match step {
                ParseStep::Continue => return,
                ParseStep::Done => {
                    self.parser = None;
                    debug!("document parsed");
                    self.finish_parsing();
                    return;
                }
                ParseStep::RunScript(node) => {
                    // Fed again while still waiting for it.
                    if self.scripts.blocking_node() == Some(node) {
                        return;
                    }
                    let prepared =
                        self.scripts
                            .prepare(&self.main_frame.document, node, &mut self.fetch_ids);
                    if prepared == Prepared::Wait && !self.run_blocking() {
                        return;
                    }
                    let Some(parser) = &mut self.parser else {
                        return;
                    };
                    step = parser.resume(&mut DomSink::new(&mut self.main_frame.document));
                }
            }
        }
    }

    /// Runs the parser blocking script if it can run now: fetched, and no
    /// style sheet before it still loading. Returns whether the parser can
    /// go on.
    fn run_blocking(&mut self) -> bool {
        // Sheets linked before the script are in the tree; make sure they
        // are loading before deciding.
        self.refresh_styles();
        let sheets_loading = self.styles.as_ref().is_some_and(Styles::loading);
        let Some(runnable) = self.scripts.take_runnable_blocking(sheets_loading) else {
            return false;
        };
        if let Some(script) = runnable {
            self.run_script(&script);
        }
        true
    }

    /// The parser finished: the document turns interactive, and the rest
    /// of its lifecycle follows as scripts and subresources allow.
    fn finish_parsing(&mut self) {
        self.lifecycle.parsed = true;
        self.set_ready_state(ReadyState::Interactive);
        self.advance();
    }

    /// Moves scripts and the lifecycle on as far as they can go: the
    /// blocking script and the parser behind it, `async` scripts that
    /// arrived, then once parsing ended `defer` scripts in order and
    /// `DOMContentLoaded`, then `load` once nothing is pending (AC-31,
    /// AC-33).
    fn advance(&mut self) {
        if self.scripts.blocking_node().is_some()
            && self.run_blocking()
            && let Some(parser) = &mut self.parser
        {
            let step = parser.resume(&mut DomSink::new(&mut self.main_frame.document));
            self.pump(step);
        }
        for script in self.scripts.take_ready_asyncs() {
            self.run_script(&script);
        }
        if self.lifecycle.parsed && !self.lifecycle.content_loaded {
            while let Some(next) = self.scripts.take_next_defer() {
                if let Some(script) = next {
                    self.run_script(&script);
                }
            }
            if !self.scripts.defers_left() {
                self.lifecycle.content_loaded = true;
                self.fire(EventTarget::Document, "DOMContentLoaded");
            }
        }
        if self.lifecycle.content_loaded && !self.lifecycle.loaded && !self.subresources_pending() {
            self.lifecycle.loaded = true;
            self.lifecycle.load_unreported = true;
            self.set_ready_state(ReadyState::Complete);
            self.fire(EventTarget::Window, "load");
        }
    }

    /// Whether a style sheet, image or script of the document is still
    /// loading.
    fn subresources_pending(&self) -> bool {
        self.parser.is_some()
            || self.scripts.pending()
            || self.styles.as_ref().is_some_and(Styles::loading)
            || self.images.as_ref().is_some_and(Images::loading)
    }

    /// The committed document's `readyState`, from its lifecycle, so a
    /// caller can see load complete without running a script (spec 0005,
    /// Engine changes this needs).
    pub fn ready_state(&self) -> ReadyState {
        match &self.lifecycle {
            Lifecycle { loaded: true, .. } => ReadyState::Complete,
            Lifecycle { parsed: true, .. } => ReadyState::Interactive,
            _ => ReadyState::Loading,
        }
    }

    /// When the page's next timer is due (spec 0004, child 0006, Event
    /// loop): the content process waits for a message until then.
    pub fn next_timer(&self) -> Option<Instant> {
        let script = self
            .script
            .as_ref()
            .and_then(|script| script.host.borrow().next_due());
        // A web font's block period ends on a timer too (spec 0006, AC-11).
        [script, self.web_fonts.next_deadline()]
            .into_iter()
            .flatten()
            .min()
    }

    /// Runs every timer due by `now`, each as its own task, earliest first
    /// (AC-33). Timers they set are due later than `now`, so a timer that
    /// keeps setting itself cannot starve the page.
    pub fn run_timers(&mut self, now: Instant) {
        let expired = self.web_fonts.expire(now);
        self.fonts_changed(expired);
        loop {
            let Some(script) = &self.script else {
                return;
            };
            let Some((id, nesting)) = script.host.borrow_mut().take_due(now) else {
                return;
            };
            script.host.borrow_mut().running_nesting = nesting;
            self.with_script(|engine| {
                let _ = engine.run_timer(id);
                let _ = engine.run_microtasks();
            });
            if let Some(script) = &self.script {
                script.host.borrow_mut().running_nesting = 0;
            }
            self.end_task();
        }
    }

    /// Whether to send `NeedsFrame` now: true once when the page has work and
    /// has not already asked (AC-12). Only the main frame paints so far, so
    /// only its document counts.
    pub fn take_frame_request(&mut self) -> bool {
        let has_work = self.needs_update
            || self.main_frame.document.needs_frame()
            || self.input.has_pending_move();
        if has_work && !self.frame_requested {
            self.frame_requested = true;
            return true;
        }
        false
    }

    /// Runs the rendering update for a `BeginFrame`. Returns the new list, or
    /// `None` when nothing changed (send `FrameSkipped`). At most one list per call.
    pub fn begin_frame(&mut self, _frame_time: f64) -> Option<DisplayList> {
        self.frame_requested = false;
        if self
            .styles
            .as_ref()
            .is_some_and(|styles| styles.render_blocked(Instant::now()))
        {
            // Hold the first paint for the head's sheets (AC-22). The work
            // stays pending, so the page asks for the next frame again.
            self.needs_update = true;
            return None;
        }
        // One hit test per frame, however many moves came in (spec 0006,
        // Edge cases).
        self.apply_pending_move();
        let frame = &mut self.main_frame;
        let document_changed = frame.document.take_needs_frame();
        if !self.needs_update && !document_changed {
            return None;
        }
        self.needs_update = false;

        // One span per stage, with an event inside giving its counts (spec
        // 0002, Observability). Both compile out of release builds with the
        // rest of the debug level.
        {
            let _span = debug_span!("style").entered();
            let stats = match &self.styles {
                Some(styles) if frame.computes_styles => styles.style_pass(&mut frame.document),
                // Hand built documents keep the styles they were given.
                _ => {
                    frame.document.restyle_dirty(|_, _, _| false);
                    StyleStats::default()
                }
            };
            debug!(
                nodes_styled = stats.styled,
                sharing_hits = stats.shared,
                "styled"
            );
            self.last_style = stats;
        }
        // The faces follow the sheets and the media queries (spec 0006,
        // AC-10).
        if let Some(styles) = &self.styles
            && frame.computes_styles
            && self.web_fonts.set_rules(&styles.stylist().font_faces())
        {
            self.relayout_text = true;
        }
        {
            let _span = debug_span!("boxes").entered();
            let rebuild_roots = frame.document.take_box_rebuild_roots();
            let mut built = 0;
            if self.needs_first_build {
                frame.boxes = BoxTree::build(&mut frame.document);
                self.needs_first_build = false;
                built = frame.boxes.len();
            } else if !rebuild_roots.is_empty() {
                built = frame.boxes.rebuild(&mut frame.document, &rebuild_roots);
            }
            apply_style_damage(frame);
            if std::mem::take(&mut self.relayout_text) {
                frame.boxes.mark_all_needs_layout();
            }
            debug!(boxes_built = built, "boxes built");
        }
        // Backgrounds start loading once a style asks for them.
        if let Some(images) = &mut self.images {
            let urls = frame
                .boxes
                .iter()
                .filter_map(|b| b.style.background.image.as_deref())
                .map(str::to_owned)
                .collect();
            images.set_backgrounds(urls, &mut self.fetch_ids, &mut self.resources);
        }
        let laid_out = {
            let _span = debug_span!("layout").entered();
            let (fragments, stats) = layout::layout(
                &mut frame.boxes,
                frame.viewport,
                &mut self.fonts,
                self.web_fonts.layer(),
            );
            debug!(
                laid_out = stats.laid_out,
                cache_hits = stats.cache_hits,
                "laid out"
            );
            frame.fragments = Some(fragments);
            stats.laid_out > 0
        };
        // Faces the text met while still unloaded start loading now (spec
        // 0006, Font trigger).
        let needed = self.fonts.take_needed_faces();
        self.web_fonts
            .need(&needed, &mut self.fetch_ids, Instant::now());
        for font in self.fonts.take_new_fonts() {
            self.resources
                .add_font(font.key, &font.data, font.index, font.variations);
        }
        self.keep_caret_visible();
        self.apply_pending_scroll();
        let edit = self.edit_paint();
        let frame = &mut self.main_frame;
        self.epoch = self.epoch.next();
        let (list, overlay) = {
            let _span = debug_span!("paint", epoch = self.epoch.0).entered();
            let pictures = self
                .images
                .as_ref()
                .map(Images::pictures)
                .unwrap_or_default();
            let context = paint::PaintContext {
                page: self.id,
                epoch: self.epoch,
                frame: FrameId::MAIN.0,
                scroll_offsets: &self.scroll,
                pictures: &pictures,
                edit,
                overlay: &self.overlay_roots,
            };
            let layers = frame
                .fragments
                .as_ref()
                .map(|fragments| paint::paint_layers(fragments, &frame.boxes, &context));
            let (list, overlay) = match layers {
                Some(layers) => (layers.base, layers.overlay),
                None => {
                    let empty = DisplayList::new(self.id, self.epoch, SizeF::default());
                    (empty.clone(), empty)
                }
            };
            debug!(
                items = list.items.len(),
                overlay = overlay.items.len(),
                "painted"
            );
            (list, overlay)
        };
        // Scroll containers that left the page take their offsets with them.
        let live: Vec<ScrollId> = list.scroll_frames().map(|(_, id)| id).collect();
        self.scroll.retain(|id, _| live.contains(id));
        // Fonts and images in use count across both layers.
        if overlay.items.is_empty() {
            self.resources.list_painted(&list);
        } else {
            let mut both = list.clone();
            both.items.extend(overlay.items.iter().cloned());
            self.resources.list_painted(&both);
        }
        self.overlay = (!self.overlay_roots.is_empty()).then_some(overlay);
        self.settle_pending_scroll(&list);
        self.after_layout(laid_out);
        Some(list)
    }

    /// The `AddFont` and `AddImage` messages to send before the next list,
    /// and the `DeleteFont` and `DeleteImage` ones to send after it.
    pub fn take_resource_messages(&mut self) -> (Vec<ipc::ToBrowser>, Vec<ipc::ToBrowser>) {
        (self.resources.take_adds(), self.resources.take_deletes())
    }

    /// Marks the page as needing a new frame, such as after a failed send.
    pub fn invalidate(&mut self) {
        self.needs_update = true;
    }

    /// Gives a newly connected iframe its own frame and empty document. The
    /// iframe may have left again, or been freed, since it reported in.
    fn attach_frame(&mut self, iframe: NodeRef) {
        let Some(parent) = self.frame(iframe.frame) else {
            return;
        };
        let document = &parent.document;
        let still_connected = document.is_alive(iframe.node) && document.is_connected(iframe.node);
        if !still_connected {
            return;
        }
        let id = FrameId(self.next_frame);
        self.next_frame += 1;
        let attached = self
            .frame_mut(iframe.frame)
            .is_some_and(|parent| parent.document.set_iframe_frame(iframe.node, id).is_ok());
        if !attached {
            return;
        }
        // Loading `src` arrives with navigation; until then every iframe
        // shows the initial about:blank document. Its viewport is sized when
        // iframe layout exists.
        self.frames.insert(
            id,
            Frame {
                document: Document::new("about:blank"),
                boxes: BoxTree::default(),
                parent: Some(iframe),
                viewport: Size::default(),
                fragments: None,
                computes_styles: false,
            },
        );
        debug!(frame = id.0, "iframe frame created");
    }

    /// Drops frame `id` and every frame nested inside it, documents and boxes together.
    fn tear_down_frame(&mut self, id: FrameId) {
        let mut doomed = vec![id];
        while let Some(id) = doomed.pop() {
            if self.frames.remove(&id).is_none() {
                continue;
            }
            debug!(frame = id.0, "iframe frame torn down");
            doomed.extend(
                self.frames
                    .iter()
                    .filter(|(_, frame)| frame.parent.is_some_and(|p| p.frame == id))
                    .map(|(&child, _)| child),
            );
        }
    }
}

/// Hands each restyled element's new style to its box: a repaint only needs
/// the box to paint with it, a relayout also marks the box for layout. An
/// element that left the tree is skipped (spec 0002, Style damage).
///
/// Text takes its style from its parent element when boxes are built, so an
/// inline element (whose inline box only marks edges in its block's
/// content), a replaced element, or a block holding inline content
/// rebuilds the boxes of the block around it instead.
fn apply_style_damage(frame: &mut Frame) {
    let mut rebuild = Vec::new();
    for (node, damage) in frame.document.take_style_damage() {
        if !frame.document.is_alive(node) {
            continue;
        }
        let Some(entry) = frame.document.node(node) else {
            continue;
        };
        let style = entry.as_element().and_then(|e| e.style()).cloned();
        match (entry.layout_box(), style) {
            (Some(box_id), Some(style)) => {
                let holds_text = frame.boxes.get(box_id).is_some_and(|b| {
                    !b.inline.is_empty() || b.kind != layout::BoxKind::BlockContainer
                });
                if holds_text {
                    rebuild.push(node);
                } else {
                    frame.boxes.restyle_box(box_id, style, damage);
                }
            }
            (None, Some(_)) => rebuild.push(node),
            _ => {}
        }
    }
    if !rebuild.is_empty() {
        frame.boxes.rebuild(&mut frame.document, &rebuild);
    }
}

/// A fresh script engine and its host for a committed document. A page
/// whose engine cannot start still loads, without scripts.
fn new_script() -> Option<Script> {
    let host = Rc::new(RefCell::new(PageHost::new()));
    let shared: Rc<RefCell<dyn ScriptHost>> = host.clone();
    match script_quickjs::QuickJsEngine::new(shared) {
        Ok(engine) => Some(Script {
            engine: Box::new(engine),
            host,
        }),
        Err(err) => {
            warn!(%err, "the script engine did not start; scripts are off");
            None
        }
    }
}

/// The box of the text input `id` and its text runs, each with its offset
/// from the box's border box, before the box scrolls.
fn field_texts(
    tree: &layout::FragmentTree,
    id: NodeId,
) -> Option<(&layout::BoxFragment, Vec<(Point, &layout::TextFragment)>)> {
    let (_, root) = tree.root.as_ref()?;
    let mut stack: Vec<&layout::Fragment> = vec![root];
    let field = loop {
        let fragment = stack.pop()?;
        if let layout::Fragment::Box(b) = fragment
            && b.node == Some(id)
        {
            break b;
        }
        stack.extend(fragment.children().iter().map(|(_, child)| &**child));
    };
    let mut texts = Vec::new();
    let mut stack: Vec<(Point, &layout::Fragment)> = field
        .children
        .iter()
        .map(|(at, child)| (*at, &**child))
        .collect();
    while let Some((at, fragment)) = stack.pop() {
        match fragment {
            layout::Fragment::Text(text) if text.node == id => texts.push((at, text)),
            _ => {
                stack.extend(fragment.children().iter().map(|(offset, child)| {
                    (Point::new(at.x + offset.x, at.y + offset.y), &**child)
                }))
            }
        }
    }
    texts.sort_by_key(|(_, text)| text.text_start);
    Some((field, texts))
}

/// Encodes `list` with `postcard` into shared memory (spec 0002, Transport).
pub fn encode(list: &DisplayList) -> Result<SharedBytes, PageError> {
    let _span = debug_span!("encode").entered();
    let bytes = postcard::to_allocvec(list)?;
    debug!(bytes = bytes.len(), "encoded");
    if bytes.len() > MAX_ENCODED_LEN {
        return Err(PageError::TooLarge(bytes.len()));
    }
    Ok(SharedBytes::from_bytes(&bytes))
}

fn to_au(size: SizeF) -> Size {
    Size::new(
        Au::from_px(size.width).max(Au::ZERO),
        Au::from_px(size.height).max(Au::ZERO),
    )
}

#[cfg(test)]
mod tests {
    use base::atoms::QualName;
    use base::local_name;
    use dom::ElementKind;

    use super::*;

    const VIEWPORT: SizeF = SizeF {
        width: 800.0,
        height: 600.0,
    };

    fn page() -> Page {
        Page::new(PageId(7), sample::document(), VIEWPORT, 2.0)
    }

    #[test]
    fn a_new_page_asks_for_one_frame_then_goes_quiet() {
        let mut page = page();
        assert!(page.take_frame_request());
        assert!(!page.take_frame_request(), "asks only once");

        let list = page.begin_frame(16.0).expect("first frame has a list");
        assert_eq!((list.page, list.epoch), (PageId(7), Epoch(1)));
        assert!(!list.items.is_empty());

        assert!(page.begin_frame(32.0).is_none(), "nothing dirty, no list");
        assert!(!page.take_frame_request(), "no work, no NeedsFrame");
    }

    #[test]
    fn only_a_real_resize_needs_a_frame_and_epochs_increase() {
        let mut page = page();
        let first = page.begin_frame(0.0).unwrap();

        page.resize(VIEWPORT, 2.0);
        assert!(!page.take_frame_request());

        page.resize(
            SizeF {
                width: 400.0,
                height: 600.0,
            },
            2.0,
        );
        assert!(page.take_frame_request());
        let second = page.begin_frame(16.0).unwrap();
        assert!(second.epoch > first.epoch);
        assert_ne!(
            first.items, second.items,
            "the boxes relaid out to the new width"
        );
        assert_eq!(
            second.viewport,
            SizeF {
                width: 400.0,
                height: 600.0
            }
        );
    }

    /// A page on a bare `<html><body>` document, already past its first frame.
    fn bare_page() -> (Page, dom::NodeId) {
        let mut doc = Document::new("about:blank");
        let html = doc.create_element(QualName::html(local_name!("html")));
        let body = doc.create_element(QualName::html(local_name!("body")));
        doc.append_child(doc.root(), html).unwrap();
        doc.append_child(html, body).unwrap();
        let mut page = Page::new(PageId(1), doc, VIEWPORT, 1.0);
        page.end_task();
        page.begin_frame(0.0);
        (page, body)
    }

    fn iframe_in(page: &mut Page, frame: FrameId, parent: dom::NodeId) -> dom::NodeId {
        let doc = &mut page.frame_mut(frame).unwrap().document;
        let iframe = doc.create_element(QualName::html(local_name!("iframe")));
        doc.append_child(parent, iframe).unwrap();
        iframe
    }

    fn child_frame(page: &Page, frame: FrameId, iframe: dom::NodeId) -> Option<FrameId> {
        let node = page.frame(frame)?.document.node(iframe)?;
        match node.as_element()?.kind() {
            ElementKind::IFrame { frame } => frame,
            _ => None,
        }
    }

    #[test]
    fn a_dom_change_asks_for_a_frame_and_rebuilds_the_boxes() {
        let (mut page, body) = bare_page();
        assert!(page.begin_frame(16.0).is_none());

        let doc = &mut page.main_frame_mut().document;
        let div = doc.create_element(QualName::html(local_name!("div")));
        doc.append_child(body, div).unwrap();
        page.end_task();
        assert!(page.take_frame_request());
        assert!(page.begin_frame(32.0).is_some());
        assert!(page.begin_frame(48.0).is_none(), "clean again");
    }

    #[test]
    fn each_iframe_gets_its_own_document_until_it_is_removed() {
        // AC-16
        let (mut page, body) = bare_page();
        let iframe = iframe_in(&mut page, FrameId::MAIN, body);
        page.end_task();

        let child = child_frame(&page, FrameId::MAIN, iframe).expect("a child frame");
        let frame = page.frame(child).unwrap();
        assert_eq!(frame.document.url(), "about:blank");
        assert_eq!(
            frame.parent,
            Some(NodeRef {
                frame: FrameId::MAIN,
                node: iframe
            })
        );
        assert_eq!(page.frame_ids().collect::<Vec<_>>(), [FrameId::MAIN, child]);

        // A nested iframe inside the child frame's document.
        let child_doc = &mut page.frame_mut(child).unwrap().document;
        let child_html = child_doc.create_element(QualName::html(local_name!("html")));
        child_doc
            .append_child(child_doc.root(), child_html)
            .unwrap();
        let nested = iframe_in(&mut page, child, child_html);
        page.end_task();
        let grandchild = child_frame(&page, child, nested).expect("a nested frame");

        page.main_frame_mut()
            .document
            .remove_child(body, iframe)
            .unwrap();
        page.end_task();
        assert!(
            page.frame(child).is_none(),
            "removing the iframe tears down its frame"
        );
        assert!(
            page.frame(grandchild).is_none(),
            "and every frame inside it"
        );
        assert_eq!(page.frame_ids().collect::<Vec<_>>(), [FrameId::MAIN]);
        assert!(
            !page.main_frame().document.is_alive(iframe),
            "the unpinned iframe element was swept"
        );
    }

    #[test]
    fn an_iframe_that_leaves_in_the_same_task_gets_no_frame() {
        let (mut page, body) = bare_page();
        let iframe = iframe_in(&mut page, FrameId::MAIN, body);
        let doc = &mut page.main_frame_mut().document;
        doc.pin(iframe).unwrap();
        doc.remove_child(body, iframe).unwrap();
        page.end_task();
        assert_eq!(page.frame_ids().count(), 1);

        // Moving an iframe reloads it: a fresh frame, never the old id.
        let doc = &mut page.main_frame_mut().document;
        doc.append_child(body, iframe).unwrap();
        page.end_task();
        let first = child_frame(&page, FrameId::MAIN, iframe).unwrap();
        let doc = &mut page.main_frame_mut().document;
        doc.remove_child(body, iframe).unwrap();
        doc.append_child(body, iframe).unwrap();
        page.end_task();
        let second = child_frame(&page, FrameId::MAIN, iframe).unwrap();
        assert!(second > first);
        assert_eq!(
            page.frame_ids().collect::<Vec<_>>(),
            [FrameId::MAIN, second]
        );
    }

    /// The sample page's first card (`html > body > main > section`).
    fn first_card(page: &Page) -> dom::NodeId {
        let doc = &page.main_frame().document;
        let html = doc.document_element().unwrap();
        let body = doc.children(html).next().unwrap();
        let main = doc.children(body).nth(1).unwrap();
        doc.children(main).next().unwrap()
    }

    fn colors(list: &DisplayList) -> Vec<display_list::ColorF> {
        list.items
            .iter()
            .filter_map(|item| match item.kind {
                display_list::ItemKind::Rect { color, .. } => Some(color),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn style_changes_reach_the_next_frame() {
        let mut page = page();
        let first = page.begin_frame(0.0).unwrap();
        let card = first_card(&page);

        // A color change repaints with the new color.
        let doc = &mut page.main_frame_mut().document;
        let mut style = (**doc
            .node(card)
            .unwrap()
            .as_element()
            .unwrap()
            .style()
            .unwrap())
        .clone();
        std::rc::Rc::make_mut(&mut style.background).color = css::Color::rgb8(0, 0, 0);
        doc.set_style(card, Some(std::rc::Rc::new(style.clone())))
            .unwrap();
        assert!(page.take_frame_request());
        let repainted = page.begin_frame(16.0).unwrap();
        let black = display_list::ColorF {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        };
        assert!(colors(&repainted).contains(&black));
        assert!(!colors(&first).contains(&black));

        // `display: none` drops the card's rect.
        std::rc::Rc::make_mut(&mut style.display_box).display = css::Display::None;
        let doc = &mut page.main_frame_mut().document;
        doc.set_style(card, Some(std::rc::Rc::new(style))).unwrap();
        let hidden = page.begin_frame(32.0).unwrap();
        assert_eq!(hidden.items.len(), first.items.len() - 1);
        assert!(page.begin_frame(48.0).is_none(), "clean again");
    }

    /// The sample page's scroll box (`main > aside`) and its scroll id.
    fn scroller(page: &Page) -> (dom::NodeId, ScrollId) {
        let doc = &page.main_frame().document;
        let html = doc.document_element().unwrap();
        let body = doc.children(html).next().unwrap();
        let main = doc.children(body).nth(1).unwrap();
        let aside = doc.children(main).last().unwrap();
        (aside, paint::scroll_id(FrameId::MAIN.0, aside))
    }

    fn painted_offset(list: &DisplayList, id: ScrollId) -> Option<ScrollOffset> {
        list.spatial.iter().find_map(|node| match node.kind {
            display_list::SpatialKind::ScrollFrame {
                scroll_id, offset, ..
            } if scroll_id == id => Some(offset),
            _ => None,
        })
    }

    #[test]
    fn script_scrolls_bump_the_generation_and_user_scrolls_do_not() {
        // AC-13
        let mut page = page();
        let first = page.begin_frame(0.0).unwrap();
        let (aside, id) = scroller(&page);
        assert_eq!(painted_offset(&first, id), Some(ScrollOffset::default()));

        page.scroll_to(
            NodeRef {
                frame: FrameId::MAIN,
                node: aside,
            },
            0.0,
            50.0,
        );
        assert!(page.take_frame_request(), "a script scroll repaints");
        let scripted = page.begin_frame(16.0).unwrap();
        let offset = painted_offset(&scripted, id).unwrap();
        assert_eq!((offset.y, offset.generation), (50.0, 1));

        // The browser reports a user scroll: same generation, new offset,
        // and no repaint needed.
        page.scroll_changed(id, 0.0, 90.0);
        assert!(!page.take_frame_request());
        assert_eq!(
            page.scroll_offset(id),
            ScrollOffset {
                x: 0.0,
                y: 90.0,
                generation: 1
            }
        );
    }

    #[test]
    fn hit_testing_follows_the_scroll_table() {
        // AC-14
        let mut page = page();
        assert_eq!(
            page.hit_test(PointF { x: 1.0, y: 1.0 }),
            None,
            "no layout yet"
        );
        page.begin_frame(0.0).unwrap();
        let (aside, id) = scroller(&page);
        let doc = &page.main_frame().document;
        let rows: Vec<_> = doc.children(aside).collect();

        // Find a point inside the scroll box's first row.
        let fragments = page.main_frame().fragments.as_ref().unwrap();
        let dump = fragments.dump(doc);
        assert!(dump.contains("scroll-container"), "{dump}");
        let first_row_at = (1..2000)
            .map(|y| PointF {
                x: 300.0,
                y: y as f32,
            })
            .find(|&p| page.hit_test(p).is_some_and(|(hit, _)| hit.node == rows[0]))
            .expect("row 0 is visible");

        // Rows are 40px with 4px margins that collapse between them, so
        // they start 44px apart. Scrolled 42px, the point falls in the gap
        // and hits the scroll box; scrolled 44px, the next row.
        page.scroll_changed(id, 0.0, 42.0);
        assert_eq!(
            page.hit_test(first_row_at).map(|(hit, _)| hit.node),
            Some(aside)
        );
        page.scroll_changed(id, 0.0, 44.0);
        let (hit, local) = page.hit_test(first_row_at).unwrap();
        assert_eq!(hit.node, rows[1]);
        assert_eq!(local.y, 0.0, "at the row's top edge");
        assert_eq!(
            page.hit_test(PointF { x: -5.0, y: 10.0 }),
            None,
            "outside the page"
        );
    }

    const NAV: NavigationId = NavigationId(1);

    /// A blank page on navigation `NAV`, with a UTF-8 document of `kind`
    /// committed and fed in `chunks`, ended when `end` is set.
    fn committed(kind: DocumentKind, chunks: &[&str], end: bool) -> Page {
        let mut page = Page::blank(PageId(1), VIEWPORT, 1.0, ua_stylesheet());
        page.navigate(NAV);
        // A declared charset, so the parser starts at once instead of
        // holding bytes for the `<meta>` prescan (AC-6).
        assert!(page.commit_navigation(NAV, "http://test/", Some("utf-8".into()), &kind));
        for chunk in chunks {
            page.fetch_chunk(NAV, FetchId::DOCUMENT, chunk.as_bytes());
        }
        if end {
            page.fetch_done(NAV, FetchId::DOCUMENT);
        }
        page.end_task();
        page
    }

    /// [`committed`] for an HTML document.
    fn loaded(chunks: &[&str], end: bool) -> Page {
        committed(DocumentKind::Document, chunks, end)
    }

    fn texts(list: &DisplayList) -> usize {
        list.items
            .iter()
            .filter(|item| matches!(item.kind, display_list::ItemKind::Text { .. }))
            .count()
    }

    #[test]
    fn a_committed_document_parses_styles_and_paints_its_text() {
        let mut page = loaded(
            &["<!DOCTYPE html><title>T</title><h1>Head</h1><p>One paragraph</p>"],
            true,
        );
        assert_eq!(page.main_frame().document.url(), "http://test/");
        assert!(page.take_frame_request());
        let list = page.begin_frame(0.0).unwrap();
        let frame = page.main_frame();
        let doc = &frame.document;
        let styles = doc.dump_styles();
        assert!(styles.contains("html>head: display: none"), "{styles}");
        assert!(styles.contains("html>body>p: display: block"), "{styles}");
        let boxes = frame.boxes.dump(doc);
        assert!(
            boxes.contains("text html>body>h1>#text \"Head\""),
            "{boxes}"
        );
        assert!(!boxes.contains("\"T\""), "the title is in the hidden head");

        let (adds, _) = page.take_resource_messages();
        if adds.is_empty() {
            // No system font on this machine.
            assert_eq!(texts(&list), 0);
            return;
        }
        // The paragraph's face, and the heading's bold one.
        assert!(
            adds.iter()
                .all(|add| matches!(add, ipc::ToBrowser::AddFont { .. }))
        );
        assert!(!adds.is_empty() && adds.len() <= 2, "{} fonts", adds.len());
        assert_eq!(texts(&list), 2, "one run per paragraph");
    }

    #[test]
    fn the_page_shows_the_first_half_before_the_rest_arrives() {
        // AC-5
        let mut page = loaded(&["<p>first</p><p>sec"], false);
        let doc = &page.main_frame().document;
        assert!(doc.dump().contains("\"first\""), "{}", doc.dump());
        assert!(page.take_frame_request());
        assert!(page.begin_frame(0.0).is_some());

        page.fetch_chunk(NAV, FetchId::DOCUMENT, b"ond</p>");
        page.fetch_done(NAV, FetchId::DOCUMENT);
        page.end_task();
        assert!(page.take_frame_request(), "the rest asks for another frame");
        page.begin_frame(16.0).unwrap();
        let boxes = page.main_frame().boxes.dump(&page.main_frame().document);
        assert!(boxes.contains("\"second\""), "{boxes}");
    }

    #[test]
    fn plain_text_shows_as_preformatted_text() {
        // AC-7
        let page = committed(DocumentKind::PlainText, &["<b>not bold</b>\n  kept"], true);
        let doc = &page.main_frame().document;
        let dump = doc.dump();
        assert!(dump.contains(r#""<b>not bold</b>\n  kept""#), "{dump}");
    }

    #[test]
    fn messages_for_another_navigation_are_dropped() {
        // AC-11, content side.
        let mut page = Page::blank(PageId(1), VIEWPORT, 1.0, ua_stylesheet());
        page.navigate(NavigationId(2));
        assert!(!page.commit_navigation(NAV, "http://old/", None, &DocumentKind::Document));
        assert_eq!(page.main_frame().document.url(), "about:blank");
        assert!(page.commit_navigation(
            NavigationId(2),
            "http://new/",
            Some("utf-8".into()),
            &DocumentKind::Document
        ));
        page.fetch_chunk(NAV, FetchId::DOCUMENT, b"<p>old</p>");
        page.fetch_chunk(NavigationId(2), FetchId(4), b"<p>not the document</p>");
        page.fetch_chunk(NavigationId(2), FetchId::DOCUMENT, b"<p>new</p>");
        let dump = page.main_frame().document.dump();
        assert!(dump.contains("\"new\"") && !dump.contains("old") && !dump.contains("not"));
    }

    #[test]
    fn lists_name_the_committed_navigation_not_the_started_one() {
        // AC-11, AC-12: the old document's frames keep its navigation until
        // the new one commits, so the browser keeps holding paint.
        let mut page = Page::blank(PageId(1), VIEWPORT, 1.0, ua_stylesheet());
        assert_eq!(page.navigation(), NavigationId(0));
        page.navigate(NAV);
        assert!(page.commit_navigation(NAV, "http://a/", None, &DocumentKind::Document));
        page.navigate(NavigationId(NAV.0 + 1));
        assert_eq!(page.navigation(), NAV, "still the old document");
        assert!(page.commit_navigation(
            NavigationId(NAV.0 + 1),
            "http://b/",
            None,
            &DocumentKind::Document
        ));
        assert_eq!(page.navigation(), NavigationId(NAV.0 + 1));
    }

    #[test]
    fn encoding_round_trips() {
        let list = page().begin_frame(0.0).unwrap();
        let bytes = encode(&list).unwrap();
        let decoded: DisplayList = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, list);
    }
}
