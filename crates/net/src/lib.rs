//! The network: every byte a page loads, fetched by the browser process
//! (spec 0004, child 0001).
//!
//! Runs only in the browser process, on its own thread with a current
//! thread `tokio` runtime. The content process never opens a socket or a
//! file; the browser decides what a navigation loads, commits it to the
//! page and streams the body straight to the content channel, so bytes
//! never pass through the main thread.
//!
//! Every policy question is decided here, before any byte reaches the
//! page: which schemes a document or subresource may use, which files a
//! `file://` page may read, mixed content, redirects, size and time limits.
//! Subresources are checked against the document the navigation actually
//! committed, never a URL the page claims.

#![forbid(unsafe_code)]

mod about;
mod body;
mod client;
mod fetch;
mod policy;

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::SystemTime;

use display_list::PageId;
use ipc::{CommitParams, Destination, FetchError, FetchId, NavigationId, ToContent};
use tokio::sync::mpsc;
use tokio::task::{JoinHandle as TaskHandle, LocalSet};
use tokio::time::MissedTickBehavior;
use tracing::{Instrument, debug, info, info_span, warn};
use url::Url;

pub use about::reason_sentence;

use crate::client::Client;
use crate::fetch::Load;
use crate::policy::Document;

/// Limits on what the network may load (spec 0004, AC-4). Each is a named
/// constant so a failure can say which one ran out.
pub mod limits {
    use std::time::Duration;

    /// The TCP connect plus the TLS handshake.
    pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
    /// A request that receives no bytes (headers or body) for this long ends.
    pub const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
    /// Redirects followed; one more fails with `TooManyRedirects`.
    pub const MAX_REDIRECTS: usize = 20;
    /// The largest document body, counted after decoding.
    pub const MAX_DOCUMENT_BYTES: usize = 64 * 1024 * 1024;
    /// The largest subresource body, counted after decoding.
    pub const MAX_SUBRESOURCE_BYTES: usize = 32 * 1024 * 1024;
    /// The largest font body (spec 0006, AC-11).
    pub const MAX_FONT_BYTES: usize = 10 * 1024 * 1024;
    /// Fetches in flight to one host.
    pub const MAX_CONNECTIONS_PER_HOST: usize = 6;
    /// Fetches in flight in all.
    pub const MAX_CONNECTIONS: usize = 32;
    /// The most bytes one body chunk message carries.
    pub const CHUNK_COALESCE: usize = 64 * 1024;
    /// How often live reload looks at a `file://` page's files (AC-38).
    pub const LIVE_RELOAD_POLL: Duration = Duration::from_millis(500);
}

/// Where the network sends what it loads: the content process's channel.
/// Called from the network thread.
pub type ContentSink = Arc<dyn Fn(ToContent) + Send + Sync>;

/// Told when a file a `file://` page loaded changed, so the browser reloads
/// that page (spec 0004, child 0001, Live reload, AC-38). Called from the
/// network thread.
pub type FilesChanged = Arc<dyn Fn(PageId) + Send + Sync>;

/// The canonical paths of the files one navigation read, which live reload
/// watches. Shared by that navigation's tasks on the network thread.
pub(crate) type Served = Rc<RefCell<BTreeSet<PathBuf>>>;

/// How the network identifies itself and whom it trusts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetConfig {
    /// The `User-Agent` header (see [`user_agent`]).
    pub user_agent: String,
    /// Root certificates (DER) trusted on top of the OS trust store. Empty
    /// in the apps; the test certificates in tests (spec 0004,
    /// Configuration required).
    pub extra_roots: Vec<Vec<u8>>,
}

/// Errors starting the network thread.
#[derive(Debug, thiserror::Error)]
pub enum NetError {
    /// The runtime or its thread could not start.
    #[error("could not start the network thread")]
    Runtime(#[source] std::io::Error),
    /// The TLS settings or the OS trust store could not be set up.
    #[error("could not set up TLS: {0}")]
    Tls(String),
}

/// A page asks for a subresource (`ToBrowser::Fetch`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchRequest {
    /// The page that wants it.
    pub page: PageId,
    /// The navigation that wants it; a stale one is dropped.
    pub navigation: NavigationId,
    /// The page's id for it, never [`FetchId::DOCUMENT`].
    pub fetch: FetchId,
    /// The URL as the page resolved it. Untrusted.
    pub url: String,
    /// What it is for.
    pub destination: Destination,
}

/// The `User-Agent` for browser version `version` (spec 0004, Value sourcing).
pub fn user_agent(version: &str) -> String {
    let os = if cfg!(target_os = "macos") {
        "Macintosh; Intel Mac OS X 10_15_7"
    } else if cfg!(target_os = "windows") {
        "Windows NT 10.0; Win64; x64"
    } else {
        "X11; Linux x86_64"
    };
    format!("Mozilla/5.0 ({os}) RustBrowser/{version}")
}

/// What a navigation loads.
#[derive(Debug)]
enum Target {
    Url(Url),
    Error { reason: FetchError, input: String },
}

#[derive(Debug)]
enum Command {
    Navigate {
        page: PageId,
        navigation: NavigationId,
        target: Target,
        params: CommitParams,
    },
    Fetch(FetchRequest),
    Cancel {
        page: PageId,
        navigation: NavigationId,
        fetch: FetchId,
    },
    CancelNavigation {
        page: PageId,
    },
    Stop {
        page: PageId,
        navigation: NavigationId,
    },
    /// From a navigation's own task: its document is committed.
    Committed {
        page: PageId,
        navigation: NavigationId,
        document: Document,
    },
    Shutdown,
}

/// Talks to the network thread. Cheap to clone; every call returns at once.
#[derive(Clone)]
pub struct NetHandle {
    commands: mpsc::UnboundedSender<Command>,
}

impl NetHandle {
    /// Loads `url` as page `page`'s document for `navigation`, cancelling
    /// every fetch of the page's previous navigation (AC-11). The result
    /// reaches the page as `CommitNavigation` plus the body, or as the
    /// error page. `params` go into the commit untouched (spec 0008).
    pub fn navigate(&self, page: PageId, navigation: NavigationId, url: Url, params: CommitParams) {
        self.send(Command::Navigate {
            page,
            navigation,
            target: Target::Url(url),
            params,
        });
    }

    /// Shows the error page for `input`, which failed before any load could
    /// start (it is not a URL, for one).
    pub fn navigate_to_error(
        &self,
        page: PageId,
        navigation: NavigationId,
        reason: FetchError,
        input: String,
        params: CommitParams,
    ) {
        self.send(Command::Navigate {
            page,
            navigation,
            target: Target::Error { reason, input },
            params,
        });
    }

    /// Loads a subresource the page asked for, after checking it against
    /// the page's committed document (AC-8, AC-9). The answer reaches the
    /// page as `FetchResponse` plus the body, or `FetchFailed`.
    pub fn fetch(&self, request: FetchRequest) {
        self.send(Command::Fetch(request));
    }

    /// Stops one subresource fetch.
    pub fn cancel(&self, page: PageId, navigation: NavigationId, fetch: FetchId) {
        self.send(Command::Cancel {
            page,
            navigation,
            fetch,
        });
    }

    /// The user pressed Stop (spec 0008, AC-26). Before `navigation`
    /// committed, its load is dropped and nothing reaches the page. After,
    /// every load still running for it ends, and the page hears each as
    /// `FetchFailed` with [`FetchError::Cancelled`], so it keeps what
    /// arrived and fires `load`. A stale navigation changes nothing.
    pub fn stop(&self, page: PageId, navigation: NavigationId) {
        self.send(Command::Stop { page, navigation });
    }

    /// Stops everything the page is loading, for a page that closed.
    pub fn cancel_navigation(&self, page: PageId) {
        self.send(Command::CancelNavigation { page });
    }

    /// Asks the network thread to stop. Loads in progress are dropped.
    pub fn shutdown(&self) {
        self.send(Command::Shutdown);
    }

    fn send(&self, command: Command) {
        // Only fails once the thread is gone, when nothing is left to load.
        if self.commands.send(command).is_err() {
            debug!("network thread already stopped");
        }
    }
}

/// The network thread.
pub struct Net {
    handle: NetHandle,
    thread: JoinHandle<()>,
}

impl Net {
    /// Starts the network thread, which sends everything it loads to `sink`
    /// and tells `files_changed` when a `file://` page's files change.
    pub fn start(
        config: NetConfig,
        sink: ContentSink,
        files_changed: FilesChanged,
    ) -> Result<Self, NetError> {
        let tls = client::tls_config(config.extra_roots)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .map_err(NetError::Runtime)?;
        let (commands, receiver) = mpsc::unbounded_channel();
        let handle = NetHandle { commands };
        let loopback = handle.clone();
        let user_agent = config.user_agent;
        let thread = thread::Builder::new()
            .name("network".into())
            .spawn(move || {
                let _span = info_span!("network").entered();
                // Loads share the client, which is not `Send`, so they run
                // as local tasks on this one thread.
                let client = Client::new(tls, &user_agent);
                let task = run(receiver, loopback, client, sink, files_changed);
                LocalSet::new().block_on(&runtime, task);
            })
            .map_err(NetError::Runtime)?;
        Ok(Self { handle, thread })
    }

    /// A handle to send it work.
    pub fn handle(&self) -> NetHandle {
        self.handle.clone()
    }

    /// Stops the thread and waits for it.
    pub fn shutdown(self) {
        self.handle.shutdown();
        if self.thread.join().is_err() {
            debug!("network thread panicked");
        }
    }
}

/// What one page is loading: its current navigation, the document that
/// navigation committed, every task loading for it, and for a `file://`
/// page the files it read and the task watching them.
struct PageLoads {
    navigation: NavigationId,
    document: Option<Document>,
    tasks: HashMap<FetchId, TaskHandle<()>>,
    served: Served,
    watcher: Option<TaskHandle<()>>,
}

impl PageLoads {
    fn new(navigation: NavigationId) -> Self {
        Self {
            navigation,
            document: None,
            tasks: HashMap::new(),
            served: Served::default(),
            watcher: None,
        }
    }

    /// Stops every load and the watch; dropping a task drops its
    /// connection.
    fn cancel_all(&mut self) {
        for (_, task) in self.tasks.drain() {
            task.abort();
        }
        if let Some(watcher) = self.watcher.take() {
            watcher.abort();
        }
    }
}

/// Live reload (AC-38): every [`limits::LIVE_RELOAD_POLL`], reads the
/// modification time of every file the navigation served. A change, or a
/// file appearing or disappearing, tells the browser once and ends the
/// watch; the reload is a new navigation with a watch of its own.
async fn watch(served: Served, page: PageId, files_changed: FilesChanged) {
    let mut seen: HashMap<PathBuf, Option<SystemTime>> = HashMap::new();
    let mut ticks = tokio::time::interval(limits::LIVE_RELOAD_POLL);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        let paths: Vec<PathBuf> = served.borrow().iter().cloned().collect();
        for path in paths {
            let stamp = tokio::fs::metadata(&path)
                .await
                .ok()
                .and_then(|metadata| metadata.modified().ok());
            if let Some(before) = seen.insert(path, stamp)
                && before != stamp
            {
                info!("a file the page loaded changed; reloading");
                files_changed(page);
                return;
            }
        }
    }
}

/// The network thread's loop.
async fn run(
    mut commands: mpsc::UnboundedReceiver<Command>,
    loopback: NetHandle,
    client: Client,
    sink: ContentSink,
    files_changed: FilesChanged,
) {
    let mut pages: HashMap<PageId, PageLoads> = HashMap::new();
    while let Some(command) = commands.recv().await {
        match command {
            Command::Navigate {
                page,
                navigation,
                target,
                params,
            } => {
                let loads = pages
                    .entry(page)
                    .or_insert_with(|| PageLoads::new(navigation));
                loads.cancel_all();
                *loads = PageLoads::new(navigation);
                let load = Load {
                    page,
                    navigation,
                    fetch: FetchId::DOCUMENT,
                    sink: Arc::clone(&sink),
                };
                let span = match &target {
                    Target::Url(url) => {
                        info_span!("navigation", host = url.host_str().unwrap_or_default())
                    }
                    Target::Error { .. } => info_span!("navigation", host = "about:error"),
                };
                let loopback = loopback.clone();
                let committed = move |document| {
                    loopback.send(Command::Committed {
                        page,
                        navigation,
                        document,
                    });
                };
                let task = client.clone().navigate(load, target, params, committed);
                loads.tasks.insert(
                    FetchId::DOCUMENT,
                    tokio::task::spawn_local(task.instrument(span)),
                );
            }
            Command::Committed {
                page,
                navigation,
                document,
            } => {
                if let Some(loads) = pages.get_mut(&page)
                    && loads.navigation == navigation
                {
                    if let Some(file) = &document.file {
                        loads.served.borrow_mut().insert(file.clone());
                        let served = Rc::clone(&loads.served);
                        let watch = watch(served, page, Arc::clone(&files_changed));
                        loads.watcher = Some(tokio::task::spawn_local(watch));
                    }
                    loads.document = Some(document);
                }
            }
            Command::Fetch(request) => fetch(&mut pages, &client, &sink, request),
            Command::Cancel {
                page,
                navigation,
                fetch,
            } => {
                if let Some(loads) = pages.get_mut(&page)
                    && loads.navigation == navigation
                    && fetch != FetchId::DOCUMENT
                    && let Some(task) = loads.tasks.remove(&fetch)
                {
                    task.abort();
                }
            }
            Command::Stop { page, navigation } => {
                if let Some(loads) = pages.get_mut(&page)
                    && loads.navigation == navigation
                {
                    let committed = loads.document.is_some();
                    for (fetch, task) in loads.tasks.drain() {
                        if task.is_finished() {
                            continue;
                        }
                        task.abort();
                        if committed {
                            let load = Load {
                                page,
                                navigation,
                                fetch,
                                sink: Arc::clone(&sink),
                            };
                            load.failed(FetchError::Cancelled);
                        }
                    }
                    if !committed {
                        pages.remove(&page);
                    }
                }
            }
            Command::CancelNavigation { page } => {
                if let Some(mut loads) = pages.remove(&page) {
                    loads.cancel_all();
                }
            }
            Command::Shutdown => break,
        }
    }
    for (_, mut loads) in pages {
        loads.cancel_all();
    }
}

/// Starts a subresource fetch, checked against the page's committed
/// document. A request for an older navigation is dropped: the page has
/// moved on and drops any answer to it too (AC-11).
fn fetch(
    pages: &mut HashMap<PageId, PageLoads>,
    client: &Client,
    sink: &ContentSink,
    request: FetchRequest,
) {
    let Some(loads) = pages.get_mut(&request.page) else {
        warn!(page = ?request.page, "fetch for a page with no navigation dropped");
        return;
    };
    if loads.navigation != request.navigation {
        debug!(navigation = ?request.navigation, "fetch for an old navigation dropped");
        return;
    }
    let load = Load {
        page: request.page,
        navigation: request.navigation,
        fetch: request.fetch,
        sink: Arc::clone(sink),
    };
    if request.fetch == FetchId::DOCUMENT || loads.tasks.contains_key(&request.fetch) {
        warn!(fetch = ?request.fetch, "fetch id already in use; dropped");
        return;
    }
    let Some(document) = loads.document.clone() else {
        // Nothing committed yet (or the error page), so nothing may load.
        load.failed(FetchError::Cancelled);
        return;
    };
    loads.tasks.retain(|_, task| !task.is_finished());
    let span = info_span!("fetch", destination = ?request.destination);
    let served = Rc::clone(&loads.served);
    let task = client
        .clone()
        .fetch(load, request.url, request.destination, document, served);
    loads.tasks.insert(
        request.fetch,
        tokio::task::spawn_local(task.instrument(span)),
    );
}
