//! Which loads a page may start (spec 0008, AC-18). Typed input is not
//! limited by these rules; only loads a link or a script asked for are.

use ipc::MAX_PAGE_URL_BYTES;
use url::Url;

/// Why a page started load was refused (spec 0008, AC-18).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refused {
    /// The URL is longer than [`MAX_PAGE_URL_BYTES`].
    #[error("URL of {0} bytes is over the limit")]
    TooLong(usize),
    /// The URL does not parse as an absolute URL.
    #[error("not an absolute URL")]
    Invalid,
    /// The document's scheme may not load the target's.
    #[error("a {from} document may not load a {to} URL")]
    Scheme {
        /// The document's scheme.
        from: String,
        /// The target's scheme.
        to: String,
    },
}

/// Parses a URL a page sent, checking its length before anything else.
pub fn parse_page_url(url: &str) -> Result<Url, Refused> {
    if url.len() > MAX_PAGE_URL_BYTES {
        return Err(Refused::TooLong(url.len()));
    }
    Url::parse(url).map_err(|_| Refused::Invalid)
}

/// Whether a page showing `document` may load `target` (AC-18). From an
/// `http` or `https` document (and any other, such as `about:` pages and
/// the error page): `http`, `https` or `about:blank`. From a `file:`
/// document: `http`, `https`, `file` or any `about:` URL. `document` is
/// the URL `net` recorded for the navigation, never the page's claim.
pub fn page_may_load(document: &Url, target: &Url) -> Result<(), Refused> {
    if target.as_str().len() > MAX_PAGE_URL_BYTES {
        return Err(Refused::TooLong(target.as_str().len()));
    }
    let allowed = match (document.scheme(), target.scheme()) {
        (_, "http" | "https") => true,
        ("file", "file" | "about") => true,
        (_, "about") => target.as_str() == "about:blank",
        _ => false,
    };
    if allowed {
        Ok(())
    } else {
        Err(Refused::Scheme {
            from: document.scheme().to_owned(),
            to: target.scheme().to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn may(document: &str, target: &str) -> bool {
        let document = Url::parse(document).unwrap();
        let target = Url::parse(target).unwrap();
        page_may_load(&document, &target).is_ok()
    }

    #[test]
    fn web_pages_reach_the_web_and_about_blank_only() {
        // AC-18
        assert!(may("https://a.test/", "http://b.test/"));
        assert!(may("http://a.test/", "https://b.test/x"));
        assert!(may("https://a.test/", "about:blank"));
        assert!(!may("https://a.test/", "file:///etc/passwd"));
        assert!(!may("https://a.test/", "about:start"));
        assert!(!may("https://a.test/", "data:text/html,hi"));
        assert!(!may("about:start", "file:///etc/passwd"));
    }

    #[test]
    fn file_pages_reach_files_and_about_pages_too() {
        // AC-18
        assert!(may("file:///docs/a.html", "file:///docs/b.html"));
        assert!(may("file:///docs/a.html", "about:start"));
        assert!(may("file:///docs/a.html", "https://b.test/"));
        assert!(!may("file:///docs/a.html", "data:text/html,hi"));
    }

    #[test]
    fn urls_over_2_mb_are_refused_before_parsing() {
        // AC-18
        let long = format!("https://a.test/{}", "a".repeat(MAX_PAGE_URL_BYTES));
        assert!(matches!(parse_page_url(&long), Err(Refused::TooLong(_))));
        assert_eq!(parse_page_url("not a url"), Err(Refused::Invalid));
        assert!(parse_page_url("https://a.test/").is_ok());
    }
}
