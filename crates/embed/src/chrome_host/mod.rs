//! The browser chrome's side effects (spec 0003): the settings file, OS
//! appearance readers and change signals, the frameless window, native
//! menus, shortcuts, file dialogs, the settings window, and the chrome
//! document's input and drawing. The decisions live in the pure `ui` crate, the chrome document
//! included; this module feeds it the OS and the window and carries out
//! its results.

mod dialogs;
mod menus;
mod settings_file;
mod settings_window;

#[cfg_attr(target_os = "macos", path = "macos.rs")]
#[cfg_attr(windows, path = "windows.rs")]
#[cfg_attr(all(unix, not(target_os = "macos")), path = "linux.rs")]
mod platform;

use std::time::{Duration, Instant};

use display_list::{PointF, RectF, SizeF};
use ipc::{CursorKind, FocusDirection, InputEvent};
use render::PageSlot;
use tracing::{error, info, warn};
use ui::{
    Accent, ChromeCommand, ChromeDocument, ChromeOutcome, ChromeSurface, ChromeTheme, Hit, Menu,
    Os, OsAppearance, ResizeEdge, Settings, Shortcut, ShortcutKey, WindowAction,
};
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::window::{CursorIcon, ResizeDirection, Theme, Window, WindowAttributes, WindowId};

use self::dialogs::{FileChoice, FileDialog};
use self::menus::NativeMenu;
use self::platform::Platform;
use self::settings_file::SettingsFile;
use self::settings_window::{SettingsWindow, View};
use crate::browser::UserEvent;
use crate::render_thread::ChromeDraw;
use crate::{Config, EmbedError, input, scrolling};

/// Two presses on the drag area closer than this maximize the window.
const DOUBLE_CLICK: Duration = Duration::from_millis(500);

/// What the chrome's OS hooks and menus tell the event loop.
#[derive(Debug)]
pub(crate) enum ChromeEvent {
    /// A native menu item was chosen; the id is a [`ChromeCommand::id`].
    #[cfg(any(target_os = "macos", windows))]
    Menu(String),
    /// A command from outside the menus (the Windows maximize button).
    #[cfg(windows)]
    Command(ChromeCommand),
    /// The pointer went onto (true) or off the Windows maximize button,
    /// which the OS hit tests itself for Snap Layouts.
    #[cfg(windows)]
    MaximizeHover(bool),
    /// An OS appearance setting changed; read them again.
    #[cfg(any(target_os = "macos", windows))]
    OsChanged,
    /// The Linux Settings portal sent fresh values.
    #[cfg(all(unix, not(target_os = "macos")))]
    Portal(platform::PortalAppearance),
    /// A file dialog closed, with the path chosen or nothing on cancel.
    FileChosen(FileChoice),
}

/// What the event loop has to do after the chrome handled something.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Nothing visible changed.
    None,
    /// The theme, zoom or window changed: send the page its viewport if
    /// the card moved.
    Redraw,
    /// A window action on the browser window, for the event loop.
    Window(WindowAction),
    /// Open the settings window, which needs the event loop.
    OpenSettings,
    /// `Tab` left the chrome: focus goes into the page from this end.
    FocusPage(FocusDirection),
    /// The back button (spec 0008, AC-1).
    Back,
    /// The forward button.
    Forward,
    /// The reload button.
    Reload,
}

/// Pointer state over the chrome.
#[derive(Default)]
struct Pointer {
    /// The cursor, in physical pixels.
    position: PhysicalPosition<f64>,
    modifiers: ModifiersState,
    /// When the drag area was last pressed, for double click.
    last_drag_press: Option<Instant>,
    /// The resize edge the cursor shape shows.
    edge: Option<ResizeEdge>,
    /// The chrome document was last told the pointer is over it.
    in_chrome: bool,
    /// The primary button went down on the chrome document and is still
    /// down, so its release goes there wherever it happens.
    pressed_in_chrome: bool,
    /// The shape the chrome document asked for.
    cursor: CursorKind,
}

/// The chrome of the browser window.
pub(crate) struct ChromeHost {
    app_name: String,
    file: SettingsFile,
    settings: Settings,
    os: OsAppearance,
    theme: ChromeTheme,
    menu: Menu,
    native_menu: NativeMenu,
    /// The frame (or the gallery), rendered by the engine.
    doc: ChromeDocument,
    /// The page card last sent to the render thread, in physical pixels.
    slot: Option<PageSlot>,
    /// OS hooks, attached once the window exists.
    platform: Option<Platform>,
    /// Where file dialog answers go, set once the window exists.
    proxy: Option<EventLoopProxy<UserEvent>>,
    /// A file dialog is waiting for the user; only one opens at a time.
    dialog_open: bool,
    /// The settings window, while it is open (AC-12).
    settings_window: Option<SettingsWindow>,
    pointer: Pointer,
}

impl ChromeHost {
    /// Loads the settings and the chrome document. The OS is read once the
    /// window exists.
    pub(crate) fn new(config: &Config) -> Result<Self, EmbedError> {
        let (file, settings) = SettingsFile::load(&config.app_id);
        let os = OsAppearance::fallback(Os::current());
        let theme = ui::resolve(&settings, &os);
        let menu = app_menu(&settings, &os, &config.title);
        let surface = if config.chrome_gallery && cfg!(debug_assertions) {
            ChromeSurface::Gallery
        } else {
            ChromeSurface::Frame
        };
        // The window's real size arrives with `attach`.
        let size = ui::SizeF {
            width: 1.0,
            height: 1.0,
        };
        let doc = ChromeDocument::load(surface, os.os, size, 1.0, None)?;
        Ok(Self {
            app_name: config.title.clone(),
            file,
            settings,
            os,
            theme,
            menu,
            native_menu: NativeMenu::default(),
            doc,
            slot: None,
            platform: None,
            proxy: None,
            dialog_open: false,
            settings_window: None,
            pointer: Pointer::default(),
        })
    }

    /// The frameless window attributes for this OS (spec 0003, AC-8).
    pub(crate) fn window_attributes(&self, attributes: WindowAttributes) -> WindowAttributes {
        platform::window_attributes(attributes)
    }

    /// Hooks the OS once the window is open: change signals, the frame,
    /// the menus. Then reads the OS appearance and styles the chrome.
    pub(crate) fn attach(&mut self, window: &Window, proxy: &EventLoopProxy<UserEvent>) {
        self.platform = Some(Platform::attach(window, proxy.clone()));
        self.native_menu.attach(proxy.clone());
        self.proxy = Some(proxy.clone());
        self.read_os(window);
        self.refresh(window);
    }

    /// Shows the page's navigation state in the frame (spec 0008, AC-1,
    /// AC-3); the next [`ChromeHost::take_draw`] draws it.
    pub(crate) fn set_navigation(&mut self, view: &ui::NavView) {
        self.doc.set_navigation(view);
    }

    /// The chrome document's next drawing for the render thread, when it
    /// changed, and whether the page card moved with it (then the page
    /// needs its new viewport). Also hands the frame's regions to the OS
    /// hit test.
    pub(crate) fn take_draw(&mut self, window: &Window) -> Option<(ChromeDraw, bool)> {
        let frame = self.doc.frame()?;
        let scale = self.chrome_scale(window);
        let layout = self.doc.layout();
        let slot = layout.page.map(|page| PageSlot {
            rect: scaled(page, scale),
            radius: layout.page_radius * scale,
        });
        if let Some(platform) = &self.platform {
            platform.set_frame(layout.clone(), scale);
        }
        let moved = slot != self.slot;
        self.slot = slot;
        let draw = ChromeDraw {
            list: frame.list,
            adds: frame.adds,
            deletes: frame.deletes,
            scale,
            slot,
        };
        Some((draw, moved))
    }

    /// The page card in the page's CSS px: the viewport the content process
    /// lays out to. Chrome zoom scales the chrome, not the page, so a chrome
    /// px is `zoom` page px. Without a card yet (or in the gallery), the
    /// whole window.
    pub(crate) fn page_viewport(&self, window: &Window) -> SizeF {
        match self.doc.layout().page {
            Some(page) => SizeF {
                width: page.size.width * self.theme.zoom,
                height: page.size.height * self.theme.zoom,
            },
            None => {
                let size = window.inner_size().to_logical::<f32>(window.scale_factor());
                SizeF {
                    width: size.width,
                    height: size.height,
                }
            }
        }
    }

    /// Where the page card's top left corner is, in physical pixels.
    pub(crate) fn page_origin(&self) -> PointF {
        self.slot.map(|slot| slot.rect.origin).unwrap_or_default()
    }

    /// Whether pointer input goes to the page: the pointer is over the card
    /// and the chrome is not holding it (an open menu, a press that began
    /// on the chrome).
    pub(crate) fn pointer_on_page(&self, window: &Window) -> bool {
        !self.doc.menu_open()
            && !self.pointer.pressed_in_chrome
            && self.doc.surface() == ChromeSurface::Frame
            && self.hit(window) == Hit::Page
    }

    /// The pointer shape to show over the chrome.
    pub(crate) fn cursor(&self) -> CursorKind {
        self.pointer.cursor
    }

    /// Handles the window events the chrome cares about: the OS theme, the
    /// window's size, the pointer on the chrome.
    pub(crate) fn window_event(&mut self, window: &Window, event: &WindowEvent) -> Outcome {
        match event {
            WindowEvent::ThemeChanged(theme) => {
                self.os.dark = *theme == Theme::Dark;
                // Linux takes its color scheme from the portal when it has one.
                self.read_os(window);
                self.refresh(window)
            }
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                self.resize_doc(window);
                self.doc.set_maximized(window.is_maximized());
                Outcome::None
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.pointer.modifiers = modifiers.state();
                Outcome::None
            }
            WindowEvent::Focused(focused) => {
                let focused = *focused;
                if !focused {
                    self.pointer.pressed_in_chrome = false;
                }
                let outcome = self.doc.input(InputEvent::WindowFocus { focused });
                self.handle(window, outcome)
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer.position = *position;
                self.update_edge_cursor(window);
                self.pointer_moved(window)
            }
            WindowEvent::CursorLeft { .. } => self.pointer_left(window),
            WindowEvent::MouseInput { state, button, .. } => self.click(window, *state, *button),
            _ => Outcome::None,
        }
    }

    /// A key, before the page sees it: the menu shortcuts (Windows and
    /// Linux; macOS's menu bar takes its own), then the chrome document
    /// when it has focus. `None` when the key is the page's.
    pub(crate) fn key(&mut self, window: &Window, event: &KeyEvent) -> Option<Outcome> {
        let pressed = event.state == ElementState::Pressed;
        if let Some(command) =
            shortcut_command(&self.menu, self.os.os, event, self.pointer.modifiers)
        {
            return Some(self.run(window, command));
        }
        if !self.doc.has_focus() && !self.doc.menu_open() {
            return None;
        }
        // A modifier alone is nobody's.
        let Some(key) = input::key(&event.logical_key) else {
            return Some(Outcome::None);
        };
        let outcome = self.doc.input(InputEvent::Key {
            key,
            modifiers: input::modifiers(self.pointer.modifiers),
            pressed,
            repeat: event.repeat,
        });
        Some(self.handle(window, outcome))
    }

    /// Focus arrives from the page by `Tab` (spec 0003, AC-7). False when
    /// the chrome has nothing to focus, so it should go back into the page.
    pub(crate) fn focus_enter(&mut self, direction: FocusDirection) -> bool {
        self.doc.focus_enter(direction)
    }

    /// The page was pressed: keyboard focus leaves the chrome.
    pub(crate) fn page_pressed(&mut self) {
        if self.doc.has_focus() || self.doc.menu_open() {
            self.doc.blur();
        }
    }

    /// A wheel over the chrome: the gallery scrolls, the frame swallows it.
    /// False when it is the page's.
    pub(crate) fn wheel(&mut self, window: &Window, delta: MouseScrollDelta) -> bool {
        if self.pointer_on_page(window) {
            return false;
        }
        let scale = self.chrome_scale(window);
        let (dx, dy) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (-x * scrolling::LINE, -y * scrolling::LINE),
            MouseScrollDelta::PixelDelta(p) => (-p.x as f32 / scale, -p.y as f32 / scale),
        };
        if self.doc.surface() == ChromeSurface::Gallery {
            self.doc.scroll_by(dx, dy);
        }
        true
    }

    /// Handles an event from a menu or an OS hook.
    pub(crate) fn chrome_event(&mut self, window: &Window, event: ChromeEvent) -> Outcome {
        match event {
            #[cfg(any(target_os = "macos", windows))]
            ChromeEvent::Menu(id) => match ChromeCommand::from_id(&id) {
                Ok(command) => self.run(window, command),
                Err(err) => {
                    warn!(%err, "menu item ignored");
                    Outcome::None
                }
            },
            #[cfg(windows)]
            ChromeEvent::Command(command) => self.run(window, command),
            #[cfg(windows)]
            ChromeEvent::MaximizeHover(over) => {
                let maximize = self
                    .doc
                    .layout()
                    .caption_buttons
                    .iter()
                    .find(|(button, _)| *button == ui::CaptionButton::Maximize)
                    .map(|(_, rect)| *rect);
                // Off maximize onto another chrome control, winit has
                // already said where the pointer is: hover goes back there.
                let at = self.chrome_point(window);
                let event = match (over, maximize) {
                    (true, Some(rect)) => InputEvent::PointerMove {
                        x: rect.origin.x + rect.size.width / 2.0,
                        y: rect.origin.y + rect.size.height / 2.0,
                    },
                    _ if self.pointer.in_chrome => InputEvent::PointerMove { x: at.x, y: at.y },
                    _ => InputEvent::PointerLeave,
                };
                let outcome = self.doc.input(event);
                self.handle(window, outcome)
            }
            #[cfg(any(target_os = "macos", windows))]
            ChromeEvent::OsChanged => {
                self.read_os(window);
                self.refresh(window)
            }
            #[cfg(all(unix, not(target_os = "macos")))]
            ChromeEvent::Portal(portal) => {
                if let Some(platform) = &mut self.platform {
                    platform.set_portal(portal);
                }
                self.read_os(window);
                self.refresh(window)
            }
            ChromeEvent::FileChosen(choice) => {
                self.dialog_open = false;
                match choice.path {
                    Some(path) => {
                        info!(dialog = ?choice.dialog, path = %path.display(), "file chosen")
                    }
                    None => info!(dialog = ?choice.dialog, "file dialog cancelled"),
                }
                Outcome::None
            }
        }
    }

    /// Handles an event of the settings window. `None` when `id` is not
    /// the settings window, so the event is the browser window's.
    pub(crate) fn settings_window_event(
        &mut self,
        window: &Window,
        id: WindowId,
        event: &WindowEvent,
    ) -> Option<Outcome> {
        let settings = self.settings_window.as_mut().filter(|s| s.id() == id)?;
        let command = match event {
            WindowEvent::KeyboardInput { event, .. } => {
                let modifiers = settings.modifiers();
                shortcut_command(&self.menu, self.os.os, event, modifiers)
                    .or_else(|| settings.key(event))
            }
            _ => settings.window_event(event),
        };
        Some(match command {
            // Its own buttons, keys and close request act on it, focused
            // or not.
            Some(ChromeCommand::Window(action)) => self.settings_action(action),
            Some(command) => self.run(window, command),
            None => Outcome::None,
        })
    }

    /// Opens the settings window, or brings it to the front (AC-12). A
    /// failure is logged; the browser goes on without it.
    pub(crate) fn open_settings(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(settings) = &self.settings_window {
            settings.focus();
            return;
        }
        let Some(proxy) = self.proxy.clone() else {
            return;
        };
        let view = view(&self.theme, &self.settings, &self.os);
        match SettingsWindow::open(event_loop, self.os.os, &view, proxy) {
            Ok(settings) => self.settings_window = Some(settings),
            Err(err) => error!(%err, "could not open the settings window"),
        }
    }

    /// Sends the settings window's next drawing, if it changed.
    pub(crate) fn pump_settings(&mut self) {
        if let Some(settings) = &mut self.settings_window {
            settings.pump();
        }
    }

    /// The render thread of `window` failed. The settings window closes
    /// and the browser keeps going; false when it is another window.
    pub(crate) fn drawing_failed(&mut self, window: WindowId) -> bool {
        if self
            .settings_window
            .as_ref()
            .is_none_or(|settings| settings.id() != window)
        {
            return false;
        }
        error!("the settings window cannot draw; closing it");
        self.close_settings();
        true
    }

    /// Closes the settings window, if it is open.
    pub(crate) fn close_settings(&mut self) {
        if let Some(settings) = self.settings_window.take() {
            settings.close();
        }
    }

    /// A window action on the settings window. It keeps one size, so it
    /// does not maximize.
    fn settings_action(&mut self, action: WindowAction) -> Outcome {
        match action {
            WindowAction::Close => self.close_settings(),
            WindowAction::Minimize => {
                if let Some(settings) = &self.settings_window {
                    settings.minimize();
                }
            }
            WindowAction::Maximize => {}
        }
        Outcome::None
    }

    /// Carries out a chrome command (spec 0003, interface surface).
    pub(crate) fn run(&mut self, window: &Window, command: ChromeCommand) -> Outcome {
        // A menu row closes its menu, with focus back on the menu button.
        if command != ChromeCommand::AppMenu && self.doc.menu_open() {
            self.doc.close_menu(true);
        }
        if let Some(settings) = command.apply(&self.settings) {
            self.settings = settings;
            self.file.save(&self.settings);
            return self.refresh(window);
        }
        match command {
            // Menus and shortcuts act on the window with focus.
            ChromeCommand::Window(action)
                if self.settings_window.as_ref().is_some_and(|s| s.focused()) =>
            {
                self.settings_action(action)
            }
            ChromeCommand::Window(action) => Outcome::Window(action),
            ChromeCommand::AppMenu => self.app_menu(window),
            ChromeCommand::OpenSettings => match &self.settings_window {
                Some(settings) => {
                    settings.focus();
                    Outcome::None
                }
                None => Outcome::OpenSettings,
            },
            ChromeCommand::DevOpenFile => self.show_dialog(window, FileDialog::Open),
            ChromeCommand::DevSaveFile => self.show_dialog(window, FileDialog::Save),
            ChromeCommand::Back => Outcome::Back,
            ChromeCommand::Forward => Outcome::Forward,
            ChromeCommand::Reload => Outcome::Reload,
            // Settings commands that changed nothing, like zooming in at 200%.
            ChromeCommand::SetTheme(_) | ChromeCommand::SetAccent(_) | ChromeCommand::Zoom(_) => {
                Outcome::None
            }
        }
    }

    /// The `≡` button (spec 0003, AC-9): the in window overlay on Linux,
    /// toggled; the native popup under the button on Windows. macOS has
    /// the menu bar instead.
    fn app_menu(&mut self, window: &Window) -> Outcome {
        #[cfg(not(windows))]
        let _ = window;
        match self.os.os {
            Os::Linux if self.doc.menu_open() => self.doc.close_menu(true),
            Os::Linux => {
                self.doc.open_menu(&self.menu);
            }
            #[cfg(windows)]
            Os::Windows => {
                let scale = self.chrome_scale(window);
                let anchor = self.doc.menu_anchor().map(|rect| {
                    (
                        rect.origin.x * scale,
                        (rect.origin.y + rect.size.height) * scale,
                    )
                });
                self.native_menu.popup(window, &self.menu, anchor);
            }
            _ => {}
        }
        Outcome::None
    }

    /// Opens a file dialog demo (spec 0003, AC-10). A second request while
    /// one is open is ignored rather than stacking dialogs.
    fn show_dialog(&mut self, window: &Window, dialog: FileDialog) -> Outcome {
        if self.dialog_open {
            info!(?dialog, "a file dialog is already open");
            return Outcome::None;
        }
        if let Some(proxy) = &self.proxy {
            self.dialog_open = dialogs::show(window, dialog, proxy.clone());
        }
        Outcome::None
    }

    fn read_os(&mut self, window: &Window) {
        if let Some(platform) = &self.platform {
            self.os = platform.read(window);
        }
    }

    /// Resolves the theme again and pushes it everywhere: the chrome
    /// document's root, the window buttons, the zoom, the menus, and the
    /// settings window (AC-12: every window restyles).
    fn refresh(&mut self, window: &Window) -> Outcome {
        self.theme = ui::resolve(&self.settings, &self.os);
        self.menu = app_menu(&self.settings, &self.os, &self.app_name);
        self.native_menu.set(&self.menu);
        self.doc.set_theme(&self.theme);
        self.doc.set_caption(&self.os.button_layout);
        self.doc.set_maximized(window.is_maximized());
        self.resize_doc(window);
        if let Some(settings) = &mut self.settings_window {
            settings.restyle(&view(&self.theme, &self.settings, &self.os));
        }
        info!(
            theme = self.theme.scheme.as_str(),
            contrast = self.theme.contrast.as_str(),
            accent = self.theme.accent.as_str(),
            zoom = self.theme.zoom,
            "chrome restyled"
        );
        Outcome::Redraw
    }

    /// Device pixels per chrome px: the scale factor times the chrome zoom
    /// (spec 0003, Value sourcing, device pixels).
    fn chrome_scale(&self, window: &Window) -> f32 {
        window.scale_factor() as f32 * self.theme.zoom
    }

    /// Tells the chrome document the window's size in chrome px.
    fn resize_doc(&mut self, window: &Window) {
        let scale = self.chrome_scale(window);
        let size = window.inner_size();
        let viewport = ui::SizeF {
            width: size.width as f32 / scale,
            height: size.height as f32 / scale,
        };
        self.doc.resize(viewport, scale);
    }

    /// The pointer in chrome px.
    fn chrome_point(&self, window: &Window) -> ui::PointF {
        let scale = self.chrome_scale(window);
        ui::PointF {
            x: self.pointer.position.x as f32 / scale,
            y: self.pointer.position.y as f32 / scale,
        }
    }

    fn hit(&self, window: &Window) -> Hit {
        self.doc
            .layout()
            .hit(self.chrome_point(window), window.is_maximized())
    }

    /// Moves the chrome's hover with the pointer while it is off the page,
    /// and tells it once when the pointer goes onto the page.
    fn pointer_moved(&mut self, window: &Window) -> Outcome {
        let to_chrome = !self.pointer_on_page(window);
        if !to_chrome {
            return self.pointer_left(window);
        }
        let entered = !self.pointer.in_chrome;
        self.pointer.in_chrome = true;
        let at = self.chrome_point(window);
        let outcome = self.doc.input(InputEvent::PointerMove { x: at.x, y: at.y });
        if entered && self.pointer.edge.is_none() {
            window.set_cursor(input::cursor_icon(self.pointer.cursor));
        }
        self.handle(window, outcome)
    }

    fn pointer_left(&mut self, window: &Window) -> Outcome {
        if !self.pointer.in_chrome {
            return Outcome::None;
        }
        self.pointer.in_chrome = false;
        let outcome = self.doc.input(InputEvent::PointerLeave);
        self.handle(window, outcome)
    }

    /// Shows a resize cursor over the edges. Windows answers this itself
    /// from the hit test, and macOS resizes natively.
    fn update_edge_cursor(&mut self, window: &Window) {
        if self.os.os != Os::Linux {
            return;
        }
        let edge = match self.hit(window) {
            Hit::Resize(edge) => Some(edge),
            _ => None,
        };
        if edge != self.pointer.edge {
            self.pointer.edge = edge;
            match edge {
                Some(edge) => window.set_cursor(resize_cursor(edge)),
                None => window.set_cursor(input::cursor_icon(self.pointer.cursor)),
            }
        }
    }

    /// Drag, double click and resize on the frame (spec 0003, AC-8), and
    /// presses on the chrome document's controls. On Windows the hit test
    /// answers the OS for drag, resize and maximize, so those presses
    /// never reach the event loop.
    fn click(&mut self, window: &Window, state: ElementState, button: MouseButton) -> Outcome {
        let Some(button) = input::pointer_button(button) else {
            return Outcome::None;
        };
        let primary = button == ipc::PointerButton::Primary;
        let at = self.chrome_point(window);
        let (x, y) = (at.x, at.y);
        match state {
            ElementState::Pressed => {
                if self.pointer_on_page(window) {
                    return Outcome::None;
                }
                match self.hit(window) {
                    Hit::Resize(edge) if primary => {
                        if let Err(err) = window.drag_resize_window(resize_direction(edge)) {
                            warn!(%err, "could not start a window resize");
                        }
                        Outcome::None
                    }
                    Hit::Drag if primary && !self.doc.menu_open() => self.drag(window),
                    _ => {
                        self.pointer.pressed_in_chrome |= primary;
                        let outcome = self.doc.input(InputEvent::PointerDown { x, y, button });
                        self.handle(window, outcome)
                    }
                }
            }
            ElementState::Released => {
                let held = primary && self.pointer.pressed_in_chrome;
                if !held && self.pointer_on_page(window) {
                    return Outcome::None;
                }
                if primary {
                    self.pointer.pressed_in_chrome = false;
                }
                let outcome = self.doc.input(InputEvent::PointerUp { x, y, button });
                self.handle(window, outcome)
            }
        }
    }

    /// A press on the drag area: a second one soon after maximizes, any
    /// other starts moving the window.
    fn drag(&mut self, window: &Window) -> Outcome {
        let now = Instant::now();
        let double = self
            .pointer
            .last_drag_press
            .is_some_and(|last| now.duration_since(last) < DOUBLE_CLICK);
        if double {
            self.pointer.last_drag_press = None;
            return Outcome::Window(WindowAction::Maximize);
        }
        self.pointer.last_drag_press = Some(now);
        if let Err(err) = window.drag_window() {
            warn!(%err, "could not start a window drag");
        }
        Outcome::None
    }

    /// Carries out what the chrome document said: its cursor, a command
    /// (an unknown one is logged and ignored, AC-13), focus leaving.
    fn handle(&mut self, window: &Window, outcome: ChromeOutcome) -> Outcome {
        if let Some(cursor) = outcome.cursor {
            self.pointer.cursor = cursor;
            if self.pointer.in_chrome && self.pointer.edge.is_none() {
                window.set_cursor(input::cursor_icon(cursor));
            }
        }
        if let Some(direction) = outcome.focus_left {
            return Outcome::FocusPage(direction);
        }
        match outcome.command {
            Some(Ok(command)) => self.run(window, command),
            Some(Err(err)) => {
                warn!(%err, "chrome command ignored");
                Outcome::None
            }
            None => Outcome::None,
        }
    }
}

/// The app menu, plus the Developer menu in dev builds.
fn app_menu(settings: &Settings, os: &OsAppearance, app_name: &str) -> Menu {
    let menu = ui::app_menu(settings, system_accent(os), os.os, app_name);
    if cfg!(debug_assertions) {
        menu.with_developer(os.os)
    } else {
        menu
    }
}

/// The preset the OS accent snaps to, for the "System (Blue)" label.
fn system_accent(os: &OsAppearance) -> Accent {
    os.accent.map_or(Accent::Blue, ui::snap)
}

/// What the settings window shows.
fn view<'a>(theme: &'a ChromeTheme, settings: &'a Settings, os: &'a OsAppearance) -> View<'a> {
    View {
        theme,
        settings,
        system_accent: system_accent(os),
        buttons: &os.button_layout,
    }
}

/// The menu command `event` is the shortcut of. On macOS the menu bar
/// takes its own shortcuts, so there is none here.
fn shortcut_command(
    menu: &Menu,
    os: Os,
    event: &KeyEvent,
    modifiers: ModifiersState,
) -> Option<ChromeCommand> {
    if os == Os::Mac || event.state != ElementState::Pressed || event.repeat {
        return None;
    }
    menu.command_for(shortcut(event, modifiers)?)
}

fn shortcut(event: &KeyEvent, modifiers: ModifiersState) -> Option<Shortcut> {
    let PhysicalKey::Code(code) = event.physical_key else {
        return None;
    };
    let key = match code {
        KeyCode::Comma => ShortcutKey::Comma,
        KeyCode::Equal => ShortcutKey::Equal,
        KeyCode::Minus => ShortcutKey::Minus,
        KeyCode::Digit0 => ShortcutKey::Digit0,
        KeyCode::KeyM => ShortcutKey::Letter('m'),
        KeyCode::KeyO => ShortcutKey::Letter('o'),
        KeyCode::KeyS => ShortcutKey::Letter('s'),
        KeyCode::KeyW => ShortcutKey::Letter('w'),
        _ => return None,
    };
    Some(Shortcut {
        modifiers: ui::Modifiers {
            primary: if cfg!(target_os = "macos") {
                modifiers.super_key()
            } else {
                modifiers.control_key()
            },
            alt: modifiers.alt_key(),
            shift: modifiers.shift_key(),
        },
        key,
    })
}

fn resize_direction(edge: ResizeEdge) -> ResizeDirection {
    match edge {
        ResizeEdge::North => ResizeDirection::North,
        ResizeEdge::South => ResizeDirection::South,
        ResizeEdge::East => ResizeDirection::East,
        ResizeEdge::West => ResizeDirection::West,
        ResizeEdge::NorthEast => ResizeDirection::NorthEast,
        ResizeEdge::NorthWest => ResizeDirection::NorthWest,
        ResizeEdge::SouthEast => ResizeDirection::SouthEast,
        ResizeEdge::SouthWest => ResizeDirection::SouthWest,
    }
}

fn resize_cursor(edge: ResizeEdge) -> CursorIcon {
    match edge {
        ResizeEdge::North | ResizeEdge::South => CursorIcon::NsResize,
        ResizeEdge::East | ResizeEdge::West => CursorIcon::EwResize,
        ResizeEdge::NorthEast | ResizeEdge::SouthWest => CursorIcon::NeswResize,
        ResizeEdge::NorthWest | ResizeEdge::SouthEast => CursorIcon::NwseResize,
    }
}

/// A chrome px rectangle in physical pixels.
fn scaled(rect: ui::RectF, scale: f32) -> RectF {
    RectF {
        origin: PointF {
            x: rect.origin.x * scale,
            y: rect.origin.y * scale,
        },
        size: SizeF {
            width: rect.size.width * scale,
            height: rect.size.height * scale,
        },
    }
}
