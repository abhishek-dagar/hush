//! The chrome document (spec 0003, milestone 3): `frame.html` (or, in dev
//! builds, the gallery) run by the engine as a page with scripts off, from
//! the embedded assets. It is pure: events and settings go in, display
//! lists, commands and the frame's regions come out. `embed` feeds it the
//! window's input and draws what it returns.
//!
//! Rust drives it only through the DOM: the root attributes that pick
//! tokens, ARIA state, `tabindex` for roving focus, text on elements found
//! by id, and the Linux app menu's rows built from the menu model. No
//! markup string is ever parsed into it after the load.

use base::atoms::{Atom, LocalName, QualName};
use display_list::{ColorF, DisplayList, PageId, SpatialKind};
use dom::{Document, FrameId, NodeId, NodeRef};
use ipc::{
    CursorKind, DocumentKind, FetchError, FetchId, FocusDirection, InputEvent, Key, NavigationId,
    PointerButton, ToBrowser,
};
use navigation::{Intent, Security, display_host};
use page::{FontContext, Page};
use url::Url;

use crate::address::{AddressBar, AddressStyle, MAX_ADDRESS_BYTES, SearchEngine};
use crate::appearance::{ButtonLayout, CaptionButton, Os, Side};
use crate::assets::{ORIGIN, asset};
use crate::command::{ChromeCommand, WindowAction, ZoomStep, parse_command};
use crate::frame::{FrameLayout, FrameMetrics, PointF, RectF, SizeF};
use crate::keyboard::{self, Composite, Item, Move, NavKey};
use crate::menu::{Menu, MenuItem};
use crate::settings::{AccentSetting, Settings};
use crate::theme::{Accent, ChromeTheme};
use crate::tokens::Tokens;
use crate::{ChromeError, TOKENS_CSS};

/// The page id chrome documents draw under. Content pages count from 1,
/// so the renderer keeps the chrome's fonts apart from theirs.
pub const CHROME_PAGE: PageId = PageId(0);

/// The one navigation a chrome document ever makes.
const NAVIGATION: NavigationId = NavigationId(1);
/// Rounds of subresource fetches answered at once: the frame needs two
/// (its sheets, then the icon font its first layout asks for).
const MAX_FETCH_ROUNDS: usize = 8;
/// The check glyph in the icon font (`icons.toml`).
const CHECK_GLYPH: &str = "\u{E00F}";
/// The address's security icons (`icons.toml`; spec 0008, AC-3).
const LOCK_GLYPH: &str = "\u{E007}";
const WARNING_GLYPH: &str = "\u{E009}";
const GLOBE_GLYPH: &str = "\u{E017}";
/// The intent row's search icon, and reload's and stop's (AC-7, AC-25).
const SEARCH_GLYPH: &str = "\u{E006}";
const RELOAD_GLYPH: &str = "\u{E002}";
const STOP_GLYPH: &str = "\u{E003}";
/// The palette's width, centered in the window (spec 0008, AC-6, as
/// the engineer asked: in the middle of the window, both ways).
const PALETTE_MAX_WIDTH: f32 = 600.0;
const PALETTE_MARGIN: f32 = 32.0;
/// How far below the card's top the loading track sits, in chrome px.
const LOADING_TOP: f32 = 12.0;
/// One sweep of the loading shimmer across its track, in ms.
const SHIMMER_PERIOD_MS: f64 = 1200.0;
/// The shimmer's share of the track's width (`frame.css`).
const SHIMMER_SHARE: f32 = 0.4;
/// The overlay ids whose boxes take the pointer before the page (spec
/// 0008, Interface surface, Paint and render).
const POINTER_OVERLAY: [&str; 2] = ["palette", "palette-scrim"];
/// The address fields: the sidebar's (inline style) and the palette's.
const FIELDS: [&str; 2] = ["address-field", "palette-field"];
/// The intent rows under them (AC-7).
const INTENT_ROWS: [&str; 2] = ["address-intent", "palette-intent"];
/// Segoe's maximize and restore glyphs (spec 0003, Value sourcing).
const MAXIMIZE_GLYPH: &str = "\u{E922}";
const RESTORE_GLYPH: &str = "\u{E923}";

/// Which chrome document to load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromeSurface {
    /// The browser window's frame, `frame.html`.
    Frame,
    /// Every control in every state, `gallery.html` (dev builds only).
    Gallery,
    /// The settings window, `settings.html` (AC-12).
    Settings,
}

impl ChromeSurface {
    fn url(self) -> String {
        let file = match self {
            Self::Frame => "frame.html",
            Self::Gallery => "gallery.html",
            Self::Settings => "settings.html",
        };
        format!("{ORIGIN}{file}")
    }
}

/// One rendering of the chrome document.
#[derive(Debug)]
pub struct ChromeFrame {
    /// What to draw, in chrome px, under [`CHROME_PAGE`].
    pub list: DisplayList,
    /// `AddFont` and `AddImage` messages, for the renderer before the list.
    pub adds: Vec<ToBrowser>,
    /// `DeleteFont` and `DeleteImage` messages, for after it.
    pub deletes: Vec<ToBrowser>,
    /// The overlay layer, drawn after the page (spec 0008, Decision);
    /// `None` for a document without one.
    pub overlay: Option<DisplayList>,
}

/// What one input event led to.
#[derive(Debug, Default)]
pub struct ChromeOutcome {
    /// A `data-command` the event activated, or why it could not be read
    /// (an unknown name or value: log it and ignore it, AC-13).
    pub command: Option<Result<ChromeCommand, ChromeError>>,
    /// `Tab` went past this end of the chrome: focus goes to the page.
    pub focus_left: Option<FocusDirection>,
    /// The pointer shape, when it changed.
    pub cursor: Option<CursorKind>,
    /// Text a copy or cut in a chrome field took, for the OS clipboard
    /// (spec 0008, AC-15). The chrome is trusted, so no request id.
    pub copied: Option<String>,
}

/// What the navigation controls show (spec 0008, Interface surface).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NavView {
    /// There is an entry to go back to.
    pub can_back: bool,
    /// There is an entry to go forward to.
    pub can_forward: bool,
    /// The committed URL; `None` before the first commit.
    pub url: Option<Url>,
    /// The URL the user asked for, while it loads: the address shows it
    /// instead (AC-11). Never set for a load a page started.
    pub pending: Option<Url>,
    /// A load runs, from its start until the new document's `load`
    /// event: reload shows as stop and the loading bar shows (AC-25).
    pub loading: bool,
}

/// The app menu while it is open (Linux, AC-9).
#[derive(Debug, Clone, Copy)]
struct OpenMenu {
    /// Where focus goes back to on `Escape`.
    opener: Option<NodeId>,
}

/// The chrome document of one window.
pub struct ChromeDocument {
    page: Page,
    surface: ChromeSurface,
    os: Os,
    metrics: FrameMetrics,
    /// The window in chrome px.
    viewport: SizeF,
    /// Regions from the last layout.
    layout: FrameLayout,
    menu: Option<OpenMenu>,
    /// The root's scroll offset and the document's size, for the gallery.
    scroll: PointF,
    content: SizeF,
    /// A rendering made early (to lay out new rows before focusing one),
    /// not handed out yet.
    pending: Option<ChromeFrame>,
    /// The `≡` button's box from the last layout.
    menu_button: Option<RectF>,
    /// The tokens, for the values the chrome sets from Rust (the field
    /// selection color).
    tokens: Tokens,
    /// The address's state (spec 0008).
    address: AddressBar,
    /// While the palette is open: what had focus before it, to give it
    /// back (AC-6). `Some(None)` means the page had it.
    palette: Option<Option<NodeId>>,
    /// A primary press began on an intent row; its release there does
    /// what Enter does (AC-7).
    intent_pressed: bool,
    /// `--loading-width`: the loading track's width.
    loading_width: f32,
    /// The OS asks for reduced motion: the shimmer holds still.
    reduce_motion: bool,
}

impl ChromeDocument {
    /// Loads `surface` for `os` in a `viewport` of chrome px drawn at
    /// `scale` device pixels per chrome px. `fonts` replaces the system
    /// fonts (tests use one fixed font). Every subresource is answered from
    /// the embedded assets before this returns, so the first frame is
    /// complete.
    pub fn load(
        surface: ChromeSurface,
        os: Os,
        viewport: SizeF,
        scale: f32,
        fonts: Option<FontContext>,
    ) -> Result<Self, ChromeError> {
        let tokens = Tokens::parse(TOKENS_CSS)?;
        let metrics = FrameMetrics::from_tokens(&tokens.resolve(&[]))?;
        let url = surface.url();
        let html = asset(&url).ok_or_else(|| ChromeError::MissingAsset(url.clone()))?;

        let mut page = Page::blank(
            CHROME_PAGE,
            page_size(viewport),
            scale,
            page::ua_stylesheet(),
        );
        page.set_scripting(false);
        if let Some(fonts) = fonts {
            page.use_fonts(fonts);
        }
        page.navigate(NAVIGATION);
        page.commit_navigation(
            NAVIGATION,
            &url,
            Some("utf-8".to_owned()),
            &DocumentKind::Document,
        );
        page.end_task();
        page.fetch_chunk(NAVIGATION, FetchId::DOCUMENT, html.bytes);
        page.end_task();
        page.fetch_done(NAVIGATION, FetchId::DOCUMENT);
        page.end_task();

        let mut document = Self {
            page,
            surface,
            os,
            metrics,
            viewport,
            layout: FrameLayout::default(),
            menu: None,
            scroll: PointF::default(),
            content: viewport,
            pending: None,
            menu_button: None,
            tokens,
            address: AddressBar::new(SearchEngine::default()),
            palette: None,
            intent_pressed: false,
            loading_width: 0.0,
            reduce_motion: false,
        };
        document.loading_width = document
            .tokens
            .resolve(&[])
            .px("--loading-width")
            .unwrap_or_default();
        document.serve();
        // Every chrome text input is capped like the address (AC-10).
        document.page.set_text_cap(Some(MAX_ADDRESS_BYTES));
        let overlay: Vec<NodeId> = {
            let doc = document.doc();
            doc.node_ids()
                .filter(|&id| attribute(doc, id, "data-layer") == Some("overlay"))
                .collect()
        };
        if !overlay.is_empty() {
            document.page.set_overlay_roots(&overlay);
        }
        document.show_address();
        document.init_roving();
        Ok(document)
    }

    /// Answers the document's fetches from the embedded assets until it
    /// asks for nothing more. Anything else fails as not found.
    fn serve(&mut self) -> bool {
        let mut served = false;
        for _ in 0..MAX_FETCH_ROUNDS {
            let messages = self.page.take_fetch_messages();
            if messages.is_empty() {
                return served;
            }
            served = true;
            for message in messages {
                let ToBrowser::Fetch {
                    navigation,
                    fetch,
                    url,
                    ..
                } = message
                else {
                    continue;
                };
                match asset(&url) {
                    Some(found) => {
                        let headers = [("content-type".to_owned(), found.mime.to_owned())];
                        self.page
                            .fetch_response(navigation, fetch, 200, &headers, &url);
                        self.page.fetch_chunk(navigation, fetch, found.bytes);
                        self.page.fetch_done(navigation, fetch);
                    }
                    None => self
                        .page
                        .fetch_failed(navigation, fetch, &FetchError::FileNotFound),
                }
                self.page.end_task();
            }
        }
        served
    }

    fn doc(&self) -> &Document {
        &self.page.main_frame().document
    }

    fn doc_mut(&mut self) -> &mut Document {
        &mut self.page.main_frame_mut().document
    }

    fn by_id(&self, id: &str) -> Option<NodeId> {
        self.doc().element_by_id(&Atom::from(id))
    }

    /// Writes the theme onto the root: the attributes that pick tokens and
    /// the root font size (spec 0003, Data model, root attributes).
    pub fn set_theme(&mut self, theme: &ChromeTheme) {
        let Some(root) = self.doc().document_element() else {
            return;
        };
        for (name, value) in theme.attributes() {
            set_attr(self.doc_mut(), root, name, value);
        }
        let style = format!("font-size: {}px", theme.root_font_px);
        set_attr(self.doc_mut(), root, "style", &style);
        self.reduce_motion = theme.motion == crate::theme::Motion::Reduce;
        // Selected field text sits on `--selection` (spec 0008, AC-14).
        if let Ok(color) = self
            .tokens
            .resolve(&theme.attributes())
            .color("--selection")
        {
            self.page.set_selection_color(ColorF {
                r: color.r,
                g: color.g,
                b: color.b,
                a: color.a,
            });
        }
    }

    /// Puts the window buttons on the side and in the order `buttons`
    /// gives; buttons it leaves out are hidden. Only Linux reads a layout;
    /// Windows always shows all three on the right (AC-8).
    pub fn set_caption(&mut self, buttons: &ButtonLayout) {
        let (Some(frame), Some(row)) = (self.by_id("frame"), self.by_id("caption-buttons")) else {
            return;
        };
        let (side, order) = match self.os {
            Os::Linux => (buttons.side, buttons.order.clone()),
            Os::Windows | Os::Mac => (
                Side::Right,
                vec![
                    CaptionButton::Minimize,
                    CaptionButton::Maximize,
                    CaptionButton::Close,
                ],
            ),
        };
        let side = match side {
            Side::Left => "left",
            Side::Right => "right",
        };
        set_attr(self.doc_mut(), frame, "data-side", side);
        let all = [
            CaptionButton::Minimize,
            CaptionButton::Maximize,
            CaptionButton::Close,
        ];
        for button in all {
            let Some(node) = self.by_id(caption_id(button)) else {
                continue;
            };
            if order.contains(&button) {
                remove_attr(self.doc_mut(), node, "hidden");
            } else {
                set_attr(self.doc_mut(), node, "hidden", "");
            }
        }
        // Moving each to the end in turn leaves them in `order`.
        let current: Vec<NodeId> = self.doc().children(row).collect();
        let wanted: Vec<NodeId> = order
            .iter()
            .filter_map(|&button| self.by_id(caption_id(button)))
            .collect();
        let in_order = current
            .iter()
            .copied()
            .filter(|node| wanted.contains(node))
            .eq(wanted.iter().copied());
        if !in_order {
            let doc = self.doc_mut();
            for node in wanted {
                if doc.remove_child(row, node).is_ok() {
                    let _ = doc.append_child(row, node);
                }
            }
        }
        self.page.end_task();
    }

    /// Shows the maximize button as restore while the window is maximized.
    pub fn set_maximized(&mut self, maximized: bool) {
        let (glyph, label) = if maximized {
            (RESTORE_GLYPH, "Restore")
        } else {
            (MAXIMIZE_GLYPH, "Maximize")
        };
        if let Some(button) = self.by_id("caption-maximize") {
            set_attr(self.doc_mut(), button, "aria-label", label);
        }
        if let Some(text) = self
            .by_id("caption-maximize-glyph")
            .and_then(|node| self.doc().node(node)?.first_child())
        {
            let _ = self.doc_mut().set_text(text, glyph);
        }
    }

    /// Shows the navigation state (spec 0008, AC-1, AC-3, AC-11, AC-25):
    /// back and forward `aria-disabled` when there is no entry that way,
    /// reload as stop and the loading bar while a load runs, and the
    /// address showing the committed URL, or the user's own pending one.
    /// While the user edits, the address text stays as typed (AC-12).
    pub fn set_navigation(&mut self, view: &NavView) {
        for (id, enabled) in [
            ("nav-back", view.can_back),
            ("nav-forward", view.can_forward),
        ] {
            let Some(button) = self.by_id(id) else {
                continue;
            };
            if enabled {
                remove_attr(self.doc_mut(), button, "aria-disabled");
            } else {
                set_attr(self.doc_mut(), button, "aria-disabled", "true");
            }
        }
        let (label, command, glyph) = if view.loading {
            ("Stop", "nav.stop", STOP_GLYPH)
        } else {
            ("Reload", "nav.reload", RELOAD_GLYPH)
        };
        if let Some(reload) = self.by_id("nav-reload") {
            set_attr(self.doc_mut(), reload, "aria-label", label);
            set_attr(self.doc_mut(), reload, "data-command", command);
        }
        self.set_text("nav-reload-icon", glyph);
        self.set_hidden("loading-bar", !view.loading);
        match &view.pending {
            Some(url) => self.address.show_pending(Some(url.clone())),
            None => self.address.show(view.url.clone()),
        }
        self.show_address();
        self.page.end_task();
    }

    /// Shows the link under the pointer or with keyboard focus in the
    /// status bubble, or hides it (spec 0008, AC-19): its display host
    /// (AC-4) and the rest of the URL. The URL comes from the page, so it
    /// is only shown, never loaded.
    pub fn set_link_hover(&mut self, url: Option<&str>) {
        let shown = url.map(|url| match Url::parse(url) {
            Ok(url) => {
                let display = display_host(&url);
                format!("{}{}", display.host, display.rest)
            }
            Err(_) => url.to_owned(),
        });
        match shown {
            Some(text) => {
                self.set_text("status-text", &text);
                self.set_hidden("status-bubble", false);
            }
            None => self.set_hidden("status-bubble", true),
        }
        self.page.end_task();
    }

    /// Picks the address style (spec 0008, AC-2); a change shows on the
    /// next frame. Leaving palette style closes an open palette.
    pub fn set_address_style(&mut self, style: AddressStyle) {
        if style == self.address.style() {
            return;
        }
        if self.palette.is_some() {
            self.close_palette(true, true);
        }
        self.address.set_style(style);
        self.show_address();
        self.page.end_task();
    }

    /// Uses `engine` for searches from now on (AC-9).
    pub fn set_search_engine(&mut self, engine: SearchEngine) {
        if *self.address.engine() != engine {
            self.address.set_engine(engine);
            self.show_address();
            self.page.end_task();
        }
    }

    /// Installs the check AC-8 needs for typed absolute paths; `embed`
    /// passes one that asks the disk, so this crate never does.
    pub fn set_file_check(&mut self, file_exists: Box<dyn Fn(&std::path::Path) -> bool>) {
        self.address.set_file_check(file_exists);
    }

    /// The address's state, for the host and tests.
    pub fn address(&self) -> &AddressBar {
        &self.address
    }

    /// Whether the palette is open (AC-6).
    pub fn palette_open(&self) -> bool {
        self.palette.is_some()
    }

    /// A press anywhere in the window outside the open palette, by any
    /// button (spec 0008, AC-6, widened by the engineer to the whole
    /// window): it closes, the shown URL comes back, and focus returns.
    /// Returns whether it was open and `at` (chrome px) is outside it, so
    /// the press is used up.
    pub fn dismiss_palette(&mut self, at: PointF) -> bool {
        if self.palette.is_none()
            || self
                .element_box("palette")
                .is_some_and(|palette| palette.contains(at))
        {
            return false;
        }
        self.close_palette(true, true);
        true
    }

    /// Whether the user is typing in the address, in either style: then
    /// Escape belongs to it, not to Stop (AC-5, AC-26).
    pub fn address_editing(&self) -> bool {
        self.palette.is_some() || self.address.editing()
    }

    /// Focuses the address (AC-5, AC-6): the inline field with its full
    /// URL selected, or the palette, opened over the page card. Pressed
    /// again while open, the palette selects all again.
    pub fn open_address(&mut self) {
        match self.address.style() {
            AddressStyle::Inline => {
                let Some(field) = self.by_id("address-field") else {
                    return;
                };
                if self.page.focused().map(|n| n.node) == Some(field) {
                    self.page.input(InputEvent::SelectAll);
                } else {
                    self.relayout();
                    self.page.move_focus(Some(field));
                }
                self.sync_address();
            }
            AddressStyle::Palette => {
                let Some(field) = self.by_id("palette-field") else {
                    return;
                };
                if self.palette.is_some() {
                    self.page.input(InputEvent::SelectAll);
                    self.page.end_task();
                    return;
                }
                if self.menu.is_some() {
                    self.close_menu(false);
                }
                self.palette = Some(self.page.focused().map(|n| n.node));
                self.address.open();
                self.set_hidden("palette", false);
                self.set_hidden("palette-scrim", false);
                self.page.end_task();
                // The field takes focus only once it has a box.
                self.relayout();
                self.page.set_input_text(field, self.address.text());
                self.page.move_focus(Some(field));
                self.page.input(InputEvent::SelectAll);
                self.show_address();
                self.page.end_task();
            }
        }
    }

    /// F6 into the sidebar (spec 0008, AC-31): the address field in
    /// inline style, with its URL selected; the pill in palette style.
    pub fn focus_sidebar(&mut self) {
        match self.address.style() {
            AddressStyle::Inline => self.open_address(),
            AddressStyle::Palette => {
                let Some(pill) = self.by_id("address-pill") else {
                    return;
                };
                self.relayout();
                self.page.move_focus(Some(pill));
                self.page.end_task();
            }
        }
    }

    /// Enter in the address, or a click on its intent row (AC-7, AC-8):
    /// the URL to load, or `None` for empty text, which does nothing. The
    /// palette closes and focus goes to the page; the address shows the
    /// URL asked for while it loads (AC-11).
    pub fn address_go(&mut self) -> Option<Url> {
        let target = self.address.enter()?;
        if self.palette.is_some() {
            self.close_palette(false, false);
        } else {
            self.page.move_focus(None);
        }
        self.show_address();
        self.page.end_task();
        Some(target)
    }

    /// Closes the palette (AC-6). With `revert`, the shown URL replaces
    /// what was typed; with `return_focus`, focus goes back to what had it
    /// before, else nowhere in the chrome (the page keeps its own).
    fn close_palette(&mut self, revert: bool, return_focus: bool) {
        let Some(opener) = self.palette.take() else {
            return;
        };
        self.address.close(revert);
        self.set_hidden("palette", true);
        self.set_hidden("palette-scrim", true);
        self.set_hidden("palette-intent", true);
        let back = if return_focus { opener } else { None };
        if !self.page.move_focus(back) {
            self.page.move_focus(None);
        }
        self.show_address();
        self.page.end_task();
    }

    /// Follows focus into and out of the address fields, and the text
    /// typed into them (AC-5, AC-7, AC-12): focus arriving shows the full
    /// URL selected, each edit is read again for the intent row, and focus
    /// leaving the inline field puts the shown URL back. Focus never
    /// leaves an open palette but through Escape, Enter or a press outside.
    fn sync_address(&mut self) {
        let field = match (self.address.style(), self.palette.is_some()) {
            (AddressStyle::Inline, _) => self.by_id("address-field"),
            (AddressStyle::Palette, true) => self.by_id("palette-field"),
            (AddressStyle::Palette, false) => None,
        };
        let focused = self.page.focused().map(|n| n.node);
        match field {
            Some(field) if focused == Some(field) => {
                if self.address.editing() {
                    let value = self.doc().input_value(field).unwrap_or_default();
                    if value != self.address.text() {
                        self.address.edit(&value);
                    }
                } else {
                    self.address.begin_edit();
                    self.page.set_input_text(field, self.address.text());
                    self.page.input(InputEvent::SelectAll);
                }
            }
            Some(field) if self.palette.is_some() => {
                self.page.move_focus(Some(field));
            }
            _ if self.address.editing() && self.palette.is_none() => {
                self.address.end_edit(true);
            }
            _ => {}
        }
        self.show_address();
    }

    /// Writes the address's state into the frame (AC-1 to AC-3, AC-7):
    /// the pill or the inline field by style, the security icons, the
    /// host or placeholder, the idle field text, and the intent row while
    /// the user types.
    fn show_address(&mut self) {
        let palette = self.address.style() == AddressStyle::Palette;
        self.set_hidden("address-pill", !palette);
        self.set_hidden("address-inline", palette);
        let shown = self
            .address
            .shown()
            .filter(|url| !matches!(url.as_str(), "about:start" | "about:blank"))
            .map(display_host);
        self.set_text("address-icon", glyph(shown.as_ref().map(|d| d.icon)));
        match &shown {
            Some(display) => {
                self.set_text("address-host", &display.host);
                self.set_hidden("address-host", false);
                self.set_hidden("address-placeholder", true);
            }
            None => {
                self.set_hidden("address-host", true);
                self.set_hidden("address-placeholder", false);
            }
        }
        let security = glyph(Some(self.address.security()));
        self.set_text("address-field-icon", security);
        self.set_text("palette-icon", security);
        let editing = self.address.editing();
        if !editing
            && let Some(field) = self.by_id("address-field")
            && self.doc().input_value(field).as_deref() != Some(self.address.text())
        {
            let text = self.address.text().to_owned();
            self.page.set_input_text(field, &text);
        }
        if let Some(inline) = self.by_id("address-inline") {
            if editing && !palette {
                set_attr(self.doc_mut(), inline, "data-editing", "");
            } else {
                remove_attr(self.doc_mut(), inline, "data-editing");
            }
        }
        let intent = self.address.intent_text().filter(|_| editing);
        let icon = match self.address.intent() {
            Some(Intent::Go(url)) => glyph(Some(display_host(url).icon)),
            Some(Intent::Search(_)) | None => SEARCH_GLYPH,
        };
        let rows = [
            (
                "address-intent",
                "address-intent-text",
                "address-intent-icon",
                !palette,
            ),
            (
                "palette-intent",
                "palette-intent-text",
                "palette-intent-icon",
                self.palette.is_some(),
            ),
        ];
        for (row, text_id, icon_id, active) in rows {
            match intent.as_deref().filter(|_| active) {
                Some(text) => {
                    self.set_text(text_id, text);
                    self.set_text(icon_id, icon);
                    self.set_hidden(row, false);
                }
                None => self.set_hidden(row, true),
            }
        }
    }

    /// Shows or hides the element with this `id`.
    fn set_hidden(&mut self, id: &str, hidden: bool) {
        let Some(node) = self.by_id(id) else {
            return;
        };
        if hidden {
            set_attr(self.doc_mut(), node, "hidden", "");
        } else {
            remove_attr(self.doc_mut(), node, "hidden");
        }
    }

    /// Shows `settings` in the settings window (AC-12): the checked theme
    /// and accent, the accent's label ("System (Blue)" with
    /// `system_accent`), the interface size, and the size buttons disabled
    /// where they would change nothing. Documents without these ids are
    /// left as they are.
    pub fn set_settings(&mut self, settings: &Settings, system_accent: Accent) {
        let groups = [
            ("theme", settings.theme.as_str()),
            ("accent", settings.accent.as_str()),
            ("address-style", settings.address_style.as_str()),
        ];
        for (group, value) in groups {
            let Some(container) = self.by_id(group) else {
                continue;
            };
            let radios: Vec<NodeId> = self.doc().children(container).collect();
            let doc = self.doc_mut();
            for radio in radios {
                let Some(checked) = attribute(doc, radio, "data-value").map(|v| v == value) else {
                    continue;
                };
                set_attr(doc, radio, "aria-checked", bool_attr(checked));
            }
        }
        // The search engine's option, and Custom's field when it is the
        // engine (spec 0008, AC-32).
        if let Some(list) = self.by_id("search-engine") {
            let options: Vec<NodeId> = self.doc().children(list).collect();
            let chosen = settings.search.as_str();
            let doc = self.doc_mut();
            for option in options {
                let Some(selected) = attribute(doc, option, "data-value").map(|v| v == chosen)
                else {
                    continue;
                };
                set_attr(doc, option, "aria-selected", bool_attr(selected));
            }
        }
        if let SearchEngine::Custom(template) = &settings.search {
            self.show_custom_search(Some(template), false);
        }
        if let Some(swatch) = self.by_id("accent-system") {
            let label = AccentSetting::System.label(system_accent);
            let doc = self.doc_mut();
            set_attr(doc, swatch, "data-accent", system_accent.as_str());
            set_attr(doc, swatch, "aria-label", &label);
        }
        self.set_text("accent-label", &settings.accent.label(system_accent));
        let zoom = settings.ui_zoom;
        self.set_text("zoom-value", &format!("{}%", zoom.percent()));
        let steps = [
            ("zoom-out", ZoomStep::Out),
            ("zoom-in", ZoomStep::In),
            ("zoom-reset", ZoomStep::Reset),
        ];
        for (id, step) in steps {
            let Some(button) = self.by_id(id) else {
                continue;
            };
            if zoom.step(step) == zoom {
                set_attr(self.doc_mut(), button, "aria-disabled", "true");
            } else {
                remove_attr(self.doc_mut(), button, "aria-disabled");
            }
        }
        self.page.end_task();
        // The checked swatch or theme becomes its group's `Tab` stop.
        self.init_roving();
    }

    /// Shows the settings window's Custom search field (spec 0008, AC-32),
    /// with `template` in it when given, and the error line when `error`.
    pub fn show_custom_search(&mut self, template: Option<&str>, error: bool) {
        self.set_hidden("search-custom", false);
        self.set_hidden("search-custom-error", !error);
        if let (Some(template), Some(field)) = (template, self.by_id("search-custom-field"))
            && self.page.focused().map(|n| n.node) != Some(field)
        {
            self.page.set_input_text(field, template);
        }
        self.page.end_task();
    }

    /// The current value of the text input with this `id`.
    pub fn input_value(&self, id: &str) -> Option<String> {
        self.doc().input_value(self.by_id(id)?)
    }

    /// Makes `content` the text of the element with this `id`.
    fn set_text(&mut self, id: &str, content: &str) {
        let Some(node) = self.by_id(id) else {
            return;
        };
        match self.doc().node(node).and_then(|n| n.first_child()) {
            Some(child) => {
                let _ = self.doc_mut().set_text(child, content);
            }
            None => text(self.doc_mut(), node, content),
        }
    }

    /// The text inside the element with this `id`, e.g. the settings
    /// window's "System (Blue)" label.
    pub fn element_text(&self, id: &str) -> Option<String> {
        self.by_id(id).map(|node| text_of(self.doc(), node))
    }

    /// The size the settings window wants, in chrome px, after the last
    /// layout: `--sheet-width` wide and as tall as its content. `None` for
    /// the other documents, which fill whatever window they get.
    pub fn natural_size(&self) -> Option<SizeF> {
        if self.surface != ChromeSurface::Settings {
            return None;
        }
        let body = self.element_box("settings-body")?;
        Some(SizeF {
            width: self.metrics.sheet_width,
            height: body.origin.y + body.size.height,
        })
    }

    /// The window changed size, scale factor or chrome zoom: `viewport` in
    /// chrome px, `scale` device pixels per chrome px.
    pub fn resize(&mut self, viewport: SizeF, scale: f32) {
        self.viewport = viewport;
        self.page.resize(page_size(viewport), scale);
        self.clamp_scroll();
    }

    /// The regions from the last layout, in chrome px.
    pub fn layout(&self) -> &FrameLayout {
        &self.layout
    }

    /// The border box of the element with this `id`, in chrome px from
    /// the document's top (no scroll), after the last layout.
    pub fn element_box(&self, id: &str) -> Option<RectF> {
        let node = self.by_id(id)?;
        self.page
            .element_boxes()
            .into_iter()
            .find(|(n, _)| *n == node)
            .map(|(_, r)| RectF::new(r.origin.x, r.origin.y, r.size.width, r.size.height))
    }

    /// The `≡` button's box, in chrome px, where a native popup menu opens
    /// (Windows). `None` where it is not shown.
    pub fn menu_anchor(&self) -> Option<RectF> {
        self.menu_button
    }

    /// Whether the app menu overlay is open (Linux).
    pub fn menu_open(&self) -> bool {
        self.menu.is_some()
    }

    /// Whether a chrome element has keyboard focus.
    pub fn has_focus(&self) -> bool {
        self.page.focused().is_some()
    }

    /// Takes one input event, in chrome px (spec 0006, Interface surface),
    /// and maps what it did to chrome commands and the keyboard model.
    pub fn input(&mut self, event: InputEvent) -> ChromeOutcome {
        let primary = match &event {
            InputEvent::PointerDown {
                x,
                y,
                button: PointerButton::Primary,
            } => Some((PointF { x: *x, y: *y }, true)),
            InputEvent::PointerUp {
                x,
                y,
                button: PointerButton::Primary,
            } => Some((PointF { x: *x, y: *y }, false)),
            _ => None,
        };
        let primary_down = primary.filter(|(_, down)| *down).map(|(at, _)| at);
        // A press outside the open menu closes it, unless it lands on the
        // button that opened it: its own activation closes it then.
        if let Some(at) = primary_down
            && self.menu.is_some()
            && !self.lands_in(at, &["app-menu", "app-menu-button"])
        {
            self.close_menu(false);
        }
        // A press of any button outside the open palette closes it and
        // puts focus back (spec 0008, AC-6). On the card's scrim it does
        // nothing else.
        if let InputEvent::PointerDown { x, y, .. } = &event {
            let at = PointF { x: *x, y: *y };
            if self.dismiss_palette(at)
                && (self.lands_in(at, &["palette-scrim"]) || self.lands_on_page(at))
            {
                return ChromeOutcome::default();
            }
        }
        // The intent row does what Enter does on release over it (AC-7).
        // Its press is kept from the engine, which would move focus off the
        // field and lose what was typed.
        if let Some((at, down)) = primary
            && (down || self.intent_pressed)
        {
            let over = self.lands_in(at, &INTENT_ROWS);
            if down && over {
                self.intent_pressed = true;
                return ChromeOutcome::default();
            }
            if !down {
                self.intent_pressed = false;
                return ChromeOutcome {
                    command: over.then_some(Ok(ChromeCommand::AddressGo)),
                    ..ChromeOutcome::default()
                };
            }
        }
        // Tab stays inside the open palette, a modal panel (AC-6).
        if self.palette.is_some() && matches!(&event, InputEvent::Key { key: Key::Tab, .. }) {
            return ChromeOutcome::default();
        }
        let outcome = self.page.input(event);
        let mut result = ChromeOutcome {
            command: outcome
                .activated
                .and_then(|node| self.command_at(node.node)),
            focus_left: outcome.focus_left,
            cursor: outcome.cursor,
            copied: outcome.copied.map(|(_, text)| text),
        };
        if let Some(key) = outcome.unused_key {
            result.command = self.unused_key(&key);
        }
        if result.focus_left.is_some() && self.menu.is_some() {
            // Tab out of an open menu closes it.
            self.close_menu(false);
        }
        self.page.end_task();
        self.sync_address();
        self.page.end_task();
        result
    }

    /// Whether `at` (chrome px) is on the page card.
    fn lands_on_page(&self, at: PointF) -> bool {
        self.layout.page.is_some_and(|page| page.contains(at))
    }

    /// Focus arrives from the page by `Tab` (AC-7). False when the chrome
    /// has nothing to focus, so focus should go back into the page.
    pub fn focus_enter(&mut self, direction: FocusDirection) -> bool {
        self.page.input(InputEvent::FocusEnter { direction });
        self.page.end_task();
        self.has_focus()
    }

    /// The page took focus: the chrome lets go of it, and an open palette
    /// closes with the shown URL back (AC-6).
    pub fn blur(&mut self) {
        if self.menu.is_some() {
            self.close_menu(false);
        }
        if self.palette.is_some() {
            self.close_palette(true, false);
        }
        self.page.move_focus(None);
        self.page.end_task();
        self.sync_address();
        self.page.end_task();
    }

    /// Scrolls the document's root by `dx`, `dy` chrome px, inside its
    /// scroll range: the gallery is taller than any window.
    pub fn scroll_by(&mut self, dx: f32, dy: f32) {
        self.scroll.x += dx;
        self.scroll.y += dy;
        self.clamp_scroll();
    }

    fn clamp_scroll(&mut self) {
        let max_x = (self.content.width - self.viewport.width).max(0.0);
        let max_y = (self.content.height - self.viewport.height).max(0.0);
        let clamped = PointF {
            x: self.scroll.x.clamp(0.0, max_x),
            y: self.scroll.y.clamp(0.0, max_y),
        };
        self.scroll = clamped;
        if let Some(root) = self.doc().document_element() {
            let node = NodeRef {
                frame: FrameId::MAIN,
                node: root,
            };
            self.page.scroll_to(node, clamped.x, clamped.y);
        }
    }

    /// Opens the app menu overlay from `menu` (Linux, AC-9), focusing its
    /// first row. False when this document has no menu overlay.
    pub fn open_menu(&mut self, menu: &Menu) -> bool {
        let (Some(panel), Some(button)) = (self.by_id("app-menu"), self.by_id("app-menu-button"))
        else {
            return false;
        };
        let opener = self.page.focused().map(|n| n.node).or(Some(button));
        let os = self.os;
        let doc = self.doc_mut();
        let old: Vec<NodeId> = doc.children(panel).collect();
        for child in old {
            let _ = doc.remove_child(panel, child);
        }
        build_rows(doc, panel, &menu.items, os);
        remove_attr(doc, panel, "hidden");
        set_attr(doc, button, "aria-expanded", "true");
        self.page.end_task();
        self.menu = Some(OpenMenu { opener });
        self.init_roving();
        // Rows exist only once laid out; focusing needs boxes.
        self.relayout();
        let first = self
            .items_of(panel, Composite::Menu)
            .first()
            .map(|(n, _)| *n);
        if let Some(first) = first {
            self.focus_item(panel, Composite::Menu, first);
        }
        true
    }

    /// Closes the app menu, and puts focus back on what opened it when
    /// `return_focus`.
    pub fn close_menu(&mut self, return_focus: bool) {
        let Some(open) = self.menu.take() else {
            return;
        };
        let (Some(panel), Some(button)) = (self.by_id("app-menu"), self.by_id("app-menu-button"))
        else {
            return;
        };
        let doc = self.doc_mut();
        set_attr(doc, panel, "hidden", "");
        set_attr(doc, button, "aria-expanded", "false");
        let rows: Vec<NodeId> = doc.children(panel).collect();
        for row in rows {
            let _ = doc.remove_child(panel, row);
        }
        if return_focus {
            self.page.move_focus(open.opener);
        } else if self
            .page
            .focused()
            .is_some_and(|focused| !self.doc().is_connected(focused.node))
        {
            self.page.move_focus(None);
        }
        self.page.end_task();
    }

    /// The next rendering, when anything changed since the last one.
    pub fn frame(&mut self) -> Option<ChromeFrame> {
        self.render();
        self.pending.take()
    }

    /// Lays out and paints now if anything changed, keeping the result for
    /// [`ChromeDocument::frame`]. Focusing a new element needs its box.
    fn relayout(&mut self) {
        self.render();
    }

    fn render(&mut self) {
        // A web font is fetched the first time laid out text needs it, so
        // a frame can ask for the icon font; answer and draw again then, so
        // no frame waits on the font's block period.
        for _ in 0..MAX_FETCH_ROUNDS {
            self.render_once();
            // The overlay sits over the card, so it is placed once the
            // card has a box, and laid out again where it moved.
            let placed = self.place_overlay();
            if !self.serve() && !placed {
                return;
            }
        }
    }

    /// Places the overlay pieces (spec 0008, AC-6, AC-19, AC-25): the
    /// palette in the middle of the window, 600 px wide or the window less
    /// 32 px (its height from its last box, so it is placed again once it
    /// has one); the scrim over the whole card; the loading track centered
    /// a little below the card's top; the status bubble in the card's
    /// bottom left corner, at most half its width. Returns whether
    /// anything moved.
    fn place_overlay(&mut self) -> bool {
        let Some(card) = self.layout.page else {
            return false;
        };
        let (x, y) = (card.origin.x, card.origin.y);
        let (width, height) = (card.size.width, card.size.height);
        let view = self.viewport;
        let inset = self.metrics.page_radius / 2.0;
        let palette_width = (view.width - PALETTE_MARGIN).clamp(0.0, PALETTE_MAX_WIDTH);
        let palette_height = self
            .element_box("palette")
            .map_or(0.0, |palette| palette.size.height);
        let loading_width = self.loading_width.min(width);
        let places = [
            (
                "palette",
                format!(
                    "left: {}px; top: {}px; width: {palette_width}px",
                    (view.width - palette_width) / 2.0,
                    ((view.height - palette_height) / 2.0).max(0.0)
                ),
            ),
            (
                "palette-scrim",
                format!("left: {x}px; top: {y}px; width: {width}px; height: {height}px"),
            ),
            (
                "loading-bar",
                format!(
                    "left: {}px; top: {}px; width: {loading_width}px",
                    x + (width - loading_width) / 2.0,
                    y + LOADING_TOP
                ),
            ),
            (
                "status-bubble",
                format!(
                    "left: {}px; bottom: {}px; max-width: {}px",
                    x + inset,
                    (view.height - (y + height) + inset).max(0.0),
                    width / 2.0
                ),
            ),
        ];
        let mut moved = false;
        for (id, style) in places {
            moved |= self.set_style(id, &style);
        }
        if moved {
            self.page.end_task();
        }
        moved
    }

    /// Moves the loading shimmer along its track for the time `ms` since
    /// any fixed start (spec 0008, AC-25): one sweep every 1.2 s, from
    /// just off the left end to just off the right. With reduced motion it
    /// stays still in the middle. The host calls this on a timer while a
    /// load runs; returns whether it moved, so a new frame is due.
    pub fn tick_loading(&mut self, ms: f64) -> bool {
        let track = self.loading_width;
        let sweep = track * SHIMMER_SHARE;
        let left = if self.reduce_motion {
            (track - sweep) / 2.0
        } else {
            let phase = (ms.rem_euclid(SHIMMER_PERIOD_MS) / SHIMMER_PERIOD_MS) as f32;
            -sweep + phase * (track + sweep)
        };
        let moved = self.set_style("loading-shimmer", &format!("left: {}px", left.round()));
        if moved {
            self.page.end_task();
        }
        moved
    }

    /// Whether the shimmer moves on its own, so the host needs a timer.
    pub fn loading_animates(&self) -> bool {
        !self.reduce_motion
    }

    /// Sets the `style` attribute of the element with this `id`, returning
    /// whether it changed.
    fn set_style(&mut self, id: &str, style: &str) -> bool {
        let Some(node) = self.by_id(id) else {
            return false;
        };
        if attribute(self.doc(), node, "style") == Some(style) {
            return false;
        }
        set_attr(self.doc_mut(), node, "style", style);
        true
    }

    fn render_once(&mut self) {
        self.page.end_task();
        if !self.page.take_frame_request() {
            return;
        }
        let list = self.page.begin_frame(0.0);
        let (adds, deletes) = self.page.take_resource_messages();
        let pending = self.pending.take();
        let overlay = match &list {
            Some(_) => self.page.overlay_list().cloned(),
            None => pending.as_ref().and_then(|p| p.overlay.clone()),
        };
        let Some(list) = list.or_else(|| pending.as_ref().map(|p| p.list.clone())) else {
            return;
        };
        self.read_layout();
        if let Some(size) = root_content_size(&list) {
            self.content = size;
        }
        // Earlier resources go first, so a font is added before any list
        // that names it.
        let (mut all_adds, mut all_deletes) =
            pending.map(|p| (p.adds, p.deletes)).unwrap_or_default();
        all_adds.extend(adds);
        all_deletes.extend(deletes);
        self.pending = Some(ChromeFrame {
            list,
            adds: all_adds,
            deletes: all_deletes,
            overlay,
        });
    }

    /// The frame's regions from the element boxes (spec 0003, frame
    /// anatomy).
    fn read_layout(&mut self) {
        let doc = self.doc();
        let mut layout = FrameLayout {
            size: self.viewport,
            page_radius: self.metrics.page_radius,
            // The settings window keeps one size.
            resize_border: match (self.surface, self.os) {
                (ChromeSurface::Settings, _) | (_, Os::Mac) => 0.0,
                (_, Os::Windows | Os::Linux) => self.metrics.resize_border,
            },
            ..FrameLayout::default()
        };
        let page_slot = doc.element_by_id(&Atom::from("page-slot"));
        let title_row = doc.element_by_id(&Atom::from("nav-row"));
        let overlay: Vec<NodeId> = POINTER_OVERLAY
            .iter()
            .filter_map(|id| doc.element_by_id(&Atom::from(*id)))
            .collect();
        let menu_button = doc.element_by_id(&Atom::from("app-menu-button"));
        let mut menu_anchor = None;
        for (node, rect) in self.page.element_boxes() {
            let rect = RectF::new(
                rect.origin.x,
                rect.origin.y,
                rect.size.width,
                rect.size.height,
            );
            if Some(node) == page_slot {
                layout.page = Some(rect);
                continue;
            }
            if Some(node) == title_row {
                layout.title_row = Some(rect);
            }
            if Some(node) == menu_button {
                menu_anchor = Some(rect);
            }
            // Only shown pieces have boxes (spec 0008, AC-6).
            if overlay.contains(&node) {
                layout.overlay.push(rect);
            }
            let Some(element) = doc.node(node).and_then(|n| n.as_element()) else {
                continue;
            };
            let attr = |name: &str| element.attribute(&QualName::attribute(LocalName::from(name)));
            let command = attr("data-command");
            let button = match command {
                Some("window.minimize") => Some(CaptionButton::Minimize),
                Some("window.maximize") => Some(CaptionButton::Maximize),
                Some("window.close") => Some(CaptionButton::Close),
                _ => None,
            };
            if let Some(button) = button {
                layout.caption_buttons.push((button, rect));
                continue;
            }
            let tag = &*element.name().local;
            let interactive = command.is_some()
                || attr("tabindex").is_some()
                || attr("role") == Some("menu")
                || matches!(tag, "button" | "input" | "select" | "textarea")
                || (tag == "a" && attr("href").is_some());
            if interactive {
                layout.controls.push(rect);
            } else if attr("data-drag-region").is_some() {
                layout.drag.push(rect);
            }
        }
        self.layout = layout;
        self.menu_button = menu_anchor;
    }

    /// Whether `at` (chrome px) lands in, or under, one of the elements
    /// with these ids.
    fn lands_in(&self, at: PointF, ids: &[&str]) -> bool {
        let point = display_list::PointF { x: at.x, y: at.y };
        let Some((hit, _)) = self.page.hit_test(point) else {
            return false;
        };
        let targets: Vec<NodeId> = ids.iter().filter_map(|id| self.by_id(id)).collect();
        ancestors(self.doc(), hit.node).any(|node| targets.contains(&node))
    }

    /// The command of the nearest `data-command` element at or above
    /// `node`, unless something on the way is `aria-disabled` (spec 0006,
    /// Follow-up).
    fn command_at(&self, node: NodeId) -> Option<Result<ChromeCommand, ChromeError>> {
        let doc = self.doc();
        for id in ancestors(doc, node) {
            let Some(element) = doc.node(id).and_then(|n| n.as_element()) else {
                continue;
            };
            let attr = |name: &str| element.attribute(&QualName::attribute(LocalName::from(name)));
            if attr("aria-disabled") == Some("true") {
                return None;
            }
            if let Some(name) = attr("data-command") {
                return Some(parse_command(name, attr("data-value")));
            }
        }
        None
    }

    /// A key the engine did not use: `Escape` closes the open menu, the
    /// rest goes to the composite around the focused element.
    fn unused_key(&mut self, key: &Key) -> Option<Result<ChromeCommand, ChromeError>> {
        let focused = self.page.focused().map(|n| n.node);
        let in_field =
            focused.is_some_and(|node| FIELDS.iter().any(|id| self.by_id(id) == Some(node)));
        if *key == Key::Enter && in_field {
            return Some(Ok(ChromeCommand::AddressGo));
        }
        if *key == Key::Enter && focused.is_some() && focused == self.by_id("search-custom-field") {
            return Some(Ok(ChromeCommand::SaveCustomSearch));
        }
        if *key == Key::Escape {
            if self.menu.is_some() {
                self.close_menu(true);
            } else if self.palette.is_some() {
                // Escape closes the palette, puts the shown URL back, and
                // returns focus (AC-6).
                self.close_palette(true, true);
            } else if in_field {
                // The inline field: the shown URL back, focus to the page
                // (AC-5).
                self.address.end_edit(true);
                self.page.move_focus(None);
                self.show_address();
            } else if self.surface == ChromeSurface::Settings {
                // The settings window is a dialog, so `Escape` closes it (AC-7).
                return Some(Ok(ChromeCommand::Window(WindowAction::Close)));
            }
            return None;
        }
        let nav = nav_key(key)?;
        let focused = self.page.focused()?.node;
        let (container, composite) = self.composite_of(focused)?;
        let items = self.items_of(container, composite);
        let current = items.iter().position(|(node, _)| *node == focused)?;
        let plain: Vec<Item> = items.iter().map(|(_, item)| item.clone()).collect();
        match keyboard::composite_key(composite, &plain, current, &nav) {
            Move::Focus(index) => {
                self.focus_item(container, composite, items[index].0);
                None
            }
            Move::FocusAndActivate(index) => {
                let node = items[index].0;
                self.focus_item(container, composite, node);
                self.command_at(node)
            }
            Move::Activate => self.command_at(focused),
            Move::Nothing => None,
        }
    }

    /// The composite `node` is an item of, with its container.
    fn composite_of(&self, node: NodeId) -> Option<(NodeId, Composite)> {
        let doc = self.doc();
        let role = attribute(doc, node, "role")?;
        let container = doc.node(node)?.parent()?;
        let vertical = attribute(doc, container, "aria-orientation") == Some("vertical");
        let composite = Composite::from_role(attribute(doc, container, "role")?, vertical)?;
        composite.holds(role).then_some((container, composite))
    }

    /// The items of `container`, in tree order.
    fn items_of(&self, container: NodeId, composite: Composite) -> Vec<(NodeId, Item)> {
        let doc = self.doc();
        doc.children(container)
            .filter(|&child| attribute(doc, child, "role").is_some_and(|r| composite.holds(r)))
            .map(|child| {
                let disabled = attribute(doc, child, "aria-disabled") == Some("true")
                    || attribute(doc, child, "disabled").is_some();
                (
                    child,
                    Item {
                        disabled,
                        label: text_of(doc, child),
                    },
                )
            })
            .collect()
    }

    /// Moves the roving stop to `item` and focuses it.
    fn focus_item(&mut self, container: NodeId, composite: Composite, item: NodeId) {
        let items: Vec<NodeId> = self
            .items_of(container, composite)
            .into_iter()
            .map(|(node, _)| node)
            .collect();
        let doc = self.doc_mut();
        for node in items {
            set_attr(doc, node, "tabindex", if node == item { "0" } else { "-1" });
        }
        self.page.end_task();
        self.relayout();
        self.page.move_focus(Some(item));
    }

    /// Makes each composite one `Tab` stop (spec 0003, keyboard model): its
    /// checked or selected item, or its first enabled one, keeps
    /// `tabindex="0"` and the others get `-1`.
    fn init_roving(&mut self) {
        let doc = self.doc();
        let containers: Vec<(NodeId, Composite)> = doc
            .node_ids()
            .filter(|&id| doc.is_connected(id))
            .filter_map(|id| {
                let vertical = attribute(doc, id, "aria-orientation") == Some("vertical");
                Some((
                    id,
                    Composite::from_role(attribute(doc, id, "role")?, vertical)?,
                ))
            })
            .collect();
        for (container, composite) in containers {
            let items = self.items_of(container, composite);
            let doc = self.doc();
            let chosen = items
                .iter()
                .find(|(node, item)| {
                    !item.disabled
                        && (attribute(doc, *node, "aria-checked") == Some("true")
                            || attribute(doc, *node, "aria-selected") == Some("true"))
                })
                .or_else(|| items.iter().find(|(_, item)| !item.disabled))
                .map(|(node, _)| *node);
            let doc = self.doc_mut();
            for (node, _) in &items {
                let stop = Some(*node) == chosen;
                set_attr(doc, *node, "tabindex", if stop { "0" } else { "-1" });
            }
        }
        self.page.end_task();
    }

    /// The surface this document shows.
    pub fn surface(&self) -> ChromeSurface {
        self.surface
    }
}

/// The icon for a security state (AC-3); the globe without a URL.
fn glyph(security: Option<Security>) -> &'static str {
    match security {
        Some(Security::Lock) => LOCK_GLYPH,
        Some(Security::Warning) => WARNING_GLYPH,
        Some(Security::Globe) | None => GLOBE_GLYPH,
    }
}

/// The page's viewport type from chrome px.
fn page_size(size: SizeF) -> display_list::SizeF {
    display_list::SizeF {
        width: size.width,
        height: size.height,
    }
}

/// The root scroll frame's content size: how far the gallery scrolls.
fn root_content_size(list: &DisplayList) -> Option<SizeF> {
    list.spatial.iter().find_map(|node| match &node.kind {
        SpatialKind::ScrollFrame { content_size, .. } => Some(SizeF {
            width: content_size.width,
            height: content_size.height,
        }),
        _ => None,
    })
}

fn caption_id(button: CaptionButton) -> &'static str {
    match button {
        CaptionButton::Minimize => "caption-minimize",
        CaptionButton::Maximize => "caption-maximize",
        CaptionButton::Close => "caption-close",
    }
}

/// `node` and its flat tree ancestors.
fn ancestors(doc: &Document, node: NodeId) -> impl Iterator<Item = NodeId> + '_ {
    std::iter::successors(Some(node), |&id| doc.flat_parent(id))
}

fn attribute<'d>(doc: &'d Document, node: NodeId, name: &str) -> Option<&'d str> {
    doc.node(node)?
        .as_element()?
        .attribute(&QualName::attribute(LocalName::from(name)))
}

/// Sets an attribute when it differs, so an unchanged theme restyles
/// nothing. The node was just found in this document, so it is alive; a
/// failure would only mean it is not an element, and nothing is set.
fn set_attr(doc: &mut Document, node: NodeId, name: &str, value: &str) {
    if attribute(doc, node, name) != Some(value) {
        let _ = doc.set_attribute(node, QualName::attribute(LocalName::from(name)), value);
    }
}

fn bool_attr(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

fn remove_attr(doc: &mut Document, node: NodeId, name: &str) {
    if attribute(doc, node, name).is_some() {
        let _ = doc.remove_attribute(node, &QualName::attribute(LocalName::from(name)));
    }
}

/// The text under `node`, for type ahead: its `.label` child's, or all of it.
fn text_of(doc: &Document, node: NodeId) -> String {
    let label = doc
        .children(node)
        .find(|&child| attribute(doc, child, "class") == Some("label"))
        .unwrap_or(node);
    let mut text = String::new();
    let mut stack = vec![label];
    while let Some(id) = stack.pop() {
        if let Some(dom::NodeData::Text(data)) = doc.node(id).map(|n| n.data()) {
            text.push_str(data);
        }
        let children: Vec<NodeId> = doc.children(id).collect();
        stack.extend(children.into_iter().rev());
    }
    text
}

/// The composite key for an engine key, if it is one.
fn nav_key(key: &Key) -> Option<NavKey> {
    Some(match key {
        Key::ArrowUp => NavKey::Up,
        Key::ArrowDown => NavKey::Down,
        Key::ArrowLeft => NavKey::Left,
        Key::ArrowRight => NavKey::Right,
        Key::Home => NavKey::Home,
        Key::End => NavKey::End,
        Key::Enter => NavKey::Enter,
        Key::Space => NavKey::Space,
        Key::Character(text) => NavKey::Char(text.chars().next()?),
        _ => return None,
    })
}

/// Appends a new element under `parent` with `attributes`.
fn element(
    doc: &mut Document,
    parent: NodeId,
    tag: &str,
    attributes: &[(&str, &str)],
) -> Option<NodeId> {
    let node = doc.create_element(QualName::html(LocalName::from(tag)));
    for &(name, value) in attributes {
        doc.set_attribute(node, QualName::attribute(LocalName::from(name)), value)
            .ok()?;
    }
    doc.append_child(parent, node).ok()?;
    Some(node)
}

fn text(doc: &mut Document, parent: NodeId, content: &str) {
    let node = doc.create_text(content);
    let _ = doc.append_child(parent, node);
}

/// Builds the menu rows under `panel` (design.md, Menu (Linux)): one level
/// deep, a submenu becomes a heading over its items.
fn build_rows(doc: &mut Document, panel: NodeId, items: &[MenuItem], os: Os) {
    for item in items {
        match item {
            MenuItem::Command {
                label,
                command,
                shortcut,
                checked,
            } => {
                let (name, value) = command.parts();
                let checked_value = checked.map(bool_attr);
                let role = if checked.is_some() {
                    "menuitemradio"
                } else {
                    "menuitem"
                };
                let mut attributes =
                    vec![("role", role), ("tabindex", "-1"), ("data-command", name)];
                if let Some(value) = value {
                    attributes.push(("data-value", value));
                }
                if let Some(checked) = checked_value {
                    attributes.push(("aria-checked", checked));
                }
                let Some(row) = element(doc, panel, "div", &attributes) else {
                    continue;
                };
                let icon_class = if checked.is_some() {
                    "icon check"
                } else {
                    "icon"
                };
                if let Some(icon) = element(
                    doc,
                    row,
                    "span",
                    &[("class", icon_class), ("aria-hidden", "true")],
                ) && checked.is_some()
                {
                    text(doc, icon, CHECK_GLYPH);
                }
                if let Some(span) = element(doc, row, "span", &[("class", "label")]) {
                    text(doc, span, label);
                }
                if let Some(shortcut) = shortcut
                    && let Some(span) = element(doc, row, "span", &[("class", "shortcut")])
                {
                    text(doc, span, &shortcut.label(os));
                }
            }
            MenuItem::Submenu { label, items } => {
                if let Some(heading) = element(
                    doc,
                    panel,
                    "div",
                    &[("class", "menu-heading"), ("role", "presentation")],
                ) {
                    text(doc, heading, label);
                }
                build_rows(doc, panel, items, os);
            }
            MenuItem::Separator => {
                let _ = element(doc, panel, "div", &[("role", "separator")]);
            }
            // The OS's own items exist only in the macOS menu bar.
            MenuItem::Native(_) => {}
        }
    }
}
