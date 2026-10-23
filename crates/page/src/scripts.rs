//! A document's script elements (spec 0004, child 0006, Script loading,
//! AC-31): which run, when, and their fetches.
//!
//! The parser pauses after every `</script>`; [`Scripts::prepare`] decides
//! what that script needs. A classic script without `defer` or `async`
//! blocks the parser until it ran, waiting for its fetch and for the
//! style sheets before it. `defer` scripts run in order once parsing ends;
//! `async` ones as soon as they arrive. Modules and unknown types never
//! run.

use base::atoms::{LocalName, QualName};
use dom::{Document, NodeData, NodeId};
use ipc::FetchId;
use tracing::{debug, warn};

use crate::FetchIds;

/// The `type` values that mean a classic script (HTML, JavaScript MIME
/// type essence match).
const JAVASCRIPT_TYPES: &[&str] = &[
    "application/ecmascript",
    "application/javascript",
    "application/x-ecmascript",
    "application/x-javascript",
    "text/ecmascript",
    "text/javascript",
    "text/javascript1.0",
    "text/javascript1.1",
    "text/javascript1.2",
    "text/javascript1.3",
    "text/javascript1.4",
    "text/javascript1.5",
    "text/jscript",
    "text/livescript",
    "text/x-ecmascript",
    "text/x-javascript",
];

/// When a script runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Now, with the parser waiting.
    Blocking,
    /// After parsing, in document order.
    Defer,
    /// As soon as it is fetched.
    Async,
}

#[derive(Debug)]
enum Source {
    Fetching {
        fetch: FetchId,
        bytes: Vec<u8>,
        charset: Option<String>,
        ok: bool,
    },
    Ready(String),
    Failed,
}

/// A script waiting to run.
#[derive(Debug)]
struct Pending {
    node: NodeId,
    /// Its URL, or the document's for an inline script.
    url: String,
    source: Source,
}

impl Pending {
    fn fetch(&self) -> Option<FetchId> {
        match self.source {
            Source::Fetching { fetch, .. } => Some(fetch),
            _ => None,
        }
    }

    fn settled(&self) -> bool {
        !matches!(self.source, Source::Fetching { .. })
    }
}

/// What the parser does after a `</script>`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Prepared {
    /// Go on: nothing to run now.
    Continue,
    /// Wait: the script blocks the parser until [`Scripts::take_runnable_blocking`].
    Wait,
}

/// A script ready to run: its source, and the URL errors name.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Runnable {
    pub(crate) source: String,
    pub(crate) url: String,
}

/// The committed document's scripts.
#[derive(Debug, Default)]
pub(crate) struct Scripts {
    blocking: Option<Pending>,
    defers: Vec<Pending>,
    asyncs: Vec<Pending>,
    requests: Vec<(FetchId, String)>,
}

impl Scripts {
    /// Takes the fetches to send.
    pub(crate) fn take_requests(&mut self) -> Vec<(FetchId, String)> {
        std::mem::take(&mut self.requests)
    }

    /// The script the parser waits for, if any.
    pub(crate) fn blocking_node(&self) -> Option<NodeId> {
        self.blocking.as_ref().map(|pending| pending.node)
    }

    /// Whether any script has not run yet.
    pub(crate) fn pending(&self) -> bool {
        self.blocking.is_some() || !self.defers.is_empty() || !self.asyncs.is_empty()
    }

    /// Whether `fetch` loads one of the scripts.
    pub(crate) fn owns(&self, fetch: FetchId) -> bool {
        self.all().any(|pending| pending.fetch() == Some(fetch))
    }

    fn all(&self) -> impl Iterator<Item = &Pending> {
        self.blocking.iter().chain(&self.defers).chain(&self.asyncs)
    }

    fn find_mut(&mut self, fetch: FetchId) -> Option<&mut Pending> {
        self.blocking
            .iter_mut()
            .chain(&mut self.defers)
            .chain(&mut self.asyncs)
            .find(|pending| pending.fetch() == Some(fetch))
    }

    /// Prepares the `<script>` the parser just closed: skips it, starts its
    /// fetch, or holds the parser for it.
    pub(crate) fn prepare(&mut self, doc: &Document, node: NodeId, ids: &mut FetchIds) -> Prepared {
        let Some(element) = doc.node(node).and_then(|n| n.as_element()) else {
            return Prepared::Continue;
        };
        let attribute = |name: &str| element.attribute(&QualName::attribute(LocalName::from(name)));
        if let Some(kind) = attribute("type") {
            let kind = kind.trim().to_ascii_lowercase();
            if !kind.is_empty() && !JAVASCRIPT_TYPES.contains(&kind.as_str()) {
                debug!(kind, "script of another type skipped");
                return Prepared::Continue;
            }
        }
        let Some(src) = attribute("src") else {
            let source: String = doc
                .children(node)
                .filter_map(|child| match doc.node(child)?.data() {
                    NodeData::Text(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            if source.trim().is_empty() {
                return Prepared::Continue;
            }
            // An inline script runs now; `defer` and `async` do not apply.
            self.blocking = Some(Pending {
                node,
                url: doc.url().to_owned(),
                source: Source::Ready(source),
            });
            return Prepared::Wait;
        };
        let url = url::Url::parse(doc.url())
            .ok()
            .and_then(|base| base.join(src.trim()).ok());
        let Some(url) = url.filter(|_| !src.trim().is_empty()) else {
            warn!(src, "script with an unusable src skipped");
            return Prepared::Continue;
        };
        let mode = if attribute("async").is_some() {
            Mode::Async
        } else if attribute("defer").is_some() {
            Mode::Defer
        } else {
            Mode::Blocking
        };
        let fetch = ids.next_id();
        let url = String::from(url);
        debug!(url, fetch = fetch.0, ?mode, "script requested");
        self.requests.push((fetch, url.clone()));
        let pending = Pending {
            node,
            url,
            source: Source::Fetching {
                fetch,
                bytes: Vec::new(),
                charset: None,
                ok: true,
            },
        };
        match mode {
            Mode::Blocking => {
                self.blocking = Some(pending);
                Prepared::Wait
            }
            Mode::Defer => {
                self.defers.push(pending);
                Prepared::Continue
            }
            Mode::Async => {
                self.asyncs.push(pending);
                Prepared::Continue
            }
        }
    }

    /// A script's headers arrived: only a 2xx answer runs, decoded with its
    /// charset (UTF-8 otherwise).
    pub(crate) fn fetch_response(
        &mut self,
        fetch: FetchId,
        status: u16,
        headers: &[(String, String)],
    ) {
        let Some(Pending {
            url,
            source: Source::Fetching { ok, charset, .. },
            ..
        }) = self.find_mut(fetch)
        else {
            return;
        };
        if !(200..300).contains(&status) {
            warn!(url, status, "script skipped");
            *ok = false;
        }
        *charset = headers
            .iter()
            .find(|(name, _)| name == "content-type")
            .and_then(|(_, value)| {
                let value = value.to_ascii_lowercase();
                let (_, rest) = value.split_once("charset=")?;
                let rest = rest.split(';').next().unwrap_or_default();
                Some(
                    rest.trim_matches(|c: char| c == '"' || c == '\'' || c.is_whitespace())
                        .to_owned(),
                )
            });
    }

    /// Body bytes of a script.
    pub(crate) fn fetch_chunk(&mut self, fetch: FetchId, data: &[u8]) {
        if let Some(Pending {
            source: Source::Fetching {
                bytes, ok: true, ..
            },
            ..
        }) = self.find_mut(fetch)
        {
            bytes.extend_from_slice(data);
        }
    }

    /// A script's body ended: it is ready to run, or skipped.
    pub(crate) fn fetch_done(&mut self, fetch: FetchId) {
        let Some(pending) = self.find_mut(fetch) else {
            return;
        };
        let Source::Fetching {
            bytes, charset, ok, ..
        } = &pending.source
        else {
            return;
        };
        pending.source = if *ok {
            let encoding = charset
                .as_deref()
                .and_then(|label| encoding_rs::Encoding::for_label(label.as_bytes()))
                .unwrap_or(encoding_rs::UTF_8);
            let (text, _, _) = encoding.decode(bytes);
            Source::Ready(text.into_owned())
        } else {
            Source::Failed
        };
    }

    /// A script failed to load: it is skipped.
    pub(crate) fn fetch_failed(&mut self, fetch: FetchId) {
        if let Some(pending) = self.find_mut(fetch) {
            warn!(url = pending.url, "script failed to load, skipped");
            pending.source = Source::Failed;
        }
    }

    /// The parser blocking script, once it can run: fetched (or failed, which
    /// returns `Some(None)` so the parser goes on) and with no style sheet
    /// still loading before it.
    pub(crate) fn take_runnable_blocking(
        &mut self,
        sheets_loading: bool,
    ) -> Option<Option<Runnable>> {
        if sheets_loading || !self.blocking.as_ref().is_some_and(Pending::settled) {
            return None;
        }
        self.blocking.take().map(runnable)
    }

    /// The `async` scripts that arrived, in arrival order.
    pub(crate) fn take_ready_asyncs(&mut self) -> Vec<Runnable> {
        let (ready, waiting): (Vec<Pending>, Vec<Pending>) = std::mem::take(&mut self.asyncs)
            .into_iter()
            .partition(Pending::settled);
        self.asyncs = waiting;
        ready.into_iter().filter_map(runnable).collect()
    }

    /// The next `defer` script, once it arrived (`Some(None)` when it
    /// failed): they run strictly in document order.
    pub(crate) fn take_next_defer(&mut self) -> Option<Option<Runnable>> {
        if !self.defers.first().is_some_and(Pending::settled) {
            return None;
        }
        Some(runnable(self.defers.remove(0)))
    }

    /// Whether `defer` scripts are still waiting to run.
    pub(crate) fn defers_left(&self) -> bool {
        !self.defers.is_empty()
    }
}

/// A settled script's source, or `None` when it failed.
fn runnable(pending: Pending) -> Option<Runnable> {
    match pending.source {
        Source::Ready(source) => Some(Runnable {
            source,
            url: pending.url,
        }),
        _ => None,
    }
}
