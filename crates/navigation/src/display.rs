//! How a URL shows in the address, the pill and the status bubble (spec
//! 0008, AC-3, AC-4).

use icu_properties::CodePointMapData;
use icu_properties::props::Script;
use url::{Position, Url};

/// The icon in front of the host (AC-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    /// `https`: the lock.
    Lock,
    /// `http`: the warning icon.
    Warning,
    /// `file`, `about` and anything else: the globe.
    Globe,
}

/// A URL split for display (spec 0008, Interface surface).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayUrl {
    /// The icon.
    pub icon: Security,
    /// The display host: the Unicode host with its port, a file's name, or
    /// an `about:` URL in full.
    pub host: String,
    /// What follows the host for the status bubble: path, query and
    /// fragment. Empty for a bare `/`, files and `about:` URLs.
    pub rest: String,
}

/// Splits `url` for display (AC-3, AC-4).
pub fn display_host(url: &Url) -> DisplayUrl {
    let icon = match url.scheme() {
        "https" => Security::Lock,
        "http" => Security::Warning,
        _ => Security::Globe,
    };
    let (host, rest) = match url.scheme() {
        "http" | "https" => {
            let mut host = url.host_str().map(unicode_host).unwrap_or_default();
            if let Some(port) = url.port() {
                host.push_str(&format!(":{port}"));
            }
            let rest = &url[Position::BeforePath..];
            let rest = if rest == "/" { "" } else { rest };
            (host, rest.to_owned())
        }
        "file" => (file_name(url), String::new()),
        _ => (url.as_str().to_owned(), String::new()),
    };
    DisplayUrl { icon, host, rest }
}

/// The host with each `xn--` label in Unicode, unless the label mixes two
/// or more scripts, the Common and Inherited scripts aside (AC-4). Such a
/// label keeps its punycode, so look alike hosts can't pass for others.
fn unicode_host(ascii: &str) -> String {
    ascii
        .split('.')
        .map(|label| {
            let is_punycode = label.len() > 4 && label[..4].eq_ignore_ascii_case("xn--");
            if !is_punycode {
                return label.to_owned();
            }
            match idna::domain_to_unicode(label) {
                (unicode, Ok(())) if !mixes_scripts(&unicode) => unicode,
                _ => label.to_owned(),
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// Whether `label` has letters from two or more scripts.
fn mixes_scripts(label: &str) -> bool {
    let scripts = CodePointMapData::<Script>::new();
    let mut seen: Option<Script> = None;
    for ch in label.chars() {
        let script = scripts.get(ch);
        if script == Script::Common || script == Script::Inherited {
            continue;
        }
        match seen {
            None => seen = Some(script),
            Some(first) if first != script => return true,
            Some(_) => {}
        }
    }
    false
}

/// The last path segment of a `file:` URL, percent decoded; `/` for the
/// root.
fn file_name(url: &Url) -> String {
    let name = url
        .path_segments()
        .and_then(|mut segments| segments.rfind(|s| !s.is_empty()))
        .unwrap_or("/");
    percent_decode(name)
}

/// Decodes `%XX` escapes, replacing bytes that are not UTF 8.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%'
            && let (Some(hi), Some(lo)) = (
                bytes.get(i + 1).copied().and_then(hex),
                bytes.get(i + 2).copied().and_then(hex),
            )
        {
            // Two hex digits make one byte: `hi * 16 + lo` is at most 255.
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(url: &str) -> DisplayUrl {
        display_host(&Url::parse(url).unwrap())
    }

    #[test]
    fn icons_follow_the_scheme() {
        // AC-3
        assert_eq!(shown("https://a.test/").icon, Security::Lock);
        assert_eq!(shown("http://a.test/").icon, Security::Warning);
        assert_eq!(shown("file:///a.html").icon, Security::Globe);
        assert_eq!(shown("about:start").icon, Security::Globe);
    }

    #[test]
    fn hosts_files_and_about_pages_show_their_short_form() {
        // AC-3
        let web = shown("https://www.example.com:8443/docs/a?b=1#c");
        assert_eq!(web.host, "www.example.com:8443");
        assert_eq!(web.rest, "/docs/a?b=1#c");
        assert_eq!(shown("https://example.com/").rest, "");
        assert_eq!(
            shown("file:///Users/me/My%20Page.html").host,
            "My Page.html"
        );
        assert_eq!(shown("file:///").host, "/");
        assert_eq!(shown("about:start").host, "about:start");
    }

    #[test]
    fn unicode_hosts_unless_a_label_mixes_scripts() {
        // AC-4
        assert_eq!(shown("https://bücher.de/").host, "bücher.de");
        // "аpple" with a Cyrillic а: Latin and Cyrillic in one label.
        let spoof = shown("https://\u{430}pple.com/");
        assert!(spoof.host.starts_with("xn--"), "{}", spoof.host);
        assert!(spoof.host.ends_with(".com"));
        // Digits and hyphens are Common, so they mix with anything.
        assert_eq!(shown("https://мой-сайт1.рф/").host, "мой-сайт1.рф");
    }
}
