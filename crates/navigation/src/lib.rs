//! Navigation rules for one page, as plain data in and out (spec 0008,
//! Decision): the session history and its entry rules, which loads a page
//! may start, and how a URL is shown in the address.
//!
//! Only the browser main thread owns a [`SessionHistory`]; the content
//! process asks with `NavigateRequest` or `HistoryGo` and never names an
//! entry (spec 0008, Key invariants). Nothing here touches IPC, the
//! network or the OS, so the app shell links it too (AC-35).

#![forbid(unsafe_code)]

mod display;
mod history;
mod policy;

pub use display::{DisplayUrl, Security, display_host};
pub use history::{
    Action, Begin, DocumentSeq, EntryId, EntryKind, HistoryEntry, LoadState, MAX_ENTRIES,
    MAX_TITLE_CHARS, Origin, PendingNavigation, SessionHistory, UserInput,
};
pub use policy::{Refused, page_may_load, parse_page_url};
