//! Navigations end to end on the network thread, against a local server
//! (spec 0004, AC-1, AC-5, AC-10).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a panic is a failed test"
)]

mod support;

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use ipc::{CommitParams, DocumentKind, FetchError, ToContent};
use support::{NAV, PAGE, body, collect, commit, start};
use url::Url;

#[test]
fn the_document_streams_to_the_page_as_it_arrives() {
    // AC-5: the server sends half the page, waits, then the rest. The first
    // half reaches the page before the wait ends.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let first = "<!DOCTYPE html><p>first half</p>";
    let second = "<p>second half</p>";
    let pause = Duration::from_millis(800);
    let (request_tx, request_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = String::new();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" || line.is_empty() {
                break;
            }
            request.push_str(&line);
        }
        request_tx.send(request).unwrap();
        let length = first.len() + second.len();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
             Content-Length: {length}\r\n\r\n{first}"
        )
        .unwrap();
        stream.flush().unwrap();
        thread::sleep(pause);
        let resumed = Instant::now();
        stream.write_all(second.as_bytes()).unwrap();
        resumed
    });

    let (net, rx) = start();
    let url = format!("http://127.0.0.1:{port}/article.html");
    net.handle().navigate(
        PAGE,
        NAV,
        Url::parse(&url).unwrap(),
        CommitParams::default(),
    );
    let messages = collect(&rx);
    let resumed = server.join().unwrap();

    let (committed, content_type, charset, kind) = commit(&messages);
    assert_eq!(committed, url);
    assert_eq!(content_type, "text/html");
    assert_eq!(charset.as_deref(), Some("utf-8"));
    assert_eq!(kind, DocumentKind::Document);
    assert_eq!(body(&messages), format!("{first}{second}"));
    let (first_chunk_at, _) = messages
        .iter()
        .find(|(_, m)| matches!(m, ToContent::FetchChunk { .. }))
        .unwrap();
    assert!(
        *first_chunk_at < resumed,
        "the first half arrived before the server sent the rest"
    );
    assert!(matches!(
        messages.last().unwrap().1,
        ToContent::FetchDone { .. }
    ));

    let request = request_rx.recv().unwrap();
    assert!(
        request.starts_with("GET /article.html HTTP/1.1\r\n"),
        "{request}"
    );
    assert!(
        request.contains("user-agent: Mozilla/5.0 (") && request.contains("RustBrowser/0.0.0-test"),
        "{request}"
    );
    assert!(request.contains("accept: text/html,"), "{request}");
    net.shutdown();
}

#[test]
fn about_start_and_the_error_page_are_built_in() {
    // AC-1, AC-10
    let (net, rx) = start();
    net.handle().navigate(
        PAGE,
        NAV,
        Url::parse("about:start").unwrap(),
        CommitParams::default(),
    );
    let messages = collect(&rx);
    assert_eq!(commit(&messages).0, "about:start");
    assert!(body(&messages).contains("<title>Start</title>"));

    net.handle().navigate_to_error(
        PAGE,
        NAV,
        FetchError::InvalidUrl,
        "a b<c>".into(),
        CommitParams::default(),
    );
    let messages = collect(&rx);
    let (url, _, _, kind) = commit(&messages);
    assert_eq!(url, "about:error");
    assert_eq!(
        kind,
        DocumentKind::Error {
            reason: FetchError::InvalidUrl,
            input: "a b<c>".into()
        }
    );
    let page = body(&messages);
    assert!(page.contains("a b&lt;c&gt;") && page.contains("not a valid URL"));

    for (input, reason) in [
        ("ftp://example.com/", FetchError::UnsupportedScheme),
        ("about:nothing", FetchError::InvalidUrl),
    ] {
        net.handle().navigate(
            PAGE,
            NAV,
            Url::parse(input).unwrap(),
            CommitParams::default(),
        );
        let messages = collect(&rx);
        let (url, _, _, kind) = commit(&messages);
        assert_eq!(url, "about:error");
        assert_eq!(
            kind,
            DocumentKind::Error {
                reason,
                input: input.into()
            }
        );
    }
    net.shutdown();
}

#[test]
fn file_documents_load_and_missing_ones_show_the_error_page() {
    // AC-1
    let dir = std::env::temp_dir().join(format!("net-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("page.html");
    let html = "<p>from a file</p>".repeat(10_000);
    std::fs::write(&path, &html).unwrap();
    let url = Url::from_file_path(&path).unwrap();

    let (net, rx) = start();
    net.handle()
        .navigate(PAGE, NAV, url.clone(), CommitParams::default());
    let messages = collect(&rx);
    let (committed, content_type, _, _) = commit(&messages);
    assert_eq!(committed, url.as_str());
    assert_eq!(content_type, "text/html");
    assert_eq!(body(&messages), html, "a big file arrives whole, in chunks");
    assert!(
        messages
            .iter()
            .filter(|(_, m)| matches!(m, ToContent::FetchChunk { .. }))
            .count()
            > 1
    );

    let missing = Url::from_file_path(dir.join("missing.html")).unwrap();
    net.handle()
        .navigate(PAGE, NAV, missing, CommitParams::default());
    let messages = collect(&rx);
    assert!(matches!(
        commit(&messages).3,
        DocumentKind::Error {
            reason: FetchError::FileNotFound,
            ..
        }
    ));
    std::fs::remove_dir_all(&dir).unwrap();
    net.shutdown();
}

#[test]
fn a_refused_connection_shows_the_error_page() {
    let port = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let (net, rx) = start();
    let url = format!("http://127.0.0.1:{port}/");
    net.handle().navigate(
        PAGE,
        NAV,
        Url::parse(&url).unwrap(),
        CommitParams::default(),
    );
    let messages = collect(&rx);
    assert_eq!(
        commit(&messages).3,
        DocumentKind::Error {
            reason: FetchError::ConnectFailed,
            input: url
        }
    );
    net.shutdown();
}
