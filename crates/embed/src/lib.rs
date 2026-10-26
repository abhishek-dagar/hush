//! The public embedding API that both apps use.
//!
//! It owns process startup: the same executable runs as the browser process
//! (window, GPU, network) or, when relaunched with
//! `--content-process <server>`, as the content process (spec 0001, process model).
//!
//! With the `chrome` feature (the browser app), the window also gets the
//! browser chrome: settings, OS appearance, a frameless frame and native
//! menus (spec 0003).

// The chrome's OS hooks need `unsafe` (AppKit, Win32), as does mapping a
// read only system font file; only their modules allow it, each block with a
// `// SAFETY:` comment.
#![deny(unsafe_code)]

mod browser;
#[cfg(feature = "chrome")]
mod chrome_host;
#[cfg(feature = "chrome")]
mod clipboard;
mod content;
mod fonts;
mod input;
mod lifecycle;
mod memory_report;
mod page_view;
mod perf;
mod render_thread;
mod scrolling;
mod start_url;

use std::env;

use tracing_subscriber::EnvFilter;

/// The flag the browser process passes when it relaunches itself as the content process.
const CONTENT_PROCESS_FLAG: &str = "--content-process";

/// How an app wants its window set up.
#[derive(Debug, Clone)]
pub struct Config {
    /// The window title, also the app's name in the macOS menu bar.
    pub title: String,
    /// The app's folder name in the OS config directory, where the browser
    /// keeps `settings.toml` (spec 0003). Renaming it later needs a move.
    pub app_id: String,
    /// What to open first, as the user typed it: a URL, a file path or a
    /// host name (spec 0004, AC-1). `None` opens `about:start`.
    pub start_url: Option<String>,
    /// Show the chrome's control gallery instead of the frame (spec 0003,
    /// `--chrome-gallery`). Dev builds only; ignored in release builds and
    /// without the `chrome` feature.
    pub chrome_gallery: bool,
}

/// Errors that end the app.
#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    /// The channel to the content process failed.
    #[error(transparent)]
    Ipc(#[from] ipc::IpcError),
    /// The browser chrome could not start (spec 0003).
    #[cfg(feature = "chrome")]
    #[error(transparent)]
    Chrome(#[from] ui::ChromeError),
    /// The renderer failed.
    #[error(transparent)]
    Render(#[from] render::RenderError),
    /// The network thread could not start.
    #[error(transparent)]
    Net(#[from] net::NetError),
    /// The OS event loop failed.
    #[error("the window system failed")]
    EventLoop(#[from] winit::error::EventLoopError),
    /// The OS refused to open the window.
    #[error("could not open the window")]
    CreateWindow(#[from] winit::error::OsError),
    /// Relaunching the executable as the content process failed.
    #[error("could not start the content process")]
    SpawnContent(#[source] std::io::Error),
    /// The content process sent something other than `Ready` first.
    #[error("the content process did not start with a ready message")]
    UnexpectedHandshake,
    /// The thread that routes the content process's messages could not start.
    #[error("could not start the IPC router thread")]
    SpawnListener(#[source] std::io::Error),
    /// The render thread could not start.
    #[error("could not start the render thread")]
    SpawnRender(#[source] std::io::Error),
    /// The content process flag came without a server name.
    #[error("{CONTENT_PROCESS_FLAG} needs a bootstrap server name")]
    MissingServerName,
    /// `--perf` named a scenario that does not exist (spec 0007, AC-11).
    #[error("--perf needs one of start, memory, scroll, load (got {0})")]
    PerfUnknownScenario(String),
    /// `--perf` came without `--perf-report <file>` (spec 0007, AC-11).
    #[error("--perf needs --perf-report <file> to name the run report")]
    PerfMissingReport,
    /// The perf run report could not be written (spec 0007, Run report).
    #[error("could not write the perf run report")]
    PerfReport(#[source] std::io::Error),
}

/// Runs the app until its window closes, or until the browser tells this
/// process to stop when it is the content process.
pub fn run(config: Config) -> Result<(), EmbedError> {
    init_tracing();

    let args: Vec<String> = env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some(CONTENT_PROCESS_FLAG) {
        let server_name = args.get(1).ok_or(EmbedError::MissingServerName)?;
        // The content process learns perf mode only from this launch
        // argument, set by the browser (spec 0007, Security model).
        let perf = perf::content_args(args.get(2..).unwrap_or(&[]));
        return content::run(server_name, perf);
    }
    // Perf flags are parsed before any window opens, so a bad flag is a
    // clear error, not a surprise (spec 0007, AC-11).
    let perf = perf::browser_args(&args)?;
    // The memory report is a launch flag too, never reachable from a page
    // (spec 0032, Security model).
    let memory_report = memory_report::path_arg(&args).map(memory_report::State::new);
    // The start scenario always measures the start page, whatever else the
    // command line said (spec 0007, Scenarios).
    let config = match &perf {
        Some(args) if args.scenario == perf::Scenario::Start => Config {
            start_url: None,
            ..config
        },
        _ => config,
    };
    browser::run(config, perf, memory_report)
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    // A second init (e.g. from a test harness) is harmless, so its error is ignored.
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}
