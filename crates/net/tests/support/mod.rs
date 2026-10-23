//! The local test server and message helpers for the network tests (spec
//! 0004, Critical test scenarios).

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers, each test file uses some of them; a panic is a failed test"
)]

use std::convert::Infallible;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::thread;
use std::time::{Duration, Instant};

use bytes::Bytes;
use display_list::PageId;
use hyper::body::{Body, Frame, Incoming};
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::{TokioExecutor, TokioIo};
use ipc::{CommitParams, DocumentKind, FetchId, NavigationId, ToContent};
use net::{Net, NetConfig};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

pub const PAGE: PageId = PageId(1);
pub const NAV: NavigationId = NavigationId(7);
pub const WAIT: Duration = Duration::from_secs(10);

/// The test certificates' folder.
pub fn certs() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/certs")
}

/// Starts the network trusting the test root, with a sink that hands every
/// message, with when it arrived, to the returned receiver.
pub fn start() -> (Net, Receiver<(Instant, ToContent)>) {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let net = Net::start(
        NetConfig {
            user_agent: net::user_agent("0.0.0-test"),
            extra_roots: vec![std::fs::read(certs().join("ca.der")).unwrap()],
        },
        Arc::new(move |message| {
            let _ = tx.lock().unwrap().send((Instant::now(), message));
        }),
        Arc::new(|_| {}),
    )
    .unwrap();
    (net, rx)
}

/// Every message of one fetch, up to its `FetchDone` or `FetchFailed`.
pub fn collect(rx: &Receiver<(Instant, ToContent)>) -> Vec<(Instant, ToContent)> {
    collect_within(rx, WAIT)
}

/// [`collect`], waiting up to `wait` for each message.
pub fn collect_within(
    rx: &Receiver<(Instant, ToContent)>,
    wait: Duration,
) -> Vec<(Instant, ToContent)> {
    let mut out = Vec::new();
    loop {
        let (at, message) = rx.recv_timeout(wait).expect("the load finished");
        let last = matches!(
            message,
            ToContent::FetchDone { .. } | ToContent::FetchFailed { .. }
        );
        out.push((at, message));
        if last {
            return out;
        }
    }
}

/// The body bytes of fetch `fetch` in `messages`.
pub fn bytes_of(messages: &[(Instant, ToContent)], fetch: FetchId) -> Vec<u8> {
    messages
        .iter()
        .filter_map(|(_, m)| match m {
            ToContent::FetchChunk { fetch: f, data, .. } if *f == fetch => Some(data.to_vec()),
            _ => None,
        })
        .flatten()
        .collect()
}

/// The document body in `messages`, as text.
pub fn body(messages: &[(Instant, ToContent)]) -> String {
    String::from_utf8(bytes_of(messages, FetchId::DOCUMENT)).unwrap()
}

/// The commit that starts `messages`: URL, content type, charset and kind.
pub fn commit(messages: &[(Instant, ToContent)]) -> (String, String, Option<String>, DocumentKind) {
    match &messages.first().unwrap().1 {
        ToContent::CommitNavigation {
            page,
            navigation,
            url,
            content_type,
            charset,
            kind,
            ..
        } => {
            assert_eq!((*page, *navigation), (PAGE, NAV));
            (
                url.clone(),
                content_type.clone(),
                charset.clone(),
                kind.clone(),
            )
        }
        other => panic!("expected a commit first, got {other:?}"),
    }
}

/// Navigates to `url` and gives every message up to the end of its body.
pub fn load(
    net: &Net,
    rx: &Receiver<(Instant, ToContent)>,
    url: &str,
) -> Vec<(Instant, ToContent)> {
    net.handle().navigate(
        PAGE,
        NAV,
        url::Url::parse(url).unwrap(),
        CommitParams::default(),
    );
    collect(rx)
}

/// The kind the navigation to `url` committed.
pub fn kind(net: &Net, rx: &Receiver<(Instant, ToContent)>, url: &str) -> DocumentKind {
    commit(&load(net, rx, url)).3
}

/// What a route answers.
#[derive(Debug, Clone, Default)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(&'static str, String)>,
    pub body: Vec<u8>,
    /// Wait this long before answering.
    pub delay: Option<Duration>,
    /// Send the headers and the body, then never end it.
    pub stall: bool,
}

impl Reply {
    pub fn ok(content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            headers: vec![("content-type", content_type.to_owned())],
            body: body.into(),
            ..Self::default()
        }
    }

    pub fn html(body: &str) -> Self {
        Self::ok("text/html; charset=utf-8", body)
    }

    pub fn redirect(status: u16, location: &str) -> Self {
        Self {
            status,
            headers: vec![("location", location.to_owned())],
            ..Self::default()
        }
    }

    pub fn header(mut self, name: &'static str, value: &str) -> Self {
        self.headers.push((name, value.to_owned()));
        self
    }

    pub fn body_bytes(mut self, body: Vec<u8>) -> Self {
        self.body = body;
        self
    }

    /// Never ends the body.
    pub fn stalled(mut self) -> Self {
        self.stall = true;
        self
    }

    /// Sends no `Content-Type` at all.
    pub fn without_type(mut self) -> Self {
        self.headers.retain(|(name, _)| *name != "content-type");
        self
    }
}

/// What a route sees of a request.
#[derive(Debug, Clone)]
pub struct Seen {
    pub path: String,
    pub version: hyper::Version,
    pub headers: Vec<(String, String)>,
}

impl Seen {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// How the server speaks.
#[derive(Debug, Clone, Copy)]
pub enum Tls {
    /// Plain HTTP/1.1.
    Plain,
    /// TLS with `tests/certs/<name>.pem`, offering these ALPN protocols.
    Cert(&'static str, &'static [&'static [u8]]),
}

/// ALPN offering both, `h2` first.
pub const H2_AND_H1: &[&[u8]] = &[b"h2", b"http/1.1"];
/// ALPN offering HTTP/1.1 only.
pub const H1_ONLY: &[&[u8]] = &[b"http/1.1"];

/// A running test server.
pub struct Server {
    pub port: u16,
    /// The most requests that were being answered at once.
    pub peak: Arc<AtomicUsize>,
    /// Every request, in arrival order.
    pub seen: Arc<Mutex<Vec<Seen>>>,
}

impl Server {
    pub fn url(&self, scheme: &str, path: &str) -> String {
        format!("{scheme}://127.0.0.1:{}{path}", self.port)
    }

    pub fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

type Route = dyn Fn(&Seen) -> Reply + Send + Sync;

/// Starts a server on its own thread that answers every request with
/// `route`. It runs until the test process ends.
pub fn serve(tls: Tls, route: impl Fn(&Seen) -> Reply + Send + Sync + 'static) -> Server {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let peak = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let route: Arc<Route> = Arc::new(route);
    let acceptor = match tls {
        Tls::Plain => None,
        Tls::Cert(name, alpn) => Some(acceptor(name, alpn)),
    };
    let (server_peak, server_seen) = (Arc::clone(&peak), Arc::clone(&seen));
    thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let listener = TcpListener::from_std(listener).unwrap();
            let active = Arc::new(AtomicUsize::new(0));
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    continue;
                };
                let (route, peak, seen, active) = (
                    Arc::clone(&route),
                    Arc::clone(&server_peak),
                    Arc::clone(&server_seen),
                    Arc::clone(&active),
                );
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |request: Request<Incoming>| {
                        let (route, peak, seen, active) = (
                            Arc::clone(&route),
                            Arc::clone(&peak),
                            Arc::clone(&seen),
                            Arc::clone(&active),
                        );
                        async move {
                            let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                            peak.fetch_max(now, Ordering::SeqCst);
                            let request = Seen {
                                path: request.uri().path().to_owned(),
                                version: request.version(),
                                headers: request
                                    .headers()
                                    .iter()
                                    .map(|(n, v)| {
                                        (n.as_str().to_owned(), v.to_str().unwrap_or("").to_owned())
                                    })
                                    .collect(),
                            };
                            seen.lock().unwrap().push(request.clone());
                            let reply = route(&request);
                            if let Some(delay) = reply.delay {
                                tokio::time::sleep(delay).await;
                            }
                            active.fetch_sub(1, Ordering::SeqCst);
                            let mut response = Response::builder().status(reply.status);
                            for (name, value) in &reply.headers {
                                response = response.header(*name, value);
                            }
                            let body = TestBody {
                                data: Some(Bytes::from(reply.body)),
                                stall: reply.stall,
                            };
                            Ok::<_, Infallible>(response.body(body).unwrap())
                        }
                    });
                    let builder =
                        hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
                    let _ = match acceptor {
                        None => {
                            builder
                                .serve_connection(TokioIo::new(stream), service)
                                .await
                        }
                        Some(acceptor) => match acceptor.accept(stream).await {
                            Ok(stream) => {
                                builder
                                    .serve_connection(TokioIo::new(stream), service)
                                    .await
                            }
                            // A client that rejects the certificate ends here.
                            Err(_) => Ok(()),
                        },
                    };
                });
            }
        });
    });
    Server { port, peak, seen }
}

fn acceptor(name: &str, alpn: &[&[u8]]) -> TlsAcceptor {
    let folder = certs();
    let chain: Vec<CertificateDer<'static>> =
        CertificateDer::pem_file_iter(folder.join(format!("{name}.pem")))
            .unwrap()
            .map(Result::unwrap)
            .collect();
    let key = PrivateKeyDer::from_pem_file(folder.join(format!("{name}.key"))).unwrap();
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(chain, key)
    .unwrap();
    config.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
    TlsAcceptor::from(Arc::new(config))
}

/// A body that sends its bytes, then ends, or never ends when `stall`.
struct TestBody {
    data: Option<Bytes>,
    stall: bool,
}

impl Body for TestBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        if let Some(data) = self.data.take()
            && !data.is_empty()
        {
            return Poll::Ready(Some(Ok(Frame::data(data))));
        }
        if self.stall {
            Poll::Pending
        } else {
            Poll::Ready(None)
        }
    }
}
