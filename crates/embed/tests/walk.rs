//! The keyboard only walk of spec 0008, AC-36, without a window: the real
//! network against the local TLS test server, a real page, the session
//! history, and the chrome's address palette, wired the way the browser
//! main thread and the content process wire them (`browser.rs`,
//! `content.rs`). Type a host, follow three links by Tab and Enter, back
//! twice and forward once landing at the saved scroll each time, reload,
//! stop a slow load, and search a phrase.
//!
//! The shortcuts that start back, forward and reload are unit tested in
//! `input.rs`; here each one is the history move it maps to.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

#[path = "../../net/tests/support/mod.rs"]
mod support;

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use display_list::{ScrollId, SizeF};
use dom::FrameId;
use ipc::{
    DocumentKind, FetchId, InputEvent, Key, Modifiers, NavigationId, PageNavigationOrigin,
    ToBrowser, ToContent,
};
use navigation::{Action, Begin, EntryKind, Origin, SessionHistory, Stopped, UserInput};
use net::Net;
use page::Page;
use support::{H2_AND_H1, PAGE, Reply, Tls, WAIT, serve, start};
use ui::{ChromeCommand, ChromeDocument, ChromeSurface, NavView, Os, SearchEngine};
use url::Url;

/// A page of the walk: its title, one link to `next`, and room to scroll.
fn page_html(title: &str, next: &str) -> String {
    format!(
        "<!doctype html><title>{title}</title>\
         <body style='margin:0'>\
         <a id=next href='{next}' style='display:block;height:20px'>Next</a>\
         <div style='height:4000px'></div></body>"
    )
}

/// The browser, cut down to what one page's navigation needs.
struct Browser {
    net: Net,
    rx: Receiver<(Instant, ToContent)>,
    page: Page,
    history: SessionHistory,
    chrome: ChromeDocument,
    /// The URL `net` committed the current document at (AC-18).
    document_url: Option<Url>,
    /// The window title (AC-24).
    title: String,
}

impl Browser {
    fn new() -> Self {
        let (net, rx) = start();
        let viewport = SizeF {
            width: 800.0,
            height: 600.0,
        };
        let window = ui::SizeF {
            width: 1280.0,
            height: 800.0,
        };
        let chrome =
            ChromeDocument::load(ChromeSurface::Frame, Os::current(), window, 1.0, None).unwrap();
        Self {
            net,
            rx,
            page: Page::blank(PAGE, viewport, 1.0, page::ua_stylesheet()),
            history: SessionHistory::new(),
            chrome,
            document_url: None,
            title: String::new(),
        }
    }

    /// What `show_navigation` hands the chrome.
    fn show_navigation(&mut self) {
        let pending = self
            .history
            .pending()
            .filter(|pending| matches!(pending.origin, Origin::User(_)))
            .map(|pending| pending.url.clone());
        self.chrome.set_navigation(&NavView {
            can_back: self.history.can_back(),
            can_forward: self.history.can_forward(),
            url: self.history.current().map(|entry| entry.url.clone()),
            pending,
            loading: self.history.loading(),
        });
        let _ = self.chrome.frame();
    }

    fn root_scroll_id(&self) -> ScrollId {
        let root = self.page.main_frame().document.document_element().unwrap();
        paint::scroll_id(FrameId::MAIN.0, root)
    }

    /// The root scroll offset's `y`.
    fn scroll_y(&self) -> f32 {
        self.page.scroll_offset(self.root_scroll_id()).y
    }

    /// Scrolls the root, as the render thread does for a key or the wheel.
    fn scroll_to(&mut self, y: f32) {
        let id = self.root_scroll_id();
        self.page.scroll_changed(id, 0.0, y);
        self.page.end_task();
    }

    /// `begin_load`: the history hands out the navigation, the entry left
    /// saves its scroll (AC-23), then `Navigate` and the network load.
    fn begin(&mut self, url: Url, origin: Origin, action: Action) -> NavigationId {
        let leaving = self.history.current().map(|entry| entry.id);
        let Begin::Load(navigation) = self.history.begin(url.clone(), origin, action) else {
            panic!("{url} did not load");
        };
        let leaves = self.history.pending().is_some_and(|pending| {
            matches!(pending.action, Action::Push | Action::Traverse { .. })
        });
        if leaves && let Some(entry) = leaving {
            let offset = self.page.scroll_offset(self.root_scroll_id());
            self.history.save_scroll(entry, (offset.x, offset.y));
        }
        let params = self.history.pending().unwrap().params;
        self.page.navigate(navigation);
        self.net.handle().navigate(PAGE, navigation, url, params);
        self.show_navigation();
        navigation
    }

    /// Hands one network message to the page, as `content.rs` does, and
    /// records a commit in the history, as `committed` does.
    fn deliver(&mut self, message: ToContent) {
        match message {
            ToContent::CommitNavigation {
                navigation,
                url,
                charset,
                kind,
                restore_scroll,
                history,
                ..
            } => {
                let entry = match &kind {
                    DocumentKind::Error { reason, .. } => EntryKind::Error(reason.clone()),
                    _ => EntryKind::Document,
                };
                let parsed = Url::parse(&url).unwrap();
                if self
                    .history
                    .commit(navigation, parsed.clone(), entry)
                    .is_some()
                {
                    self.document_url = Some(parsed);
                }
                if self
                    .page
                    .commit_navigation(navigation, &url, charset, &kind)
                {
                    self.page.restore_scroll(navigation, restore_scroll);
                    self.page.set_history(history);
                }
            }
            ToContent::FetchResponse {
                navigation,
                fetch,
                status,
                headers,
                final_url,
                ..
            } => self
                .page
                .fetch_response(navigation, fetch, status, &headers, &final_url),
            ToContent::FetchChunk {
                navigation,
                fetch,
                data,
                ..
            } => self.page.fetch_chunk(navigation, fetch, &data),
            ToContent::FetchDone {
                navigation, fetch, ..
            } => self.page.fetch_done(navigation, fetch),
            ToContent::FetchFailed {
                navigation,
                fetch,
                reason,
                ..
            } => self.page.fetch_failed(navigation, fetch, &reason),
            other => panic!("unexpected from the network: {other:?}"),
        }
        self.page.end_task();
        self.after_task();
    }

    /// What the page left for the browser: its title (AC-24) and its
    /// `load` event (AC-25). The walk's pages load nothing else.
    fn after_task(&mut self) {
        for message in self.page.take_head_messages() {
            if let ToBrowser::TitleChanged { title, .. } = message {
                self.history.title(&title);
                self.title = title;
            }
        }
        if let Some(ToBrowser::LoadComplete { navigation, .. }) = self.page.take_load_message() {
            self.history.load_complete(navigation);
            self.show_navigation();
        }
    }

    /// Delivers network messages until `navigation`'s document is in,
    /// then draws frames until layout and any scroll restore settle.
    fn finish(&mut self, navigation: NavigationId) {
        loop {
            let (_, message) = self.rx.recv_timeout(WAIT).expect("the load finished");
            let done = matches!(
                message,
                ToContent::FetchDone { navigation: n, fetch, .. }
                    | ToContent::FetchFailed { navigation: n, fetch, .. }
                    if n == navigation && fetch == FetchId::DOCUMENT
            );
            self.deliver(message);
            if done {
                break;
            }
        }
        for _ in 0..8 {
            if self.page.begin_frame(0.0).is_none() && !self.page.take_frame_request() {
                break;
            }
        }
        self.page.end_task();
        self.after_task();
        assert_eq!(self.history.committed(), Some(navigation));
    }

    /// One key press and release in the page; an activated link becomes
    /// the browser's `page_navigate`: policy first (AC-18), then a push.
    fn page_key(&mut self, key: Key) -> Option<NavigationId> {
        let mut activated = None;
        for pressed in [true, false] {
            let outcome = self.page.input(InputEvent::Key {
                key: key.clone(),
                modifiers: Modifiers::default(),
                pressed,
                repeat: false,
            });
            activated = activated.or(outcome.activated);
        }
        let url = self.page.link_url(activated?)?;
        let target = navigation::parse_page_url(&url).unwrap();
        navigation::page_may_load(self.document_url.as_ref().unwrap(), &target).unwrap();
        let origin = Origin::Page(PageNavigationOrigin::Link);
        Some(self.begin(target, origin, Action::Push))
    }

    /// Tab to the page's link, scroll down by `y` (the focus ring stays on
    /// the link), Enter. The next page loads.
    fn follow_link(&mut self, y: f32) {
        assert!(self.page_key(Key::Tab).is_none());
        self.scroll_to(y);
        assert_eq!(self.scroll_y(), y);
        let navigation = self.page_key(Key::Enter).expect("Enter follows the link");
        self.finish(navigation);
    }

    /// Cmd+L, the text, Enter (AC-5, AC-6, AC-7): the palette's URL to
    /// load, begun as a typed load.
    fn type_address(&mut self, text: &str) -> NavigationId {
        self.chrome.open_address();
        assert!(self.chrome.palette_open());
        self.chrome.input(InputEvent::Text { text: text.into() });
        let _ = self.chrome.frame();
        let mut command = None;
        for pressed in [true, false] {
            let outcome = self.chrome.input(InputEvent::Key {
                key: Key::Enter,
                modifiers: Modifiers::default(),
                pressed,
                repeat: false,
            });
            command = command.or(outcome.command);
        }
        assert!(matches!(command, Some(Ok(ChromeCommand::AddressGo))));
        let url = self.chrome.address_go().expect("Enter loads something");
        assert!(!self.chrome.palette_open());
        self.begin(url, Origin::User(UserInput::Typed), Action::Push)
    }

    /// Back, forward or reload by `delta`, as the shortcuts do (AC-21,
    /// AC-29).
    fn go(&mut self, delta: i32) {
        let action = self.history.go(delta).expect("an entry that way");
        let url = match action {
            Action::Traverse { to } => self.history.entry(to).unwrap().url.clone(),
            _ => self.history.current().unwrap().url.clone(),
        };
        let origin = Origin::User(UserInput::Shortcut);
        let navigation = self.begin(url, origin, action);
        self.finish(navigation);
    }

    fn path(&self) -> String {
        self.history.current().unwrap().url.path().to_owned()
    }

    fn entries(&self) -> (u32, u32) {
        let info = self.history.info();
        (info.length, info.index)
    }
}

#[test]
fn the_keyboard_only_walk() {
    // Spec 0008, AC-36
    let server = serve(Tls::Cert("localhost", H2_AND_H1), |seen| {
        match seen.path.as_str() {
            "/a.html" => Reply::html(&page_html("A", "b.html")),
            "/b.html" => Reply::html(&page_html("B", "c.html")),
            "/c.html" => Reply::html(&page_html("C", "d.html")),
            "/d.html" => Reply::html(&page_html("D", "a.html")),
            "/slow.html" => Reply {
                delay: Some(Duration::from_secs(1)),
                ..Reply::html(&page_html("Slow", "a.html"))
            },
            "/search" => Reply::html("<!doctype html><title>Results</title><p>results"),
            _ => Reply {
                status: 404,
                ..Reply::html("missing")
            },
        }
    });
    let port = server.port;
    let mut browser = Browser::new();

    // Type a host (AC-8): it loads over https, the address shows the typed
    // URL until the commit (AC-11), then the host (AC-3).
    let typed = browser.type_address(&format!("127.0.0.1:{port}/a.html"));
    let expected = format!("https://127.0.0.1:{port}/a.html");
    assert_eq!(browser.history.pending().unwrap().url.as_str(), expected);
    assert_eq!(browser.chrome.address().text(), expected);
    browser.finish(typed);
    assert_eq!(browser.title, "A", "AC-24");
    assert_eq!(
        browser.chrome.address().pill_host(),
        format!("127.0.0.1:{port}")
    );
    assert!(!browser.history.loading(), "loading ends at `load` (AC-25)");

    // Follow three links, leaving each page scrolled somewhere else (AC-17).
    browser.follow_link(1200.0);
    browser.follow_link(800.0);
    browser.follow_link(400.0);
    assert_eq!(browser.path(), "/d.html");
    assert_eq!(browser.title, "D");
    assert_eq!(browser.entries(), (4, 3), "four entries (AC-22)");
    assert!(browser.history.can_back() && !browser.history.can_forward());

    // Back twice and forward once, each landing where it was left (AC-21,
    // AC-23).
    browser.go(-1);
    assert_eq!(
        (browser.path().as_str(), browser.scroll_y()),
        ("/c.html", 400.0)
    );
    browser.go(-1);
    assert_eq!(
        (browser.path().as_str(), browser.scroll_y()),
        ("/b.html", 800.0)
    );
    assert_eq!(browser.title, "B");
    browser.go(1);
    assert_eq!(
        (browser.path().as_str(), browser.scroll_y()),
        ("/c.html", 400.0)
    );
    assert_eq!(browser.entries(), (4, 2));
    assert!(browser.history.can_back() && browser.history.can_forward());

    // Reload: the same entry, loaded again (AC-22).
    browser.go(0);
    assert_eq!(
        (browser.path().as_str(), browser.entries()),
        ("/c.html", (4, 2))
    );
    assert_eq!(browser.title, "C");

    // Stop a slow load before it commits (AC-26): the page, its URL and
    // the history stay, and nothing of the slow page arrives.
    let slow = browser.type_address(&format!("127.0.0.1:{port}/slow.html"));
    assert!(browser.history.loading());
    let Stopped::BeforeCommit { dropped, shown } = browser.history.stop() else {
        panic!("the slow load had not committed");
    };
    assert_eq!(dropped, slow);
    browser.net.handle().stop(PAGE, dropped);
    browser.show_navigation();
    assert!(shown.is_some() && !browser.history.loading());
    assert_eq!(
        (browser.path().as_str(), browser.entries()),
        ("/c.html", (4, 2))
    );
    assert_eq!(
        browser.chrome.address().pill_host(),
        format!("127.0.0.1:{port}")
    );
    match browser.rx.recv_timeout(Duration::from_secs(2)) {
        Err(RecvTimeoutError::Timeout) => {}
        other => panic!("the stopped load still sent {other:?}"),
    }

    // Search a phrase with a custom engine (AC-8, AC-9, AC-32): the
    // search pushes after the current entry, dropping the one ahead.
    let template = format!("https://127.0.0.1:{port}/search?q=%s");
    browser
        .chrome
        .set_search_engine(SearchEngine::custom(&template).unwrap());
    let search = browser.type_address("rust borrow checker");
    browser.finish(search);
    assert_eq!(
        browser.history.current().unwrap().url.as_str(),
        format!("https://127.0.0.1:{port}/search?q=rust+borrow+checker")
    );
    assert_eq!(browser.title, "Results");
    assert_eq!(browser.entries(), (4, 3));
    assert!(!browser.history.can_forward());

    browser.net.shutdown();
}
