//! What a load may do, decided before any byte is read: schemes, the
//! `file://` folder rule, mixed content, and what a response is (spec 0004,
//! child 0001, Content types and Subresource fetch policy; spec 0035, Type
//! table).
//!
//! Every check runs against the navigation's recorded document, never a
//! URL the page claims (spec 0004, Security model).

use std::path::{Path, PathBuf};

use ipc::FetchError;
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

/// What a navigation response is, before the page gets it (spec 0035,
/// Type table). [`DocumentKind`] adds the file name and size later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// HTML.
    Document,
    /// Text shown wrapped.
    PlainText,
    /// JSON text, with the pretty print checkbox.
    Json,
    /// An image page.
    Image,
    /// The "can't show" page; its body is never read.
    Unviewable,
}

/// The type table's answer for a response: its kind, or sniff it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Classified {
    /// The headers decide it.
    Known(Kind),
    /// No type: sniff the first bytes (AC-10).
    Sniff,
}

/// The type table (spec 0035, Type table): what a response with MIME type
/// `mime` (lowercased, no parameters) and `Content-Disposition`
/// `disposition` is. First match wins.
pub(crate) fn classify(mime: &str, disposition: Option<&str>) -> Classified {
    if disposition.is_some_and(is_attachment) {
        return Classified::Known(Kind::Unviewable);
    }
    let (top, sub) = mime.split_once('/').unwrap_or((mime, ""));
    let kind = match mime {
        "" => return Classified::Sniff,
        "text/html" | "application/xhtml+xml" => Kind::Document,
        "application/json" | "text/json" => Kind::Json,
        _ if sub.ends_with("+json") => Kind::Json,
        // Text until spec 0034 lands its SVG document path.
        "image/svg+xml" => Kind::PlainText,
        _ if top == "image" => Kind::Image,
        "application/xml"
        | "application/javascript"
        | "application/x-javascript"
        | "application/ecmascript" => Kind::PlainText,
        _ if top == "text" || sub.ends_with("+xml") => Kind::PlainText,
        _ => Kind::Unviewable,
    };
    Classified::Known(kind)
}

/// Whether a `Content-Disposition` value's type is `attachment`.
fn is_attachment(disposition: &str) -> bool {
    disposition
        .split(';')
        .next()
        .is_some_and(|kind| kind.trim().eq_ignore_ascii_case("attachment"))
}

/// A `file://` document's kind and MIME type by its extension, or `None`
/// to sniff it (spec 0035, AC-11).
pub(crate) fn kind_for_extension(path: &Path) -> Option<(Kind, &'static str)> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "html" | "htm" | "xhtml" => Some((Kind::Document, "text/html")),
        "json" => Some((Kind::Json, "application/json")),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" => {
            Some((Kind::Image, file_mime(path)?))
        }
        "txt" | "text" | "md" | "rs" | "css" | "js" | "mjs" | "xml" | "svg" | "csv" | "toml"
        | "yaml" | "yml" | "log" => Some((Kind::PlainText, "text/plain")),
        _ => None,
    }
}

/// Sniffs an untyped body from its first bytes (spec 0035, AC-10), in
/// order: an image signature, HTML when the first byte that is not
/// whitespace is `<`, a binary byte, else plain text. Gives the kind and
/// the MIME type it stands for.
pub(crate) fn sniff(first: &[u8]) -> (Kind, &'static str) {
    let first = &first[..first.len().min(SNIFF_BYTES)];
    if let Some(mime) = image_signature(first) {
        return (Kind::Image, mime);
    }
    let html = first
        .iter()
        .find(|b| !matches!(b, b'\t' | b'\n' | b'\x0C' | b'\r' | b' '))
        == Some(&b'<');
    if html {
        (Kind::Document, "text/html")
    } else if first.iter().any(|&b| is_binary(b)) {
        (Kind::Unviewable, "application/octet-stream")
    } else {
        (Kind::PlainText, "text/plain")
    }
}

/// The image type `first` starts with, if any.
fn image_signature(first: &[u8]) -> Option<&'static str> {
    const SIGNATURES: [(&[u8], &str); 7] = [
        (b"\x89PNG\r\n\x1a\n", "image/png"),
        (b"\xFF\xD8\xFF", "image/jpeg"),
        (b"GIF87a", "image/gif"),
        (b"GIF89a", "image/gif"),
        (b"BM", "image/bmp"),
        (b"\x00\x00\x01\x00", "image/x-icon"),
        (b"\x00\x00\x02\x00", "image/x-icon"),
    ];
    if first.len() >= 12 && &first[..4] == b"RIFF" && &first[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    SIGNATURES
        .iter()
        .find(|(signature, _)| first.starts_with(signature))
        .map(|&(_, mime)| mime)
}

/// The standard's binary data bytes: control characters other than tab,
/// line feed, form feed, carriage return and escape.
fn is_binary(byte: u8) -> bool {
    matches!(byte, 0x00..=0x08 | 0x0B | 0x0E..=0x1A | 0x1C..=0x1F)
}

/// The file name the image and "can't show" pages show (spec 0035, Value
/// sourcing): `Content-Disposition`'s `filename*`, else its `filename`,
/// else the URL's last non empty path segment, percent decoded, else the
/// host. A `file://` URL gives its path's file name.
pub(crate) fn file_name(disposition: Option<&str>, url: &Url) -> String {
    if let Some(name) = disposition.and_then(disposition_file_name) {
        return name;
    }
    let segment = url
        .path_segments()
        .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
        .map(percent_decode)
        .filter(|name| !name.is_empty());
    segment
        .or_else(|| url.host_str().map(str::to_owned))
        .unwrap_or_else(|| url.as_str().to_owned())
}

/// The file name a `Content-Disposition` value names: `filename*`
/// (RFC 5987, UTF-8 only) wins over `filename`.
fn disposition_file_name(disposition: &str) -> Option<String> {
    let mut plain = None;
    let mut extended = None;
    for param in disposition.split(';').skip(1) {
        let Some((name, value)) = param.split_once('=') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        if name.eq_ignore_ascii_case("filename*") {
            // `UTF-8'<language>'<percent encoded>`.
            let mut parts = value.splitn(3, '\'');
            if let (Some(charset), Some(_), Some(encoded)) =
                (parts.next(), parts.next(), parts.next())
                && charset.eq_ignore_ascii_case("utf-8")
            {
                extended = Some(percent_decode(encoded));
            }
        } else if name.eq_ignore_ascii_case("filename") {
            plain = Some(value.trim_matches('"').to_owned());
        }
    }
    extended.or(plain).filter(|name| !name.trim().is_empty())
}

/// Decodes `%XX` escapes as UTF-8, replacing invalid sequences.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|pair| std::str::from_utf8(pair).ok())
            .and_then(|pair| u8::from_str_radix(pair, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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
    fn the_type_table_picks_each_kind() {
        // Spec 0035, Type table.
        let known = |mime, disposition| classify(mime, disposition);
        assert_eq!(known("", None), Classified::Sniff);
        for (mime, kind) in [
            ("text/html", Kind::Document),
            ("application/xhtml+xml", Kind::Document),
            ("application/json", Kind::Json),
            ("text/json", Kind::Json),
            ("application/ld+json", Kind::Json),
            ("image/svg+xml", Kind::PlainText),
            ("image/png", Kind::Image),
            ("image/avif", Kind::Image),
            ("text/plain", Kind::PlainText),
            ("text/css", Kind::PlainText),
            ("text/csv", Kind::PlainText),
            ("application/xml", Kind::PlainText),
            ("application/rss+xml", Kind::PlainText),
            ("application/javascript", Kind::PlainText),
            ("application/x-javascript", Kind::PlainText),
            ("application/ecmascript", Kind::PlainText),
            ("application/octet-stream", Kind::Unviewable),
            ("application/pdf", Kind::Unviewable),
            ("video/mp4", Kind::Unviewable),
        ] {
            assert_eq!(known(mime, None), Classified::Known(kind), "{mime}");
        }
    }

    #[test]
    fn an_attachment_is_never_shown_whatever_its_type() {
        // AC-8
        for disposition in [
            "attachment",
            "Attachment; filename=a.html",
            " ATTACHMENT ;x=1",
        ] {
            assert_eq!(
                classify("text/html", Some(disposition)),
                Classified::Known(Kind::Unviewable),
                "{disposition}"
            );
        }
        assert_eq!(
            classify("image/png", Some("inline; filename=a.png")),
            Classified::Known(Kind::Image)
        );
    }

    #[test]
    fn sniffing_goes_image_html_binary_text() {
        // AC-10
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n...."), (Kind::Image, "image/png"));
        assert_eq!(sniff(b"\xFF\xD8\xFF\xE0"), (Kind::Image, "image/jpeg"));
        assert_eq!(sniff(b"GIF89a.."), (Kind::Image, "image/gif"));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), (Kind::Image, "image/webp"));
        assert_eq!(sniff(b"BM...."), (Kind::Image, "image/bmp"));
        assert_eq!(sniff(b"\0\0\x01\0\x01\0"), (Kind::Image, "image/x-icon"));
        assert_eq!(sniff(b" \n\t<p>hi").0, Kind::Document);
        assert_eq!(
            sniff(b"PK\x03\x04zip"),
            (Kind::Unviewable, "application/octet-stream")
        );
        assert_eq!(sniff(b"plain <b>\x1b[0m").0, Kind::PlainText);
        assert_eq!(sniff(b"").0, Kind::PlainText);
        // Only the first 512 bytes count.
        let mut late = vec![b'a'; SNIFF_BYTES];
        late.push(0);
        assert_eq!(sniff(&late).0, Kind::PlainText);
    }

    #[test]
    fn file_extensions_pick_the_kind() {
        // AC-11
        let kind = |path: &str| kind_for_extension(Path::new(path));
        assert_eq!(kind("/a/B.HTM"), Some((Kind::Document, "text/html")));
        assert_eq!(kind("/a/data.json"), Some((Kind::Json, "application/json")));
        assert_eq!(kind("/a/cat.PNG"), Some((Kind::Image, "image/png")));
        assert_eq!(kind("/a/cat.jpeg"), Some((Kind::Image, "image/jpeg")));
        assert_eq!(kind("/a/fav.ico"), Some((Kind::Image, "image/x-icon")));
        for text in [
            "md", "rs", "css", "js", "mjs", "xml", "svg", "csv", "toml", "yaml", "yml", "log",
        ] {
            assert_eq!(
                kind(&format!("/a/f.{text}")),
                Some((Kind::PlainText, "text/plain")),
                "{text}"
            );
        }
        assert_eq!(kind("/a/page"), None);
        assert_eq!(kind("/a/archive.zip"), None);
    }

    #[test]
    fn file_names_come_from_the_disposition_then_the_url() {
        // Spec 0035, Value sourcing, file name.
        let at = url("https://host.example/files/report%20final.pdf?x=1");
        assert_eq!(file_name(None, &at), "report final.pdf");
        assert_eq!(
            file_name(Some("attachment; filename=\"a b.zip\""), &at),
            "a b.zip"
        );
        assert_eq!(
            file_name(
                Some("attachment; filename=plain.txt; filename*=UTF-8''%E2%82%AC%20rates.csv"),
                &at
            ),
            "\u{20ac} rates.csv"
        );
        assert_eq!(file_name(Some("inline"), &at), "report final.pdf");
        assert_eq!(file_name(None, &url("https://host.example/dir/")), "dir");
        assert_eq!(
            file_name(None, &url("https://host.example/")),
            "host.example"
        );
        assert_eq!(file_name(None, &url("file:///tmp/a%20b.bin")), "a b.bin");
    }
}
