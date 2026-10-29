//! Loading a navigation's document and a page's subresources: over HTTP,
//! from a file, or built in (spec 0004, child 0001, Navigation and
//! Subresource fetch policy).

use std::path::Path;

use display_list::PageId;
use hyper::header::{
    CONTENT_DISPOSITION, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, HeaderMap, HeaderValue,
};
use ipc::{
    CommitParams, Destination, DocumentKind, FetchError, FetchId, IpcBytes, NavigationId, ToContent,
};
use tokio::fs::File;
use tracing::{info, warn};
use url::Url;

use crate::body::{self, Body, Encoding};
use crate::client::{Client, Response};
use crate::limits::{CHUNK_COALESCE, MAX_DOCUMENT_BYTES, MAX_FONT_BYTES, MAX_SUBRESOURCE_BYTES};
use crate::policy::{self, Classified, Document, Kind, Plan, SNIFF_BYTES};
use crate::{ContentSink, Served, Target, about};

/// What a document request accepts (spec 0004, Value sourcing).
const DOCUMENT_ACCEPT: &str = "text/html,application/xhtml+xml;q=0.9,*/*;q=0.8";

/// What a subresource request accepts, by destination (spec 0004, Value
/// sourcing).
fn accept(destination: Destination) -> &'static str {
    match destination {
        Destination::Style => "text/css,*/*;q=0.1",
        Destination::Script => "*/*",
        Destination::Image | Destination::Favicon => "image/webp,image/png,image/*;q=0.8",
        Destination::Font => "font/ttf,font/otf,*/*;q=0.1",
    }
}

/// The largest body a subresource may have, by destination: fonts are
/// capped lower (spec 0006, AC-11).
fn max_bytes(destination: Destination) -> usize {
    match destination {
        Destination::Font => MAX_FONT_BYTES,
        _ => MAX_SUBRESOURCE_BYTES,
    }
}

/// One fetch's messages: where they go and what they are tagged with.
pub(crate) struct Load {
    pub(crate) page: PageId,
    pub(crate) navigation: NavigationId,
    pub(crate) fetch: FetchId,
    pub(crate) sink: ContentSink,
}

impl Load {
    fn send(&self, message: ToContent) {
        (self.sink)(message);
    }

    /// Commits the document to the page, with the history `params` the
    /// browser decided when the navigation began (spec 0008).
    fn commit(
        &self,
        params: CommitParams,
        url: &str,
        content_type: &str,
        charset: Option<String>,
        kind: DocumentKind,
    ) {
        self.send(ToContent::CommitNavigation {
            page: self.page,
            navigation: self.navigation,
            url: url.to_owned(),
            content_type: content_type.to_owned(),
            charset,
            kind,
            restore_scroll: params.restore_scroll,
            history: params.history,
        });
    }

    /// A subresource's headers.
    fn response(&self, status: u16, headers: Vec<(String, String)>, final_url: &Url) {
        self.send(ToContent::FetchResponse {
            page: self.page,
            navigation: self.navigation,
            fetch: self.fetch,
            status,
            headers,
            final_url: final_url.as_str().to_owned(),
        });
    }

    /// Sends body bytes, split into chunk sized messages.
    pub(crate) fn chunk(&self, bytes: &[u8]) {
        for piece in bytes.chunks(CHUNK_COALESCE) {
            self.send(ToContent::FetchChunk {
                page: self.page,
                navigation: self.navigation,
                fetch: self.fetch,
                data: IpcBytes::from_bytes(piece),
            });
        }
    }

    pub(crate) fn done(&self) {
        self.send(ToContent::FetchDone {
            page: self.page,
            navigation: self.navigation,
            fetch: self.fetch,
        });
    }

    pub(crate) fn failed(&self, reason: FetchError) {
        self.send(ToContent::FetchFailed {
            page: self.page,
            navigation: self.navigation,
            fetch: self.fetch,
            reason,
        });
    }

    /// Commits the "can't show" page (spec 0035, AC-8): no body follows,
    /// so the load is done at once. An attachment with no type shows as
    /// `application/octet-stream`.
    fn commit_unviewable(
        &self,
        params: CommitParams,
        url: &Url,
        file_name: String,
        mime: &str,
        size: Option<u64>,
    ) {
        let mime = if mime.is_empty() {
            "application/octet-stream"
        } else {
            mime
        };
        info!(mime, ?size, "showing the can't show page");
        let kind = DocumentKind::Unviewable {
            file_name,
            mime: mime.to_owned(),
            size,
        };
        self.commit(params, url.as_str(), mime, None, kind);
        self.done();
    }

    /// Commits a whole body that is already in memory.
    fn commit_body(&self, params: CommitParams, url: &str, body: &str) {
        self.commit(params, url, "text/html", None, DocumentKind::Document);
        self.chunk(body.as_bytes());
        self.done();
    }

    /// Commits the error page for `input`, the load that failed (AC-10).
    fn commit_error(&self, params: CommitParams, reason: FetchError, input: &str) {
        info!(%reason, "showing the error page");
        let body = about::error_page(&reason, input);
        self.commit(
            params,
            "about:error",
            "text/html",
            None,
            DocumentKind::Error {
                reason,
                input: input.to_owned(),
            },
        );
        self.chunk(body.as_bytes());
        self.done();
    }
}

impl Client {
    /// Loads `target` as the document and streams it to the page. Calls
    /// `committed` with the document once it is committed, so later
    /// subresource fetches are checked against it.
    pub(crate) async fn navigate(
        self,
        load: Load,
        target: Target,
        params: CommitParams,
        committed: impl Fn(Document),
    ) {
        let url = match target {
            Target::Error { reason, input } => return load.commit_error(params, reason, &input),
            Target::Url(url) => url,
        };
        let input = url.as_str().to_owned();
        let result = match policy::check_document(&url, false) {
            Err(reason) => Err(reason),
            Ok(()) => match url.scheme() {
                "about" => match about::page(url.path()) {
                    Some(body) => {
                        load.commit_body(params, url.as_str(), body);
                        Ok(())
                    }
                    None => Err(FetchError::InvalidUrl),
                },
                "file" => load_file_document(&load, &url, params, &committed).await,
                _ => {
                    self.load_http_document(&load, url, params, &committed)
                        .await
                }
            },
        };
        if let Err(reason) = result {
            load.commit_error(params, reason, &input);
        }
    }

    /// Fetches a document over HTTP(S). An error means "show the error
    /// page": any failure before the commit, or a body over the size limit
    /// after it (AC-4). Other failures after the commit end the stream with
    /// `FetchFailed`, and the page keeps what it got.
    async fn load_http_document(
        &self,
        load: &Load,
        url: Url,
        params: CommitParams,
        committed: &impl Fn(Document),
    ) -> Result<(), FetchError> {
        let Response {
            url,
            status,
            headers,
            body,
            permits: _permits,
        } = self
            .get(url, DOCUMENT_ACCEPT, |next| {
                policy::check_document(next, true)
            })
            .await?;
        info!(status, "response");
        let encoding = Encoding::from_header(header(&headers, CONTENT_ENCODING));
        // Any status renders its body: a server's 404 page is a page.
        let (mime, charset) = parse_content_type(headers.get(CONTENT_TYPE));
        let disposition = header(&headers, CONTENT_DISPOSITION);
        let classified = policy::classify(&mime, disposition);
        // A file that is not shown is never read, so it has no size limit
        // (spec 0035, AC-8).
        if classified != Classified::Known(Kind::Unviewable) {
            check_length(&headers, encoding, MAX_DOCUMENT_BYTES)?;
        }
        let file_name = policy::file_name(disposition, &url);
        let length = (encoding == Encoding::Identity)
            .then(|| header(&headers, CONTENT_LENGTH)?.trim().parse::<u64>().ok())
            .flatten();
        let mut body = Body::http(body, encoding, MAX_DOCUMENT_BYTES);
        let (kind, mime, first) = match classified {
            Classified::Known(kind) => (kind, mime, Vec::new()),
            Classified::Sniff => {
                let first = body.read_at_least(SNIFF_BYTES).await?;
                let (kind, mime) = policy::sniff(&first);
                (kind, mime.to_owned(), first)
            }
        };
        let document = Document {
            url,
            folder: None,
            file: None,
        };
        if kind == Kind::Unviewable {
            drop(body);
            load.commit_unviewable(params, &document.url, file_name, &mime, length);
            committed(document);
            return Ok(());
        }
        let kind = document_kind(kind, file_name, &mime);
        load.commit(params, document.url.as_str(), &mime, charset, kind);
        committed(document);
        after_commit(body::stream(load, body, first).await)
    }

    /// Fetches a subresource for `document`'s page, checked against it
    /// first (AC-8, AC-9). Every outcome reaches the page: the response
    /// and its body, or `FetchFailed`.
    pub(crate) async fn fetch(
        self,
        load: Load,
        url: String,
        destination: Destination,
        document: Document,
        served: Served,
    ) {
        if let Err(reason) = self
            .fetch_checked(&load, &url, destination, &document, &served)
            .await
        {
            if reason == FetchError::MixedContentBlocked {
                warn!(url, "insecure subresource of a secure page blocked");
            }
            load.failed(reason);
        }
    }

    /// An error is a failure before the response; one after it was
    /// already reported by the stream.
    async fn fetch_checked(
        &self,
        load: &Load,
        url: &str,
        destination: Destination,
        document: &Document,
        served: &Served,
    ) -> Result<(), FetchError> {
        let url = Url::parse(url).map_err(|_| FetchError::InvalidUrl)?;
        match policy::check_subresource(&url, document)? {
            Plan::File(path) => fetch_file(load, &url, &path, destination, document, served).await,
            Plan::Http { url, upgraded } => {
                let result = self.fetch_http(load, url, destination, document).await;
                match result {
                    // Any failure of an upgraded fetch blocks it (AC-9).
                    Err(reason) if upgraded => {
                        warn!(%reason, "the https upgrade failed");
                        Err(FetchError::MixedContentBlocked)
                    }
                    other => other,
                }
            }
        }
    }

    async fn fetch_http(
        &self,
        load: &Load,
        url: Url,
        destination: Destination,
        document: &Document,
    ) -> Result<(), FetchError> {
        let Response {
            url,
            status,
            headers,
            body,
            permits: _permits,
        } = self
            .get(url, accept(destination), |next| {
                policy::check_subresource_redirect(next, document)
            })
            .await?;
        let encoding = Encoding::from_header(header(&headers, CONTENT_ENCODING));
        let limit = max_bytes(destination);
        check_length(&headers, encoding, limit)?;
        let list = headers
            .iter()
            .map(|(name, value)| {
                let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
                (name.as_str().to_owned(), value)
            })
            .collect();
        load.response(status, list, &url);
        // A failure from here on was reported by the stream.
        let _ = body::stream(load, Body::http(body, encoding, limit), Vec::new()).await;
        Ok(())
    }
}

/// Reads a `file://` document, typed by its extension or sniffed (spec
/// 0035, AC-11).
async fn load_file_document(
    load: &Load,
    url: &Url,
    params: CommitParams,
    committed: &impl Fn(Document),
) -> Result<(), FetchError> {
    let path = url.to_file_path().map_err(|()| FetchError::InvalidUrl)?;
    let (file, length) = open_file(&path).await?;
    let mut body = Body::file(file, MAX_DOCUMENT_BYTES);
    let (kind, mime, first) = match policy::kind_for_extension(&path) {
        Some((kind, mime)) => (kind, mime, Vec::new()),
        None => {
            let first = body.read_at_least(SNIFF_BYTES).await?;
            let (kind, mime) = policy::sniff(&first);
            (kind, mime, first)
        }
    };
    // A file that is not shown is never read, so it has no size limit
    // (AC-8).
    if kind != Kind::Unviewable && length > MAX_DOCUMENT_BYTES as u64 {
        return Err(FetchError::TooLarge);
    }
    // Its subresources may read only below this folder (AC-8). Without a
    // canonical folder they may read no file at all.
    let file = tokio::fs::canonicalize(&path).await.ok();
    let folder = file
        .as_deref()
        .and_then(|path| path.parent().map(Path::to_path_buf));
    let document = Document {
        url: url.clone(),
        folder,
        file,
    };
    let file_name = policy::file_name(None, url);
    if kind == Kind::Unviewable {
        drop(body);
        load.commit_unviewable(params, url, file_name, mime, Some(length));
        committed(document);
        return Ok(());
    }
    load.commit(
        params,
        url.as_str(),
        mime,
        None,
        document_kind(kind, file_name, mime),
    );
    committed(document);
    after_commit(body::stream(load, body, first).await)
}

/// The kind the page gets for a response that is shown.
fn document_kind(kind: Kind, file_name: String, mime: &str) -> DocumentKind {
    match kind {
        Kind::Document => DocumentKind::Document,
        Kind::PlainText => DocumentKind::PlainText,
        Kind::Json => DocumentKind::Json,
        Kind::Image => DocumentKind::Image {
            file_name,
            mime: mime.to_owned(),
        },
        // Callers commit it with `commit_unviewable`; this keeps the
        // match total.
        Kind::Unviewable => DocumentKind::Unviewable {
            file_name,
            mime: mime.to_owned(),
            size: None,
        },
    }
}

/// Reads a file subresource after the folder rule allows it (AC-8): the
/// path is canonicalized and checked before any byte is read. A file read
/// joins `served`, the files live reload watches (AC-38).
async fn fetch_file(
    load: &Load,
    url: &Url,
    path: &Path,
    destination: Destination,
    document: &Document,
    served: &Served,
) -> Result<(), FetchError> {
    let canonical = tokio::fs::canonicalize(path)
        .await
        .map_err(|_| FetchError::FileNotFound)?;
    policy::check_file(&canonical, document)?;
    served.borrow_mut().insert(canonical.clone());
    let limit = max_bytes(destination);
    let file = open(&canonical, limit).await?;
    let headers = policy::file_mime(&canonical)
        .map(|mime| vec![(CONTENT_TYPE.as_str().to_owned(), mime.to_owned())])
        .unwrap_or_default();
    load.response(200, headers, url);
    let _ = body::stream(load, Body::file(file, limit), Vec::new()).await;
    Ok(())
}

/// Opens a file to read, refusing directories and files over `limit`.
async fn open(path: &Path, limit: usize) -> Result<File, FetchError> {
    let (file, length) = open_file(path).await?;
    if length > limit as u64 {
        return Err(FetchError::TooLarge);
    }
    Ok(file)
}

/// Opens a file to read, refusing directories, and gives its length.
async fn open_file(path: &Path) -> Result<(File, u64), FetchError> {
    let file = File::open(path)
        .await
        .map_err(|_| FetchError::FileNotFound)?;
    let metadata = file
        .metadata()
        .await
        .map_err(|_| FetchError::FileNotFound)?;
    if metadata.is_dir() {
        return Err(FetchError::UnsupportedContentType("inode/directory".into()));
    }
    Ok((file, metadata.len()))
}

/// What the document's stream ending means for the page: over the size
/// limit shows the error page (AC-4); any other failure keeps what the
/// page already has.
fn after_commit(result: Result<(), FetchError>) -> Result<(), FetchError> {
    match result {
        Err(FetchError::TooLarge) => Err(FetchError::TooLarge),
        _ => Ok(()),
    }
}

/// Fails early when an uncompressed body says it is over `limit`.
fn check_length(headers: &HeaderMap, encoding: Encoding, limit: usize) -> Result<(), FetchError> {
    let length = header(headers, CONTENT_LENGTH).and_then(|v| v.trim().parse::<u64>().ok());
    if encoding == Encoding::Identity && length.is_some_and(|length| length > limit as u64) {
        return Err(FetchError::TooLarge);
    }
    Ok(())
}

fn header(headers: &HeaderMap, name: hyper::header::HeaderName) -> Option<&str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// The MIME type without parameters, lowercased, and the `charset`
/// parameter if there is one.
fn parse_content_type(header: Option<&HeaderValue>) -> (String, Option<String>) {
    let Some(value) = header.and_then(|v| v.to_str().ok()) else {
        return (String::new(), None);
    };
    let mut parts = value.split(';');
    let mime = parts.next().unwrap_or_default().trim().to_ascii_lowercase();
    let charset = parts.find_map(|param| {
        let (name, value) = param.split_once('=')?;
        name.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| value.trim().trim_matches('"').to_owned())
    });
    (mime, charset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fonts_ask_for_font_types_and_get_the_lower_cap() {
        // Spec 0006, AC-11.
        assert_eq!(accept(Destination::Font), "font/ttf,font/otf,*/*;q=0.1");
        assert_eq!(max_bytes(Destination::Font), 10 * 1024 * 1024);
        assert_eq!(max_bytes(Destination::Image), MAX_SUBRESOURCE_BYTES);
    }

    #[test]
    fn content_type_and_charset() {
        let header = HeaderValue::from_static("Text/HTML; Charset=\"ISO-8859-1\"");
        assert_eq!(
            parse_content_type(Some(&header)),
            ("text/html".into(), Some("ISO-8859-1".into()))
        );
        assert_eq!(parse_content_type(None), (String::new(), None));
    }
}
