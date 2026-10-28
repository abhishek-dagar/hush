//! The browser process: owns the window, the GPU, the network and the
//! content process.

use std::env;
use std::process::{self, Child, Command};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use display_list::{PageId, PointF, SizeF};
use ipc::{
    BootstrapServer, ColorScheme, ContentChannel, ContentReceiver, ContentSender, CursorKind,
    DocumentKind, FetchError, HistoryInfo, InputEvent, NavigationId, PageNavigationAction,
    PageNavigationOrigin, ScrollTarget, ToBrowser, ToContent,
};
use navigation::{
    Action, Allow, Begin, EntryId, EntryKind, Origin, ScriptThrottle, SessionHistory, Stopped,
    UserInput,
};
use net::{ContentSink, FetchRequest, Net, NetConfig, NetHandle};
use render::Renderer;
use tracing::{debug, error, info, info_span, warn};
use url::Url;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::dpi::PhysicalPosition;
use winit::event::{
    ElementState, KeyEvent, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::window::{Theme, Window, WindowId};

#[cfg(feature = "chrome")]
use crate::chrome_host::{ChromeEvent, ChromeHost, Outcome};
use crate::input;
#[cfg(feature = "chrome")]
use crate::input::EditKey;
use crate::lifecycle::{self, CrashLog, PaintHold, Shortcut};
use crate::memory_report;
use crate::perf;
use crate::render_thread::{self, RenderEvent, RenderMsg};
use crate::scrolling;
use crate::start_url::{StartPage, start_page};
use crate::{CONTENT_PROCESS_FLAG, Config, EmbedError};

/// How long the content process gets to connect and say it is ready.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the content process gets to exit after being asked to, before
/// it is killed (spec 0004, child 0001, Reload, shortcuts and crash relaunch).
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);
/// The one page this window shows until tabs exist.
const MAIN_PAGE: PageId = PageId(1);
/// The launch number of the first content process.
const FIRST_PROCESS: u32 = 1;
/// How long a `Copy` or `Cut` sent to the page may be answered (spec 0008,
/// Key invariants).
#[cfg(feature = "chrome")]
const COPY_ANSWER: Duration = Duration::from_secs(1);
/// How often the loading shimmer moves while a load runs (spec 0008,
/// AC-25): about 30 frames a second.
#[cfg(feature = "chrome")]
const SHIMMER_TICK: Duration = Duration::from_millis(33);

/// The sender to the live content process, shared with the network thread
/// so a relaunch can point its stream at the new process (AC-14).
type ContentLink = Arc<Mutex<Option<ContentSender>>>;

/// What the page loads when it starts a navigation.
#[derive(Debug, Clone, PartialEq)]
enum Load {
    /// The start page from the command line (spec 0004, AC-1).
    Start,
    /// A URL, with who asked and what it does to the session history
    /// (spec 0008).
    Url {
        /// What to load.
        url: Url,
        /// Who asked.
        origin: Origin,
        /// What it does to the history.
        action: Action,
    },
    /// The crash error page for the current entry (AC-14; spec 0008,
    /// AC-34).
    CrashPage,
}

/// What beginning a load in the session history led to.
enum Begun {
    /// A navigation, and what the network fetches for it.
    Load(NavigationId, Fetch),
    /// A move to another fragment of the document on screen (spec 0008,
    /// AC-20): no load, the entry `leaving` was left.
    Moved(EntryId, HistoryInfo),
}

/// What the network is asked to load for a navigation.
enum Fetch {
    /// The document at this URL.
    Url(Url),
    /// The error page for `input`, without loading anything.
    Error(FetchError, String),
}

/// Everything other threads and OS hooks hand the event loop.
#[derive(Debug)]
pub(crate) enum UserEvent {
    /// From the content listener thread of content process `process`.
    Content {
        /// Which launch of the content process it came from; a relaunch
        /// takes the next number, so leftovers from a dead one are dropped.
        process: u32,
        /// What arrived.
        event: FromContent,
    },
    /// From the render thread.
    Render(RenderEvent),
    /// From the network thread: a file the page loaded changed (spec 0004,
    /// AC-38).
    FilesChanged(PageId),
    /// From the network thread: it committed a document to the page, at
    /// this final URL (spec 0008, Value sourcing, committed URL). Sent
    /// before the commit reaches the page, so it arrives first.
    Committed {
        /// Which page.
        page: PageId,
        /// Which navigation.
        navigation: NavigationId,
        /// The final URL after redirects; `about:error` for the error page.
        url: String,
        /// How the page treats it.
        kind: DocumentKind,
    },
    /// From a native menu or an OS appearance hook (spec 0003).
    #[cfg(feature = "chrome")]
    Chrome(ChromeEvent),
    /// From the perf wake thread: 2 s after `load`, time to ask the content
    /// process for its marks and footprint (spec 0007, AC-5).
    PerfSampleNow,
    /// From the perf wake thread: 500 ms after `load`, time to start the
    /// scroll driver (spec 0007, AC-6).
    PerfScrollStart,
    /// From the memory report wake thread: 2 s after `load`, time to ask
    /// both processes for their counters (spec 0032, AC-10).
    MemoryReportNow,
}

/// Where the IPC router sends each content message (spec 0004, Threads):
/// drawing to the render thread, fetches to the network thread, the rest
/// to the main thread.
#[derive(Clone)]
struct Routes {
    proxy: EventLoopProxy<UserEvent>,
    render: Sender<RenderMsg>,
    net: NetHandle,
}

/// What the content listener thread hands the event loop.
#[derive(Debug)]
pub(crate) enum FromContent {
    /// A message from the content process.
    Message(ToBrowser),
    /// The channel closed: the content process exited or crashed.
    Gone,
}

pub(crate) fn run(
    config: Config,
    perf: Option<perf::PerfArgs>,
    memory_report: Option<memory_report::State>,
) -> Result<(), EmbedError> {
    let _span = info_span!("browser", pid = process::id()).entered();
    if let Some(args) = &perf {
        perf::log_start(args);
    }

    #[allow(unused_mut, reason = "only macOS with the chrome changes the builder")]
    let mut builder = EventLoop::<UserEvent>::with_user_event();
    // The chrome builds the whole menu bar from its menu model (spec 0003, AC-9).
    #[cfg(all(feature = "chrome", target_os = "macos"))]
    {
        use winit::platform::macos::EventLoopBuilderExtMacOS;
        builder.with_default_menu(false);
    }
    let event_loop = builder.build()?;
    let proxy = event_loop.create_proxy();
    // The render thread starts with the window; until then its messages
    // wait in the channel.
    let (render, render_receiver) = mpsc::channel();
    let link: ContentLink = Arc::new(Mutex::new(None));
    let net = start_net(Arc::clone(&link), proxy.clone())?;
    let routes = Routes {
        proxy: proxy.clone(),
        render: render.clone(),
        net: net.handle(),
    };
    let content = match ContentProcess::spawn(routes.clone(), FIRST_PROCESS, perf.is_some()) {
        Ok(content) => content,
        Err(err) => {
            net.shutdown();
            return Err(err);
        }
    };
    if let Ok(mut link) = link.lock() {
        *link = Some(content.sender.clone());
    }
    let mut app = App {
        #[cfg(feature = "chrome")]
        chrome: ChromeHost::new(&config)?,
        proxy,
        routes,
        render,
        render_receiver: Some(render_receiver),
        start: start_page(config.start_url.as_deref()),
        history: SessionHistory::new(),
        document_url: None,
        config,
        perf: perf.map(|args| perf::PerfState {
            started: Instant::now(),
            args,
        }),
        perf_scroll_report: None,
        memory_report,
        browse_step: 0,
        window: None,
        content: Some(content),
        process: FIRST_PROCESS,
        link,
        net: Some(net),
        pending: Some(Load::Start),
        navigation: NavigationId(0),
        shown: None,
        hold: PaintHold::default(),
        crashes: CrashLog::default(),
        modifiers: ModifiersState::empty(),
        cursor: None,
        pointer_in_page: false,
        pressed_in_page: false,
        page_cursor: CursorKind::Default,
        latest_bars: Vec::new(),
        bar_pressed: false,
        viewport_sent: None,
        throttle: ScriptThrottle::default(),
        #[cfg(feature = "chrome")]
        copies: Vec::new(),
        #[cfg(feature = "chrome")]
        next_copy: 0,
        #[cfg(feature = "chrome")]
        clock: Instant::now(),
        failure: None,
    };
    let result = event_loop.run_app(&mut app);
    // `exiting` normally stops the network and the content process. This
    // covers an event loop that failed before it reached it.
    app.stop();
    result?;
    app.failure.map_or(Ok(()), Err)
}

/// The window and the render thread drawing into it, started together
/// once the OS allows it.
struct AppWindow {
    window: Arc<Window>,
    /// Joined in `stop`, before the window goes away.
    render: Option<JoinHandle<()>>,
}

struct App {
    config: Config,
    /// Live perf scenario, when `--perf` started one (spec 0007).
    perf: Option<perf::PerfState>,
    /// The scroll scenario's report, held while the content process is
    /// asked for its `nav_to_load` (spec 0007, Scenarios).
    perf_scroll_report: Option<perf::RunReport>,
    /// The `--memory-report` breakdown, gathering both processes' counters
    /// (spec 0032, AC-10). `None` without the flag.
    memory_report: Option<memory_report::State>,
    /// How far the `browse` scenario got: the article, the start page, back
    /// (spec 0032, AC-11). Only read while that scenario runs.
    browse_step: u8,
    /// The browser chrome around the page (spec 0003).
    #[cfg(feature = "chrome")]
    chrome: ChromeHost,
    /// Handed to the chrome's OS hooks once the window exists.
    proxy: EventLoopProxy<UserEvent>,
    /// Handed to each content process's router.
    routes: Routes,
    /// To the render thread.
    render: Sender<RenderMsg>,
    /// Taken when the window opens and the render thread starts.
    render_receiver: Option<Receiver<RenderMsg>>,
    window: Option<AppWindow>,
    /// Taken and stopped in `exiting`; `None` while crashed and not
    /// relaunched (AC-14).
    content: Option<ContentProcess>,
    /// The launch number of the content process events are taken from.
    process: u32,
    /// Where the network streams to: the live content process, if any.
    link: ContentLink,
    /// The network thread; taken and stopped in `exiting`.
    net: Option<Net>,
    /// What the command line asked to open first.
    start: StartPage,
    /// The page's session history and load state (spec 0008). Only this
    /// thread changes it.
    history: SessionHistory,
    /// The URL `net` committed the current document at, which page
    /// started loads are checked against (spec 0008, AC-18).
    document_url: Option<Url>,
    /// What to load once the page exists in the content process.
    pending: Option<Load>,
    /// The page's latest navigation, pending or committed (spec 0004,
    /// AC-11).
    navigation: NavigationId,
    /// The navigation whose list the render thread has on screen, if any.
    shown: Option<NavigationId>,
    /// Keeps the old frame up while a new navigation loads (AC-12).
    hold: PaintHold,
    /// Recent content process crashes (AC-14).
    crashes: CrashLog,
    /// The keyboard modifiers held, for the shortcuts (AC-13).
    modifiers: ModifiersState,
    /// The pointer position in physical pixels, while it is over the window.
    cursor: Option<PhysicalPosition<f64>>,
    /// The page was last told the pointer is over it (spec 0006, AC-12).
    pointer_in_page: bool,
    /// The primary button went down over the page and is still down, so
    /// its release goes to the page wherever it happens (AC-12).
    pressed_in_page: bool,
    /// The pointer shape the page asked for, shown while the pointer is
    /// over it (AC-18).
    page_cursor: CursorKind,
    /// The latest bar geometry reported by the render thread, so the main
    /// thread can hit test presses against bars before forwarding them to
    /// the page (spec 0009, child 0002, AC-8).
    latest_bars: Vec<scrolling::BarHit>,
    /// A bar thumb press is in progress, so the matching release goes to
    /// the render thread, not the page.
    bar_pressed: bool,
    /// The last viewport and scale sent to the page, to skip repeats.
    viewport_sent: Option<(SizeF, f32)>,
    /// The page's recent script started loads (spec 0008, AC-28).
    throttle: ScriptThrottle,
    /// The `Copy` and `Cut` requests sent to the page and not answered,
    /// with when they went out (spec 0008, Key invariants).
    #[cfg(feature = "chrome")]
    copies: Vec<(u32, Instant)>,
    /// The next copy request id.
    #[cfg(feature = "chrome")]
    next_copy: u32,
    /// The loading shimmer's time zero.
    #[cfg(feature = "chrome")]
    clock: Instant,
    /// The first error that stopped the event loop, reported after it returns.
    failure: Option<EmbedError>,
}

impl App {
    /// Opens the window and its surface here on the main thread (the OS
    /// wants that), then hands the renderer to a new render thread.
    fn open_window(
        &self,
        event_loop: &ActiveEventLoop,
        receiver: Receiver<RenderMsg>,
    ) -> Result<AppWindow, EmbedError> {
        let attributes = Window::default_attributes().with_title(&self.config.title);
        #[cfg(feature = "chrome")]
        let attributes = self.chrome.window_attributes(attributes);
        let attributes = if self.perf.is_some() {
            // Perf scenarios need a fixed, known viewport so runs compare
            // (spec 0007, Scenarios): 1280 by 800 logical px, always.
            attributes.with_inner_size(LogicalSize::new(
                perf::PERF_WINDOW_SIZE.0,
                perf::PERF_WINDOW_SIZE.1,
            ))
        } else {
            #[cfg(feature = "chrome")]
            let attributes = if let Some(size) = self.chrome.window_size() {
                // Open at the size the user last left it, not the OS default.
                attributes.with_inner_size(LogicalSize::new(
                    f64::from(size.width),
                    f64::from(size.height),
                ))
            } else {
                attributes
            };
            attributes
        };
        let window = Arc::new(event_loop.create_window(attributes)?);
        let size = window.inner_size();
        let renderer = Renderer::new(Arc::clone(&window), size.width, size.height)?;
        let scenario = self.perf.as_ref().map(|state| state.args.scenario);
        let render = render_thread::spawn(
            renderer,
            Arc::clone(&window),
            MAIN_PAGE,
            receiver,
            self.proxy.clone(),
            scenario,
            self.render.clone(),
        )?;
        info!(width = size.width, height = size.height, "window opened");
        Ok(AppWindow {
            window,
            render: Some(render),
        })
    }

    /// Tells the render thread. It runs until `stop`, so a failure means it
    /// already ended on an error the main thread hears about.
    fn render(&self, message: RenderMsg) {
        if self.render.send(message).is_err() {
            debug!("the render thread is gone");
        }
    }

    /// Tells the render thread the window's size and scale.
    fn send_window(&self) {
        let Some(app_window) = &self.window else {
            return;
        };
        let window = &app_window.window;
        let size = window.inner_size();
        self.render(RenderMsg::Window {
            width: size.width,
            height: size.height,
            scale: window.scale_factor() as f32,
        });
    }

    /// Sends the chrome document's new drawing, if it changed, and the
    /// page its new viewport if the card moved (spec 0003, milestone 3).
    /// The settings window draws too, when it is open.
    #[cfg(feature = "chrome")]
    fn pump_chrome(&mut self) {
        self.chrome.pump_settings();
        let Some(app_window) = &self.window else {
            return;
        };
        let Some((draw, moved)) = self.chrome.take_draw(&app_window.window) else {
            return;
        };
        self.render(RenderMsg::Chrome(Box::new(draw)));
        if moved {
            self.send_viewport();
        }
    }

    fn fail(&mut self, event_loop: &ActiveEventLoop, err: EmbedError) {
        error!(%err, "stopping");
        self.failure.get_or_insert(err);
        event_loop.exit();
    }

    /// A perf scenario finished on the render side: the first content frame
    /// (start) or the 240th scroll frame (scroll). Merge the marks and end
    /// the scenario the way it ends (spec 0007, Run report; Scenarios).
    fn perf_done(&mut self, event_loop: &ActiveEventLoop, marks: perf::RenderMarks) {
        let Some(state) = &self.perf else {
            // Marks without perf mode cannot happen; ignore them.
            return;
        };
        match state.args.scenario {
            // Cold start ends at the first content frame (AC-4).
            perf::Scenario::Start => {
                let report = perf::RunReport {
                    scenario: state.args.scenario.as_str(),
                    first_frame_unix_us: marks.first_frame_unix_us,
                    ..perf::RunReport::default()
                };
                self.perf_finish(event_loop, report);
            }
            // The scroll frames landed; the scenario also records
            // `nav_to_load`, which only the content process knows, so ask
            // it before writing (spec 0007, Scenarios).
            perf::Scenario::Scroll => {
                self.perf_scroll_report = Some(perf::RunReport {
                    scenario: state.args.scenario.as_str(),
                    scroll_frames_us: marks.scroll_frames_us,
                    ..perf::RunReport::default()
                });
                self.send(ToContent::PerfSample);
            }
            // Memory and load end from the content process's answer.
            perf::Scenario::Memory | perf::Scenario::Load | perf::Scenario::Browse => {}
        }
    }

    /// A perf scenario that waits for `load` takes its next step now
    /// (spec 0007, Scenarios).
    fn perf_after_load(&mut self) {
        let Some(state) = &self.perf else {
            return;
        };
        match state.args.scenario {
            // The footprints are sampled 2 s after `load` (AC-5).
            perf::Scenario::Memory => {
                self.schedule_wake(perf::MEMORY_SAMPLE_DELAY, UserEvent::PerfSampleNow);
            }
            // The scroll driver starts 500 ms after `load` (AC-6).
            perf::Scenario::Scroll => {
                self.schedule_wake(perf::SCROLL_START_DELAY, UserEvent::PerfScrollStart);
            }
            // The load scenario ends at `load`: ask for the marks now.
            perf::Scenario::Load => self.send(ToContent::PerfSample),
            // Browse walks the session, then samples like `memory` (spec
            // 0032, AC-11).
            perf::Scenario::Browse => self.browse_next(),
            perf::Scenario::Start => {}
        }
    }

    /// One step of the `browse` scenario (spec 0032, AC-11): load the
    /// start page after the article, come back to the article, then wait 2 s
    /// and sample. One window holds one page, so this is the shortest
    /// session that replaces a document and returns to it.
    fn browse_next(&mut self) {
        match self.browse_step {
            0 => {
                self.browse_step = 1;
                if let StartPage::Url(url) = start_page(None) {
                    self.load(Load::Url {
                        url,
                        origin: Origin::Browser,
                        action: Action::Push,
                    });
                }
            }
            1 => {
                self.browse_step = 2;
                self.go(-1, Origin::Browser);
            }
            _ => self.schedule_wake(perf::MEMORY_SAMPLE_DELAY, UserEvent::PerfSampleNow),
        }
    }

    /// One shot wake after `delay`: a small thread sleeps, then sends
    /// `event` to the event loop (spec 0007, Scenarios; spec 0032, AC-10).
    /// One scenario and one report run at a time, so one thread per wake is
    /// all it takes.
    fn schedule_wake(&self, delay: Duration, event: UserEvent) {
        let proxy = self.proxy.clone();
        let awake = thread::Builder::new().name("wake".into()).spawn(move || {
            thread::sleep(delay);
            let _ = proxy.send_event(event);
        });
        if let Err(err) = awake {
            warn!(%err, "could not start the wake thread");
        }
    }

    /// The content process answered `PerfSample`: finish the scenario from
    /// its marks (spec 0007, Run report).
    fn perf_content_marks(
        &mut self,
        event_loop: &ActiveEventLoop,
        nav_to_load_us: Option<u64>,
        script_us: u64,
        footprint_content_bytes: Option<u64>,
    ) {
        let Some(state) = &self.perf else {
            return;
        };
        match state.args.scenario {
            // Memory and browse end here: the browser adds its own footprint
            // (AC-5; spec 0032, AC-11).
            perf::Scenario::Memory | perf::Scenario::Browse => {
                let report = perf::RunReport {
                    scenario: state.args.scenario.as_str(),
                    nav_to_load_us,
                    script_us: Some(script_us),
                    footprint_browser_bytes: perf::footprint_bytes(),
                    footprint_content_bytes,
                    ..perf::RunReport::default()
                };
                self.perf_finish(event_loop, report);
            }
            // The load scenario ends here: navigation to `load` and the
            // script time (AC-7).
            perf::Scenario::Load => {
                let report = perf::RunReport {
                    scenario: state.args.scenario.as_str(),
                    nav_to_load_us,
                    script_us: Some(script_us),
                    ..perf::RunReport::default()
                };
                self.perf_finish(event_loop, report);
            }
            // Scroll waited for `nav_to_load` after the frames landed.
            perf::Scenario::Scroll => {
                let Some(mut report) = self.perf_scroll_report.take() else {
                    return;
                };
                report.nav_to_load_us = nav_to_load_us;
                self.perf_finish(event_loop, report);
            }
            perf::Scenario::Start => {}
        }
    }

    /// Writes the run report and ends the browser (spec 0007, Scenarios:
    /// the browser exits by itself when the scenario ends).
    fn perf_finish(&mut self, event_loop: &ActiveEventLoop, report: perf::RunReport) {
        let Some(state) = self.perf.take() else {
            return;
        };
        match report.write(&state.args.report) {
            Ok(()) => {
                let duration = state.started.elapsed();
                info!(
                    scenario = state.args.scenario.as_str(),
                    run_duration_ms = duration.as_millis() as u64,
                    "perf scenario done"
                );
                event_loop.exit();
            }
            Err(err) => self.fail(event_loop, EmbedError::PerfReport(err)),
        }
    }

    /// Sends a message to the content process. A failure is logged, not
    /// fatal: the window keeps showing the last good frame (spec 0002).
    fn send(&self, message: ToContent) {
        let Some(content) = &self.content else {
            return;
        };
        if let Err(err) = content.sender.send(message) {
            warn!(%err, "could not reach the content process");
        }
    }

    /// The page's viewport in CSS px, and its device pixels per CSS px:
    /// the page card with the chrome, the whole window without it.
    fn viewport(&self, window: &Window) -> (SizeF, f32) {
        let scale = window.scale_factor();
        #[cfg(feature = "chrome")]
        let viewport = self.chrome.page_viewport(window);
        #[cfg(not(feature = "chrome"))]
        let viewport = {
            let size = window.inner_size().to_logical::<f32>(scale);
            SizeF {
                width: size.width,
                height: size.height,
            }
        };
        (viewport, scale as f32)
    }

    /// The OS "reduce motion" setting, read from the platform (spec 0010,
    /// AC-13). macOS asks `NSWorkspace`, Windows asks
    /// `SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION)` (inverted), and
    /// other platforms default to `false` (`no-preference`, the value the
    /// spec allows when the setting can't be read).
    fn reduced_motion() -> bool {
        Self::os_reduced_motion()
    }

    /// Whether to reduce motion given whether the OS animates client areas.
    /// Windows reports "animations on"; the setting inverts it (spec 0010,
    /// AC-13).
    #[allow(
        dead_code,
        reason = "used by the OS reader on Windows and by its unit test"
    )]
    fn animations_on_to_reduce_motion(animations_on: bool) -> bool {
        !animations_on
    }

    #[cfg(target_os = "macos")]
    fn os_reduced_motion() -> bool {
        use objc2_app_kit::NSWorkspace;
        NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
    }

    #[cfg(windows)]
    fn os_reduced_motion() -> bool {
        use windows_sys::Win32::System::SystemParametersInfo::{
            SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW,
        };
        let mut enabled = 1i32;
        // SAFETY: `SPI_GETCLIENTAREAANIMATION` writes one `BOOL` into
        // `enabled`.
        let ok = unsafe {
            SystemParametersInfoW(
                SPI_GETCLIENTAREAANIMATION,
                0,
                (&raw mut enabled).cast::<c_void>(),
                0,
            )
        };
        if ok == 0 {
            warn!("failed to read the OS reduce-motion setting; defaulting to no-preference");
            return false;
        }
        // Animations on means reduce motion is off.
        animations_on_to_reduce_motion(enabled != 0)
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    fn os_reduced_motion() -> bool {
        false
    }

    /// Tells the page its viewport when it changed since the last time.
    fn send_viewport(&mut self) {
        let Some(app_window) = &self.window else {
            return;
        };
        let (viewport, scale) = self.viewport(&app_window.window);
        let message = match self.viewport_sent {
            Some(sent) if sent == (viewport, scale) => return,
            Some(_) => ToContent::Resize {
                page: MAIN_PAGE,
                viewport,
                scale,
            },
            None => ToContent::CreatePage {
                page: MAIN_PAGE,
                viewport,
                scale,
            },
        };
        let created = self.viewport_sent.is_none();
        self.viewport_sent = Some((viewport, scale));
        self.send(message);
        if created {
            // Tell the page the OS color scheme and reduced motion setting
            // right after the page exists, so `@media (prefers-color-scheme)`
            // and `@media (prefers-reduced-motion)` evaluate from the first
            // frame (spec 0010, AC-13).
            let color_scheme = match app_window.window.theme() {
                Some(Theme::Dark) => ColorScheme::Dark,
                _ => ColorScheme::Light,
            };
            self.send(ToContent::Preferences {
                color_scheme,
                reduced_motion: Self::reduced_motion(),
            });
            if let Some(load) = self.pending.take() {
                self.begin_load(load);
            }
        }
    }

    /// Starts a navigation of the page: a new `NavigationId` from the
    /// session history (spec 0008, Key invariants), paint holding for the
    /// old frame (AC-12), `Navigate` to the page, then the load in the
    /// network, which cancels the old navigation's fetches (AC-11).
    /// `Navigate` goes out before the network can commit anything, so the
    /// page always knows the navigation first (spec 0004, Navigation
    /// lifecycle).
    fn begin_load(&mut self, load: Load) {
        let Some(net) = &self.net else {
            return;
        };
        let net = net.handle();
        let leaving = self.history.current().map(|entry| entry.id);
        let (navigation, fetch) = match self.begin_history(load) {
            None => return,
            Some(Begun::Moved(leaving, history)) => {
                self.same_document_move(leaving, ScrollTarget::Fragment, history);
                return;
            }
            Some(Begun::Load(navigation, fetch)) => (navigation, fetch),
        };
        // A push or a traverse leaves the current entry: the render thread
        // says where it was scrolled, for coming back (AC-23).
        let leaves = self.history.pending().is_some_and(|pending| {
            matches!(pending.action, Action::Push | Action::Traverse { .. })
        });
        if leaves && let Some(entry) = leaving {
            self.render(RenderMsg::SaveScroll { entry });
        }
        let params = self
            .history
            .pending()
            .map(|pending| pending.params)
            .unwrap_or_default();
        self.navigation = navigation;
        let page = MAIN_PAGE;
        // Before `Navigate`, so the render thread knows the navigation
        // before any list of it can reach it.
        self.render(RenderMsg::NavigationStarted { navigation });
        self.hold.start(Instant::now(), self.shown.is_some());
        self.send(ToContent::Navigate { page, navigation });
        match fetch {
            Fetch::Url(url) => {
                info!(%url, "opening");
                net.navigate(page, navigation, url, params);
            }
            Fetch::Error(reason, input) => {
                net.navigate_to_error(page, navigation, reason, input, params);
            }
        }
        self.show_navigation();
    }

    /// Begins `load` in the session history: its navigation, and what the
    /// network should fetch. `None` when the history refuses it (a
    /// traverse target that is gone).
    fn begin_history(&mut self, load: Load) -> Option<Begun> {
        let begin = |history: &mut SessionHistory, url: Url, origin, action| match history
            .begin(url, origin, action)
        {
            Begin::Load(navigation) => Some(Ok(navigation)),
            Begin::SameDocument { leaving, history } => Some(Err(Begun::Moved(leaving, history))),
            Begin::Refused => {
                debug!(?action, "the history entry is gone; nothing loads");
                None
            }
        };
        // Only a URL load can land in the same document; the others name
        // no fragment of it.
        let load_of = |begun: Result<NavigationId, Begun>, fetch: Fetch| match begun {
            Ok(navigation) => Begun::Load(navigation, fetch),
            Err(moved) => moved,
        };
        match load {
            Load::Start => match &self.start {
                StartPage::Url(url) => {
                    let begun = begin(
                        &mut self.history,
                        url.clone(),
                        Origin::Browser,
                        Action::Push,
                    )?;
                    Some(load_of(begun, Fetch::Url(url.clone())))
                }
                StartPage::Invalid(input) => Some(Begun::Load(
                    self.history.begin_detached(),
                    Fetch::Error(FetchError::InvalidUrl, input.clone()),
                )),
            },
            Load::Url {
                url,
                origin,
                action,
            } => {
                let begun = begin(&mut self.history, url.clone(), origin, action)?;
                Some(load_of(begun, Fetch::Url(url)))
            }
            // The crash page stands in for the current entry, whose URL a
            // reload tries again (AC-34).
            Load::CrashPage => {
                let current = self.history.current().map(|entry| entry.url.clone());
                match current {
                    Some(url) => {
                        let input = url.to_string();
                        let begun = begin(&mut self.history, url, Origin::Browser, Action::Reload)?;
                        Some(load_of(
                            begun,
                            Fetch::Error(FetchError::ContentCrashed, input),
                        ))
                    }
                    None => Some(Begun::Load(
                        self.history.begin_detached(),
                        Fetch::Error(FetchError::ContentCrashed, self.start.input()),
                    )),
                }
            }
        }
    }

    /// Loads `load` now, or once the page exists, or in a relaunched
    /// content process when the last one is gone for good (AC-14).
    fn load(&mut self, load: Load) {
        if self.content.is_none() {
            self.relaunch(load);
        } else if self.viewport_sent.is_none() {
            self.pending = Some(load);
        } else {
            self.begin_load(load);
        }
    }

    /// Moves `delta` entries through the session history (spec 0008,
    /// AC-21): a traverse loads the entry's URL again; 0 reloads; out of
    /// range does nothing.
    fn go(&mut self, delta: i32, origin: Origin) {
        match self.history.go(delta) {
            Some(Action::Reload) => self.reload(origin),
            Some(Action::Traverse { to }) if self.content.is_some() => {
                // Inside one document it only scrolls (AC-21, AC-23).
                if let Some(moved) = self.history.traverse_within(to) {
                    let scroll = moved
                        .scroll
                        .map_or(ScrollTarget::Fragment, |(x, y)| ScrollTarget::Offset(x, y));
                    info!(delta, "history traverse in the same document");
                    self.same_document_move(moved.leaving, scroll, moved.history);
                    return;
                }
                self.traverse(delta, to, origin);
            }
            Some(Action::Traverse { to }) => self.traverse(delta, to, origin),
            _ => debug!(delta, "no history entry that way"),
        }
    }

    /// Loads entry `to` again (AC-21).
    fn traverse(&mut self, delta: i32, to: EntryId, origin: Origin) {
        let Some(url) = self.history.entry(to).map(|entry| entry.url.clone()) else {
            return;
        };
        info!(delta, "history traverse");
        self.load(Load::Url {
            url,
            origin,
            action: Action::Traverse { to },
        });
    }

    /// A move inside the document on screen (spec 0008, AC-20, AC-21):
    /// the render thread saves where `leaving` was scrolled, the page takes
    /// the current entry's URL and scrolls to `scroll`, and the address
    /// shows the new URL. No navigation starts, so nothing loading stops.
    fn same_document_move(&mut self, leaving: EntryId, scroll: ScrollTarget, history: HistoryInfo) {
        self.render(RenderMsg::SaveScroll { entry: leaving });
        let (Some(navigation), Some(url)) = (
            self.history.committed(),
            self.history.current().map(|entry| entry.url.clone()),
        ) else {
            return;
        };
        self.send(ToContent::SameDocument {
            page: MAIN_PAGE,
            navigation,
            url: url.to_string(),
            scroll,
            history,
        });
        self.document_url = Some(url);
        self.show_navigation();
    }

    /// The network committed `navigation`'s document at `url` (spec 0008,
    /// AC-22): the history records it, and the chrome shows it.
    fn committed(&mut self, navigation: NavigationId, url: &str, kind: &DocumentKind) {
        let Ok(url) = Url::parse(url) else {
            warn!(url, "commit with a URL that does not parse ignored");
            return;
        };
        let kind = match kind {
            DocumentKind::Error { reason, .. } => EntryKind::Error(reason.clone()),
            DocumentKind::Document | DocumentKind::PlainText => EntryKind::Document,
        };
        if self.history.commit(navigation, url.clone(), kind).is_some() {
            self.document_url = Some(url);
            self.show_navigation();
        }
    }

    /// The page asks to load `url` (spec 0008, AC-17, AC-27). Only the
    /// document on screen may ask, only within the page policy (AC-18)
    /// checked against the URL `net` recorded for it, never the page's
    /// claim.
    fn page_navigate(
        &mut self,
        navigation: NavigationId,
        url: &str,
        origin: PageNavigationOrigin,
        action: PageNavigationAction,
    ) {
        if self.history.committed() != Some(navigation) {
            debug!(
                ?navigation,
                "navigation request from an old document dropped"
            );
            return;
        }
        let Some(document) = &self.document_url else {
            return;
        };
        let target = navigation::parse_page_url(url)
            .and_then(|target| navigation::page_may_load(document, &target).map(|()| target));
        let target = match target {
            Ok(target) => target,
            Err(err) => {
                warn!(%err, "page navigation refused");
                return;
            }
        };
        // Script loads past the limit are dropped (AC-28); user actions
        // and links never count.
        if origin == PageNavigationOrigin::Script && !self.script_load_allowed() {
            return;
        }
        let origin = Origin::Page(origin);
        match action {
            PageNavigationAction::Reload => self.reload(origin),
            PageNavigationAction::Push => self.load(Load::Url {
                url: target,
                origin,
                action: Action::Push,
            }),
            PageNavigationAction::Replace => self.load(Load::Url {
                url: target,
                origin,
                action: Action::Replace,
            }),
        }
    }

    /// Counts a script started load against the throttle (spec 0008,
    /// AC-28): false when it is over the limit, with one `warn` a window.
    fn script_load_allowed(&mut self) -> bool {
        match self.throttle.allow(Instant::now()) {
            Allow::Yes => true,
            Allow::No { warn } => {
                if warn {
                    warn!(
                        max = navigation::MAX_SCRIPT_LOADS,
                        window_secs = navigation::SCRIPT_WINDOW.as_secs(),
                        "too many script started loads; dropping them"
                    );
                }
                false
            }
        }
    }

    /// Shows the history's state in the chrome (spec 0008, AC-1, AC-3,
    /// AC-11, AC-25): back and forward enabled or not, the address with
    /// the committed URL, or the user's own pending one, and whether a
    /// load runs.
    fn show_navigation(&mut self) {
        #[cfg(feature = "chrome")]
        {
            let pending = self
                .history
                .pending()
                .filter(|pending| matches!(pending.origin, Origin::User(_)))
                .map(|pending| pending.url.clone());
            let view = ui::NavView {
                can_back: self.history.can_back(),
                can_forward: self.history.can_forward(),
                url: self.history.current().map(|entry| entry.url.clone()),
                pending,
                loading: self.history.loading(),
            };
            self.chrome.set_navigation(&view);
        }
    }

    /// Stop (spec 0008, AC-26). Before commit the load is dropped: the
    /// network forgets it, and the render thread takes the old document's
    /// lists again. After commit the page and the network end what is
    /// still loading, and loading ends at the `load` event.
    fn stop_load(&mut self) {
        let page = MAIN_PAGE;
        match self.history.stop() {
            Stopped::Nothing => return,
            Stopped::BeforeCommit { dropped, shown } => {
                info!("stopped before commit; the page stays");
                if let Some(net) = &self.net {
                    net.handle().stop(page, dropped);
                }
                if let Some(shown) = shown {
                    self.navigation = shown;
                    self.render(RenderMsg::NavigationStarted { navigation: shown });
                }
                self.hold.painted();
            }
            Stopped::AfterCommit(navigation) => {
                info!("stopped; keeping what arrived");
                self.send(ToContent::Stop { page, navigation });
                if let Some(net) = &self.net {
                    net.handle().stop(page, navigation);
                }
            }
        }
        self.show_navigation();
    }

    /// A platform edit key or Edit menu row (spec 0008, AC-15): the
    /// focused chrome field takes it, else the page.
    #[cfg(feature = "chrome")]
    fn edit(&mut self, event_loop: &ActiveEventLoop, edit: EditKey) {
        let outcome = self
            .window
            .as_ref()
            .and_then(|app_window| self.chrome.edit(&app_window.window, edit));
        match outcome {
            Some(outcome) => self.apply(event_loop, outcome),
            None => self.page_edit(edit),
        }
    }

    /// A platform edit key for the page (spec 0008, AC-15): select all
    /// goes as it is; paste carries the clipboard read right now; copy and
    /// cut carry a fresh request id the answer must name within a second.
    #[cfg(feature = "chrome")]
    fn page_edit(&mut self, edit: EditKey) {
        let event = match edit {
            EditKey::SelectAll => InputEvent::SelectAll,
            EditKey::Paste => match self.chrome.clipboard_text() {
                Some(text) => InputEvent::Paste { text },
                None => return,
            },
            EditKey::Copy | EditKey::Cut => {
                let request = self.next_copy;
                self.next_copy = self.next_copy.wrapping_add(1);
                let now = Instant::now();
                self.copies
                    .retain(|(_, at)| now.saturating_duration_since(*at) < COPY_ANSWER);
                self.copies.push((request, now));
                if edit == EditKey::Copy {
                    InputEvent::Copy { request }
                } else {
                    InputEvent::Cut { request }
                }
            }
        };
        self.send(ToContent::Input {
            page: MAIN_PAGE,
            event,
        });
    }

    /// The page answered a copy or cut: the clipboard takes the text only
    /// for a request this browser sent in the last second, each id once
    /// (spec 0008, Key invariants).
    #[cfg(feature = "chrome")]
    fn clipboard_write(&mut self, request: u32, text: &str) {
        let now = Instant::now();
        let outstanding = self
            .copies
            .iter()
            .position(|&(id, at)| id == request && now.saturating_duration_since(at) < COPY_ANSWER);
        match outstanding {
            Some(index) => {
                self.copies.swap_remove(index);
                self.chrome.set_clipboard(text);
            }
            None => warn!(request, "clipboard write without a matching copy dropped"),
        }
    }

    /// Shows the page's favicon as the window icon (AC-37). macOS has no
    /// window icons, so there it is decoded but unused.
    fn set_favicon(&self, width: u32, height: u32, rgba: &[u8]) {
        if cfg!(target_os = "macos") {
            debug!(width, height, "favicon decoded; macOS shows no window icon");
            return;
        }
        let Some(app_window) = &self.window else {
            return;
        };
        match winit::window::Icon::from_rgba(rgba.to_vec(), width, height) {
            Ok(icon) => app_window.window.set_window_icon(Some(icon)),
            Err(err) => warn!(%err, "favicon refused by the window"),
        }
    }

    /// Loads the current entry's URL again into the same entry (AC-13;
    /// spec 0008, AC-22), or the start page before anything committed.
    /// With the content process gone for good (AC-14), it tries a
    /// relaunch first.
    fn reload(&mut self, origin: Origin) {
        info!("reload");
        let load = match self.history.current() {
            Some(entry) => Load::Url {
                url: entry.url.clone(),
                origin,
                action: Action::Reload,
            },
            None => Load::Start,
        };
        self.load(load);
    }

    /// The content process exited or crashed: relaunch it and show the crash
    /// page, unless it crashed too often lately (AC-14).
    fn content_gone(&mut self) {
        if let Some(content) = self.content.take() {
            content.reap();
        }
        self.set_link(None);
        // Its pending load died with it; the entries stay (AC-34).
        self.history.crashed();
        if self.crashes.record(Instant::now()) {
            error!("the content process went away; relaunching it");
            self.relaunch(Load::CrashPage);
        } else {
            error!(
                crashes = lifecycle::MAX_CRASHES,
                window_secs = lifecycle::CRASH_WINDOW.as_secs(),
                "the content process keeps crashing; not relaunching until a reload"
            );
        }
    }

    /// Starts a new content process, creates the page in it, and loads
    /// `load` there. A failed start keeps the last frame; a reload tries
    /// again.
    fn relaunch(&mut self, load: Load) {
        let process = self.process.wrapping_add(1);
        let content = match ContentProcess::spawn(self.routes.clone(), process, self.perf.is_some())
        {
            Ok(content) => content,
            Err(err) => {
                error!(%err, "could not relaunch the content process");
                return;
            }
        };
        self.set_link(Some(content.sender.clone()));
        self.content = Some(content);
        self.process = process;
        self.viewport_sent = None;
        self.pending = Some(load);
        // Sends `CreatePage`, which starts the load. Without a window yet,
        // `resumed` does it.
        self.send_viewport();
    }

    /// Points the network's stream at `sender`, or nowhere.
    fn set_link(&self, sender: Option<ContentSender>) {
        match self.link.lock() {
            Ok(mut link) => *link = sender,
            Err(_) => warn!("the content link is poisoned"),
        }
    }

    /// Stops the render thread (while the window it draws into is still
    /// open), then the content process (AC-13), then the network. The
    /// network's stream is cut before, so loads still running go nowhere.
    fn stop(&mut self) {
        #[cfg(feature = "chrome")]
        self.chrome.close_settings();
        if let Some(render) = self.window.as_mut().and_then(|w| w.render.take()) {
            self.render(RenderMsg::Shutdown);
            if render.join().is_err() {
                error!("the render thread panicked");
            }
        }
        self.set_link(None);
        if let Some(content) = self.content.take() {
            content.shutdown();
        }
        if let Some(net) = self.net.take() {
            net.shutdown();
        }
    }

    /// The pointer position in page CSS px, while it is over the window.
    fn page_point(&self) -> Option<PointF> {
        let (Some(app_window), Some(cursor)) = (&self.window, self.cursor) else {
            return None;
        };
        let window = &app_window.window;
        let scale = window.scale_factor() as f32;
        // The page is drawn at `scale`, moved into the page card when the
        // chrome draws one, so undo both to get page CSS px.
        #[cfg(feature = "chrome")]
        let offset = self.chrome.page_origin();
        #[cfg(not(feature = "chrome"))]
        let offset = PointF::default();
        Some(PointF {
            x: (cursor.x as f32 - offset.x) / scale,
            y: (cursor.y as f32 - offset.y) / scale,
        })
    }

    /// Tells the page where the pointer is: a move while it is over the
    /// page, and one leave when it goes off the page or the window (spec
    /// 0006, Value sourcing, Pointer event).
    fn pointer_moved(&mut self) {
        let over_page = self.page_point().filter(|at| {
            self.window.as_ref().is_some_and(|app_window| {
                let window = &app_window.window;
                #[cfg(feature = "chrome")]
                if !self.chrome.pointer_on_page(window) {
                    return false;
                }
                let (viewport, _) = self.viewport(window);
                (0.0..viewport.width).contains(&at.x) && (0.0..viewport.height).contains(&at.y)
            })
        });
        let event = match over_page {
            Some(at) => InputEvent::PointerMove { x: at.x, y: at.y },
            None if self.pointer_in_page => InputEvent::PointerLeave,
            None => return,
        };
        let entered = over_page.is_some() != self.pointer_in_page;
        self.pointer_in_page = over_page.is_some();
        // The page's shape shows only over the page (spec 0006, AC-18).
        if entered && let Some(app_window) = &self.window {
            #[cfg(feature = "chrome")]
            let off_page = self.chrome.cursor();
            #[cfg(not(feature = "chrome"))]
            let off_page = CursorKind::Default;
            let cursor = if self.pointer_in_page {
                self.page_cursor
            } else {
                off_page
            };
            app_window.window.set_cursor(input::cursor_icon(cursor));
        }
        self.send(ToContent::Input {
            page: MAIN_PAGE,
            event,
        });
        // Tell the render thread where the pointer is, so it can show and
        // widen the bar under it (spec 0009, child 0002, AC-8).
        self.render(RenderMsg::Pointer { at: over_page });
    }

    /// A button press or release: to the page while the pointer is over
    /// it, and a primary release that began on the page wherever it lands
    /// (spec 0006, AC-12, AC-15).
    fn pointer_button(&mut self, state: ElementState, button: MouseButton) {
        let Some(button) = input::pointer_button(button) else {
            return;
        };
        let Some(at) = self.page_point() else {
            return;
        };
        let primary = button == ipc::PointerButton::Primary;
        match state {
            ElementState::Pressed if self.pointer_in_page => {
                self.pressed_in_page |= primary;
                // A press on a scroll bar is handled by the render thread
                // and never reaches the page (spec 0009, child 0002, AC-8).
                if primary && self.bar_hit(at) {
                    self.bar_pressed = true;
                    self.render(RenderMsg::BarPress { at });
                    return;
                }
                // Keyboard focus follows the press into the page.
                #[cfg(feature = "chrome")]
                self.chrome.page_pressed();
                self.send(ToContent::Input {
                    page: MAIN_PAGE,
                    event: InputEvent::PointerDown {
                        x: at.x,
                        y: at.y,
                        button,
                    },
                });
            }
            ElementState::Released if self.pointer_in_page || (primary && self.pressed_in_page) => {
                if primary {
                    self.pressed_in_page = false;
                }
                if self.bar_pressed {
                    self.bar_pressed = false;
                    self.render(RenderMsg::BarRelease);
                    return;
                }
                self.send(ToContent::Input {
                    page: MAIN_PAGE,
                    event: InputEvent::PointerUp {
                        x: at.x,
                        y: at.y,
                        button,
                    },
                });
            }
            _ => {}
        }
    }

    /// Whether `at` (page CSS px) lies on a scroll bar track, so the press
    // belongs to the render thread, not the page (spec 0009, child 0002,
    // AC-8).
    fn bar_hit(&self, at: PointF) -> bool {
        self.latest_bars.iter().any(|hit| {
            at.x >= hit.track.origin.x
                && at.x < hit.track.origin.x + hit.track.size.width
                && at.y >= hit.track.origin.y
                && at.y < hit.track.origin.y + hit.track.size.height
        })
    }

    /// A key that is no browser shortcut (spec 0008, AC-26, AC-29): Escape
    /// stops a load when nothing in the chrome wants it, an edit key goes
    /// to the focused chrome field or the page, then the chrome's own keys
    /// and focused controls, then the page.
    fn key(&mut self, event_loop: &ActiveEventLoop, event: &KeyEvent) {
        let _ = event_loop;
        let escape = event.state == ElementState::Pressed
            && !event.repeat
            && input::key(&event.logical_key) == Some(ipc::Key::Escape);
        #[cfg(feature = "chrome")]
        let chrome_wants = self.chrome.wants_escape();
        #[cfg(not(feature = "chrome"))]
        let chrome_wants = false;
        if cfg!(feature = "chrome") && escape && !chrome_wants && self.history.loading() {
            self.stop_load();
            return;
        }
        #[cfg(feature = "chrome")]
        if let Some(edit) = input::edit_key(event, self.modifiers) {
            self.edit(event_loop, edit);
            return;
        }
        // The chrome's shortcuts and focused controls first.
        #[cfg(feature = "chrome")]
        if let Some(app_window) = &self.window
            && let Some(outcome) = self.chrome.key(&app_window.window, event)
        {
            self.apply(event_loop, outcome);
            return;
        }
        self.forward_key(event);
        // Key scrolling no longer happens here: the page gets the key first,
        // and answers with `ToBrowser::ScrollKey` for the key it did not use,
        // which `user_event` forwards to the render thread (spec 0009,
        // child 0002, AC-9).
    }

    /// Forwards a key the browser did not use to the page (spec 0006,
    /// Interface surface), then the text it types (spec 0008, AC-13).
    /// Modifiers alone stay here.
    fn forward_key(&self, event: &KeyEvent) {
        let Some(key) = input::key(&event.logical_key) else {
            return;
        };
        self.send(ToContent::Input {
            page: MAIN_PAGE,
            event: InputEvent::Key {
                key,
                modifiers: input::modifiers(self.modifiers),
                pressed: event.state == ElementState::Pressed,
                repeat: event.repeat,
            },
        });
        if let Some(text) = input::typed_text(event, self.modifiers) {
            self.send(ToContent::Input {
                page: MAIN_PAGE,
                event: InputEvent::Text { text },
            });
        }
    }

    /// Scrolls whatever is under the pointer, on the render thread, which
    /// tells the page where it ended up (spec 0002, Scrolling).
    fn wheel(&mut self, delta: MouseScrollDelta, phase: TouchPhase) {
        #[cfg(feature = "chrome")]
        if let Some(app_window) = &self.window
            && self.chrome.wheel(&app_window.window, delta)
        {
            return;
        }
        let (Some(app_window), Some(at)) = (&self.window, self.page_point()) else {
            return;
        };
        let scale = app_window.window.scale_factor() as f32;
        // Wheel deltas point the way the content moves; offsets grow the
        // other way.
        let (dx, dy) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (-x * scrolling::LINE, -y * scrolling::LINE),
            MouseScrollDelta::PixelDelta(p) => (-p.x as f32 / scale, -p.y as f32 / scale),
        };
        let phase = match phase {
            TouchPhase::Started => scrolling::GesturePhase::Start,
            TouchPhase::Moved => scrolling::GesturePhase::Move,
            TouchPhase::Ended => scrolling::GesturePhase::End,
            TouchPhase::Cancelled => scrolling::GesturePhase::None,
        };
        self.render(RenderMsg::Scroll { at, dx, dy, phase });
    }

    /// Carries out what the chrome asked for.
    #[cfg(feature = "chrome")]
    fn apply(&mut self, event_loop: &ActiveEventLoop, outcome: Outcome) {
        let Some(app_window) = &self.window else {
            return;
        };
        let window = &app_window.window;
        match outcome {
            Outcome::None => {}
            Outcome::Redraw => {
                self.send_window();
                self.pump_chrome();
                self.send_viewport();
            }
            Outcome::FocusPage(direction) => self.send(ToContent::Input {
                page: MAIN_PAGE,
                event: InputEvent::FocusEnter { direction },
            }),
            Outcome::Window(ui::WindowAction::Minimize) => window.set_minimized(true),
            Outcome::Window(ui::WindowAction::Maximize) => {
                window.set_maximized(!window.is_maximized());
            }
            Outcome::Window(ui::WindowAction::Close) => {
                info!("window closed");
                event_loop.exit();
            }
            Outcome::OpenSettings => self.chrome.open_settings(event_loop),
            Outcome::Back => self.go(-1, Origin::User(UserInput::Toolbar)),
            Outcome::Forward => self.go(1, Origin::User(UserInput::Toolbar)),
            Outcome::Reload => self.reload(Origin::User(UserInput::Toolbar)),
            Outcome::Edit(edit) => self.edit(event_loop, edit),
            Outcome::Stop => self.stop_load(),
            Outcome::Load(url) => {
                info!(%url, "typed load");
                self.load(Load::Url {
                    url,
                    origin: Origin::User(UserInput::Typed),
                    action: Action::Push,
                });
            }
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let Some(receiver) = self.render_receiver.take() else {
            return;
        };
        match self.open_window(event_loop, receiver) {
            Ok(window) => {
                #[cfg(feature = "chrome")]
                self.chrome.attach(&window.window, &self.proxy);
                self.window = Some(window);
                self.send_window();
                // The first chrome frame lays out the page card, so the
                // page is created at its size.
                #[cfg(feature = "chrome")]
                self.pump_chrome();
                self.send_viewport();
                // A new window on the start page opens the palette, ready
                // to type (spec 0008, Edge cases).
                #[cfg(feature = "chrome")]
                if matches!(&self.start, StartPage::Url(url) if url.as_str() == "about:start") {
                    self.chrome.open_address();
                    self.pump_chrome();
                }
            }
            Err(err) => self.fail(event_loop, err),
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        let event = match event {
            UserEvent::Content { process, event } if process == self.process => event,
            UserEvent::Content { process, .. } => {
                debug!(process, "message from an old content process dropped");
                return;
            }
            UserEvent::Render(RenderEvent::Painted { navigation }) => {
                self.shown = Some(navigation);
                if navigation == self.navigation {
                    self.hold.painted();
                }
                return;
            }
            UserEvent::Render(RenderEvent::Bars { hits }) => {
                self.latest_bars = hits;
                return;
            }
            UserEvent::Render(RenderEvent::ScrollSaved { entry, offset }) => {
                self.history.save_scroll(entry, offset);
                return;
            }
            UserEvent::Render(RenderEvent::PerfDone(marks)) => {
                self.perf_done(event_loop, marks);
                return;
            }
            UserEvent::PerfSampleNow => {
                // 2 s after `load`: ask the content process for its marks
                // and footprint (spec 0007, AC-5).
                self.send(ToContent::PerfSample);
                return;
            }
            UserEvent::PerfScrollStart => {
                // 500 ms after `load`: the render thread starts feeding
                // wheel steps (spec 0007, AC-6).
                self.render(RenderMsg::PerfScrollStart);
                return;
            }
            UserEvent::MemoryReportNow => {
                // 2 s after `load`: both processes count what they hold
                // (spec 0032, AC-10).
                self.send(ToContent::ReportMemory);
                self.render(RenderMsg::ReportMemory);
                return;
            }
            UserEvent::Render(RenderEvent::Memory(counters)) => {
                if let Some(report) = &mut self.memory_report {
                    report.browser(counters);
                    report.write_if_ready();
                }
                return;
            }
            UserEvent::Render(RenderEvent::Failed { window, error }) => {
                // Only the browser window's failure stops the browser.
                #[cfg(feature = "chrome")]
                if self.chrome.drawing_failed(window) {
                    return;
                }
                #[cfg(not(feature = "chrome"))]
                let _ = window;
                self.fail(event_loop, error.into());
                return;
            }
            UserEvent::FilesChanged(page) => {
                if page == MAIN_PAGE {
                    info!("live reload");
                    self.reload(Origin::Browser);
                }
                return;
            }
            UserEvent::Committed {
                page,
                navigation,
                url,
                kind,
            } => {
                if page == MAIN_PAGE {
                    self.committed(navigation, &url, &kind);
                }
                return;
            }
            #[cfg(feature = "chrome")]
            UserEvent::Chrome(event) => {
                if let Some(app_window) = &self.window {
                    let outcome = self.chrome.chrome_event(&app_window.window, event);
                    self.apply(event_loop, outcome);
                }
                return;
            }
        };
        match event {
            FromContent::Message(ToBrowser::TitleChanged { page, title }) if page == MAIN_PAGE => {
                debug!(title, "window title");
                self.history.title(&title);
                if let Some(app_window) = &self.window {
                    app_window.window.set_title(&title);
                }
            }
            FromContent::Message(ToBrowser::SetCursor { page, cursor }) if page == MAIN_PAGE => {
                self.page_cursor = cursor;
                if self.pointer_in_page
                    && let Some(app_window) = &self.window
                {
                    app_window.window.set_cursor(input::cursor_icon(cursor));
                }
            }
            FromContent::Message(ToBrowser::ScrollKey { page, key, target })
                if page == MAIN_PAGE =>
            {
                // The page did not use this scroll key: move the frame it
                // names, or the viewport, on the render thread (AC-9).
                self.render(RenderMsg::ScrollKey { key, target });
            }
            // Tab past the page's end goes into the chrome, or straight back
            // into the page from the other end when the chrome has nothing
            // to focus (spec 0003, AC-7; spec 0006, AC-13).
            FromContent::Message(ToBrowser::FocusLeft { page, direction }) if page == MAIN_PAGE => {
                #[cfg(feature = "chrome")]
                if self.chrome.focus_enter(direction) {
                    return;
                }
                self.send(ToContent::Input {
                    page,
                    event: InputEvent::FocusEnter { direction },
                });
            }
            FromContent::Message(ToBrowser::NavigateRequest {
                page,
                navigation,
                url,
                origin,
                action,
            }) if page == MAIN_PAGE => self.page_navigate(navigation, &url, origin, action),
            FromContent::Message(ToBrowser::HistoryGo {
                page,
                navigation,
                delta,
            }) if page == MAIN_PAGE => {
                // Counted like every script started load (AC-28).
                if self.history.committed() == Some(navigation) && self.script_load_allowed() {
                    self.go(delta, Origin::Page(PageNavigationOrigin::Script));
                }
            }
            FromContent::Message(ToBrowser::LoadComplete { page, navigation })
                if page == MAIN_PAGE =>
            {
                self.history.load_complete(navigation);
                self.show_navigation();
                // A perf scenario that waits for `load` takes its next step
                // now (spec 0007, Scenarios).
                self.perf_after_load();
                // The memory report is taken 2 s after `load` (spec 0032,
                // AC-10).
                if self.memory_report.is_some() {
                    self.schedule_wake(memory_report::REPORT_DELAY, UserEvent::MemoryReportNow);
                }
            }
            FromContent::Message(ToBrowser::PerfSample {
                nav_to_load_us,
                script_us,
                footprint_bytes,
            }) => {
                self.perf_content_marks(event_loop, nav_to_load_us, script_us, footprint_bytes);
            }
            FromContent::Message(ToBrowser::MemoryReport { counters }) => {
                if let Some(report) = &mut self.memory_report {
                    report.content(counters);
                    report.write_if_ready();
                }
            }
            FromContent::Message(ToBrowser::LinkHover { page, url }) if page == MAIN_PAGE => {
                #[cfg(feature = "chrome")]
                self.chrome.set_link_hover(url.as_deref());
                #[cfg(not(feature = "chrome"))]
                let _ = url;
            }
            FromContent::Message(ToBrowser::ClipboardWrite {
                page,
                request,
                text,
            }) if page == MAIN_PAGE => {
                #[cfg(feature = "chrome")]
                self.clipboard_write(request, &text);
                #[cfg(not(feature = "chrome"))]
                let _ = (request, text);
            }
            FromContent::Message(ToBrowser::FaviconChanged {
                page,
                width,
                height,
                rgba,
            }) if page == MAIN_PAGE => self.set_favicon(width, height, &rgba),
            // The router sends drawing to the render thread and fetches to
            // the network; nothing else is expected.
            FromContent::Message(message) => warn!(
                ?message,
                "unexpected message from the content process ignored"
            ),
            FromContent::Gone => self.content_gone(),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        // The settings window's events are the chrome's alone; the page
        // never sees them (spec 0003, AC-12).
        #[cfg(feature = "chrome")]
        if let Some(app_window) = &self.window
            && let Some(outcome) = self
                .chrome
                .settings_window_event(&app_window.window, id, &event)
        {
            self.apply(event_loop, outcome);
            return;
        }
        #[cfg(not(feature = "chrome"))]
        let _ = id;
        #[cfg(feature = "chrome")]
        if let Some(app_window) = &self.window {
            let outcome = self.chrome.window_event(&app_window.window, &event);
            self.apply(event_loop, outcome);
        }
        if self.window.is_none() {
            return;
        }
        match event {
            WindowEvent::CloseRequested => {
                info!("window closed");
                event_loop.exit();
            }
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                self.send_window();
                #[cfg(feature = "chrome")]
                self.pump_chrome();
                self.send_viewport();
            }
            WindowEvent::ThemeChanged(theme) => {
                // The OS color scheme changed: tell the page so
                // `@media (prefers-color-scheme)` restyles on the next frame
                // (spec 0010, AC-13). Re-read reduced motion too.
                let color_scheme = match theme {
                    Theme::Dark => ColorScheme::Dark,
                    Theme::Light => ColorScheme::Light,
                };
                self.send(ToContent::Preferences {
                    color_scheme,
                    reduced_motion: Self::reduced_motion(),
                });
            }
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::KeyboardInput { event, .. } => {
                let shortcut = Origin::User(UserInput::Shortcut);
                match lifecycle::shortcut(&event, self.modifiers) {
                    Some(Shortcut::Reload) => self.reload(shortcut),
                    Some(Shortcut::Back) => self.go(-1, shortcut),
                    Some(Shortcut::Forward) => self.go(1, shortcut),
                    // The shell has no browser shortcuts (spec 0008, AC-35).
                    Some(Shortcut::Stop) => {
                        #[cfg(feature = "chrome")]
                        self.stop_load();
                    }
                    Some(Shortcut::FocusAddress) => {
                        #[cfg(feature = "chrome")]
                        self.chrome.open_address();
                    }
                    // With two regions, F6 and Shift+F6 land in the same
                    // place; they stay apart for when tabs add a third
                    // (spec 0008, AC-31).
                    Some(Shortcut::Region { backward }) => {
                        #[cfg(feature = "chrome")]
                        if self.chrome.cycle_region() {
                            let direction = if backward {
                                ipc::FocusDirection::Backward
                            } else {
                                ipc::FocusDirection::Forward
                            };
                            self.send(ToContent::Input {
                                page: MAIN_PAGE,
                                event: InputEvent::FocusEnter { direction },
                            });
                        }
                        #[cfg(not(feature = "chrome"))]
                        let _ = backward;
                    }
                    Some(Shortcut::Quit) => {
                        info!("quit");
                        event_loop.exit();
                    }
                    None => self.key(event_loop, &event),
                }
            }
            // Buttons 4 and 5 go back and forward anywhere in the window
            // (spec 0008, AC-30).
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Back,
                ..
            } => self.go(-1, Origin::User(UserInput::MouseButton)),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Forward,
                ..
            } => self.go(1, Origin::User(UserInput::MouseButton)),
            WindowEvent::MouseInput { state, button, .. } => self.pointer_button(state, button),
            WindowEvent::Focused(focused) => {
                if !focused {
                    self.pressed_in_page = false;
                }
                self.send(ToContent::Input {
                    page: MAIN_PAGE,
                    event: InputEvent::WindowFocus { focused },
                });
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = Some(position);
                self.pointer_moved();
            }
            WindowEvent::CursorLeft { .. } => {
                self.cursor = None;
                self.pointer_moved();
            }
            WindowEvent::MouseWheel { delta, phase, .. } => self.wheel(delta, phase),
            WindowEvent::RedrawRequested => self.render(RenderMsg::Redraw),
            _ => {}
        }
    }

    /// Wakes the loop for the paint hold deadline (AC-12). The old frame
    /// stays on screen until the new navigation paints (or an error page
    /// paints); we do not clear to white on timeout.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        // While a load runs, the shimmer moves on a timer (spec 0008,
        // AC-25); idle, nothing wakes the loop.
        #[cfg(feature = "chrome")]
        let shimmer = (self.history.loading()
            && self
                .chrome
                .tick_loading(now.duration_since(self.clock).as_secs_f64() * 1000.0))
        .then(|| now + SHIMMER_TICK);
        #[cfg(not(feature = "chrome"))]
        let shimmer: Option<Instant> = None;
        // Hover and other chrome changes since the last turn draw once.
        #[cfg(feature = "chrome")]
        self.pump_chrome();
        // Clear the paint hold deadline so the event loop doesn't keep
        // waking up, but keep the old frame on screen.
        let _ = self.hold.expired(now);
        let wake = [self.hold.deadline(), shimmer].into_iter().flatten().min();
        event_loop.set_control_flow(match wake {
            Some(deadline) => ControlFlow::WaitUntil(deadline),
            None => ControlFlow::Wait,
        });
    }

    // The one hook every exit path reaches. On macOS, Quit (⌘Q) ends the
    // process right after this returns, so `run_app` never gets to return.
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        #[cfg(feature = "chrome")]
        if let Some(app_window) = &self.window
            && !app_window.window.is_maximized()
        {
            let size = app_window
                .window
                .inner_size()
                .to_logical::<f32>(app_window.window.scale_factor());
            self.chrome.remember_window_size(ui::SizeF {
                width: size.width,
                height: size.height,
            });
        }
        self.stop();
    }
}

/// Starts the network thread, streaming what it loads straight into the
/// content process's channel (spec 0004, child 0001), and telling the event
/// loop about each commit (spec 0008) and when a `file://` page's files
/// change (AC-38).
fn start_net(link: ContentLink, proxy: EventLoopProxy<UserEvent>) -> Result<Net, EmbedError> {
    let proxy = Arc::new(Mutex::new(proxy));
    let commits = Arc::clone(&proxy);
    let sink: ContentSink = Arc::new(move |message| {
        // The event loop hears of a commit before the page does, so the
        // history knows the document before anything the page says of it.
        if let ToContent::CommitNavigation {
            page,
            navigation,
            url,
            kind,
            ..
        } = &message
            && let Ok(proxy) = commits.lock()
        {
            let _ = proxy.send_event(UserEvent::Committed {
                page: *page,
                navigation: *navigation,
                url: url.clone(),
                kind: kind.clone(),
            });
        }
        let Ok(sender) = link.lock() else {
            return;
        };
        // No process (crashed, or shutting down): its loads are dropped.
        let Some(sender) = sender.as_ref() else {
            return;
        };
        if let Err(err) = sender.send(message) {
            warn!(%err, "could not stream to the content process");
        }
    });
    let config = NetConfig {
        user_agent: net::user_agent(env!("CARGO_PKG_VERSION")),
        // Only the OS trust store (spec 0004, AC-3).
        extra_roots: Vec::new(),
    };
    let files_changed: net::FilesChanged = Arc::new(move |page| {
        if let Ok(proxy) = proxy.lock() {
            let _ = proxy.send_event(UserEvent::FilesChanged(page));
        }
    });
    Ok(Net::start(config, sink, files_changed)?)
}

/// The running content process and the channel to it.
struct ContentProcess {
    child: Child,
    sender: ContentSender,
}

impl ContentProcess {
    /// Relaunches this executable as the content process, waits until it is
    /// ready, then routes everything it sends. In perf mode the content
    /// process is launched with `--perf` too (spec 0007, IPC messages).
    fn spawn(routes: Routes, process: u32, perf: bool) -> Result<Self, EmbedError> {
        let server = BootstrapServer::new()?;
        let exe = env::current_exe().map_err(EmbedError::SpawnContent)?;
        let mut command = Command::new(exe);
        command.arg(CONTENT_PROCESS_FLAG).arg(server.name());
        if perf {
            // The content process learns perf mode only from its launch
            // argument (spec 0007, Security model).
            command.arg("--perf");
        }
        let mut child = command.spawn().map_err(EmbedError::SpawnContent)?;

        let started = Self::handshake(server).and_then(|channel| {
            let (sender, receiver) = channel.split();
            // Before the router starts, so the render thread knows this
            // process before anything from it arrives.
            let _ = routes.render.send(RenderMsg::ContentStarted {
                process,
                sender: sender.clone(),
            });
            Self::route(receiver, routes, process)?;
            Ok(sender)
        });
        match started {
            Ok(sender) => Ok(Self { child, sender }),
            Err(err) => {
                // Don't leave a half started child behind.
                let _ = child.kill();
                let _ = child.wait();
                Err(err)
            }
        }
    }

    fn handshake(server: BootstrapServer) -> Result<ContentChannel, EmbedError> {
        let channel = server.accept(HANDSHAKE_TIMEOUT)?;
        // Nothing else may arrive before `Ready`: no page exists yet.
        match channel.recv_timeout(HANDSHAKE_TIMEOUT)? {
            ToBrowser::Ready { pid } => info!(content_pid = pid, "content process ready"),
            _ => return Err(EmbedError::UnexpectedHandshake),
        }
        Ok(channel)
    }

    /// The IPC router (spec 0004, Threads): waits for content messages on
    /// a thread of its own and sends drawing to the render thread, fetches
    /// to the network thread (which checks each against the page's
    /// committed document itself, AC-8, AC-9), and the rest to the event
    /// loop. The thread ends when the content process closes the channel
    /// or the event loop is gone.
    fn route(receiver: ContentReceiver, routes: Routes, process: u32) -> Result<(), EmbedError> {
        thread::Builder::new()
            .name("ipc-router".into())
            .spawn(move || {
                loop {
                    let Ok(message) = receiver.recv() else {
                        let _ = routes.render.send(RenderMsg::ContentGone { process });
                        let event = FromContent::Gone;
                        let _ = routes
                            .proxy
                            .send_event(UserEvent::Content { process, event });
                        return;
                    };
                    let delivered = match message {
                        ToBrowser::NeedsFrame { .. }
                        | ToBrowser::DisplayList { .. }
                        | ToBrowser::FrameSkipped { .. }
                        | ToBrowser::AddFont { .. }
                        | ToBrowser::AddImage { .. }
                        | ToBrowser::DeleteFont { .. }
                        | ToBrowser::DeleteImage { .. } => routes
                            .render
                            .send(RenderMsg::Content { process, message })
                            .is_ok(),
                        ToBrowser::Fetch {
                            page,
                            navigation,
                            fetch,
                            url,
                            destination,
                        } if page == MAIN_PAGE => {
                            routes.net.fetch(FetchRequest {
                                page,
                                navigation,
                                fetch,
                                url,
                                destination,
                            });
                            true
                        }
                        ToBrowser::CancelFetch {
                            page,
                            navigation,
                            fetch,
                        } if page == MAIN_PAGE => {
                            routes.net.cancel(page, navigation, fetch);
                            true
                        }
                        message => {
                            let event = FromContent::Message(message);
                            routes
                                .proxy
                                .send_event(UserEvent::Content { process, event })
                                .is_ok()
                        }
                    };
                    if !delivered {
                        return;
                    }
                }
            })
            .map_err(EmbedError::SpawnListener)?;
        Ok(())
    }

    /// Collects a content process that went away by itself, killing it in
    /// case only its channel died.
    fn reap(mut self) {
        let _ = self.child.kill();
        match self.child.wait() {
            Ok(status) => info!(%status, "content process gone"),
            Err(err) => warn!(%err, "could not collect the content process"),
        }
    }

    /// Asks the content process to exit, and kills it if it does not in time.
    fn shutdown(mut self) {
        if let Err(err) = self.sender.send(ToContent::Shutdown) {
            warn!(%err, "could not ask the content process to stop");
        }

        let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    info!(%status, "content process exited");
                    return;
                }
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                Ok(None) => break,
                Err(err) => {
                    warn!(%err, "could not check on the content process");
                    break;
                }
            }
        }

        warn!("content process did not exit in time, killing it");
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn animations_on_inverts_to_reduce_motion() {
        assert!(!App::animations_on_to_reduce_motion(true));
        assert!(App::animations_on_to_reduce_motion(false));
    }
}
