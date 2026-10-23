//! The chrome's files, embedded in the binary (spec 0003, key invariants:
//! no `file:` and no network). Chrome documents live under [`ORIGIN`], and
//! their `Fetch` requests are answered from this table.

/// The URL every chrome document and its subresources live under.
pub const ORIGIN: &str = "chrome://ui/";

/// An embedded file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Asset {
    /// Its bytes.
    pub bytes: &'static [u8],
    /// Its MIME type, sent as `content-type`.
    pub mime: &'static str,
}

const ASSETS: &[(&str, &[u8], &str)] = &[
    (
        "frame.html",
        include_bytes!("../assets/frame.html"),
        "text/html",
    ),
    (
        "tokens.css",
        include_bytes!("../assets/tokens.css"),
        "text/css",
    ),
    (
        "controls.css",
        include_bytes!("../assets/controls.css"),
        "text/css",
    ),
    (
        "frame.css",
        include_bytes!("../assets/frame.css"),
        "text/css",
    ),
    (
        "settings.html",
        include_bytes!("../assets/settings.html"),
        "text/html",
    ),
    (
        "settings.css",
        include_bytes!("../assets/settings.css"),
        "text/css",
    ),
    (
        "icons.ttf",
        include_bytes!("../assets/icons.ttf"),
        "font/ttf",
    ),
];

/// The gallery ships in dev builds only (spec 0003, configuration).
#[cfg(debug_assertions)]
const DEV_ASSETS: &[(&str, &[u8], &str)] = &[
    (
        "gallery.html",
        include_bytes!("../assets/gallery.html"),
        "text/html",
    ),
    (
        "gallery.css",
        include_bytes!("../assets/gallery.css"),
        "text/css",
    ),
];
#[cfg(not(debug_assertions))]
const DEV_ASSETS: &[(&str, &[u8], &str)] = &[];

/// The embedded file at `url`, or `None` for anything outside [`ORIGIN`]
/// or not in the table. A query or fragment is ignored.
pub fn asset(url: &str) -> Option<Asset> {
    let path = url.strip_prefix(ORIGIN)?;
    let path = path.split(['?', '#']).next().unwrap_or(path);
    ASSETS
        .iter()
        .chain(DEV_ASSETS)
        .find(|(name, _, _)| *name == path)
        .map(|&(_, bytes, mime)| Asset { bytes, mime })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chrome_urls_find_their_files_and_nothing_else_does() {
        let tokens = asset("chrome://ui/tokens.css").unwrap();
        assert_eq!(tokens.mime, "text/css");
        assert!(!tokens.bytes.is_empty());
        assert_eq!(asset("chrome://ui/icons.ttf?v=1").unwrap().mime, "font/ttf");
        assert_eq!(asset("chrome://ui/missing.css"), None);
        assert_eq!(asset("file:///etc/passwd"), None);
        assert_eq!(asset("https://example.com/tokens.css"), None);
        assert_eq!(asset("chrome://ui/../tokens.css"), None);
    }
}
