//! One page's session history and load state (spec 0008, Data model and
//! State transitions).
//!
//! Every top level load goes through [`SessionHistory::begin`], which
//! hands out its `NavigationId`; [`SessionHistory::commit`] applies the
//! entry rules (AC-22) once the network committed the document.

use std::collections::VecDeque;

use ipc::{CommitParams, FetchError, HistoryInfo, NavigationId, PageNavigationOrigin};
use url::Url;

/// The most entries a history keeps; the oldest go first (AC-22).
pub const MAX_ENTRIES: usize = 50;
/// The longest entry title, in characters (spec 0008, Data model).
pub const MAX_TITLE_CHARS: usize = 1024;

/// One entry's id, unique within its history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntryId(pub u64);

/// Which document an entry belongs to. Entries share one only when a
/// same document move made them (AC-20).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DocumentSeq(pub u64);

/// What an entry shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryKind {
    /// A document the network loaded.
    Document,
    /// The error page for a load that failed. Back leaves it, and reload
    /// tries its URL again (AC-22).
    Error(FetchError),
}

/// One page in the session history (spec 0008, Data model).
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryEntry {
    /// The entry's id.
    pub id: EntryId,
    /// The final URL after redirects, with its fragment; for a failed
    /// load, the URL that was asked for.
    pub url: Url,
    /// The title, at most [`MAX_TITLE_CHARS`]; empty until known.
    pub title: String,
    /// The root scroll offset in CSS px when it was left; `None` until
    /// then (AC-23).
    pub scroll: Option<(f32, f32)>,
    /// What it shows.
    pub kind: EntryKind,
    /// Its document.
    pub document: DocumentSeq,
}

/// The user's way of starting a load (spec 0008, Data model, `Origin`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserInput {
    /// Typed into the address.
    Typed,
    /// A button in the browser's own UI.
    Toolbar,
    /// A keyboard shortcut.
    Shortcut,
    /// Mouse button 4 or 5.
    MouseButton,
}

/// Who started a load. Decides which URL the address shows meanwhile
/// (AC-11), the page policy (AC-18) and the script throttle (AC-28).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The user.
    User(UserInput),
    /// The page: a link or a script.
    Page(PageNavigationOrigin),
    /// The browser on its own: the start URL, live reload, and the crash
    /// page after a relaunch.
    Browser,
}

/// What a load does to the history (spec 0008, Data model).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// A new entry after the current one; entries ahead are dropped.
    Push,
    /// The current entry gets the new document.
    Replace,
    /// The current URL loads again into the current entry.
    Reload,
    /// Back or forward to an existing entry.
    Traverse {
        /// The entry to land on.
        to: EntryId,
    },
}

/// Where the page's load is (spec 0008, State transitions).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LoadState {
    /// Nothing loaded yet.
    #[default]
    Idle,
    /// A load began and nothing committed yet.
    Requesting,
    /// A document committed and its `load` event has not fired.
    Loading,
    /// The document's `load` event fired.
    Complete,
}

/// A load that has not committed yet; at most one per page.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingNavigation {
    /// Its id.
    pub navigation: NavigationId,
    /// The URL asked for.
    pub url: Url,
    /// Who asked.
    pub origin: Origin,
    /// What it does to the history.
    pub action: Action,
    /// What the commit carries to the page.
    pub params: CommitParams,
    /// The load state before it began, for Stop before commit.
    before: LoadState,
}

/// What [`SessionHistory::begin`] decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Begin {
    /// Load the URL as this navigation.
    Load(NavigationId),
    /// The URL is the document on screen at another fragment (AC-20): no
    /// load. The entry is already in the history; the page only scrolls.
    SameDocument {
        /// The entry left, whose scroll offset to save (AC-23).
        leaving: EntryId,
        /// The history now.
        history: HistoryInfo,
    },
    /// Nothing loads: the target entry is gone.
    Refused,
}

/// A move between two entries of the same document (AC-21): no load, only
/// a scroll.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SameDocumentMove {
    /// The entry left, whose scroll offset to save (AC-23).
    pub leaving: EntryId,
    /// The offset the entry moved to was left at, if it saved one.
    pub scroll: Option<(f32, f32)>,
    /// The history now.
    pub history: HistoryInfo,
}

/// What [`SessionHistory::stop`] did (spec 0008, AC-26).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    /// Nothing was loading.
    Nothing,
    /// The load had not committed: it is dropped, and the document on
    /// screen stays, as the given navigation.
    BeforeCommit {
        /// The dropped load.
        dropped: NavigationId,
        /// The document still on screen, if any.
        shown: Option<NavigationId>,
    },
    /// The document committed and its `load` has not fired: the page
    /// cancels what is left, and loading ends at its `load` event.
    AfterCommit(NavigationId),
}

/// One page's session history (spec 0008, Data model). Only the browser
/// main thread changes it.
#[derive(Debug, Default)]
pub struct SessionHistory {
    /// Oldest first. Empty until the first load commits.
    entries: VecDeque<HistoryEntry>,
    /// The current entry's index; 0 while empty.
    current: usize,
    next_entry: u64,
    next_document: u64,
    /// The last navigation handed out.
    last_navigation: NavigationId,
    /// The navigation whose document is on screen.
    committed: Option<NavigationId>,
    pending: Option<PendingNavigation>,
    /// A load that adds no entry (see [`SessionHistory::begin_detached`]).
    detached: Option<NavigationId>,
    load: LoadState,
}

impl SessionHistory {
    /// An empty history.
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts a load of `url` (spec 0008, Key invariants: every top level
    /// load comes through here). A newer load replaces a pending one. A
    /// push of exactly the current URL replaces the current entry instead
    /// (AC-22). The caller has already checked page loads with
    /// [`crate::page_may_load`].
    pub fn begin(&mut self, url: Url, origin: Origin, action: Action) -> Begin {
        if matches!(action, Action::Push | Action::Replace)
            && let Some(begin) = self.same_document(&url, action)
        {
            return begin;
        }
        let action = match action {
            Action::Push if self.current().is_some_and(|entry| entry.url == url) => Action::Replace,
            Action::Traverse { to } if self.index_of(to).is_none() => return Begin::Refused,
            action => action,
        };
        let params = self.params_for(action);
        let before = match &self.pending {
            Some(pending) => pending.before,
            None => self.load,
        };
        self.last_navigation = self.last_navigation.next();
        let navigation = self.last_navigation;
        self.detached = None;
        self.pending = Some(PendingNavigation {
            navigation,
            url,
            origin,
            action,
            params,
            before,
        });
        self.load = LoadState::Requesting;
        Begin::Load(navigation)
    }

    /// A move to another fragment of the document on screen (AC-20): its
    /// URL equals the current entry's but for the fragment, the target has
    /// one, the entry is no error page, and no other load is pending. A
    /// push adds an entry of the same document, dropping those ahead; the
    /// same URL, or a replace, keeps the entry and changes its URL.
    fn same_document(&mut self, url: &Url, action: Action) -> Option<Begin> {
        let current = self.current()?;
        let same = self.pending.is_none()
            && current.kind == EntryKind::Document
            && url.fragment().is_some()
            && without_fragment(url) == without_fragment(&current.url);
        if !same {
            return None;
        }
        let leaving = current.id;
        let document = current.document;
        if action == Action::Replace || *url == current.url {
            let entry = &mut self.entries[self.current];
            entry.url = url.clone();
        } else {
            let title = current.title.clone();
            self.push(url.clone(), EntryKind::Document, document);
            if let Some(entry) = self.entries.get_mut(self.current) {
                entry.title = title;
            }
        }
        Some(Begin::SameDocument {
            leaving,
            history: self.info(),
        })
    }

    /// Back or forward to entry `to` when it shows the same document as
    /// the current one (AC-21): only the current entry moves, nothing
    /// loads. `None` when it is another document, gone, or a load is
    /// pending; then the caller loads it.
    pub fn traverse_within(&mut self, to: EntryId) -> Option<SameDocumentMove> {
        let index = self.index_of(to)?;
        let current = self.current()?;
        if self.pending.is_some()
            || index == self.current
            || self.entries[index].document != current.document
        {
            return None;
        }
        let leaving = current.id;
        self.current = index;
        Some(SameDocumentMove {
            leaving,
            scroll: self.entries[index].scroll,
            history: self.info(),
        })
    }

    /// Starts a load that adds no entry: the error page for start input
    /// that is no URL at all (spec 0004, AC-1), or the crash page before
    /// anything committed. It replaces a pending load.
    pub fn begin_detached(&mut self) -> NavigationId {
        if let Some(pending) = self.pending.take() {
            self.load = pending.before;
        }
        self.last_navigation = self.last_navigation.next();
        self.detached = Some(self.last_navigation);
        self.load = LoadState::Requesting;
        self.last_navigation
    }

    /// What the commit of `action` carries: the history once it lands,
    /// and for a traverse the offset the entry was left at.
    fn params_for(&self, action: Action) -> CommitParams {
        let len = self.entries.len();
        let (length, index, restore_scroll) = match action {
            _ if len == 0 => (1, 0, None),
            Action::Push => {
                let length = (self.current + 2).min(MAX_ENTRIES);
                (length, length - 1, None)
            }
            Action::Replace | Action::Reload => (len, self.current, None),
            Action::Traverse { to } => {
                let index = self.index_of(to).unwrap_or(self.current);
                (len, index, self.entries.get(index).and_then(|e| e.scroll))
            }
        };
        CommitParams {
            restore_scroll,
            history: info(length, index),
        }
    }

    /// The network committed `navigation`'s document at `final_url`:
    /// applies the entry rules (AC-22) and returns the history now. `None`
    /// for a navigation that is not the pending one (stale).
    pub fn commit(
        &mut self,
        navigation: NavigationId,
        final_url: Url,
        kind: EntryKind,
    ) -> Option<HistoryInfo> {
        if self
            .detached
            .take_if(|&mut detached| detached == navigation)
            .is_some()
        {
            self.committed = Some(navigation);
            self.load = LoadState::Loading;
            return Some(self.info());
        }
        let pending = self
            .pending
            .take_if(|pending| pending.navigation == navigation)?;
        // A failed load is stored under the URL asked for, so reload
        // retries it (AC-22).
        let url = match kind {
            EntryKind::Document => final_url,
            EntryKind::Error(_) => pending.url,
        };
        let document = DocumentSeq(self.next_document);
        self.next_document += 1;
        let target = match pending.action {
            Action::Traverse { to } => self.index_of(to),
            Action::Replace | Action::Reload => Some(self.current),
            Action::Push => None,
        };
        match target.filter(|&index| index < self.entries.len()) {
            Some(index) => {
                self.current = index;
                let entry = &mut self.entries[index];
                entry.url = url;
                entry.title.clear();
                entry.kind = kind;
                entry.document = document;
                // Leaving it again saves a fresh offset; until then the
                // traverse restore is in flight (AC-23).
                entry.scroll = None;
            }
            None => self.push(url, kind, document),
        }
        self.committed = Some(navigation);
        self.load = LoadState::Loading;
        Some(self.info())
    }

    /// Appends an entry after the current one, dropping those ahead and
    /// the oldest past [`MAX_ENTRIES`].
    fn push(&mut self, url: Url, kind: EntryKind, document: DocumentSeq) {
        if !self.entries.is_empty() {
            self.entries.truncate(self.current + 1);
        }
        let id = EntryId(self.next_entry);
        self.next_entry += 1;
        self.entries.push_back(HistoryEntry {
            id,
            url,
            title: String::new(),
            scroll: None,
            kind,
            document,
        });
        while self.entries.len() > MAX_ENTRIES {
            self.entries.pop_front();
        }
        self.current = self.entries.len() - 1;
    }

    /// The committed document's `load` event fired (AC-25).
    pub fn load_complete(&mut self, navigation: NavigationId) {
        if self.committed == Some(navigation) && self.load == LoadState::Loading {
            self.load = LoadState::Complete;
        }
    }

    /// The current entry's title changed (AC-24).
    pub fn title(&mut self, title: &str) {
        if let Some(entry) = self.entries.get_mut(self.current) {
            entry.title = title.chars().take(MAX_TITLE_CHARS).collect();
        }
    }

    /// Records where entry `id` was scrolled when it was left (AC-23). An
    /// evicted entry is ignored.
    pub fn save_scroll(&mut self, id: EntryId, offset: (f32, f32)) {
        if let Some(index) = self.index_of(id) {
            self.entries[index].scroll = Some(offset);
        }
    }

    /// The action for moving `delta` entries from the current one: a
    /// reload for 0, a traverse inside the history, `None` out of range
    /// (AC-27).
    pub fn go(&self, delta: i32) -> Option<Action> {
        if delta == 0 {
            return (!self.entries.is_empty()).then_some(Action::Reload);
        }
        let index = i64::try_from(self.current).ok()? + i64::from(delta);
        let index = usize::try_from(index).ok()?;
        self.entries
            .get(index)
            .map(|entry| Action::Traverse { to: entry.id })
    }

    /// The user pressed Stop (AC-26, State transitions). Before commit
    /// the pending load is dropped and the state goes back to what it
    /// was; after commit the state stays `Loading` until `load` fires.
    /// The document still on screen lost its own loads when the dropped
    /// one began (the network cancels them, spec 0004 AC-11), so it can
    /// never fire `load`: it counts as complete.
    pub fn stop(&mut self) -> Stopped {
        let dropped = match (self.pending.take(), self.detached.take()) {
            (Some(pending), _) => Some(pending.navigation),
            (None, detached) => detached,
        };
        if let Some(dropped) = dropped {
            self.load = match self.committed {
                Some(_) => LoadState::Complete,
                None => LoadState::Idle,
            };
            return Stopped::BeforeCommit {
                dropped,
                shown: self.committed,
            };
        }
        match (self.load, self.committed) {
            (LoadState::Loading, Some(navigation)) => Stopped::AfterCommit(navigation),
            _ => Stopped::Nothing,
        }
    }

    /// Whether a load is running: from its start until the new document's
    /// `load` event (AC-25).
    pub fn loading(&self) -> bool {
        matches!(self.load, LoadState::Requesting | LoadState::Loading)
    }

    /// The content process crashed: its pending load is gone with it
    /// (spec 0008, Edge cases). The entries stay (AC-34).
    pub fn crashed(&mut self) {
        if let Some(pending) = self.pending.take() {
            self.load = pending.before;
        }
        self.detached = None;
    }

    /// Whether there is an entry before the current one.
    pub fn can_back(&self) -> bool {
        self.current > 0 && !self.entries.is_empty()
    }

    /// Whether there is an entry after the current one.
    pub fn can_forward(&self) -> bool {
        self.current + 1 < self.entries.len()
    }

    /// The entry count and the current index.
    pub fn info(&self) -> HistoryInfo {
        info(self.entries.len(), self.current)
    }

    /// The current entry, once a load committed.
    pub fn current(&self) -> Option<&HistoryEntry> {
        self.entries.get(self.current)
    }

    /// The entry `id`, unless evicted.
    pub fn entry(&self, id: EntryId) -> Option<&HistoryEntry> {
        self.index_of(id).map(|index| &self.entries[index])
    }

    /// The load that has not committed yet.
    pub fn pending(&self) -> Option<&PendingNavigation> {
        self.pending.as_ref()
    }

    /// The navigation whose document is on screen.
    pub fn committed(&self) -> Option<NavigationId> {
        self.committed
    }

    /// The most recent navigation handed out, pending or committed.
    pub fn latest(&self) -> NavigationId {
        self.last_navigation
    }

    /// Where the load is.
    pub fn load_state(&self) -> LoadState {
        self.load
    }

    fn index_of(&self, id: EntryId) -> Option<usize> {
        self.entries.iter().position(|entry| entry.id == id)
    }
}

/// `url` without its fragment, to compare documents.
fn without_fragment(url: &Url) -> Url {
    let mut url = url.clone();
    url.set_fragment(None);
    url
}

/// A [`HistoryInfo`] from sizes that never exceed [`MAX_ENTRIES`].
fn info(length: usize, index: usize) -> HistoryInfo {
    HistoryInfo {
        length: u32::try_from(length).unwrap_or(u32::MAX),
        index: u32::try_from(index).unwrap_or(u32::MAX),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(path: &str) -> Url {
        Url::parse(&format!("https://a.test/{path}")).unwrap()
    }

    /// Begins and commits a load of `path`, returning its navigation.
    fn load(history: &mut SessionHistory, path: &str, action: Action) -> NavigationId {
        let Begin::Load(navigation) = history.begin(url(path), Origin::Browser, action) else {
            panic!("refused");
        };
        history.commit(navigation, url(path), EntryKind::Document);
        navigation
    }

    fn urls(history: &SessionHistory) -> Vec<String> {
        history
            .entries
            .iter()
            .map(|e| e.url.path().to_owned())
            .collect()
    }

    fn back(history: &mut SessionHistory) {
        let Some(Action::Traverse { to }) = history.go(-1) else {
            panic!("no back");
        };
        let target = history.entry(to).unwrap().url.clone();
        let Begin::Load(navigation) =
            history.begin(target.clone(), Origin::Browser, Action::Traverse { to })
        else {
            panic!("refused");
        };
        history.commit(navigation, target, EntryKind::Document);
    }

    #[test]
    fn a_push_drops_every_entry_ahead() {
        // AC-22
        let mut history = SessionHistory::new();
        for path in ["a", "b", "c"] {
            load(&mut history, path, Action::Push);
        }
        back(&mut history);
        back(&mut history);
        assert_eq!(history.info(), info(3, 0));
        assert!(history.can_forward() && !history.can_back());
        load(&mut history, "d", Action::Push);
        assert_eq!(urls(&history), ["/a", "/d"]);
        assert!(!history.can_forward() && history.can_back());
    }

    #[test]
    fn reload_replace_and_the_same_url_keep_the_entry() {
        // AC-22
        let mut history = SessionHistory::new();
        load(&mut history, "a", Action::Push);
        let first = history.current().unwrap().id;
        load(&mut history, "a", Action::Push);
        load(&mut history, "a", Action::Reload);
        load(&mut history, "b", Action::Replace);
        assert_eq!(urls(&history), ["/b"]);
        assert_eq!(history.current().unwrap().id, first);
    }

    #[test]
    fn only_the_final_url_after_redirects_is_stored() {
        // AC-22
        let mut history = SessionHistory::new();
        let Begin::Load(navigation) = history.begin(url("old"), Origin::Browser, Action::Push)
        else {
            panic!("refused");
        };
        history.commit(navigation, url("new"), EntryKind::Document);
        assert_eq!(urls(&history), ["/new"]);
    }

    #[test]
    fn a_failed_load_is_an_entry_under_the_url_asked_for() {
        // AC-22
        let mut history = SessionHistory::new();
        load(&mut history, "a", Action::Push);
        let Begin::Load(navigation) = history.begin(url("down"), Origin::Browser, Action::Push)
        else {
            panic!("refused");
        };
        let error = Url::parse("about:error").unwrap();
        history.commit(navigation, error, EntryKind::Error(FetchError::DnsFailed));
        assert_eq!(urls(&history), ["/a", "/down"]);
        assert_eq!(
            history.current().unwrap().kind,
            EntryKind::Error(FetchError::DnsFailed)
        );
        assert!(history.can_back());
    }

    #[test]
    fn at_most_50_entries_and_the_oldest_go_first() {
        // AC-22
        let mut history = SessionHistory::new();
        for n in 0..60 {
            load(&mut history, &n.to_string(), Action::Push);
        }
        assert_eq!(history.info(), info(MAX_ENTRIES, MAX_ENTRIES - 1));
        assert_eq!(history.entries.front().unwrap().url.path(), "/10");
    }

    #[test]
    fn a_stale_or_replaced_load_does_not_commit() {
        // AC-22, spec 0008 Edge cases
        let mut history = SessionHistory::new();
        let Begin::Load(old) = history.begin(url("a"), Origin::Browser, Action::Push) else {
            panic!("refused");
        };
        let Begin::Load(new) = history.begin(url("b"), Origin::Browser, Action::Push) else {
            panic!("refused");
        };
        assert!(new > old);
        assert_eq!(history.commit(old, url("a"), EntryKind::Document), None);
        assert_eq!(
            history.commit(new, url("b"), EntryKind::Document),
            Some(info(1, 0))
        );
    }

    #[test]
    fn commit_params_predict_the_history_and_carry_the_saved_scroll() {
        // AC-23, AC-27
        let mut history = SessionHistory::new();
        load(&mut history, "a", Action::Push);
        let a = history.current().unwrap().id;
        history.save_scroll(a, (0.0, 640.0));
        load(&mut history, "b", Action::Push);
        let Some(Action::Traverse { to }) = history.go(-1) else {
            panic!("no back");
        };
        history.begin(url("a"), Origin::Browser, Action::Traverse { to });
        let params = history.pending().unwrap().params;
        assert_eq!(params.restore_scroll, Some((0.0, 640.0)));
        assert_eq!(params.history, info(2, 0));
        // A newer push replaces the traverse before it commits, so it
        // lands after "b", where the history still stands.
        let Begin::Load(navigation) = history.begin(url("c"), Origin::Browser, Action::Push) else {
            panic!("refused");
        };
        assert_eq!(history.pending().unwrap().params.history, info(3, 2));
        assert_eq!(
            history.commit(navigation, url("c"), EntryKind::Document),
            Some(info(3, 2))
        );
    }

    #[test]
    fn go_moves_inside_the_history_and_zero_reloads() {
        // AC-21, AC-27
        let mut history = SessionHistory::new();
        assert_eq!(history.go(0), None);
        load(&mut history, "a", Action::Push);
        load(&mut history, "b", Action::Push);
        assert_eq!(history.go(0), Some(Action::Reload));
        assert!(matches!(history.go(-1), Some(Action::Traverse { .. })));
        assert_eq!(history.go(1), None);
        assert_eq!(history.go(-2), None);
        assert_eq!(history.go(i32::MIN), None);
    }

    #[test]
    fn an_evicted_traverse_target_is_refused() {
        // Spec 0008, Edge cases
        let mut history = SessionHistory::new();
        assert_eq!(
            history.begin(
                url("a"),
                Origin::Browser,
                Action::Traverse { to: EntryId(7) }
            ),
            Begin::Refused
        );
    }

    #[test]
    fn load_state_follows_begin_commit_and_load() {
        // Spec 0008, State transitions
        let mut history = SessionHistory::new();
        assert_eq!(history.load_state(), LoadState::Idle);
        let Begin::Load(navigation) = history.begin(url("a"), Origin::Browser, Action::Push) else {
            panic!("refused");
        };
        assert_eq!(history.load_state(), LoadState::Requesting);
        history.commit(navigation, url("a"), EntryKind::Document);
        assert_eq!(history.load_state(), LoadState::Loading);
        history.load_complete(NavigationId(99));
        assert_eq!(history.load_state(), LoadState::Loading, "stale");
        history.load_complete(navigation);
        assert_eq!(history.load_state(), LoadState::Complete);
        history.begin(url("b"), Origin::Browser, Action::Push);
        history.crashed();
        assert_eq!(history.load_state(), LoadState::Complete);
        assert!(history.pending().is_none());
    }

    #[test]
    fn a_detached_load_commits_without_an_entry() {
        // Spec 0004, AC-1
        let mut history = SessionHistory::new();
        load(&mut history, "a", Action::Push);
        let navigation = history.begin_detached();
        let error = Url::parse("about:error").unwrap();
        let kind = EntryKind::Error(FetchError::InvalidUrl);
        assert_eq!(history.commit(navigation, error, kind), Some(info(1, 0)));
        assert_eq!(history.committed(), Some(navigation));
        assert_eq!(urls(&history), ["/a"]);
    }

    #[test]
    fn titles_are_capped_and_cleared_by_a_new_document() {
        // AC-24
        let mut history = SessionHistory::new();
        load(&mut history, "a", Action::Push);
        history.title(&"t".repeat(2000));
        assert_eq!(history.current().unwrap().title.len(), MAX_TITLE_CHARS);
        load(&mut history, "a", Action::Reload);
        assert_eq!(history.current().unwrap().title, "");
    }

    #[test]
    fn stop_before_commit_drops_the_load_and_keeps_the_page() {
        // AC-26, State transitions
        let mut history = SessionHistory::new();
        let first = load(&mut history, "a", Action::Push);
        history.load_complete(first);
        let Begin::Load(slow) = history.begin(url("slow"), Origin::Browser, Action::Push) else {
            panic!("refused");
        };
        assert!(history.loading());
        assert_eq!(
            history.stop(),
            Stopped::BeforeCommit {
                dropped: slow,
                shown: Some(first)
            }
        );
        assert!(!history.loading());
        assert_eq!(urls(&history), ["/a"], "no entry added");
        assert_eq!(history.commit(slow, url("slow"), EntryKind::Document), None);
        assert_eq!(history.stop(), Stopped::Nothing);
    }

    #[test]
    fn stop_after_commit_waits_for_the_load_event() {
        // AC-26
        let mut history = SessionHistory::new();
        let navigation = load(&mut history, "a", Action::Push);
        assert_eq!(history.stop(), Stopped::AfterCommit(navigation));
        assert!(history.loading(), "loading ends at `load`");
        history.load_complete(navigation);
        assert!(!history.loading());
        assert_eq!(history.stop(), Stopped::Nothing);
    }

    #[test]
    fn stop_while_the_page_on_screen_still_loads_counts_it_complete() {
        // AC-26: its own loads were cancelled when the newer one began.
        let mut history = SessionHistory::new();
        load(&mut history, "a", Action::Push);
        history.begin(url("b"), Origin::Browser, Action::Push);
        history.stop();
        assert_eq!(history.load_state(), LoadState::Complete);
    }

    fn same_document(history: &mut SessionHistory, path: &str) -> Begin {
        history.begin(
            url(path),
            Origin::Page(PageNavigationOrigin::Link),
            Action::Push,
        )
    }

    #[test]
    fn a_fragment_of_the_page_on_screen_pushes_without_a_load() {
        // AC-20, AC-22
        let mut history = SessionHistory::new();
        let navigation = load(&mut history, "a", Action::Push);
        history.title("Docs");
        let first = history.current().unwrap().id;
        let Begin::SameDocument {
            leaving,
            history: info,
        } = same_document(&mut history, "a#intro")
        else {
            panic!("a load");
        };
        assert_eq!(leaving, first);
        assert_eq!(info, super::info(2, 1));
        assert_eq!(history.committed(), Some(navigation), "the same document");
        assert_eq!(history.current().unwrap().url.fragment(), Some("intro"));
        assert_eq!(history.current().unwrap().title, "Docs");
        // The same fragment again scrolls, and adds nothing.
        assert!(matches!(
            same_document(&mut history, "a#intro"),
            Begin::SameDocument { .. }
        ));
        assert_eq!(history.info(), super::info(2, 1));
        // Another document loads.
        assert!(matches!(
            same_document(&mut history, "b#intro"),
            Begin::Load(_)
        ));
    }

    #[test]
    fn a_fragment_on_an_error_page_or_without_one_loads() {
        // AC-20: error documents always load; no fragment is a load.
        let mut history = SessionHistory::new();
        load(&mut history, "a", Action::Push);
        assert!(matches!(same_document(&mut history, "a"), Begin::Load(_)));
        let mut history = SessionHistory::new();
        let Begin::Load(navigation) = history.begin(url("x"), Origin::Browser, Action::Push) else {
            panic!("refused");
        };
        history.commit(
            navigation,
            url("x"),
            EntryKind::Error(FetchError::DnsFailed),
        );
        assert!(matches!(same_document(&mut history, "x#a"), Begin::Load(_)));
    }

    #[test]
    fn back_and_forward_inside_one_document_only_scroll() {
        // AC-21, AC-23
        let mut history = SessionHistory::new();
        load(&mut history, "a", Action::Push);
        let top = history.current().unwrap().id;
        same_document(&mut history, "a#b");
        history.save_scroll(top, (0.0, 300.0));
        let Some(Action::Traverse { to }) = history.go(-1) else {
            panic!("no back");
        };
        let moved = history.traverse_within(to).expect("same document");
        assert_eq!(moved.scroll, Some((0.0, 300.0)));
        assert_eq!(moved.history, super::info(2, 0));
        assert_eq!(history.current().unwrap().id, top);
        // An entry of another document is the caller's to load.
        load(&mut history, "c", Action::Push);
        let Some(Action::Traverse { to }) = history.go(-1) else {
            panic!("no back");
        };
        assert_eq!(history.traverse_within(to), None);
    }
}
