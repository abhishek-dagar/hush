//! The settings window (spec 0003, AC-12): a second frameless window with
//! its own render thread, drawing `settings.html`. The host keeps at most
//! one, restyles it with the browser window, and runs the commands it
//! sends back. It keeps one size, `--sheet-width` wide and as tall as its
//! content, both in chrome px, so it grows with the interface size.

use std::sync::Arc;
use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;

use ipc::{CursorKind, InputEvent, PointerButton};
use render::Renderer;
use tracing::{debug, error, info, warn};
use ui::{
    Accent, ButtonLayout, CHROME_PAGE, ChromeCommand, ChromeDocument, ChromeOutcome, ChromeSurface,
    ChromeTheme, Hit, Os, Settings,
};
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowButtons, WindowId};

use super::platform;
use crate::browser::UserEvent;
use crate::input::EditKey;
use crate::render_thread::{self, ChromeDraw, RenderMsg};
use crate::{EmbedError, input};

/// The first layout's size in chrome px (spec 0003, build plan task 18:
/// "about 480 by 560", taller since spec 0008 added the address and search
/// groups); the window then takes the size its content asks for.
const FIRST_GUESS: ui::SizeF = ui::SizeF {
    width: 480.0,
    height: 760.0,
};

/// What the settings window shows, handed in on open and on every restyle.
pub(super) struct View<'a> {
    pub(super) theme: &'a ChromeTheme,
    pub(super) settings: &'a Settings,
    /// The preset the OS accent snaps to, for "System (Blue)".
    pub(super) system_accent: Accent,
    pub(super) buttons: &'a ButtonLayout,
}

/// The open settings window.
pub(super) struct SettingsWindow {
    window: Arc<Window>,
    render: Sender<RenderMsg>,
    /// Joined in `close`, before the window goes away.
    thread: Option<JoinHandle<()>>,
    doc: ChromeDocument,
    /// The chrome zoom the document is laid out at.
    zoom: f32,
    /// The cursor, in physical pixels.
    position: PhysicalPosition<f64>,
    modifiers: ModifiersState,
    focused: bool,
}

impl SettingsWindow {
    /// Lays out the settings document, opens a window of the size it asks
    /// for, and starts its render thread with the first drawing.
    pub(super) fn open(
        event_loop: &ActiveEventLoop,
        os: Os,
        view: &View<'_>,
        proxy: EventLoopProxy<UserEvent>,
    ) -> Result<Self, EmbedError> {
        let zoom = view.theme.zoom;
        let mut doc = ChromeDocument::load(ChromeSurface::Settings, os, FIRST_GUESS, zoom, None)?;
        show(&mut doc, view);
        let first = doc.frame();
        let size = doc.natural_size().unwrap_or(FIRST_GUESS);
        let attributes = Window::default_attributes()
            .with_title("Settings")
            .with_inner_size(logical(size, zoom))
            .with_resizable(false)
            .with_enabled_buttons(WindowButtons::CLOSE | WindowButtons::MINIMIZE);
        let window = Arc::new(event_loop.create_window(platform::window_attributes(attributes))?);
        let inner = window.inner_size();
        let renderer = Renderer::new(Arc::clone(&window), inner.width, inner.height)?;
        let (render, receiver) = mpsc::channel();
        // No page: the render thread draws only the chrome document. No
        // perf scenario runs in a settings window (spec 0007).
        let thread = render_thread::spawn(
            renderer,
            Arc::clone(&window),
            CHROME_PAGE,
            receiver,
            proxy,
            None,
            render.clone(),
        )?;
        let mut settings = Self {
            window,
            render,
            thread: Some(thread),
            doc,
            zoom,
            position: PhysicalPosition::default(),
            modifiers: ModifiersState::empty(),
            focused: true,
        };
        settings.resize_doc();
        settings.send_window();
        if let Some(first) = first {
            settings.send_draw(first);
        }
        info!(
            width = size.width,
            height = size.height,
            "settings window opened"
        );
        Ok(settings)
    }

    /// The OS window's id, to route its events here.
    pub(super) fn id(&self) -> WindowId {
        self.window.id()
    }

    /// Whether it has keyboard focus: window actions from the menus and
    /// shortcuts go to it then.
    pub(super) fn focused(&self) -> bool {
        self.focused
    }

    /// Brings it to the front (`settings.open` while it is open).
    pub(super) fn focus(&self) {
        self.window.set_minimized(false);
        self.window.focus_window();
    }

    /// Minimizes it.
    pub(super) fn minimize(&self) {
        self.window.set_minimized(true);
    }

    /// Shows the new theme and settings; a new zoom resizes the window.
    pub(super) fn restyle(&mut self, view: &View<'_>) {
        show(&mut self.doc, view);
        self.zoom = view.theme.zoom;
        self.resize_doc();
    }

    /// Sends the next drawing to the render thread when anything changed,
    /// and asks the OS for the size the content wants when that moved.
    pub(super) fn pump(&mut self) {
        let Some(frame) = self.doc.frame() else {
            return;
        };
        self.send_draw(frame);
        let Some(size) = self.doc.natural_size() else {
            return;
        };
        let wanted = logical(size, self.zoom);
        let current = self
            .window
            .inner_size()
            .to_logical::<f64>(self.window.scale_factor());
        if (current.width - wanted.width).abs() > 0.5
            || (current.height - wanted.height).abs() > 0.5
        {
            // The OS answers with `Resized`, or it already has the size.
            let _ = self.window.request_inner_size(wanted);
        }
    }

    /// Handles one of its window events. A command it activated comes back
    /// for the host to run; closing the window comes back as
    /// `window.close`.
    pub(super) fn window_event(&mut self, event: &WindowEvent) -> Option<ChromeCommand> {
        match event {
            WindowEvent::CloseRequested => Some(ChromeCommand::Window(ui::WindowAction::Close)),
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                self.resize_doc();
                self.send_window();
                None
            }
            WindowEvent::RedrawRequested => {
                self.render(RenderMsg::Redraw);
                None
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
                None
            }
            WindowEvent::Focused(focused) => {
                self.focused = *focused;
                let focused = *focused;
                self.input(InputEvent::WindowFocus { focused })
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.position = *position;
                let at = self.chrome_point();
                self.input(InputEvent::PointerMove { x: at.x, y: at.y })
            }
            WindowEvent::CursorLeft { .. } => self.input(InputEvent::PointerLeave),
            WindowEvent::MouseInput { state, button, .. } => self.click(*state, *button),
            _ => None,
        }
    }

    /// The modifiers held over this window, for the shortcuts.
    pub(super) fn modifiers(&self) -> ModifiersState {
        self.modifiers
    }

    /// A key the shortcuts did not take goes to the document, then the
    /// text it types (spec 0008, AC-13).
    pub(super) fn key(&mut self, event: &KeyEvent) -> Option<ChromeCommand> {
        // A modifier alone is nobody's.
        let key = input::key(&event.logical_key)?;
        let command = self.input(InputEvent::Key {
            key,
            modifiers: input::modifiers(self.modifiers),
            pressed: event.state == ElementState::Pressed,
            repeat: event.repeat,
        });
        if command.is_some() {
            return command;
        }
        let text = input::typed_text(event, self.modifiers)?;
        self.input(InputEvent::Text { text })
    }

    /// A platform edit key in its text field (spec 0008, AC-15), with the
    /// clipboard's text for a paste. Returns text a copy or cut took.
    pub(super) fn edit(&mut self, edit: EditKey, paste: Option<String>) -> Option<String> {
        let event = match edit {
            EditKey::SelectAll => InputEvent::SelectAll,
            EditKey::Copy => InputEvent::Copy { request: 0 },
            EditKey::Cut => InputEvent::Cut { request: 0 },
            EditKey::Paste => InputEvent::Paste { text: paste? },
        };
        self.doc.input(event).copied
    }

    /// Shows the Custom search field, with `template` in it when given,
    /// and its error line when `error` (spec 0008, AC-32).
    pub(super) fn show_custom_search(&mut self, template: Option<&str>, error: bool) {
        self.doc.show_custom_search(template, error);
    }

    /// The Custom search field's text.
    pub(super) fn custom_search_text(&self) -> String {
        self.doc
            .input_value("search-custom-field")
            .unwrap_or_default()
    }

    /// Stops the render thread while the window still exists, then lets
    /// the window go.
    pub(super) fn close(mut self) {
        self.render(RenderMsg::Shutdown);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            error!("the settings render thread panicked");
        }
        info!("settings window closed");
    }

    /// A press on empty frame drags the window; anything else is the
    /// document's.
    fn click(&mut self, state: ElementState, button: MouseButton) -> Option<ChromeCommand> {
        let button = input::pointer_button(button)?;
        let at = self.chrome_point();
        let (x, y) = (at.x, at.y);
        match state {
            ElementState::Pressed => {
                if button == PointerButton::Primary && self.doc.layout().hit(at, false) == Hit::Drag
                {
                    if let Err(err) = self.window.drag_window() {
                        warn!(%err, "could not start a window drag");
                    }
                    return None;
                }
                self.input(InputEvent::PointerDown { x, y, button })
            }
            ElementState::Released => self.input(InputEvent::PointerUp { x, y, button }),
        }
    }

    /// Feeds the document an event and carries out what it said: its
    /// cursor, and `Tab` past either end coming round again, since the
    /// window has no page to hand focus to. An unknown command is logged
    /// and ignored (AC-13).
    fn input(&mut self, event: InputEvent) -> Option<ChromeCommand> {
        let ChromeOutcome {
            command,
            focus_left,
            cursor,
            ..
        } = self.doc.input(event);
        if let Some(cursor) = cursor {
            self.set_cursor(cursor);
        }
        if let Some(direction) = focus_left {
            self.doc.focus_enter(direction);
        }
        match command? {
            Ok(command) => Some(command),
            Err(err) => {
                warn!(%err, "chrome command ignored");
                None
            }
        }
    }

    fn set_cursor(&self, cursor: CursorKind) {
        self.window.set_cursor(input::cursor_icon(cursor));
    }

    /// Device pixels per chrome px.
    fn scale(&self) -> f32 {
        self.window.scale_factor() as f32 * self.zoom
    }

    /// The pointer in chrome px.
    fn chrome_point(&self) -> ui::PointF {
        let scale = self.scale();
        ui::PointF {
            x: self.position.x as f32 / scale,
            y: self.position.y as f32 / scale,
        }
    }

    /// Lays the document out at its own width and the window's height.
    /// Keeping the width while a new size is on its way means the height
    /// it asks for is already the right one.
    fn resize_doc(&mut self) {
        let scale = self.scale();
        let width = self
            .doc
            .natural_size()
            .map_or(FIRST_GUESS.width, |size| size.width);
        let viewport = ui::SizeF {
            width,
            height: self.window.inner_size().height as f32 / scale,
        };
        self.doc.resize(viewport, scale);
    }

    fn send_window(&self) {
        let size = self.window.inner_size();
        self.render(RenderMsg::Window {
            width: size.width,
            height: size.height,
            scale: self.window.scale_factor() as f32,
        });
    }

    fn send_draw(&self, frame: ui::ChromeFrame) {
        self.render(RenderMsg::Chrome(Box::new(ChromeDraw {
            list: frame.list,
            adds: frame.adds,
            deletes: frame.deletes,
            scale: self.scale(),
            slot: None,
            overlay: frame.overlay,
        })));
    }

    /// Tells the render thread. It runs until `close`, so a failure means
    /// it already ended on an error the main thread hears about.
    fn render(&self, message: RenderMsg) {
        if self.render.send(message).is_err() {
            debug!("the settings render thread is gone");
        }
    }
}

/// Writes the theme and the settings into the document.
fn show(doc: &mut ChromeDocument, view: &View<'_>) {
    doc.set_theme(view.theme);
    doc.set_caption(view.buttons);
    doc.set_settings(view.settings, view.system_accent);
}

/// A chrome px size at `zoom`, in logical pixels.
fn logical(size: ui::SizeF, zoom: f32) -> LogicalSize<f64> {
    LogicalSize::new(f64::from(size.width * zoom), f64::from(size.height * zoom))
}
