//! AC-38: a `file://` page's document and the files it loaded are polled;
//! a change tells the browser to reload. Web pages are never polled.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

mod support;

use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use display_list::PageId;
use ipc::{CommitParams, Destination, FetchId, ToContent};
use net::{FetchRequest, Net, NetConfig, limits};
use support::{NAV, PAGE, Reply, Tls, collect, serve};
use url::Url;

/// The network, its content messages, and the pages it said changed.
fn start() -> (Net, Receiver<(Instant, ToContent)>, Receiver<PageId>) {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let (changed_tx, changed) = mpsc::channel();
    let changed_tx = Mutex::new(changed_tx);
    let net = Net::start(
        NetConfig {
            user_agent: net::user_agent("0.0.0-test"),
            extra_roots: Vec::new(),
        },
        Arc::new(move |message| {
            let _ = tx.lock().unwrap().send((Instant::now(), message));
        }),
        Arc::new(move |page| {
            let _ = changed_tx.lock().unwrap().send(page);
        }),
    )
    .unwrap();
    (net, rx, changed)
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("net-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

/// Long enough for the watch to look twice.
fn settle() {
    std::thread::sleep(limits::LIVE_RELOAD_POLL * 2 + Duration::from_millis(100));
}

#[test]
fn editing_a_served_file_reloads_the_page_and_nothing_else_does() {
    // AC-38
    let dir = temp_dir("live");
    std::fs::write(
        dir.join("page.html"),
        "<link rel=stylesheet href=style.css>",
    )
    .unwrap();
    std::fs::write(dir.join("style.css"), "p { color: red }").unwrap();
    std::fs::write(dir.join("other.css"), "p { color: blue }").unwrap();
    let (net, rx, changed) = start();

    let page = Url::from_file_path(dir.join("page.html")).unwrap();
    net.handle()
        .navigate(PAGE, NAV, page.clone(), CommitParams::default());
    collect(&rx);
    net.handle().fetch(FetchRequest {
        page: PAGE,
        navigation: NAV,
        fetch: FetchId(1),
        url: Url::from_file_path(dir.join("style.css")).unwrap().into(),
        destination: Destination::Style,
    });
    collect(&rx);
    settle();
    assert!(changed.try_recv().is_err(), "nothing changed yet");

    // A file in the folder the page never loaded does not count.
    std::fs::write(dir.join("other.css"), "p { color: green }").unwrap();
    settle();
    assert!(changed.try_recv().is_err());

    std::fs::write(dir.join("style.css"), "p { color: purple }").unwrap();
    let started = Instant::now();
    let page_changed = changed.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(page_changed, PAGE);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "within a second"
    );
    // Once per change: the reload's own navigation watches again.
    settle();
    assert!(changed.try_recv().is_err());

    // A web page is never polled.
    let server = serve(Tls::Plain, |_| Reply::html("<p>web"));
    net.handle().navigate(
        PAGE,
        NAV,
        Url::parse(&server.url("http", "/")).unwrap(),
        CommitParams::default(),
    );
    collect(&rx);
    std::fs::write(dir.join("page.html"), "<p>edited").unwrap();
    settle();
    assert!(changed.try_recv().is_err());

    std::fs::remove_dir_all(&dir).unwrap();
    net.shutdown();
}
