//! Property tests for the navigation rules (spec 0008, AC-36): any
//! sequence of loads, failures, stops and history moves keeps the session
//! history equal to a plain list model of the AC-22 entry rules, and
//! `interpret` reads any text the way AC-8 says.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a panic is a failed test, as allow-unwrap-in-tests intends"
)]

use std::path::Path;

use ipc::{FetchError, HistoryInfo};
use navigation::{
    Action, Begin, EntryKind, Intent, MAX_ENTRIES, Origin, SessionHistory, Stopped, UserInput,
    interpret,
};
use proptest::prelude::*;
use url::Url;

/// One thing a user or page does to the history.
#[derive(Debug, Clone)]
enum Op {
    /// Load page `n` as a new entry.
    Push(u8),
    /// Load page `n` into the current entry.
    Replace(u8),
    /// Load the current entry again.
    Reload,
    /// Back or forward by this many entries.
    Go(i32),
    /// Load page `n`, which fails: the error page is an entry.
    Fail(u8),
    /// Start loading page `n`, then press Stop before it commits.
    Abandon(u8),
}

fn op() -> impl Strategy<Value = Op> {
    // Few pages, so pushing the page on screen again comes up often.
    let page = 0u8..6;
    prop_oneof![
        4 => page.clone().prop_map(Op::Push),
        1 => page.clone().prop_map(Op::Replace),
        1 => Just(Op::Reload),
        3 => (-3i32..=3).prop_map(Op::Go),
        1 => page.clone().prop_map(Op::Fail),
        1 => page.prop_map(Op::Abandon),
    ]
}

fn page_url(n: u8) -> Url {
    Url::parse(&format!("https://site.test/p{n}")).unwrap()
}

/// The AC-22 rules over a plain list of URLs.
#[derive(Debug, Default)]
struct Model {
    entries: Vec<Url>,
    current: usize,
}

impl Model {
    fn push(&mut self, url: Url) {
        if self.entries.get(self.current) == Some(&url) {
            return;
        }
        if !self.entries.is_empty() {
            self.entries.truncate(self.current + 1);
        }
        self.entries.push(url);
        if self.entries.len() > MAX_ENTRIES {
            self.entries.remove(0);
        }
        self.current = self.entries.len() - 1;
    }

    fn replace(&mut self, url: Url) {
        match self.entries.get_mut(self.current) {
            Some(entry) => *entry = url,
            None => self.push(url),
        }
    }

    fn go(&mut self, delta: i32) {
        let index = self.current as i64 + i64::from(delta);
        if delta != 0 && (0..self.entries.len() as i64).contains(&index) {
            self.current = index as usize;
        }
    }

    fn info(&self) -> HistoryInfo {
        HistoryInfo {
            length: self.entries.len() as u32,
            index: self.current as u32,
        }
    }
}

/// Begins `url` with `action`, then commits it as `kind` and fires its
/// `load`. Checks that the commit lands where the begin predicted.
fn load(history: &mut SessionHistory, url: Url, action: Action, kind: EntryKind) {
    let origin = Origin::User(UserInput::Shortcut);
    let Begin::Load(navigation) = history.begin(url.clone(), origin, action) else {
        panic!("{action:?} of {url} did not load");
    };
    assert!(history.loading());
    let predicted = history.pending().unwrap().params.history;
    let landed = history.commit(navigation, url, kind).expect("it commits");
    assert_eq!(landed, predicted, "the commit params predict the history");
    history.load_complete(navigation);
    assert!(!history.loading());
}

proptest! {
    /// AC-21, AC-22, AC-26: the history always matches the model, never
    /// holds more than 50 entries, and back and forward are enabled
    /// exactly when there is an entry that way.
    #[test]
    fn the_history_follows_the_entry_rules(ops in prop::collection::vec(op(), 0..160)) {
        let mut history = SessionHistory::new();
        let mut model = Model::default();
        for op in ops {
            match op {
                Op::Push(n) => {
                    load(&mut history, page_url(n), Action::Push, EntryKind::Document);
                    model.push(page_url(n));
                }
                Op::Replace(n) => {
                    load(&mut history, page_url(n), Action::Replace, EntryKind::Document);
                    model.replace(page_url(n));
                }
                Op::Reload => match history.go(0) {
                    Some(action) => {
                        let url = history.current().unwrap().url.clone();
                        load(&mut history, url, action, EntryKind::Document);
                    }
                    None => prop_assert!(model.entries.is_empty()),
                },
                Op::Go(delta) => {
                    if let Some(action @ Action::Traverse { to }) = history.go(delta) {
                        let url = history.entry(to).unwrap().url.clone();
                        load(&mut history, url, action, EntryKind::Document);
                    }
                    model.go(delta);
                }
                Op::Fail(n) => {
                    let failed = EntryKind::Error(FetchError::ConnectFailed);
                    load(&mut history, page_url(n), Action::Push, failed);
                    model.push(page_url(n));
                }
                Op::Abandon(n) => {
                    let before = history.info();
                    let origin = Origin::User(UserInput::Typed);
                    let begun = history.begin(page_url(n), origin, Action::Push);
                    prop_assert!(matches!(begun, Begin::Load(_)));
                    let stopped = history.stop();
                    prop_assert!(
                        matches!(stopped, Stopped::BeforeCommit { .. }),
                        "{stopped:?}"
                    );
                    prop_assert_eq!(history.info(), before);
                    prop_assert!(!history.loading());
                }
            }
            prop_assert_eq!(history.info(), model.info());
            prop_assert!(model.entries.len() <= MAX_ENTRIES);
            prop_assert_eq!(history.can_back(), model.current > 0);
            prop_assert_eq!(history.can_forward(), model.current + 1 < model.entries.len());
            prop_assert_eq!(
                history.current().map(|entry| &entry.url),
                model.entries.get(model.current)
            );
        }
    }

    /// AC-8: any text at all is read without a panic, and a URL it loads
    /// is always one the engine can load.
    #[test]
    fn any_text_reads_as_a_loadable_url_or_a_search(text in any::<String>()) {
        if let Some(Intent::Go(url)) = interpret(&text, no_files) {
            prop_assert!(
                matches!(url.scheme(), "http" | "https" | "file" | "about"),
                "{url}"
            );
        }
    }

    /// AC-8: space around the text never changes what it means.
    #[test]
    fn surrounding_space_is_ignored(text in "[a-z0-9.:/? ]{0,30}", before in "[ \t]{0,3}", after in "[ \t\n]{0,3}") {
        let padded = format!("{before}{text}{after}");
        prop_assert_eq!(interpret(&padded, no_files), interpret(&text, no_files));
    }

    /// AC-8: a leading `?` always searches, for the rest as typed.
    #[test]
    fn a_question_mark_always_searches(query in "[a-z0-9.:/][a-z0-9.:/ ]{0,30}") {
        let expected = Intent::Search(query.trim_end().to_owned());
        prop_assert_eq!(interpret(&format!("?{query}"), no_files), Some(expected));
    }

    /// AC-8: words with spaces between them search.
    #[test]
    fn words_search(words in "[a-z]{1,8}( [a-z]{1,8}){1,4}") {
        prop_assert_eq!(
            interpret(&words, no_files),
            Some(Intent::Search(words.clone()))
        );
    }

    /// AC-8: a dotted name, with or without a path, loads over https.
    #[test]
    fn dotted_names_load_over_https(name in "[a-z]{1,10}\\.[a-z]{2,6}", path in "(/[a-z0-9]{0,8})?") {
        let Some(Intent::Go(url)) = interpret(&format!("{name}{path}"), no_files) else {
            return Err(TestCaseError::fail(format!("{name}{path} did not load")));
        };
        prop_assert_eq!(url.scheme(), "https");
        prop_assert_eq!(url.host_str(), Some(name.as_str()));
    }
}

fn no_files(_: &Path) -> bool {
    false
}
