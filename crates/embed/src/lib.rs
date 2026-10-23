//! The public embedding API that both apps use.
//!
//! It owns process startup: the same executable runs as the browser process
//! (window, GPU, network) or, when relaunched with
//! `--content-process <server>`, as the content process (spec 0001, process model).
//!
//! With the `chrome` feature (the browser app), the window also gets the
//! browser chrome: settings, OS appearance, a frameless frame and native
//! menus (spec 0003).

// The chrome's OS hooks need `unsafe` (AppKit, Win32); only their modules
// allow it, each block with a `// SAFETY:` comment.
#![cfg_attr(not(feature = "chrome"), forbid(unsafe_code))]
#![cfg_attr(feature = "chrome", deny(unsafe_code))]

mod browser;
#[cfg(feature = "chrome")]
mod chrome_host;
mod content;
mod input;
mod lifecycle;
mod page_view;
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
}

/// Runs the app until its window closes, or until the browser tells this
/// process to stop when it is the content process.
pub fn run(config: Config) -> Result<(), EmbedError> {
    init_tracing();

    let mut args = env::args().skip(1);
    if args.next().as_deref() == Some(CONTENT_PROCESS_FLAG) {
        let server_name = args.next().ok_or(EmbedError::MissingServerName)?;
        return content::run(&server_name);
    }
    browser::run(config)
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    // A second init (e.g. from a test harness) is harmless, so its error is ignored.
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}
