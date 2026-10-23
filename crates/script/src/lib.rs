//! The script boundary (spec 0004, child 0006; spec 0001, script boundary).
//!
//! Engine neutral: the page runs scripts through a [`ScriptEngine`], and
//! implements [`ScriptHost`], every DOM operation the bindings need, in
//! terms of [`NodeRef`] and plain values. Only `script-quickjs` names the
//! JavaScript engine, so swapping engines means rewriting that crate alone.

#![forbid(unsafe_code)]

use dom::{DomError, NodeRef};

/// Why a script, a callback or an event handler stopped.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScriptError {
    /// It threw, and nothing caught it.
    #[error("{url}:{line}: {message}")]
    Exception {
        /// The exception, as `String(error)` gives it.
        message: String,
        /// The script it came from.
        url: String,
        /// The line it was thrown at, 0 when unknown.
        line: u32,
    },
    /// It ran past the task's 5 s deadline and was stopped (AC-35).
    #[error("the script ran past its 5 s deadline")]
    Interrupted,
    /// It ran out of the page's 256 MB heap (AC-35).
    #[error("the script ran out of its heap")]
    OutOfMemory,
    /// The engine itself failed, such as at startup.
    #[error("the script engine failed: {0}")]
    Engine(String),
}

/// A timer, by its `setTimeout` id: positive, per page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TimerId(pub u32);

/// Where a lifecycle event is fired (AC-33).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventTarget {
    /// `window`: `load`.
    Window,
    /// `document`: `DOMContentLoaded`, which bubbles to `window`.
    Document,
}

/// A `console` method's level, mapped onto `tracing`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleLevel {
    /// `console.debug`.
    Debug,
    /// `console.log`.
    Log,
    /// `console.info`.
    Info,
    /// `console.warn`.
    Warn,
    /// `console.error`.
    Error,
}

/// What kind of node a wrapper stands for, which picks its prototype.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// An element.
    Element,
    /// A text node.
    Text,
    /// A comment.
    Comment,
    /// The document.
    Document,
    /// A doctype.
    Doctype,
    /// A document fragment or shadow root.
    Fragment,
}

impl NodeKind {
    /// The DOM `nodeType` number.
    pub fn node_type(self) -> u16 {
        match self {
            Self::Element => 1,
            Self::Text => 3,
            Self::Comment => 8,
            Self::Document => 9,
            Self::Doctype => 10,
            Self::Fragment => 11,
        }
    }
}

/// The document's `readyState` (spec 0004, Navigation lifecycle).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReadyState {
    /// Parsing.
    #[default]
    Loading,
    /// Parsing ended, `defer` scripts ran, `DOMContentLoaded` fired.
    Interactive,
    /// Every subresource finished and `load` fired.
    Complete,
}

impl ReadyState {
    /// The string `document.readyState` gives.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Loading => "loading",
            Self::Interactive => "interactive",
            Self::Complete => "complete",
        }
    }
}

/// A rejected DOM call, which scripts see as a `DOMException` with the
/// standard name (AC-32).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DomException {
    /// `HierarchyRequestError`.
    #[error("the node cannot be inserted there")]
    HierarchyRequest,
    /// `NotFoundError`.
    #[error("the node was not found")]
    NotFound,
    /// `InvalidStateError`.
    #[error("the node is not in a state that allows this")]
    InvalidState,
    /// `InvalidCharacterError`: a bad element or attribute name.
    #[error("the name is not valid")]
    InvalidCharacter,
    /// `SyntaxError`: a selector that does not parse, or is not supported.
    #[error("the selector is not valid")]
    Syntax,
    /// `NotSupportedError`.
    #[error("the operation is not supported")]
    NotSupported,
}

impl DomException {
    /// The `DOMException` name.
    pub fn name(self) -> &'static str {
        match self {
            Self::HierarchyRequest => "HierarchyRequestError",
            Self::NotFound => "NotFoundError",
            Self::InvalidState => "InvalidStateError",
            Self::InvalidCharacter => "InvalidCharacterError",
            Self::Syntax => "SyntaxError",
            Self::NotSupported => "NotSupportedError",
        }
    }
}

impl From<DomError> for DomException {
    fn from(err: DomError) -> Self {
        match err {
            DomError::HierarchyRequest => Self::HierarchyRequest,
            DomError::NotFound => Self::NotFound,
            DomError::InvalidState => Self::InvalidState,
            DomError::NotSupported => Self::NotSupported,
        }
    }
}

/// Everything the bindings ask of the page (spec 0004, child 0006). Each
/// call does one DOM operation and returns; the page never holds a borrow
/// of its state across a call into the engine.
///
/// Nodes come back from scripts, so any [`NodeRef`] may name a node that
/// was freed or lives in another frame: those count as missing.
pub trait ScriptHost {
    /// The document node.
    fn document(&self) -> NodeRef;
    /// `document.documentElement`.
    fn document_element(&self) -> Option<NodeRef>;
    /// `document.body`.
    fn body(&self) -> Option<NodeRef>;
    /// `document.head`.
    fn head(&self) -> Option<NodeRef>;
    /// `document.readyState`.
    fn ready_state(&self) -> ReadyState;
    /// `document.getElementById`.
    fn element_by_id(&self, id: &str) -> Option<NodeRef>;
    /// The elements below `scope` that match `selectors`, in tree order
    /// (`querySelectorAll`; `querySelector` takes the first).
    fn query_selector_all(
        &self,
        scope: NodeRef,
        selectors: &str,
    ) -> Result<Vec<NodeRef>, DomException>;
    /// `document.createElement`.
    fn create_element(&mut self, local_name: &str) -> Result<NodeRef, DomException>;
    /// `document.createTextNode`.
    fn create_text_node(&mut self, data: &str) -> NodeRef;

    /// What kind of node `node` is, or `None` when it is missing.
    fn node_kind(&self, node: NodeRef) -> Option<NodeKind>;
    /// `nodeName`: the upper case tag name of an HTML element, `#text`,
    /// `#comment`, `#document`, and so on.
    fn node_name(&self, node: NodeRef) -> Option<String>;
    /// `parentNode`.
    fn parent(&self, node: NodeRef) -> Option<NodeRef>;
    /// `firstChild`.
    fn first_child(&self, node: NodeRef) -> Option<NodeRef>;
    /// `lastChild`.
    fn last_child(&self, node: NodeRef) -> Option<NodeRef>;
    /// `nextSibling`.
    fn next_sibling(&self, node: NodeRef) -> Option<NodeRef>;
    /// `previousSibling`.
    fn previous_sibling(&self, node: NodeRef) -> Option<NodeRef>;
    /// `childNodes`, as it is now.
    fn child_nodes(&self, node: NodeRef) -> Vec<NodeRef>;
    /// `textContent`: `None` for the document and doctypes.
    fn text_content(&self, node: NodeRef) -> Option<String>;
    /// Sets `textContent`: an element's children are replaced by one text
    /// node (none for empty text).
    fn set_text_content(&mut self, node: NodeRef, text: &str) -> Result<(), DomException>;
    /// `insertBefore`; with no `reference`, `appendChild`.
    fn insert_before(
        &mut self,
        parent: NodeRef,
        child: NodeRef,
        reference: Option<NodeRef>,
    ) -> Result<(), DomException>;
    /// `removeChild`.
    fn remove_child(&mut self, parent: NodeRef, child: NodeRef) -> Result<(), DomException>;
    /// `replaceChild`.
    fn replace_child(
        &mut self,
        parent: NodeRef,
        new_child: NodeRef,
        old_child: NodeRef,
    ) -> Result<(), DomException>;

    /// `getAttribute`.
    fn attribute(&self, element: NodeRef, name: &str) -> Option<String>;
    /// `setAttribute`.
    fn set_attribute(
        &mut self,
        element: NodeRef,
        name: &str,
        value: &str,
    ) -> Result<(), DomException>;
    /// `removeAttribute`.
    fn remove_attribute(&mut self, element: NodeRef, name: &str) -> Result<(), DomException>;

    /// A wrapper for `node` now exists: keep the node alive (0002's pin
    /// rule, AC-34).
    fn pin(&mut self, node: NodeRef);
    /// The wrapper for `node` was collected.
    fn unpin(&mut self, node: NodeRef);

    /// Schedules a timer `delay_ms` from now, clamped as the HTML standard
    /// says, and returns its id.
    fn set_timeout(&mut self, delay_ms: f64) -> TimerId;
    /// Cancels a timer.
    fn clear_timeout(&mut self, id: TimerId);
    /// A `console` call, its arguments already joined.
    fn console(&mut self, level: ConsoleLevel, message: &str);
}

/// A page's script engine: one runtime and one global object per committed
/// document (spec 0004, child 0006, Runtime per page). Each call is one
/// piece of a task; the page runs microtasks after it.
pub trait ScriptEngine {
    /// Runs a classic script's source; `url` names it in errors.
    fn eval_classic(&mut self, source: &str, url: &str) -> Result<(), ScriptError>;
    /// Fires a lifecycle event (AC-33).
    fn fire_event(&mut self, target: EventTarget, event: &str) -> Result<(), ScriptError>;
    /// Runs a due timer's callback, which is then forgotten.
    fn run_timer(&mut self, id: TimerId) -> Result<(), ScriptError>;
    /// Runs promise jobs until none are left.
    fn run_microtasks(&mut self) -> Result<(), ScriptError>;
    /// Unpins the nodes whose wrappers were collected, and forgets them.
    /// Runs at the end of each task, never inside the collector.
    fn sweep_wrappers(&mut self);
}
