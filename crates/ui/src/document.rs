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
use display_list::{DisplayList, PageId, SpatialKind};
use dom::{Document, FrameId, NodeId, NodeRef};
use ipc::{
    CursorKind, DocumentKind, FetchError, FetchId, FocusDirection, InputEvent, Key, NavigationId,
    PointerButton, ToBrowser,
};
use navigation::{Security, display_host};
use page::{FontContext, Page};
use url::Url;

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
}

/// What the navigation controls show (spec 0008, Interface surface).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NavView {
    /// There is an entry to go back to.
    pub can_back: bool,
    /// There is an entry to go forward to.
    pub can_forward: bool,
    /// The URL the address shows; `None` before the first commit.
    pub url: Option<Url>,
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
        };
        document.serve();
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

    /// Shows the navigation state (spec 0008, AC-1, AC-3): back and
    /// forward `aria-disabled` when there is no entry that way, and the
    /// address's security icon and display host. `about:start`,
    /// `about:blank` and no URL at all show the placeholder instead.
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
        let shown = view
            .url
            .as_ref()
            .filter(|url| !matches!(url.as_str(), "about:start" | "about:blank"))
            .map(display_host);
        let glyph = match shown.as_ref().map(|d| d.icon) {
            Some(Security::Lock) => LOCK_GLYPH,
            Some(Security::Warning) => WARNING_GLYPH,
            Some(Security::Globe) | None => GLOBE_GLYPH,
        };
        self.set_text("address-icon", glyph);
        let (Some(host), Some(placeholder)) = (
            self.by_id("address-host"),
            self.by_id("address-placeholder"),
        ) else {
            self.page.end_task();
            return;
        };
        match &shown {
            Some(display) => {
                self.set_text("address-host", &display.host);
                remove_attr(self.doc_mut(), host, "hidden");
                set_attr(self.doc_mut(), placeholder, "hidden", "");
            }
            None => {
                set_attr(self.doc_mut(), host, "hidden", "");
                remove_attr(self.doc_mut(), placeholder, "hidden");
            }
        }
        self.page.end_task();
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
        let primary_down = match &event {
            InputEvent::PointerDown {
                x,
                y,
                button: PointerButton::Primary,
            } => Some(PointF { x: *x, y: *y }),
            _ => None,
        };
        // A press outside the open menu closes it, unless it lands on the
        // button that opened it: its own activation closes it then.
        if let Some(at) = primary_down
            && self.menu.is_some()
            && !self.lands_in(at, &["app-menu", "app-menu-button"])
        {
            self.close_menu(false);
        }
        let outcome = self.page.input(event);
        let mut result = ChromeOutcome {
            command: outcome
                .activated
                .and_then(|node| self.command_at(node.node)),
            focus_left: outcome.focus_left,
            cursor: outcome.cursor,
        };
        if let Some(key) = outcome.unused_key {
            result.command = self.unused_key(&key);
        }
        if result.focus_left.is_some() && self.menu.is_some() {
            // Tab out of an open menu closes it.
            self.close_menu(false);
        }
        self.page.end_task();
        result
    }

    /// Focus arrives from the page by `Tab` (AC-7). False when the chrome
    /// has nothing to focus, so focus should go back into the page.
    pub fn focus_enter(&mut self, direction: FocusDirection) -> bool {
        self.page.input(InputEvent::FocusEnter { direction });
        self.page.end_task();
        self.has_focus()
    }

    /// The page took focus: the chrome lets go of it.
    pub fn blur(&mut self) {
        if self.menu.is_some() {
            self.close_menu(false);
        }
        self.page.move_focus(None);
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
            if !self.serve() {
                return;
            }
        }
    }

    fn render_once(&mut self) {
        self.page.end_task();
        if !self.page.take_frame_request() {
            return;
        }
        let list = self.page.begin_frame(0.0);
        let (adds, deletes) = self.page.take_resource_messages();
        let pending = self.pending.take();
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
            if Some(node) == menu_button {
                menu_anchor = Some(rect);
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
        if *key == Key::Escape {
            if self.menu.is_some() {
                self.close_menu(true);
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
