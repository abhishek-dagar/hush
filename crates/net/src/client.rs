//! The HTTP client: HTTP/1.1 and HTTP/2 over TLS, with the time limits,
//! connection caps and redirect handling of spec 0004, child 0001, The
//! `net` crate and Redirects (AC-2, AC-3, AC-4).

use std::cell::RefCell;
use std::collections::HashMap;
use std::error::Error as StdError;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::Bytes;
use http_body_util::Empty;
use hyper::body::Incoming;
use hyper::header::{ACCEPT, ACCEPT_ENCODING, HeaderMap, LOCATION, USER_AGENT};
use hyper::{Request, Uri};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::client::legacy::Client as HyperClient;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use ipc::FetchError;
use rustls::pki_types::CertificateDer;
use rustls_platform_verifier::Verifier;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tower_service::Service;
use tracing::{debug, warn};
use url::Url;

use crate::NetError;
use crate::limits::{
    CONNECT_TIMEOUT, IDLE_TIMEOUT, MAX_CONNECTIONS, MAX_CONNECTIONS_PER_HOST, MAX_REDIRECTS,
};

/// What every request offers to decode (spec 0004, child 0001, Compression).
const ACCEPT_ENCODINGS: &str = "gzip, deflate, br";

/// The TLS settings: the `ring` provider, the OS trust store through the
/// platform verifier plus `extra_roots` (DER, tests only), and no way to
/// skip the check (AC-3).
pub(crate) fn tls_config(extra_roots: Vec<Vec<u8>>) -> Result<rustls::ClientConfig, NetError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let roots = extra_roots.into_iter().map(CertificateDer::from);
    let verifier = Verifier::new_with_extra_roots(roots, Arc::clone(&provider))
        .map_err(|err| NetError::Tls(err.to_string()))?;
    Ok(rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|err| NetError::Tls(err.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth())
}

type Connector = TimeoutConnector<HttpsConnector<HttpConnector>>;

/// The client every load on the network thread shares. Cheap to clone.
/// Not `Send`: it lives on the network thread's local task set.
#[derive(Clone)]
pub(crate) struct Client {
    http: HyperClient<Connector, Empty<Bytes>>,
    user_agent: Arc<str>,
    caps: Rc<Caps>,
}

/// A response whose redirects are followed, holding its connection
/// permits until it is dropped.
pub(crate) struct Response {
    /// The URL after redirects.
    pub(crate) url: Url,
    pub(crate) status: u16,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Incoming,
    /// Held until the body ends, so the caps count it (AC-4).
    pub(crate) permits: Permits,
}

impl Client {
    pub(crate) fn new(tls: rustls::ClientConfig, user_agent: &str) -> Self {
        let mut http = HttpConnector::new();
        // `https` URLs go through it too, under the TLS layer.
        http.enforce_http(false);
        // ALPN offers `h2`, then `http/1.1` (AC-2).
        let https = HttpsConnectorBuilder::new()
            .with_tls_config(tls)
            .https_or_http()
            .enable_all_versions()
            .wrap_connector(http);
        Self {
            http: HyperClient::builder(TokioExecutor::new())
                .build(TimeoutConnector { inner: https }),
            user_agent: user_agent.into(),
            caps: Rc::new(Caps::new()),
        }
    }

    /// GETs `url`, following up to [`MAX_REDIRECTS`] redirects. `hop`
    /// checks each redirect target against policy before it is fetched.
    /// Any status other than a followed redirect is the response.
    pub(crate) async fn get(
        &self,
        mut url: Url,
        accept: &str,
        hop: impl Fn(&Url) -> Result<(), FetchError>,
    ) -> Result<Response, FetchError> {
        let mut redirects = 0;
        loop {
            let permits = self.caps.acquire(&url).await?;
            let request = Request::get(uri(&url)?)
                .header(USER_AGENT, &*self.user_agent)
                .header(ACCEPT, accept)
                .header(ACCEPT_ENCODING, ACCEPT_ENCODINGS)
                .body(Empty::new())
                .map_err(|_| FetchError::InvalidUrl)?;
            let response = tokio::time::timeout(IDLE_TIMEOUT, self.http.request(request))
                .await
                .map_err(|_| FetchError::TimedOut)?
                .map_err(|err| {
                    warn!(%err, "request failed");
                    classify(&err)
                })?;
            let status = response.status().as_u16();
            // 301, 302 and 303 turn into a GET without a body, 307 and 308
            // keep the method; every request here is a bodiless GET, so
            // all five are followed the same way.
            let next = matches!(status, 301 | 302 | 303 | 307 | 308)
                .then(|| response.headers().get(LOCATION))
                .flatten()
                .and_then(|location| location.to_str().ok())
                .and_then(|location| url.join(location).ok());
            if let Some(next) = next {
                if redirects == MAX_REDIRECTS {
                    return Err(FetchError::TooManyRedirects);
                }
                hop(&next)?;
                redirects += 1;
                debug!(%next, status, "redirect");
                url = next;
                continue;
            }
            let (parts, body) = response.into_parts();
            return Ok(Response {
                url,
                status,
                headers: parts.headers,
                body,
                permits,
            });
        }
    }
}

/// `url` as a request URI, without its fragment, which is never sent.
fn uri(url: &Url) -> Result<Uri, FetchError> {
    let mut url = url.clone();
    url.set_fragment(None);
    url.as_str().parse().map_err(|_| FetchError::InvalidUrl)
}

/// The connection caps (AC-4): the client pools connections but does not
/// limit them, so each fetch holds a permit for its host and one of the
/// global ones from before its request until its body ends.
struct Caps {
    all: Arc<Semaphore>,
    hosts: RefCell<HashMap<String, Arc<Semaphore>>>,
}

/// A fetch's two permits, given back when dropped.
pub(crate) struct Permits {
    _host: OwnedSemaphorePermit,
    _all: OwnedSemaphorePermit,
}

impl Caps {
    fn new() -> Self {
        Self {
            all: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
            hosts: RefCell::new(HashMap::new()),
        }
    }

    async fn acquire(&self, url: &Url) -> Result<Permits, FetchError> {
        let key = format!(
            "{}://{}:{}",
            url.scheme(),
            url.host_str().unwrap_or_default(),
            url.port_or_known_default().unwrap_or_default()
        );
        let host = {
            let mut hosts = self.hosts.borrow_mut();
            // Forget hosts nobody holds a permit for, so the map stays small.
            hosts.retain(|_, semaphore| Arc::strong_count(semaphore) > 1);
            Arc::clone(
                hosts
                    .entry(key)
                    .or_insert_with(|| Arc::new(Semaphore::new(MAX_CONNECTIONS_PER_HOST))),
            )
        };
        // The host's permit first, so a fetch queued behind its host does
        // not hold one of the global permits while it waits. The semaphores
        // are never closed, so acquiring only fails if that changes.
        let host = host
            .acquire_owned()
            .await
            .map_err(|_| FetchError::Cancelled)?;
        let all = Arc::clone(&self.all)
            .acquire_owned()
            .await
            .map_err(|_| FetchError::Cancelled)?;
        Ok(Permits {
            _host: host,
            _all: all,
        })
    }
}

/// The TCP connect plus TLS handshake ran past [`CONNECT_TIMEOUT`].
#[derive(Debug, thiserror::Error)]
#[error("connecting took longer than the limit")]
struct ConnectTimedOut;

/// Gives a connector the [`CONNECT_TIMEOUT`] limit for its whole connect,
/// TLS handshake included, which `HttpConnector`'s own timeout does not
/// cover.
#[derive(Clone)]
pub(crate) struct TimeoutConnector<C> {
    inner: C,
}

impl<C> Service<Uri> for TimeoutConnector<C>
where
    C: Service<Uri>,
    C::Future: Send + 'static,
    C::Error: Into<Box<dyn StdError + Send + Sync>>,
{
    type Response = C::Response;
    type Error = Box<dyn StdError + Send + Sync>;
    type Future = Pin<Box<dyn Future<Output = Result<C::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let connecting = self.inner.call(uri);
        Box::pin(async move {
            match tokio::time::timeout(CONNECT_TIMEOUT, connecting).await {
                Ok(result) => result.map_err(Into::into),
                Err(_) => Err(Box::new(ConnectTimedOut) as Self::Error),
            }
        })
    }
}

/// The failure reason for a request that got no response, from the
/// error's cause chain.
fn classify(err: &hyper_util::client::legacy::Error) -> FetchError {
    let mut cause = err.source();
    while let Some(current) = cause {
        if let Some(reason) = reason_of(current) {
            return reason;
        }
        // An `io::Error`'s own `source` skips the error it wraps, and the
        // TLS error sits inside one (inside another), so step into it.
        cause = match current
            .downcast_ref::<io::Error>()
            .and_then(io::Error::get_ref)
        {
            Some(inner) => Some(inner as &(dyn StdError + 'static)),
            None => current.source(),
        };
    }
    FetchError::ConnectFailed
}

fn reason_of(err: &(dyn StdError + 'static)) -> Option<FetchError> {
    if err.is::<ConnectTimedOut>() {
        return Some(FetchError::TimedOut);
    }
    if let Some(tls) = err.downcast_ref::<rustls::Error>() {
        return Some(FetchError::TlsFailed(tls_detail(tls)));
    }
    if err
        .downcast_ref::<io::Error>()
        .is_some_and(|io| io.kind() == io::ErrorKind::TimedOut)
    {
        return Some(FetchError::TimedOut);
    }
    // The resolver reports a failed lookup only in its message.
    err.to_string()
        .contains("dns error")
        .then_some(FetchError::DnsFailed)
}

/// A short phrase for the error page: "The site's security certificate is
/// not trusted (detail)."
fn tls_detail(err: &rustls::Error) -> String {
    use rustls::CertificateError as Cert;
    match err {
        rustls::Error::InvalidCertificate(Cert::Expired | Cert::ExpiredContext { .. }) => {
            "it has expired".into()
        }
        rustls::Error::InvalidCertificate(
            Cert::NotValidForName | Cert::NotValidForNameContext { .. },
        ) => "it is for a different site".into(),
        rustls::Error::InvalidCertificate(Cert::UnknownIssuer) => {
            "it is not signed by a trusted authority".into()
        }
        // The platform verifier's own words, e.g. macOS's "certificate is
        // expired".
        rustls::Error::InvalidCertificate(Cert::Other(other)) => other.to_string(),
        other => other.to_string(),
    }
}
