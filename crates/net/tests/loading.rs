//! Loading against the local test server: protocols, compression,
//! redirects, TLS, limits, content types and the subresource policy (spec
//! 0004, AC-2 to AC-4, AC-7 to AC-10).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests: a panic is a failed test"
)]

mod support;

use std::io::Write;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use flate2::Compression;
use flate2::write::{GzEncoder, ZlibEncoder};
use ipc::{CommitParams, Destination, DocumentKind, FetchError, FetchId, NavigationId, ToContent};
use net::{FetchRequest, Net};
use support::{
    H1_ONLY, H2_AND_H1, NAV, PAGE, Reply, Tls, body, bytes_of, collect, commit, kind, load, serve,
    start,
};
use url::Url;

const TEXT: &str = "<!DOCTYPE html><p>the article text</p>";

fn error(kind: DocumentKind) -> FetchError {
    match kind {
        DocumentKind::Error { reason, .. } => reason,
        other => panic!("expected the error page, got {other:?}"),
    }
}

/// Asks for subresource `fetch` of the current page and gives its messages.
fn fetch(
    net: &Net,
    rx: &Receiver<(Instant, ToContent)>,
    fetch: u32,
    url: &str,
    destination: Destination,
) -> Vec<(Instant, ToContent)> {
    net.handle().fetch(FetchRequest {
        page: PAGE,
        navigation: NAV,
        fetch: FetchId(fetch),
        url: url.to_owned(),
        destination,
    });
    collect(rx)
}

/// The reason a subresource's messages ended with `FetchFailed`, if any.
fn failure(messages: &[(Instant, ToContent)]) -> Option<FetchError> {
    match &messages.last().unwrap().1 {
        ToContent::FetchFailed { reason, .. } => Some(reason.clone()),
        _ => None,
    }
}

#[test]
fn https_speaks_h2_by_alpn_and_falls_back_to_http1() {
    // AC-2
    let version = |seen: &support::Seen| Reply::html(&format!("{:?}", seen.version));
    let h2 = serve(Tls::Cert("localhost", H2_AND_H1), version);
    let h1 = serve(Tls::Cert("localhost", H1_ONLY), version);
    let plain = serve(Tls::Plain, version);
    let (net, rx) = start();
    for (server, scheme, expected) in [
        (&h2, "https", "HTTP/2.0"),
        (&h1, "https", "HTTP/1.1"),
        (&plain, "http", "HTTP/1.1"),
    ] {
        let messages = load(&net, &rx, &server.url(scheme, "/"));
        assert_eq!(commit(&messages).3, DocumentKind::Document);
        assert_eq!(
            body(&messages),
            expected,
            "{scheme} on port {}",
            server.port
        );
    }
    let seen = &h2.seen()[0];
    assert_eq!(seen.header("accept-encoding"), Some("gzip, deflate, br"));
    assert!(
        seen.header("user-agent")
            .unwrap()
            .contains("RustBrowser/0.0.0-test")
    );
    net.shutdown();
}

#[test]
fn gzip_deflate_and_brotli_bodies_are_decoded() {
    // AC-2
    let server = serve(Tls::Cert("localhost", H2_AND_H1), |seen| {
        let mut encoded = Vec::new();
        match seen.path.as_str() {
            "/gzip" => {
                let mut encoder = GzEncoder::new(&mut encoded, Compression::default());
                encoder.write_all(TEXT.as_bytes()).unwrap();
                encoder.finish().unwrap();
            }
            "/deflate" => {
                let mut encoder = ZlibEncoder::new(&mut encoded, Compression::default());
                encoder.write_all(TEXT.as_bytes()).unwrap();
                encoder.finish().unwrap();
            }
            _ => {
                let mut encoder = brotli::CompressorWriter::new(&mut encoded, 4096, 5, 22);
                encoder.write_all(TEXT.as_bytes()).unwrap();
                drop(encoder);
            }
        }
        Reply::html("")
            .header("content-encoding", &seen.path[1..])
            .body_bytes(encoded)
    });
    let (net, rx) = start();
    for path in ["/gzip", "/deflate", "/br"] {
        let messages = load(&net, &rx, &server.url("https", path));
        assert_eq!(body(&messages), TEXT, "{path}");
    }
    net.shutdown();
}

#[test]
fn twenty_redirects_are_followed_and_the_twenty_first_fails() {
    // AC-2
    let server = serve(Tls::Plain, |seen| {
        let hops: u32 = seen.path.trim_start_matches("/hop/").parse().unwrap_or(0);
        match hops {
            0 => Reply::html(TEXT),
            // Alternate the redirect kinds; all of them are followed.
            n => Reply::redirect(
                [301, 302, 303, 307, 308][n as usize % 5],
                &format!("/hop/{}", n - 1),
            ),
        }
    });
    let (net, rx) = start();
    let messages = load(&net, &rx, &server.url("http", "/hop/20"));
    let (url, _, _, committed) = commit(&messages);
    assert_eq!(committed, DocumentKind::Document);
    assert_eq!(
        url,
        server.url("http", "/hop/0"),
        "the document URL is the final URL"
    );
    assert_eq!(body(&messages), TEXT);

    assert_eq!(
        error(kind(&net, &rx, &server.url("http", "/hop/21"))),
        FetchError::TooManyRedirects
    );
    // A redirect may not leave for a file or an internal page.
    let file = serve(Tls::Plain, |_| Reply::redirect(302, "file:///etc/hosts"));
    assert_eq!(
        error(kind(&net, &rx, &file.url("http", "/"))),
        FetchError::UnsupportedScheme
    );
    net.shutdown();
}

#[test]
fn untrusted_certificates_show_the_error_page() {
    // AC-3: there is no way past it.
    let (net, rx) = start();
    for name in ["expired", "self-signed", "wrong-host"] {
        let server = serve(Tls::Cert(name, H2_AND_H1), |_| Reply::html(TEXT));
        let messages = load(&net, &rx, &server.url("https", "/"));
        let reason = error(commit(&messages).3);
        assert!(
            matches!(reason, FetchError::TlsFailed(_)),
            "{name}: {reason:?}"
        );
        let page = body(&messages);
        assert!(
            page.contains("security certificate is not trusted"),
            "{name}: {page}"
        );
    }
    net.shutdown();
}

#[test]
fn bodies_over_the_cap_fail_even_when_they_arrive_small() {
    // AC-4
    let server = serve(Tls::Plain, |seen| match seen.path.as_str() {
        // Says it is too big before sending anything.
        "/declared" => Reply::html("")
            .header("content-length", "70000000")
            .stalled(),
        // 70 MB of zeros, gzipped to a few hundred KB.
        _ => {
            let mut bomb = GzEncoder::new(Vec::new(), Compression::fast());
            let zeros = vec![0; 1 << 20];
            for _ in 0..70 {
                bomb.write_all(&zeros).unwrap();
            }
            Reply::html("")
                .header("content-encoding", "gzip")
                .body_bytes(bomb.finish().unwrap())
        }
    });
    let (net, rx) = start();
    assert_eq!(
        error(kind(&net, &rx, &server.url("http", "/declared"))),
        FetchError::TooLarge
    );

    // The bomb commits, then stops at the cap and shows the error page.
    let messages = load(&net, &rx, &server.url("http", "/bomb"));
    assert_eq!(commit(&messages).3, DocumentKind::Document);
    assert_eq!(failure(&messages), Some(FetchError::TooLarge));
    let delivered = bytes_of(&messages, FetchId::DOCUMENT).len();
    assert!(delivered <= net::limits::MAX_DOCUMENT_BYTES, "{delivered}");
    let error_page = collect(&rx);
    assert_eq!(error(commit(&error_page).3), FetchError::TooLarge);
    net.shutdown();
}

#[test]
fn at_most_six_fetches_run_against_one_host() {
    // AC-4
    let assets = serve(Tls::Plain, |_| Reply {
        delay: Some(Duration::from_millis(300)),
        ..Reply::ok("image/png", vec![1, 2, 3])
    });
    let page = serve(Tls::Plain, |_| Reply::html(TEXT));
    let (net, rx) = start();
    load(&net, &rx, &page.url("http", "/"));
    for fetch in 1..=10 {
        net.handle().fetch(FetchRequest {
            page: PAGE,
            navigation: NAV,
            fetch: FetchId(fetch),
            url: assets.url("http", &format!("/{fetch}.png")),
            destination: Destination::Image,
        });
    }
    let mut done = 0;
    while done < 10 {
        let (_, message) = rx.recv_timeout(support::WAIT).unwrap();
        if matches!(message, ToContent::FetchDone { .. }) {
            done += 1;
        }
    }
    assert_eq!(assets.seen().len(), 10);
    let peak = assets.peak.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        peak <= net::limits::MAX_CONNECTIONS_PER_HOST,
        "{peak} at once"
    );
    assert!(peak > 1, "fetches still run in parallel");
    net.shutdown();
}

#[test]
#[ignore = "waits out the 30 s idle limit; run with --ignored"]
fn a_silent_server_times_out() {
    // AC-4
    let server = serve(Tls::Plain, |_| Reply::html("<p>half").stalled());
    let (net, rx) = start();
    let started = Instant::now();
    net.handle().navigate(
        PAGE,
        NAV,
        Url::parse(&server.url("http", "/")).unwrap(),
        CommitParams::default(),
    );
    let messages = support::collect_within(&rx, Duration::from_secs(40));
    assert_eq!(failure(&messages), Some(FetchError::TimedOut));
    assert!(started.elapsed() >= net::limits::IDLE_TIMEOUT);
    net.shutdown();
}

#[test]
fn content_types_pick_the_kind_or_the_error_page() {
    // AC-7
    let server = serve(Tls::Plain, |seen| match seen.path.as_str() {
        "/plain" => Reply::ok("text/plain; charset=utf-8", "just text"),
        "/xhtml" => Reply::ok("application/xhtml+xml", TEXT),
        "/pdf" => Reply::ok("application/pdf", "%PDF"),
        "/untyped-html" => Reply::ok("", format!("  \n{TEXT}")).without_type(),
        "/untyped-text" => Reply::ok("", "no markup here").without_type(),
        _ => Reply {
            status: 404,
            ..Reply::html("<p>not found page")
        },
    });
    let (net, rx) = start();
    let at = |path: &str| server.url("http", path);
    assert_eq!(kind(&net, &rx, &at("/plain")), DocumentKind::PlainText);
    assert_eq!(kind(&net, &rx, &at("/xhtml")), DocumentKind::Document);
    assert_eq!(
        error(kind(&net, &rx, &at("/pdf"))),
        FetchError::UnsupportedContentType("application/pdf".into())
    );
    let sniffed = load(&net, &rx, &at("/untyped-html"));
    assert_eq!(commit(&sniffed).1, "text/html");
    assert_eq!(
        body(&sniffed),
        format!("  \n{TEXT}"),
        "the sniffed bytes are not lost"
    );
    assert_eq!(
        kind(&net, &rx, &at("/untyped-text")),
        DocumentKind::PlainText
    );
    let missing = load(&net, &rx, &at("/missing"));
    assert_eq!(
        commit(&missing).3,
        DocumentKind::Document,
        "a 404 page is a page"
    );
    assert!(body(&missing).contains("not found page"));

    let dir = temp_dir("types");
    std::fs::write(dir.join("notes.txt"), "<b>text</b>").unwrap();
    std::fs::write(dir.join("README"), "<!-- html -->").unwrap();
    let file = |name: &str| Url::from_file_path(dir.join(name)).unwrap().to_string();
    assert_eq!(kind(&net, &rx, &file("notes.txt")), DocumentKind::PlainText);
    assert_eq!(kind(&net, &rx, &file("README")), DocumentKind::Document);
    assert_eq!(
        error(kind(
            &net,
            &rx,
            Url::from_directory_path(&dir).unwrap().as_str()
        )),
        FetchError::UnsupportedContentType("inode/directory".into())
    );
    std::fs::remove_dir_all(&dir).unwrap();
    net.shutdown();
}

#[test]
fn file_pages_read_only_inside_their_folder_and_web_pages_read_none() {
    // AC-8
    let root = temp_dir("folder");
    let site = root.join("site");
    std::fs::create_dir_all(site.join("img")).unwrap();
    std::fs::write(site.join("index.html"), TEXT).unwrap();
    std::fs::write(site.join("img/a.png"), [9, 8, 7]).unwrap();
    std::fs::write(root.join("secret.txt"), "secret").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.join("secret.txt"), site.join("link.txt")).unwrap();

    let (net, rx) = start();
    load(
        &net,
        &rx,
        Url::from_file_path(site.join("index.html"))
            .unwrap()
            .as_str(),
    );
    let inside = format!("{}img/a.png", Url::from_directory_path(&site).unwrap());
    let messages = fetch(&net, &rx, 1, &inside, Destination::Image);
    assert!(failure(&messages).is_none(), "{messages:?}");
    assert_eq!(bytes_of(&messages, FetchId(1)), [9, 8, 7]);
    assert!(matches!(
        &messages[0].1,
        ToContent::FetchResponse { status: 200, headers, .. }
            if headers.contains(&("content-type".into(), "image/png".into()))
    ));

    let up = format!("{}../secret.txt", Url::from_directory_path(&site).unwrap());
    assert_eq!(
        failure(&fetch(&net, &rx, 2, &up, Destination::Style)),
        Some(FetchError::FileAccessDenied)
    );
    #[cfg(unix)]
    {
        let link = Url::from_file_path(site.join("link.txt")).unwrap();
        assert_eq!(
            failure(&fetch(&net, &rx, 3, link.as_str(), Destination::Style)),
            Some(FetchError::FileAccessDenied),
            "a symlink out of the folder"
        );
    }
    let missing = Url::from_file_path(site.join("none.css")).unwrap();
    assert_eq!(
        failure(&fetch(&net, &rx, 4, missing.as_str(), Destination::Style)),
        Some(FetchError::FileNotFound)
    );
    assert_eq!(
        failure(&fetch(&net, &rx, 5, "about:blank", Destination::Style)),
        Some(FetchError::UnsupportedScheme)
    );

    // An http page reads no file at all.
    let server = serve(Tls::Plain, |_| Reply::html(TEXT));
    load(&net, &rx, &server.url("http", "/"));
    let hosts = Url::from_file_path(root.join("secret.txt")).unwrap();
    assert_eq!(
        failure(&fetch(&net, &rx, 1, hosts.as_str(), Destination::Image)),
        Some(FetchError::FileAccessDenied)
    );
    std::fs::remove_dir_all(&root).unwrap();
    net.shutdown();
}

#[test]
fn secure_pages_upgrade_insecure_subresources_or_block_them() {
    // AC-9
    let secure = serve(Tls::Cert("localhost", H2_AND_H1), |seen| {
        match seen.path.as_str() {
            "/down" => Reply::redirect(302, "http://127.0.0.1:1/a.png"),
            "/" => Reply::html(TEXT),
            _ => Reply::ok("image/png", vec![1]),
        }
    });
    let plain = serve(Tls::Plain, |_| Reply::ok("image/png", vec![2]));
    let (net, rx) = start();
    load(&net, &rx, &secure.url("https", "/"));

    // `http` on a port that also speaks TLS: the upgrade works.
    let messages = fetch(
        &net,
        &rx,
        1,
        &secure.url("http", "/a.png"),
        Destination::Image,
    );
    assert!(matches!(
        &messages[0].1,
        ToContent::FetchResponse { final_url, .. } if final_url == &secure.url("https", "/a.png")
    ));
    assert_eq!(bytes_of(&messages, FetchId(1)), [1]);
    // A port that only speaks plain HTTP: the upgrade fails and is blocked.
    let blocked = fetch(
        &net,
        &rx,
        2,
        &plain.url("http", "/a.png"),
        Destination::Image,
    );
    assert_eq!(failure(&blocked), Some(FetchError::MixedContentBlocked));
    assert!(plain.seen().is_empty(), "nothing was sent in the clear");
    // A secure subresource that redirects to http is blocked too.
    let down = fetch(
        &net,
        &rx,
        3,
        &secure.url("https", "/down"),
        Destination::Image,
    );
    assert_eq!(failure(&down), Some(FetchError::MixedContentBlocked));
    net.shutdown();
}

#[test]
fn subresources_send_their_accept_header_and_stale_fetches_are_dropped() {
    // Value sourcing: Accept per destination. AC-11: a fetch for an old
    // navigation is never started.
    let server = serve(Tls::Plain, |_| Reply::html(TEXT));
    let (net, rx) = start();
    load(&net, &rx, &server.url("http", "/"));
    fetch(
        &net,
        &rx,
        1,
        &server.url("http", "/s.css"),
        Destination::Style,
    );
    fetch(
        &net,
        &rx,
        2,
        &server.url("http", "/i.png"),
        Destination::Image,
    );
    let seen = server.seen();
    assert_eq!(seen[1].header("accept"), Some("text/css,*/*;q=0.1"));
    assert_eq!(
        seen[2].header("accept"),
        Some("image/webp,image/png,image/*;q=0.8")
    );

    net.handle().fetch(FetchRequest {
        page: PAGE,
        navigation: NavigationId(NAV.0 - 1),
        fetch: FetchId(3),
        url: server.url("http", "/old.png"),
        destination: Destination::Image,
    });
    assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
    assert_eq!(server.seen().len(), 3);
    net.shutdown();
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("net-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Canonical, so the page's folder and the files agree on macOS's
    // `/var` to `/private/var` link.
    dir.canonicalize().unwrap()
}
