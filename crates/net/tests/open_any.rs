//! Opening any response (spec 0035): every row of the type table and the
//! sniff order over HTTP and `file://`, attachments, the "can't show"
//! page's file name and size, and the image page's single request.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests: a panic is a failed test"
)]

mod support;

use std::time::Duration;

use ipc::{DocumentKind, FetchError, FetchId, ToContent};
use support::{Reply, Tls, bytes_of, collect_within, commit, kind, load, serve, start};
use url::Url;

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

fn unviewable(file_name: &str, mime: &str, size: Option<u64>) -> DocumentKind {
    DocumentKind::Unviewable {
        file_name: file_name.into(),
        mime: mime.into(),
        size,
    }
}

fn image(file_name: &str, mime: &str) -> DocumentKind {
    DocumentKind::Image {
        file_name: file_name.into(),
        mime: mime.into(),
    }
}

#[test]
fn every_type_table_row_over_http() {
    // AC-1, AC-3, AC-4, AC-8, AC-13
    let server = serve(Tls::Plain, |seen| {
        let mime = seen.path.trim_start_matches("/t/").replace('~', "/");
        Reply::ok(&mime, "body")
    });
    let (net, rx) = start();
    let at = |mime: &str| server.url("http", &format!("/t/{}", mime.replace('/', "~")));
    for (mime, expected) in [
        ("text/html", DocumentKind::Document),
        ("application/xhtml+xml", DocumentKind::Document),
        ("application/json", DocumentKind::Json),
        ("text/json", DocumentKind::Json),
        ("application/geo+json", DocumentKind::Json),
        ("image/svg+xml", DocumentKind::PlainText),
        ("text/plain", DocumentKind::PlainText),
        ("text/css", DocumentKind::PlainText),
        ("text/javascript", DocumentKind::PlainText),
        ("application/xml", DocumentKind::PlainText),
        ("application/atom+xml", DocumentKind::PlainText),
        ("application/javascript", DocumentKind::PlainText),
        ("application/x-javascript", DocumentKind::PlainText),
        ("application/ecmascript", DocumentKind::PlainText),
        ("image/png", image("image~png", "image/png")),
        ("image/avif", image("image~avif", "image/avif")),
    ] {
        assert_eq!(kind(&net, &rx, &at(mime)), expected, "{mime}");
    }
    for mime in ["application/octet-stream", "application/zip", "video/mp4"] {
        let file_name = mime.replace('/', "~");
        assert_eq!(
            kind(&net, &rx, &at(mime)),
            // The test server sends no `Content-Length`.
            unviewable(&file_name, mime, None),
            "{mime}"
        );
    }
    net.shutdown();
}

#[test]
fn attachments_never_show_and_name_their_file() {
    // AC-8, AC-12: the file name is passed through as text, never markup.
    let server = serve(Tls::Plain, |seen| match seen.path.as_str() {
        "/script" => Reply::html("<p>page</p>")
            .header("content-length", "11")
            .header(
                "content-disposition",
                "attachment; filename=\"<script>alert(1)</script>.bin\"",
            ),
        "/utf8" => Reply::ok("", "x").without_type().header(
            "content-disposition",
            "attachment; filename=a.txt; filename*=UTF-8''r%C3%A9sum%C3%A9.pdf",
        ),
        _ => Reply::ok("application/pdf", "x"),
    });
    let (net, rx) = start();
    let messages = load(&net, &rx, &server.url("http", "/script"));
    assert_eq!(
        commit(&messages).3,
        unviewable("<script>alert(1)</script>.bin", "text/html", Some(11))
    );
    assert!(
        bytes_of(&messages, FetchId::DOCUMENT).is_empty(),
        "no body is sent"
    );
    let messages = load(&net, &rx, &server.url("http", "/utf8"));
    let (_, content_type, _, committed) = commit(&messages);
    assert_eq!(content_type, "application/octet-stream");
    assert_eq!(
        committed,
        unviewable("r\u{e9}sum\u{e9}.pdf", "application/octet-stream", None)
    );
    let named = kind(&net, &rx, &server.url("http", "/files/report%20Q3.pdf"));
    assert_eq!(named, unviewable("report Q3.pdf", "application/pdf", None));
    net.shutdown();
}

#[test]
fn a_file_that_is_not_shown_is_not_read() {
    // AC-8: a 50 MB PDF commits at once, over the document size limit,
    // with the body still unsent.
    let server = serve(Tls::Plain, |_| {
        Reply::ok("application/pdf", "%PDF-")
            .header("content-length", &(50 * 1024 * 1024).to_string())
            .stalled()
    });
    let (net, rx) = start();
    let messages = collect_after_load(&net, &rx, &server.url("http", "/big.pdf"));
    assert_eq!(
        commit(&messages).3,
        unviewable("big.pdf", "application/pdf", Some(50 * 1024 * 1024))
    );
    assert!(matches!(
        messages.last().unwrap().1,
        ToContent::FetchDone { .. }
    ));
    assert!(bytes_of(&messages, FetchId::DOCUMENT).is_empty());
    net.shutdown();
}

/// Loads `url` and gives its messages, waiting at most two seconds each.
fn collect_after_load(
    net: &net::Net,
    rx: &std::sync::mpsc::Receiver<(std::time::Instant, ToContent)>,
    url: &str,
) -> Vec<(std::time::Instant, ToContent)> {
    net.handle().navigate(
        support::PAGE,
        support::NAV,
        Url::parse(url).unwrap(),
        ipc::CommitParams::default(),
    );
    collect_within(rx, Duration::from_secs(2))
}

#[test]
fn untyped_bodies_are_sniffed_image_html_binary_text() {
    // AC-10
    let server = serve(Tls::Plain, |seen| {
        let body: Vec<u8> = match seen.path.as_str() {
            "/cat" => PNG.to_vec(),
            "/page" => b"  <p>hi".to_vec(),
            "/archive.zip" => b"PK\x03\x04\x14\0\0\0".to_vec(),
            _ => b"just words".to_vec(),
        };
        Reply::ok("", body).without_type()
    });
    let (net, rx) = start();
    let at = |path: &str| server.url("http", path);
    let messages = load(&net, &rx, &at("/cat"));
    assert_eq!(commit(&messages).3, image("cat", "image/png"));
    assert_eq!(commit(&messages).1, "image/png");
    assert_eq!(
        bytes_of(&messages, FetchId::DOCUMENT),
        PNG,
        "the sniffed bytes reach the page"
    );
    assert_eq!(kind(&net, &rx, &at("/page")), DocumentKind::Document);
    assert_eq!(
        kind(&net, &rx, &at("/archive.zip")),
        unviewable("archive.zip", "application/octet-stream", None)
    );
    assert_eq!(kind(&net, &rx, &at("/notes")), DocumentKind::PlainText);
    net.shutdown();
}

#[test]
fn an_image_page_makes_one_request_and_streams_its_bytes() {
    // AC-6
    let server = serve(Tls::Plain, |_| Reply::ok("image/png", PNG));
    let (net, rx) = start();
    let messages = load(&net, &rx, &server.url("http", "/cat.png"));
    assert_eq!(commit(&messages).3, image("cat.png", "image/png"));
    assert_eq!(bytes_of(&messages, FetchId::DOCUMENT), PNG);
    assert_eq!(server.seen().len(), 1);
    net.shutdown();
}

#[test]
fn file_urls_are_typed_by_extension_then_sniffed() {
    // AC-11, AC-13
    let dir = std::env::temp_dir().join(format!("net-open-any-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.json"), "{\"a\":1}").unwrap();
    std::fs::write(dir.join("b.png"), PNG).unwrap();
    std::fs::write(dir.join("c.unknownext"), b"\0\x01\x02binary").unwrap();
    std::fs::write(dir.join("d.yaml"), "a: 1").unwrap();
    std::fs::write(dir.join("e.svg"), "<svg/>").unwrap();
    std::fs::write(dir.join("f"), PNG).unwrap();
    let (net, rx) = start();
    let file = |name: &str| Url::from_file_path(dir.join(name)).unwrap().to_string();
    assert_eq!(kind(&net, &rx, &file("a.json")), DocumentKind::Json);
    let messages = load(&net, &rx, &file("b.png"));
    assert_eq!(commit(&messages).3, image("b.png", "image/png"));
    assert_eq!(bytes_of(&messages, FetchId::DOCUMENT), PNG);
    assert_eq!(
        kind(&net, &rx, &file("c.unknownext")),
        unviewable("c.unknownext", "application/octet-stream", Some(9))
    );
    assert_eq!(kind(&net, &rx, &file("d.yaml")), DocumentKind::PlainText);
    assert_eq!(kind(&net, &rx, &file("e.svg")), DocumentKind::PlainText);
    assert_eq!(kind(&net, &rx, &file("f")), image("f", "image/png"));
    let folder = kind(&net, &rx, Url::from_directory_path(&dir).unwrap().as_str());
    assert!(
        matches!(
            folder,
            DocumentKind::Error {
                reason: FetchError::UnsupportedContentType(ref mime),
                ..
            } if mime == "inode/directory"
        ),
        "a folder still shows the error page: {folder:?}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
    net.shutdown();
}
