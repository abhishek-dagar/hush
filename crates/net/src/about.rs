//! The built in `about:` pages and the error page (spec 0004, child 0001,
//! `about:` pages).
//!
//! They are plain HTML with inline CSS and no subresources. The error page
//! is filled from one template with the HTML escaped input, so a crafted
//! URL cannot inject markup (spec 0004, Security model).

use ipc::FetchError;

const START: &str = include_str!("about/start.html");
const ERROR: &str = include_str!("about/error.html");

/// The body of `about:<name>`, if it exists.
pub(crate) fn page(name: &str) -> Option<&'static str> {
    match name {
        "start" => Some(START),
        "blank" => Some(""),
        _ => None,
    }
}

/// The plain sentence the error page shows for `reason` (spec 0004, child
/// 0001, reason table).
pub fn reason_sentence(reason: &FetchError) -> String {
    match reason {
        FetchError::InvalidUrl => "This address is not a valid URL.".into(),
        FetchError::UnsupportedScheme => "This browser cannot open this kind of address.".into(),
        FetchError::DnsFailed => "The site's address could not be found.".into(),
        FetchError::ConnectFailed => "The site could not be reached.".into(),
        FetchError::TlsFailed(detail) => {
            format!("The site's security certificate is not trusted ({detail}).")
        }
        FetchError::TimedOut => "The site took too long to respond.".into(),
        FetchError::TooManyRedirects => "The site redirected too many times.".into(),
        FetchError::TooLarge => "The page is too large to load.".into(),
        FetchError::FileNotFound => "The file does not exist.".into(),
        FetchError::FileAccessDenied => "This page is not allowed to read that file.".into(),
        FetchError::UnsupportedContentType(kind) => {
            format!("This browser cannot display files of type {kind}.")
        }
        FetchError::ContentCrashed => "The page stopped working.".into(),
        // Never shown as a page; a plain sentence all the same.
        FetchError::MixedContentBlocked => "The page asked for an insecure resource.".into(),
        FetchError::Cancelled => "The load was cancelled.".into(),
    }
}

/// The error page for `input` failing with `reason`.
pub(crate) fn error_page(reason: &FetchError, input: &str) -> String {
    ERROR
        .replace("{input}", &escape(input))
        .replace("{reason}", &escape(&reason_sentence(reason)))
}

/// Escapes text for HTML element content and attribute values.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_error_page_escapes_the_input() {
        let page = error_page(&FetchError::InvalidUrl, "<script>alert('x')</script>&");
        assert!(page.contains("&lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt;&amp;"));
        assert!(!page.contains("<script>"));
        assert!(page.contains("This address is not a valid URL."));
        assert!(!page.contains("{input}") && !page.contains("{reason}"));
    }

    #[test]
    fn about_pages() {
        assert!(page("start").is_some_and(|p| p.contains("<title>")));
        assert_eq!(page("blank"), Some(""));
        assert_eq!(page("nope"), None);
    }
}
