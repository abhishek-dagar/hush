//! What Enter in the address does (spec 0008, AC-8, AC-9).
//!
//! The rules are plain data in and out: the caller passes the file check, so
//! this crate stays pure and the app shell can use it too.

use std::path::Path;

use url::{Host, Url};

/// What the address text asks for (spec 0008, Data model, `Intent`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    /// Load this URL.
    Go(Url),
    /// Search for this text with the user's engine.
    Search(String),
}

/// Reads `input` the way Enter does (AC-8): trimmed first, then a leading
/// `?` forces a search, then the shape rules decide URL or search.
/// `file_exists` says whether an absolute path names a file; the caller
/// supplies it, so nothing here touches the disk. `None` for empty input,
/// which does nothing on Enter.
pub fn interpret(input: &str, file_exists: impl Fn(&Path) -> bool) -> Option<Intent> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    if let Some(query) = input.strip_prefix('?') {
        return Some(Intent::Search(query.to_owned()));
    }
    // (1) An absolute URL with a scheme the engine loads.
    if let Ok(url) = Url::parse(input)
        && matches!(url.scheme(), "http" | "https" | "file" | "about")
    {
        return Some(Intent::Go(url));
    }
    // (2) An absolute path to a file that exists.
    let path = Path::new(input);
    if path.is_absolute() && file_exists(path) {
        return Url::from_file_path(path).ok().map(Intent::Go);
    }
    // (3) A host shaped input: no spaces, and `https://` in front parses to
    // a host that is localhost, an address literal or a dotted name, or the
    // input names a port.
    if !input.chars().any(char::is_whitespace)
        && let Ok(url) = Url::parse(&format!("https://{input}"))
    {
        let host = match url.host() {
            Some(Host::Domain(name)) => name == "localhost" || name.contains('.'),
            Some(Host::Ipv4(_) | Host::Ipv6(_)) => true,
            None => false,
        };
        if host || url.port().is_some() || names_a_port(input) {
            return Some(Intent::Go(url));
        }
    }
    Some(Intent::Search(input.to_owned()))
}

/// Whether the text ends in a port (`host:8080`), for the inputs `url`
/// hides the default port of (`host:443` over `https`).
fn names_a_port(input: &str) -> bool {
    input
        .rsplit_once(':')
        .is_some_and(|(_, port)| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn go(input: &str) -> Option<String> {
        match interpret(input, |_| false) {
            Some(Intent::Go(url)) => Some(url.to_string()),
            _ => None,
        }
    }

    fn search(input: &str) -> Option<String> {
        match interpret(input, |_| false) {
            Some(Intent::Search(text)) => Some(text),
            _ => None,
        }
    }

    #[test]
    fn a_scheme_url_loads_as_it_is() {
        // AC-8 (1)
        assert_eq!(
            go("https://example.com/a?b=1#c").as_deref(),
            Some("https://example.com/a?b=1#c")
        );
        assert_eq!(go("about:start").as_deref(), Some("about:start"));
        assert_eq!(
            go("file:///Users/me/page.html").as_deref(),
            Some("file:///Users/me/page.html")
        );
        assert_eq!(
            go("HTTP://EXAMPLE.COM/").as_deref(),
            Some("http://example.com/")
        );
    }

    #[test]
    fn host_shaped_input_loads() {
        // AC-8 (3)
        assert_eq!(go("example.com").as_deref(), Some("https://example.com/"));
        assert_eq!(
            go("example.com/docs?x=1").as_deref(),
            Some("https://example.com/docs?x=1")
        );
        assert_eq!(go("localhost").as_deref(), Some("https://localhost/"));
        assert_eq!(
            go("localhost:8080").as_deref(),
            Some("https://localhost:8080/")
        );
        assert_eq!(
            go("192.168.1.1/admin").as_deref(),
            Some("https://192.168.1.1/admin")
        );
        assert_eq!(go("[::1]:3000").as_deref(), Some("https://[::1]:3000/"));
        assert_eq!(
            go("intranet:8080").as_deref(),
            Some("https://intranet:8080/")
        );
    }

    #[test]
    fn words_and_bare_names_search() {
        // AC-8
        assert_eq!(
            search("rust borrow checker").as_deref(),
            Some("rust borrow checker")
        );
        assert_eq!(search("intranet").as_deref(), Some("intranet"));
        assert_eq!(search("what is 2 + 2?").as_deref(), Some("what is 2 + 2?"));
    }

    #[test]
    fn a_leading_question_mark_always_searches() {
        // AC-8
        assert_eq!(search("?example.com").as_deref(), Some("example.com"));
        assert_eq!(search(" ?rust").as_deref(), Some("rust"));
    }

    #[test]
    fn empty_input_does_nothing() {
        // AC-8
        assert_eq!(interpret("", |_| false), None);
        assert_eq!(interpret("   \t\n", |_| false), None);
    }

    #[test]
    fn an_absolute_path_that_exists_loads_as_a_file() {
        // AC-8 (2)
        let existing = |path: &Path| path == Path::new("/tmp/page.html");
        assert_eq!(
            interpret("/tmp/page.html", existing),
            Some(Intent::Go(Url::parse("file:///tmp/page.html").unwrap()))
        );
        // The same path when it does not exist is neither a URL nor a
        // search shaped host, so it searches.
        assert_eq!(
            interpret("/tmp/gone.html", existing),
            Some(Intent::Search("/tmp/gone.html".to_owned()))
        );
    }

    #[test]
    fn surrounding_space_is_trimmed_before_reading() {
        // AC-8
        assert_eq!(
            go("  example.com  ").as_deref(),
            Some("https://example.com/")
        );
        assert_eq!(
            search("  rust borrow checker  ").as_deref(),
            Some("rust borrow checker")
        );
    }
}
