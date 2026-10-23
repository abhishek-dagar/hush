//! What the browser opens first, from its command line (spec 0004, child
//! 0001, CLI input, AC-1).

use std::path::Path;

use url::Url;

/// The first page to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StartPage {
    /// Load this URL.
    Url(Url),
    /// The input is no URL at all: show the error page naming it.
    Invalid(String),
}

impl StartPage {
    /// The page as the error page names it: the URL, or the input as typed.
    pub(crate) fn input(&self) -> String {
        match self {
            Self::Url(url) => url.to_string(),
            Self::Invalid(input) => input.clone(),
        }
    }
}

/// Reads the start input the way AC-1 says: an absolute URL as itself, an
/// existing file as its `file://` URL, anything else as `https://<input>`.
/// No input opens `about:start`.
pub(crate) fn start_page(input: Option<&str>) -> StartPage {
    let Some(input) = input.map(str::trim).filter(|i| !i.is_empty()) else {
        return Url::parse("about:start")
            .map_or_else(|_| StartPage::Invalid(String::new()), StartPage::Url);
    };
    let as_url = Url::parse(input).ok();
    // A Windows path such as `C:\page.html` parses as a URL with the scheme
    // `c`; an existing file wins over a one letter scheme.
    let drive_letter = as_url.as_ref().is_some_and(|url| url.scheme().len() == 1);
    if let Some(url) = as_url
        && !drive_letter
    {
        return StartPage::Url(url);
    }
    let path = Path::new(input);
    if path.exists()
        && let Some(url) = path
            .canonicalize()
            .ok()
            .and_then(|path| Url::from_file_path(path).ok())
    {
        return StartPage::Url(url);
    }
    match Url::parse(&format!("https://{input}")) {
        Ok(url) => StartPage::Url(url),
        Err(_) => StartPage::Invalid(input.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(input: Option<&str>) -> String {
        match start_page(input) {
            StartPage::Url(url) => url.to_string(),
            StartPage::Invalid(input) => format!("invalid: {input}"),
        }
    }

    #[test]
    fn urls_files_bare_hosts_and_nothing() {
        // AC-1
        assert_eq!(url(None), "about:start");
        assert_eq!(url(Some("  ")), "about:start");
        assert_eq!(url(Some("http://example.com/a")), "http://example.com/a");
        assert_eq!(url(Some("example.com")), "https://example.com/");
        assert_eq!(
            url(Some("example.com/path?q=1")),
            "https://example.com/path?q=1"
        );

        let file = std::env::temp_dir().join(format!("start-url-{}.html", std::process::id()));
        std::fs::write(&file, "<p>x</p>").unwrap();
        let expected = Url::from_file_path(file.canonicalize().unwrap()).unwrap();
        assert_eq!(url(file.to_str()), expected.to_string());
        std::fs::remove_file(&file).unwrap();

        assert_eq!(url(Some("exa mple")), "invalid: exa mple");
    }
}
