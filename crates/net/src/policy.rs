//! What a load may do, decided before any byte is read: schemes, the
//! `file://` folder rule, mixed content, and what a response is (spec 0004,
//! child 0001, Content types and Subresource fetch policy).
//!
//! Every check runs against the navigation's recorded document, never a
//! URL the page claims (spec 0004, Security model).

use std::path::{Path, PathBuf};

use ipc::{DocumentKind, FetchError};
use url::Url;

/// How many decoded bytes sniffing looks at when a response has no type.
pub(crate) const SNIFF_BYTES: usize = 512;

/// The committed document a page's subresources are checked against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Document {
    /// Its final URL.
    pub(crate) url: Url,
    /// For a `file://` document, the canonical folder it is in.
    pub(crate) folder: Option<PathBuf>,
    /// For a `file://` document, its canonical path, watched for live
    /// reload (AC-38).
    pub(crate) file: Option<PathBuf>,
}

/// How a subresource is loaded, once policy allows it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Plan {
    /// Read this canonical path.
    File(PathBuf),
    /// Fetch over the network. `upgraded` marks an `http` URL rewritten to
    /// `https` for an `https` page: any failure is then `MixedContentBlocked`.
    Http { url: Url, upgraded: bool },
}

/// Whether `url` may be a top level document, and on which hop: the first
/// load may use any of `http`, `https`, `file` or `about`, a redirect only
/// `http` or `https`.
pub(crate) fn check_document(url: &Url, redirect: bool) -> Result<(), FetchError> {
    match url.scheme() {
        "http" | "https" => Ok(()),
        "file" | "about" if !redirect => Ok(()),
        _ => Err(FetchError::UnsupportedScheme),
    }
}

/// The scheme and mixed content rules for a subresource of `document`
/// (AC-8, AC-9). A `file://` URL still needs [`check_file`] once its path
/// is canonical.
pub(crate) fn check_subresource(url: &Url, document: &Document) -> Result<Plan, FetchError> {
    match url.scheme() {
        "https" => Ok(Plan::Http {
            url: url.clone(),
            upgraded: false,
        }),
        "http" if document.url.scheme() == "https" => {
            let mut upgraded = url.clone();
            upgraded
                .set_scheme("https")
                .map_err(|()| FetchError::MixedContentBlocked)?;
            Ok(Plan::Http {
                url: upgraded,
                upgraded: true,
            })
        }
        "http" => Ok(Plan::Http {
            url: url.clone(),
            upgraded: false,
        }),
        "file" if document.folder.is_some() => url
            .to_file_path()
            .map(Plan::File)
            .map_err(|()| FetchError::InvalidUrl),
        // An `http(s)` page never reads a file (AC-8).
        "file" => Err(FetchError::FileAccessDenied),
        _ => Err(FetchError::UnsupportedScheme),
    }
}

/// A redirect hop of a subresource: `http(s)` only, and an `https` page's
/// subresource never lands on `http` (AC-9).
pub(crate) fn check_subresource_redirect(url: &Url, document: &Document) -> Result<(), FetchError> {
    match url.scheme() {
        "https" => Ok(()),
        "http" if document.url.scheme() == "https" => Err(FetchError::MixedContentBlocked),
        "http" => Ok(()),
        _ => Err(FetchError::UnsupportedScheme),
    }
}

/// The folder rule (AC-8): `canonical` (already canonical, so `..` and
/// symlinks are resolved) must be inside the document's folder.
pub(crate) fn check_file(canonical: &Path, document: &Document) -> Result<(), FetchError> {
    match &document.folder {
        Some(folder) if canonical.starts_with(folder) => Ok(()),
        _ => Err(FetchError::FileAccessDenied),
    }
}

/// What a response with MIME type `mime` (lowercased, no parameters) is,
/// or `None` when it has no type and must be sniffed (AC-7).
pub(crate) fn kind_for_mime(mime: &str) -> Result<Option<DocumentKind>, FetchError> {
    match mime {
        "" => Ok(None),
        "text/html" | "application/xhtml+xml" => Ok(Some(DocumentKind::Document)),
        "text/plain" => Ok(Some(DocumentKind::PlainText)),
        other => Err(FetchError::UnsupportedContentType(other.to_owned())),
    }
}

/// A `file://` document's kind and MIME type by its extension, or `None`
/// to sniff it.
pub(crate) fn kind_for_extension(path: &Path) -> Option<(DocumentKind, &'static str)> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "html" | "htm" | "xhtml" => Some((DocumentKind::Document, "text/html")),
        "txt" | "text" | "md" | "rs" | "css" | "js" | "json" => {
            Some((DocumentKind::PlainText, "text/plain"))
        }
        _ => None,
    }
}

/// Sniffs an untyped body from its first bytes: HTML when the first byte
/// that is not whitespace is `<`, else plain text. Gives the kind and the
/// MIME type it stands for.
pub(crate) fn sniff(first: &[u8]) -> (DocumentKind, &'static str) {
    let html = first
        .iter()
        .take(SNIFF_BYTES)
        .find(|b| !matches!(b, b'\t' | b'\n' | b'\x0C' | b'\r' | b' '))
        == Some(&b'<');
    if html {
        (DocumentKind::Document, "text/html")
    } else {
        (DocumentKind::PlainText, "text/plain")
    }
}

/// The `Content-Type` a subresource read from a file gets, by extension.
pub(crate) fn file_mime(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match extension.as_str() {
        "html" | "htm" => "text/html",
        "xhtml" => "application/xhtml+xml",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "txt" | "text" | "md" => "text/plain",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(url: &str, folder: Option<&str>) -> Document {
        Document {
            url: Url::parse(url).unwrap(),
            folder: folder.map(PathBuf::from),
            file: None,
        }
    }

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    #[test]
    fn font_files_are_typed_by_extension() {
        // Spec 0006, Value sourcing, Font MIME for `file:`.
        assert_eq!(file_mime(Path::new("icons.TTF")), Some("font/ttf"));
        assert_eq!(file_mime(Path::new("a.otf")), Some("font/otf"));
        assert_eq!(file_mime(Path::new("a.woff")), Some("font/woff"));
        assert_eq!(file_mime(Path::new("a.woff2")), Some("font/woff2"));
    }

    #[test]
    fn documents_take_four_schemes_and_redirects_two() {
        for ok in ["http://a/", "https://a/", "file:///a.html", "about:start"] {
            assert_eq!(check_document(&url(ok), false), Ok(()), "{ok}");
        }
        assert_eq!(
            check_document(&url("ftp://a/"), false),
            Err(FetchError::UnsupportedScheme)
        );
        assert_eq!(
            check_document(&url("file:///a"), true),
            Err(FetchError::UnsupportedScheme)
        );
        assert_eq!(
            check_document(&url("about:blank"), true),
            Err(FetchError::UnsupportedScheme)
        );
    }

    #[test]
    fn web_pages_never_read_files_and_about_is_never_a_subresource() {
        // AC-8
        let page = document("http://site/", None);
        assert_eq!(
            check_subresource(&url("file:///etc/hosts"), &page),
            Err(FetchError::FileAccessDenied)
        );
        assert_eq!(
            check_subresource(&url("about:blank"), &page),
            Err(FetchError::UnsupportedScheme)
        );
    }

    #[test]
    fn file_pages_read_only_inside_their_folder() {
        // AC-8: the paths here are already canonical.
        let page = document("file:///site/index.html", Some("/site"));
        assert_eq!(
            check_subresource(&url("file:///site/a.css"), &page),
            Ok(Plan::File("/site/a.css".into()))
        );
        assert_eq!(check_file(Path::new("/site/sub/a.png"), &page), Ok(()));
        assert_eq!(
            check_file(Path::new("/secret.txt"), &page),
            Err(FetchError::FileAccessDenied)
        );
        assert_eq!(
            check_file(Path::new("/site-other/a"), &page),
            Err(FetchError::FileAccessDenied)
        );
    }

    #[test]
    fn https_pages_upgrade_http_and_block_downgrades() {
        // AC-9
        let page = document("https://site/", None);
        assert_eq!(
            check_subresource(&url("http://cdn:8080/a.png"), &page),
            Ok(Plan::Http {
                url: url("https://cdn:8080/a.png"),
                upgraded: true
            })
        );
        assert_eq!(
            check_subresource_redirect(&url("http://cdn/a.png"), &page),
            Err(FetchError::MixedContentBlocked)
        );
        let plain = document("http://site/", None);
        assert_eq!(
            check_subresource_redirect(&url("http://cdn/a.png"), &plain),
            Ok(())
        );
        assert_eq!(
            check_subresource_redirect(&url("file:///a"), &plain),
            Err(FetchError::UnsupportedScheme)
        );
    }

    #[test]
    fn content_types_and_sniffing() {
        // AC-7
        assert_eq!(kind_for_mime("text/html"), Ok(Some(DocumentKind::Document)));
        assert_eq!(
            kind_for_mime("application/xhtml+xml"),
            Ok(Some(DocumentKind::Document))
        );
        assert_eq!(
            kind_for_mime("text/plain"),
            Ok(Some(DocumentKind::PlainText))
        );
        assert_eq!(kind_for_mime(""), Ok(None));
        assert_eq!(
            kind_for_mime("application/pdf"),
            Err(FetchError::UnsupportedContentType("application/pdf".into()))
        );
        assert_eq!(sniff(b" \n\t<p>hi").0, DocumentKind::Document);
        assert_eq!(sniff(b"plain <b>").0, DocumentKind::PlainText);
        assert_eq!(sniff(b"").0, DocumentKind::PlainText);
        assert_eq!(
            kind_for_extension(Path::new("/a/B.HTM")),
            Some((DocumentKind::Document, "text/html"))
        );
        assert_eq!(
            kind_for_extension(Path::new("/a/notes.md")),
            Some((DocumentKind::PlainText, "text/plain"))
        );
        assert_eq!(kind_for_extension(Path::new("/a/page")), None);
    }
}
