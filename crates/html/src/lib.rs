//! The HTML parser: bytes in, DOM calls out (spec 0004, child 0002).
//!
//! The parser decodes bytes as they arrive, tokenizes them and builds the
//! tree through a [`TreeSink`], so it never names a `dom` type and the
//! standard test suites can drive it with a sink of their own. It never
//! fails: every input gives some tree (AC-17).
//!
//! Both stages are complete: every tokenizer state with the full named
//! character reference table (AC-15), and every tree construction mode with
//! the adoption agency, foster parenting, templates and foreign content
//! (AC-16), each checked by its `html5lib-tests` suite. Elements never nest
//! deeper than [`MAX_DEPTH`] (AC-17).

#![forbid(unsafe_code)]

mod decode;
mod entities;
mod prescan;
mod tokenizer;
mod tree_builder;

use base::atoms::{LocalName, QualName};

use crate::decode::Decoder;
use crate::tokenizer::{TextState, Token, Tokenizer};
use crate::tree_builder::TreeBuilder;

/// The deepest the open element stack grows. A deeper element is inserted
/// into the current node but not pushed, so no tree gets deeper than this
/// (spec 0004, AC-17).
pub const MAX_DEPTH: usize = 512;

/// One attribute as the tokenizer read it, names already lowercased.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribute {
    /// The attribute's name.
    pub name: QualName,
    /// Its value with character references resolved.
    pub value: String,
}

/// Extra facts about an element the tree builder creates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CreateFlags {
    /// The element is a `<template>`, so it gets a content fragment.
    pub template: bool,
}

/// Something to insert: an existing node, or text that joins a trailing
/// text node when there is one, so adjacent text merges into one node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeOrText<'a, H> {
    /// A node the sink created.
    Node(H),
    /// A run of text.
    Text(&'a str),
}

/// The document's quirks mode, set from its doctype.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum QuirksMode {
    /// Standards mode.
    #[default]
    NoQuirks,
    /// Almost standards mode.
    LimitedQuirks,
    /// Quirks mode.
    Quirks,
}

/// Where the parser builds its tree (spec 0004, child 0002, `TreeSink`).
///
/// `page` implements it over the DOM's mutation API; tests implement it
/// over a plain tree. Every call must succeed: the parser only passes
/// handles the sink gave it.
pub trait TreeSink {
    /// The sink's name for a node.
    type Handle: Copy + Eq;

    /// The document node.
    fn document(&self) -> Self::Handle;

    /// Creates an element that is not in the tree yet.
    fn create_element(
        &mut self,
        name: QualName,
        attrs: Vec<Attribute>,
        flags: CreateFlags,
    ) -> Self::Handle;

    /// Creates a comment that is not in the tree yet.
    fn create_comment(&mut self, text: &str) -> Self::Handle;

    /// Appends `child` as `parent`'s last child; text joins a trailing
    /// text node.
    fn append(&mut self, parent: Self::Handle, child: NodeOrText<'_, Self::Handle>);

    /// Inserts `child` just before `sibling`; text joins a text node just
    /// before it.
    fn insert_before(&mut self, sibling: Self::Handle, child: NodeOrText<'_, Self::Handle>);

    /// Appends a doctype to the document.
    fn append_doctype(&mut self, name: &str, public_id: &str, system_id: &str);

    /// Adds each attribute `target` does not have yet.
    fn add_attrs_if_missing(&mut self, target: Self::Handle, attrs: Vec<Attribute>);

    /// Takes `target` out of its parent, if it has one.
    fn remove_from_parent(&mut self, target: Self::Handle);

    /// Moves every child of `from` to the end of `to`.
    fn reparent_children(&mut self, from: Self::Handle, to: Self::Handle);

    /// A `<template>`'s content fragment.
    fn template_contents(&self, template: Self::Handle) -> Self::Handle;

    /// `node`'s parent, if it has one.
    fn parent(&self, node: Self::Handle) -> Option<Self::Handle>;

    /// Records the document's quirks mode.
    fn set_quirks_mode(&mut self, mode: QuirksMode);

    /// The name of the element `node`.
    fn element_name(&self, node: Self::Handle) -> &QualName;
}

/// How to parse one document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParserOptions {
    /// The charset from the `Content-Type` header, if any. A byte order
    /// mark still wins over it.
    pub encoding_hint: Option<String>,
    /// Whether scripting is on, which makes `<noscript>` content raw text.
    pub scripting: bool,
    /// The document is `text/plain`: every byte is text, shown inside one
    /// `<pre>` (spec 0004, AC-7).
    pub plain_text: bool,
}

/// What the parser needs next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseStep<H> {
    /// Everything given so far is in the tree; feed more bytes or end.
    Continue,
    /// Paused right after the end tag of this `<script>` (scripting on):
    /// the page prepares it, runs it if it must, then calls
    /// [`Parser::resume`] (spec 0004, AC-31). Input fed meanwhile waits.
    RunScript(H),
    /// The document is complete.
    Done,
}

/// A streaming HTML parser for one document, building nodes named by `H`.
///
/// The sink is passed to each call rather than owned, so the caller keeps
/// its document between chunks (the page lays it out while it loads). Every
/// call must pass a sink over the same tree.
pub struct Parser<H> {
    decoder: Decoder,
    tokenizer: Tokenizer,
    builder: TreeBuilder<H>,
    /// A plain text document whose `<pre>` is not inserted yet.
    plain_text: bool,
    /// Whether to pause at scripts.
    scripting: bool,
    /// The script it is paused at.
    paused: Option<H>,
    done: bool,
}

impl<H: Copy + Eq> Parser<H> {
    /// A parser at the start of a document.
    pub fn new(options: ParserOptions) -> Self {
        Self {
            decoder: Decoder::new(options.encoding_hint.as_deref()),
            tokenizer: Tokenizer::new(),
            builder: TreeBuilder::new(options.scripting),
            plain_text: options.plain_text,
            scripting: options.scripting,
            paused: None,
            done: false,
        }
    }

    /// The name of the encoding the document is decoded with, once known.
    pub fn encoding(&self) -> &'static str {
        self.decoder.encoding_name()
    }

    /// How many tokenizer parse errors the document had so far. Errors
    /// never stop the parser; this is for logs and counters.
    pub fn parse_errors(&self) -> u32 {
        self.tokenizer.errors()
    }

    /// Parses `bytes`, the next part of the document, as far as they go.
    /// Input that ends partway through a tag waits for the next call. While
    /// paused at a script, the bytes only queue.
    pub fn feed<S: TreeSink<Handle = H>>(&mut self, sink: &mut S, bytes: &[u8]) -> ParseStep<H> {
        if self.done {
            return ParseStep::Done;
        }
        let text = self.decoder.decode(bytes, false);
        self.tokenizer.push(&text);
        self.run(sink)
    }

    /// Parses whatever is left: the document has no more bytes. While
    /// paused at a script, the end only queues.
    pub fn end<S: TreeSink<Handle = H>>(&mut self, sink: &mut S) -> ParseStep<H> {
        if self.done {
            return ParseStep::Done;
        }
        let text = self.decoder.decode(&[], true);
        self.tokenizer.push(&text);
        self.tokenizer.set_eof();
        self.run(sink)
    }

    /// Goes on after the script it paused at.
    pub fn resume<S: TreeSink<Handle = H>>(&mut self, sink: &mut S) -> ParseStep<H> {
        self.paused = None;
        if self.done {
            return ParseStep::Done;
        }
        self.run(sink)
    }

    /// The script the parser is paused at, if any.
    pub fn paused_at(&self) -> Option<H> {
        self.paused
    }

    /// A parser for the suite's fragment cases: the children of the
    /// returned root are the fragment parsed in `context`.
    #[cfg(test)]
    pub(crate) fn fragment<S: TreeSink<Handle = H>>(
        sink: &mut S,
        options: ParserOptions,
        context: QualName,
    ) -> (Self, H) {
        let (builder, root, state) = TreeBuilder::fragment(sink, options.scripting, context);
        let mut tokenizer = Tokenizer::new();
        tokenizer.switch_to(state);
        tokenizer.set_cdata_allowed(builder.in_foreign_content());
        let parser = Self {
            decoder: Decoder::new(options.encoding_hint.as_deref()),
            tokenizer,
            builder,
            plain_text: false,
            scripting: options.scripting,
            paused: None,
            done: false,
        };
        (parser, root)
    }

    fn run<S: TreeSink<Handle = H>>(&mut self, sink: &mut S) -> ParseStep<H> {
        if let Some(script) = self.paused {
            return ParseStep::RunScript(script);
        }
        if std::mem::take(&mut self.plain_text) {
            // The standard's text/plain steps: act as if the tokenizer had
            // emitted a `<pre>` start tag and a line feed, then read the
            // rest in the PLAINTEXT state.
            let pre = Token::StartTag {
                name: LocalName::from("pre"),
                attrs: Vec::new(),
                self_closing: false,
            };
            self.builder.process(sink, pre);
            self.builder.process(sink, Token::Characters("\n".into()));
            self.tokenizer.switch_to(TextState::PlainText);
        }
        while let Some(token) = self.tokenizer.next_token() {
            let eof = matches!(token, tokenizer::Token::Eof);
            if let Some(state) = self.builder.process(sink, token) {
                self.tokenizer.switch_to(state);
            }
            self.tokenizer
                .set_cdata_allowed(self.builder.in_foreign_content());
            if let Some(script) = self.builder.take_pending_script()
                && self.scripting
            {
                self.paused = Some(script);
                return ParseStep::RunScript(script);
            }
            if eof {
                self.done = true;
                return ParseStep::Done;
            }
        }
        ParseStep::Continue
    }
}

#[cfg(test)]
mod tests;
