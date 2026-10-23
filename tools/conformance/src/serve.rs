//! Answers every fetch a page makes from the snapshot folder, under the
//! origin `http://web-platform.test:8000/` (spec 0005, AC-4). Nothing goes
//! over the network.

use std::path::{Path, PathBuf};

use url::Url;

/// The origin every test page loads under.
pub const ORIGIN: &str = "http://web-platform.test:8000/";

/// One answered fetch.
#[derive(Debug)]
pub struct Response {
    /// 200, or 404 for a missing file or a URL off the origin.
    pub status: u16,
    /// `content-type`, then the sidecar's headers.
    pub headers: Vec<(String, String)>,
    /// The file's bytes; empty for a 404.
    pub body: Vec<u8>,
}

/// The URL of `path`, a path from the WPT root.
pub fn url_for(path: &str) -> String {
    format!("{ORIGIN}{path}")
}

/// The path from the WPT root that `url` names, if it is on the origin and
/// stays inside the root. The query and fragment are dropped.
pub fn path_for(url: &Url) -> Option<String> {
    if url.scheme() != "http" || url.host_str() != Some("web-platform.test") {
        return None;
    }
    if url.port() != Some(8000) {
        return None;
    }
    let path = percent_decode(url.path().trim_start_matches('/'))?;
    let safe = !path.is_empty()
        && !path.contains('\\')
        && path.split('/').all(|s| !s.is_empty() && s != "." && s != "..");
    safe.then_some(path)
}

/// Answers `url` from `root`.
pub fn serve(root: &Path, url: &str) -> Response {
    let file = Url::parse(url)
        .ok()
        .as_ref()
        .and_then(path_for)
        .map(|path| root.join(path))
        .filter(|file| file.is_file());
    let Some(file) = file else {
        return Response {
            status: 404,
            headers: vec![("content-type".to_owned(), "text/plain".to_owned())],
            body: Vec::new(),
        };
    };
    match std::fs::read(&file) {
        Ok(body) => Response {
            status: 200,
            headers: headers(&file),
            body,
        },
        Err(_) => Response {
            status: 404,
            headers: Vec::new(),
            body: Vec::new(),
        },
    }
}

/// The headers for `file`: its content type, replaced by the sidecar's
/// `Content-Type` when it has one, then the sidecar's other headers.
pub fn headers(file: &Path) -> Vec<(String, String)> {
    let mut headers = vec![("content-type".to_owned(), content_type(file).to_owned())];
    for (name, value) in sidecar(file) {
        let name = name.to_ascii_lowercase();
        match headers.iter_mut().find(|(n, _)| *n == name) {
            Some(slot) if name == "content-type" => slot.1 = value,
            _ => headers.push((name, value)),
        }
    }
    headers
}

/// The content type by extension (spec 0005, Value sourcing). Images and
/// fonts are sniffed by the engine.
pub fn content_type(file: &Path) -> &'static str {
    let extension = file
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("html" | "htm" | "xht" | "xhtml") => "text/html",
        Some("css") => "text/css",
        Some("js") => "text/javascript",
        _ => "application/octet-stream",
    }
}

/// The `Name: value` lines of `<file>.headers`, if it exists.
fn sidecar(file: &Path) -> Vec<(String, String)> {
    let mut name = file.as_os_str().to_owned();
    name.push(".headers");
    let Ok(text) = std::fs::read_to_string(PathBuf::from(name)) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
        .filter(|(name, _)| !name.is_empty())
        .collect()
}

/// The `charset` parameter of a `Content-Type` value, if any.
pub fn charset(headers: &[(String, String)]) -> Option<String> {
    let (_, value) = headers.iter().find(|(name, _)| name == "content-type")?;
    value.split(';').skip(1).find_map(|param| {
        let (key, value) = param.split_once('=')?;
        key.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| value.trim().trim_matches('"').to_owned())
    })
}

/// Decodes `%XX` escapes; `None` when they do not decode to UTF-8.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b == b'%'
            && let Some(hex) = text.get(i + 1..i + 3)
            && let Ok(value) = u8::from_str_radix(hex, 16)
        {
            out.push(value);
            i += 3;
        } else {
            out.push(b);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(url: &str) -> Option<String> {
        path_for(&Url::parse(url).unwrap())
    }

    #[test]
    fn only_paths_on_the_origin_and_inside_the_root_are_served() {
        assert_eq!(path(&url_for("css/a b.html")).as_deref(), Some("css/a b.html"));
        assert_eq!(
            path("http://web-platform.test:8000/css/x.html?q#f").as_deref(),
            Some("css/x.html")
        );
        assert_eq!(path("http://web-platform.test:8000/a/../b.css").as_deref(), Some("b.css"));
        assert_eq!(path("http://web-platform.test:8000/a/%2e%2e/%2e%2e/etc"), None);
        assert_eq!(path("http://web-platform.test:8000/a/..%2f..%2fetc"), None);
        assert_eq!(path("http://www.w3.org/css.html"), None);
        assert_eq!(path("http://web-platform.test:8001/x.html"), None);
    }

    #[test]
    fn content_types_follow_the_extension() {
        assert_eq!(content_type(Path::new("a.xht")), "text/html");
        assert_eq!(content_type(Path::new("a.CSS")), "text/css");
        assert_eq!(content_type(Path::new("a.js")), "text/javascript");
        assert_eq!(content_type(Path::new("a.png")), "application/octet-stream");
        assert_eq!(content_type(Path::new("a")), "application/octet-stream");
    }

    #[test]
    fn the_charset_comes_from_the_content_type() {
        let headers = [(
            "content-type".to_owned(),
            "text/html; charset=\"ISO-8859-1\"".to_owned(),
        )];
        assert_eq!(charset(&headers).as_deref(), Some("ISO-8859-1"));
    }
}
