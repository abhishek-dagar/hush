//! The tree builder: tokens to tree calls (spec 0004, child 0002, Tree
//! builder).
//!
//! The standard's tree construction stage in full: every insertion mode,
//! the stack of open elements, the list of active formatting elements and
//! the adoption agency, foster parenting for stray table content, the
//! template mode stack, foreign content (SVG and MathML, with their name
//! fixes) and quirks mode from the doctype. `<select>` follows the 2025
//! rules (its content is parsed like any other), which is what the
//! vendored `html5lib-tests` expect.
//!
//! Two departures, both deliberate:
//! - The depth cap (AC-17): with [`MAX_DEPTH`] elements open, a new
//!   element is inserted but not opened, so no tree is deeper.
//! - Text arrives in runs, not one character at a time. Modes that treat
//!   whitespace and other text differently get the run split at each
//!   change; the rest take it whole.

use base::atoms::{LocalName, Namespace, Prefix, QualName};
use base::ns;

use crate::tokenizer::{TextState, Token};
use crate::{Attribute, CreateFlags, MAX_DEPTH, NodeOrText, QuirksMode, TreeSink};

mod names;

use names::*;

/// How many times one token may be handed between modes. The standard's
/// hand offs always end well before this; the cap only guards against a
/// bug turning into a hang.
const MAX_REPROCESS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Initial,
    BeforeHtml,
    BeforeHead,
    InHead,
    InHeadNoscript,
    AfterHead,
    InBody,
    /// Inside a raw text or RCDATA element, until its end tag.
    Text,
    InTable,
    InTableText,
    InCaption,
    InColumnGroup,
    InTableBody,
    InRow,
    InCell,
    InTemplate,
    AfterBody,
    InFrameset,
    AfterFrameset,
    AfterAfterBody,
    AfterAfterFrameset,
}

impl Mode {
    /// Whether the mode treats whitespace and other text alike, so a text
    /// run can go to it whole.
    fn takes_whole_text(self) -> bool {
        matches!(
            self,
            Mode::InBody
                | Mode::Text
                | Mode::InTable
                | Mode::InTableText
                | Mode::InCaption
                | Mode::InTableBody
                | Mode::InRow
                | Mode::InCell
                | Mode::InTemplate
        )
    }
}

/// An element's namespace, as far as tree construction cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ns {
    Html,
    MathMl,
    Svg,
}

impl Ns {
    fn atom(self) -> Namespace {
        match self {
            Ns::Html => ns!("http://www.w3.org/1999/xhtml"),
            Ns::MathMl => ns!("http://www.w3.org/1998/Math/MathML"),
            Ns::Svg => ns!("http://www.w3.org/2000/svg"),
        }
    }
}

/// An entry on the stack of open elements.
#[derive(Debug, Clone)]
struct Open<H> {
    handle: H,
    ns: Ns,
    name: LocalName,
    /// A MathML `annotation-xml` whose `encoding` makes its content HTML.
    html_annotation: bool,
}

impl<H> Open<H> {
    fn is_html(&self, name: &str) -> bool {
        self.ns == Ns::Html && &*self.name == name
    }

    fn is_html_in(&self, names: &[&str]) -> bool {
        self.ns == Ns::Html && names.contains(&&*self.name)
    }

    fn is_special(&self) -> bool {
        match self.ns {
            Ns::Html => SPECIAL_HTML.contains(&&*self.name),
            Ns::MathMl => {
                MATHML_TEXT_INTEGRATION.contains(&&*self.name) || &*self.name == "annotation-xml"
            }
            Ns::Svg => SVG_HTML_INTEGRATION.contains(&&*self.name),
        }
    }

    fn is_mathml_text_integration(&self) -> bool {
        self.ns == Ns::MathMl && MATHML_TEXT_INTEGRATION.contains(&&*self.name)
    }

    fn is_html_integration(&self) -> bool {
        match self.ns {
            Ns::MathMl => self.html_annotation,
            Ns::Svg => SVG_HTML_INTEGRATION.contains(&&*self.name),
            Ns::Html => false,
        }
    }

    /// Whether foster parenting applies when this is the insertion target.
    fn is_table_part(&self) -> bool {
        self.is_html_in(&["table", "tbody", "tfoot", "thead", "tr"])
    }
}

/// A formatting element's start tag, kept to make copies of it.
#[derive(Debug, Clone)]
struct Tag {
    name: LocalName,
    attrs: Vec<Attribute>,
}

/// An entry in the list of active formatting elements.
#[derive(Debug, Clone)]
enum Formatting<H> {
    Marker,
    Element { handle: H, tag: Tag },
}

/// Where a node goes: the standard's "appropriate place for inserting a
/// node".
#[derive(Debug, Clone, Copy)]
enum Place<H> {
    Append(H),
    Before(H),
}

/// Which elements end a scope search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    Default,
    ListItem,
    Button,
    Table,
}

/// What a mode did with a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Done,
    /// The mode changed: run the same token again.
    Reprocess,
}

/// Where the adoption agency puts the formatting element's copy in the
/// list of active formatting elements.
#[derive(Debug, Clone, Copy)]
enum Bookmark<H> {
    Replace(H),
    InsertAfter(H),
}

pub(crate) struct TreeBuilder<H> {
    mode: Mode,
    /// The mode to return to after `Text` or `InTableText`.
    original_mode: Mode,
    template_modes: Vec<Mode>,
    open: Vec<Open<H>>,
    formatting: Vec<Formatting<H>>,
    head: Option<H>,
    form: Option<H>,
    /// The raw text or RCDATA element `Text` mode fills.
    text_element: Option<H>,
    /// A `<script>` whose end tag was just processed, for the parser to
    /// pause at (spec 0004, child 0002, Scripts and the parser).
    pending_script: Option<H>,
    /// The fragment parsing context element, for the suite's fragment cases.
    context: Option<Open<H>>,
    scripting: bool,
    frameset_ok: bool,
    foster_parenting: bool,
    quirks: QuirksMode,
    pending_table_text: String,
    pending_table_text_is_space: bool,
    /// The next line feed is dropped (right after `<pre>`, `<listing>` and
    /// `<textarea>`).
    skip_newline: bool,
    /// The tokenizer state a mode asked for while handling the token.
    switch_to: Option<TextState>,
}

fn is_whitespace(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\x0C' | '\r' | ' ')
}

fn is_all_whitespace(text: &str) -> bool {
    text.chars().all(is_whitespace)
}

/// The leading run of `text` that is all whitespace or all not.
fn leading_run(text: &str) -> &str {
    let first_space = text.starts_with(is_whitespace);
    let end = text
        .find(|c| is_whitespace(c) != first_space)
        .unwrap_or(text.len());
    &text[..end]
}

fn attr<'a>(attrs: &'a [Attribute], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|a| &*a.name.local == name)
        .map(|a| a.value.as_str())
}

/// Whether the attribute lists hold the same names and values, in any
/// order (the "Noah's Ark" comparison).
fn same_attributes(a: &[Attribute], b: &[Attribute]) -> bool {
    a.len() == b.len()
        && a.iter()
            .all(|x| b.iter().any(|y| y.name == x.name && y.value == x.value))
}

impl<H: Copy + Eq> TreeBuilder<H> {
    pub(crate) fn new(scripting: bool) -> Self {
        Self {
            mode: Mode::Initial,
            original_mode: Mode::InBody,
            template_modes: Vec::new(),
            open: Vec::new(),
            formatting: Vec::new(),
            head: None,
            form: None,
            text_element: None,
            pending_script: None,
            context: None,
            scripting,
            frameset_ok: true,
            foster_parenting: false,
            quirks: QuirksMode::NoQuirks,
            pending_table_text: String::new(),
            pending_table_text_is_space: true,
            skip_newline: false,
            switch_to: None,
        }
    }

    /// A builder for the fragment parsing algorithm, with `context` as the
    /// context element: the root `html` element is created and opened, and
    /// the mode is set from the context. Returns the builder, the root and
    /// the tokenizer state the context asks for.
    #[cfg(test)]
    pub(crate) fn fragment<S: TreeSink<Handle = H>>(
        sink: &mut S,
        scripting: bool,
        context: QualName,
    ) -> (Self, H, TextState) {
        let mut builder = Self::new(scripting);
        let ns = if context.ns == Ns::Svg.atom() {
            Ns::Svg
        } else if context.ns == Ns::MathMl.atom() {
            Ns::MathMl
        } else {
            Ns::Html
        };
        let state = match (ns, &*context.local) {
            (Ns::Html, "title" | "textarea") => TextState::RcData,
            (Ns::Html, "style" | "xmp" | "iframe" | "noembed" | "noframes") => TextState::RawText,
            (Ns::Html, "noscript") if scripting => TextState::RawText,
            (Ns::Html, "script") => TextState::ScriptData,
            (Ns::Html, "plaintext") => TextState::PlainText,
            _ => TextState::Data,
        };
        let context_handle =
            sink.create_element(context.clone(), Vec::new(), CreateFlags::default());
        let root = sink.create_element(
            QualName::html(LocalName::from("html")),
            Vec::new(),
            CreateFlags::default(),
        );
        sink.append(sink.document(), NodeOrText::Node(root));
        builder.open.push(Open {
            handle: root,
            ns: Ns::Html,
            name: LocalName::from("html"),
            html_annotation: false,
        });
        if ns == Ns::Html && &*context.local == "template" {
            builder.template_modes.push(Mode::InTemplate);
        }
        builder.context = Some(Open {
            handle: context_handle,
            ns,
            name: context.local,
            html_annotation: false,
        });
        builder.reset_insertion_mode();
        (builder, root, state)
    }

    /// The `<script>` whose end tag the last token was, if any.
    pub(crate) fn take_pending_script(&mut self) -> Option<H> {
        self.pending_script.take()
    }

    /// Whether `<![CDATA[` opens a CDATA section next: the adjusted
    /// current node is not an HTML element.
    pub(crate) fn in_foreign_content(&self) -> bool {
        self.adjusted_current()
            .is_some_and(|node| node.ns != Ns::Html)
    }

    /// Builds from one token, and says which text state the tokenizer
    /// should switch to, if any.
    pub(crate) fn process<S: TreeSink<Handle = H>>(
        &mut self,
        sink: &mut S,
        token: Token,
    ) -> Option<TextState> {
        let mut token = token;
        if std::mem::take(&mut self.skip_newline)
            && let Token::Characters(text) = &mut token
            && text.starts_with('\n')
        {
            text.remove(0);
        }
        match &token {
            Token::Characters(text) => self.characters(sink, text),
            _ => self.dispatch(sink, &token),
        }
        self.switch_to.take()
    }

    /// Hands a text run to the modes, split where a mode needs it split.
    fn characters<S: TreeSink<Handle = H>>(&mut self, sink: &mut S, text: &str) {
        let mut rest = text;
        while !rest.is_empty() {
            let whole = !self.uses_html_rules_for_text() || self.mode.takes_whole_text();
            let run = if whole { rest } else { leading_run(rest) };
            self.dispatch(sink, &Token::Characters(run.to_owned()));
            rest = &rest[run.len()..];
        }
    }

    /// The tree construction dispatcher: HTML rules by mode, or the rules
    /// for foreign content.
    fn dispatch<S: TreeSink<Handle = H>>(&mut self, sink: &mut S, token: &Token) {
        for _ in 0..MAX_REPROCESS {
            let step = if self.uses_html_rules(token) {
                self.run_mode(self.mode, sink, token)
            } else {
                self.foreign_content(sink, token)
            };
            if step == Step::Done {
                return;
            }
        }
        tracing::trace!("tree builder: token handed on too often, dropped");
    }

    fn uses_html_rules_for_text(&self) -> bool {
        self.uses_html_rules(&Token::Characters(String::new()))
    }

    fn uses_html_rules(&self, token: &Token) -> bool {
        let Some(node) = self.adjusted_current() else {
            return true;
        };
        if node.ns == Ns::Html {
            return true;
        }
        match token {
            Token::StartTag { name, .. } => {
                (node.is_mathml_text_integration() && !matches!(&**name, "mglyph" | "malignmark"))
                    || (node.ns == Ns::MathMl
                        && &*node.name == "annotation-xml"
                        && &**name == "svg")
                    || node.is_html_integration()
            }
            Token::Characters(_) => node.is_mathml_text_integration() || node.is_html_integration(),
            Token::Eof => true,
            _ => false,
        }
    }

    fn run_mode<S: TreeSink<Handle = H>>(
        &mut self,
        mode: Mode,
        sink: &mut S,
        token: &Token,
    ) -> Step {
        match mode {
            Mode::Initial => self.initial(sink, token),
            Mode::BeforeHtml => self.before_html(sink, token),
            Mode::BeforeHead => self.before_head(sink, token),
            Mode::InHead => self.in_head(sink, token),
            Mode::InHeadNoscript => self.in_head_noscript(sink, token),
            Mode::AfterHead => self.after_head(sink, token),
            Mode::InBody => self.in_body(sink, token),
            Mode::Text => self.text(sink, token),
            Mode::InTable => self.in_table(sink, token),
            Mode::InTableText => self.in_table_text(sink, token),
            Mode::InCaption => self.in_caption(sink, token),
            Mode::InColumnGroup => self.in_column_group(sink, token),
            Mode::InTableBody => self.in_table_body(sink, token),
            Mode::InRow => self.in_row(sink, token),
            Mode::InCell => self.in_cell(sink, token),
            Mode::InTemplate => self.in_template(sink, token),
            Mode::AfterBody => self.after_body(sink, token),
            Mode::InFrameset => self.in_frameset(sink, token),
            Mode::AfterFrameset => self.after_frameset(sink, token),
            Mode::AfterAfterBody => self.after_after_body(sink, token),
            Mode::AfterAfterFrameset => self.after_after_frameset(sink, token),
        }
    }

    // ----- The insertion modes, in the standard's order -----

    fn initial<S: TreeSink<Handle = H>>(&mut self, sink: &mut S, token: &Token) -> Step {
        match token {
            Token::Characters(text) if is_all_whitespace(text) => return Step::Done,
            Token::Comment(text) => {
                self.append_comment_to(sink, sink.document(), text);
                return Step::Done;
            }
            Token::Doctype {
                name,
                public_id,
                system_id,
                force_quirks,
            } => {
                sink.append_doctype(
                    name.as_deref().unwrap_or_default(),
                    public_id.as_deref().unwrap_or_default(),
                    system_id.as_deref().unwrap_or_default(),
                );
                let mode = quirks_mode(
                    *force_quirks,
                    name.as_deref(),
                    public_id.as_deref(),
                    system_id.as_deref(),
                );
                self.set_quirks(sink, mode);
                self.mode = Mode::BeforeHtml;
                return Step::Done;
            }
            _ => {}
        }
        self.set_quirks(sink, QuirksMode::Quirks);
        self.mode = Mode::BeforeHtml;
        Step::Reprocess
    }

    fn before_html<S: TreeSink<Handle = H>>(&mut self, sink: &mut S, token: &Token) -> Step {
        match token {
            Token::Doctype { .. } => return Step::Done,
            Token::Comment(text) => {
                self.append_comment_to(sink, sink.document(), text);
                return Step::Done;
            }
            Token::Characters(text) if is_all_whitespace(text) => return Step::Done,
            Token::StartTag { name, attrs, .. } if &**name == "html" => {
                self.insert_root(sink, attrs.clone());
                self.mode = Mode::BeforeHead;
                return Step::Done;
            }
            Token::EndTag { name } if !matches!(&**name, "head" | "body" | "html" | "br") => {
                return Step::Done;
            }
            _ => {}
        }
        self.insert_root(sink, Vec::new());
        self.mode = Mode::BeforeHead;
        Step::Reprocess
    }

    fn before_head<S: TreeSink<Handle = H>>(&mut self, sink: &mut S, token: &Token) -> Step {
        match token {
            Token::Characters(text) if is_all_whitespace(text) => return Step::Done,
            Token::Comment(text) => {
                self.insert_comment(sink, text);
                return Step::Done;
            }
            Token::Doctype { .. } => return Step::Done,
            Token::StartTag { name, .. } if &**name == "html" => return self.in_body(sink, token),
            Token::StartTag { name, attrs, .. } if &**name == "head" => {
                let head = self.insert_html(sink, name, attrs.clone());
                self.head = Some(head);
                self.mode = Mode::InHead;
                return Step::Done;
            }
            Token::EndTag { name } if !matches!(&**name, "head" | "body" | "html" | "br") => {
                return Step::Done;
            }
            _ => {}
        }
        let head = self.insert_html(sink, &LocalName::from("head"), Vec::new());
        self.head = Some(head);
        self.mode = Mode::InHead;
        Step::Reprocess
    }

    fn in_head<S: TreeSink<Handle = H>>(&mut self, sink: &mut S, token: &Token) -> Step {
        match token {
            Token::Characters(text) if is_all_whitespace(text) => {
                self.insert_text(sink, text);
                return Step::Done;
            }
            Token::Comment(text) => {
                self.insert_comment(sink, text);
                return Step::Done;
            }
            Token::Doctype { .. } => return Step::Done,
            Token::StartTag { name, attrs, .. } => match &**name {
                "html" => return self.in_body(sink, token),
                "base" | "basefont" | "bgsound" | "link" | "meta" => {
                    self.insert_html_void(sink, name, attrs.clone());
                    return Step::Done;
                }
                "title" => return self.insert_text_element(sink, name, attrs, TextState::RcData),
                "noscript" if self.scripting => {
                    return self.insert_text_element(sink, name, attrs, TextState::RawText);
                }
                "noframes" | "style" => {
                    return self.insert_text_element(sink, name, attrs, TextState::RawText);
                }
                "noscript" => {
                    self.insert_html(sink, name, attrs.clone());
                    self.mode = Mode::InHeadNoscript;
                    return Step::Done;
                }
                "script" => {
                    return self.insert_text_element(sink, name, attrs, TextState::ScriptData);
                }
                "template" => {
                    let template = self.insert_html(sink, name, attrs.clone());
                    if self.is_current(template) {
                        self.formatting.push(Formatting::Marker);
                        self.frameset_ok = false;
                        self.mode = Mode::InTemplate;
                        self.template_modes.push(Mode::InTemplate);
                    }
                    return Step::Done;
                }
                "head" => return Step::Done,
                _ => {}
            },
            Token::EndTag { name } => match &**name {
                "head" => {
                    self.pop();
                    self.mode = Mode::AfterHead;
                    return Step::Done;
                }
                "template" => {
                    self.end_template();
                    return Step::Done;
                }
                "body" | "html" | "br" => {}
                _ => return Step::Done,
            },
            _ => {}
        }
        self.pop();
        self.mode = Mode::AfterHead;
        Step::Reprocess
    }

    fn end_template(&mut self) {
        if !self.open.iter().any(|e| e.is_html("template")) {
            return;
        }
        self.generate_implied_end_tags_thoroughly();
        self.pop_until_html(&["template"]);
        self.clear_formatting_to_marker();
        self.template_modes.pop();
        self.reset_insertion_mode();
    }

    fn in_head_noscript<S: TreeSink<Handle = H>>(&mut self, sink: &mut S, token: &Token) -> Step {
        match token {
            Token::Doctype { .. } => return Step::Done,
            Token::StartTag { name, .. } if &**name == "html" => return self.in_body(sink, token),
            Token::EndTag { name } if &**name == "noscript" => {
                self.pop();
                self.mode = Mode::InHead;
                return Step::Done;
            }
            Token::Characters(text) if is_all_whitespace(text) => return self.in_head(sink, token),
            Token::Comment(_) => return self.in_head(sink, token),
            Token::StartTag { name, .. }
                if matches!(
                    &**name,
                    "basefont" | "bgsound" | "link" | "meta" | "noframes" | "style"
                ) =>
            {
                return self.in_head(sink, token);
            }
            Token::StartTag { name, .. } if matches!(&**name, "head" | "noscript") => {
                return Step::Done;
            }
            Token::EndTag { name } if &**name != "br" => return Step::Done,
            _ => {}
        }
        self.pop();
        self.mode = Mode::InHead;
        Step::Reprocess
    }

    fn after_head<S: TreeSink<Handle = H>>(&mut self, sink: &mut S, token: &Token) -> Step {
        match token {
            Token::Characters(text) if is_all_whitespace(text) => {
                self.insert_text(sink, text);
                return Step::Done;
            }
            Token::Comment(text) => {
                self.insert_comment(sink, text);
                return Step::Done;
            }
            Token::Doctype { .. } => return Step::Done,
            Token::StartTag { name, attrs, .. } => match &**name {
                "html" => return self.in_body(sink, token),
                "body" => {
                    self.insert_html(sink, name, attrs.clone());
                    self.frameset_ok = false;
                    self.mode = Mode::InBody;
                    return Step::Done;
                }
                "frameset" => {
                    self.insert_html(sink, name, attrs.clone());
                    self.mode = Mode::InFrameset;
                    return Step::Done;
                }
                "base" | "basefont" | "bgsound" | "link" | "meta" | "noframes" | "script"
                | "style" | "template" | "title" => {
                    // Head elements after `</head>` still go into the head.
                    let Some(head) = self.head else {
                        return Step::Done;
                    };
                    self.open.push(Open {
                        handle: head,
                        ns: Ns::Html,
                        name: LocalName::from("head"),
                        html_annotation: false,
                    });
                    let step = self.in_head(sink, token);
                    if let Some(index) = self.open.iter().rposition(|e| e.handle == head) {
                        self.open.remove(index);
                    }
                    return step;
                }
                "head" => return Step::Done,
                _ => {}
            },
            Token::EndTag { name } => match &**name {
                "template" => return self.in_head(sink, token),
                "body" | "html" | "br" => {}
                _ => return Step::Done,
            },
            _ => {}
        }
        self.insert_html(sink, &LocalName::from("body"), Vec::new());
        self.mode = Mode::InBody;
        Step::Reprocess
    }

    fn in_body<S: TreeSink<Handle = H>>(&mut self, sink: &mut S, token: &Token) -> Step {
        match token {
            Token::Characters(text) => {
                // The tokenizer keeps NUL in data; in body drops it.
                let text = if text.contains('\0') {
                    text.replace('\0', "")
                } else {
                    text.clone()
                };
                if text.is_empty() {
                    return Step::Done;
                }
                self.reconstruct_formatting(sink);
                self.insert_text(sink, &text);
                if !is_all_whitespace(&text) {
                    self.frameset_ok = false;
                }
                Step::Done
            }
            Token::Comment(text) => {
                self.insert_comment(sink, text);
                Step::Done
            }
            Token::Doctype { .. } => Step::Done,
            Token::StartTag {
                name,
                attrs,
                self_closing,
            } => self.in_body_start_tag(sink, name, attrs, *self_closing),
            Token::EndTag { name } => self.in_body_end_tag(sink, name),
            Token::Eof => {
                if !self.template_modes.is_empty() {
                    return self.in_template(sink, token);
                }
                Step::Done
            }
        }
    }

    fn in_body_start_tag<S: TreeSink<Handle = H>>(
        &mut self,
        sink: &mut S,
        name: &LocalName,
        attrs: &[Attribute],
        self_closing: bool,
    ) -> Step {
        let in_template = self.open.iter().any(|e| e.is_html("template"));
        match &**name {
            "html" => {
                if !in_template && let Some(root) = self.open.first() {
                    sink.add_attrs_if_missing(root.handle, attrs.to_vec());
                }
            }
            "base" | "basefont" | "bgsound" | "link" | "meta" | "noframes" | "script" | "style"
            | "template" | "title" => {
                return self.in_head(sink, &start_tag(name, attrs, self_closing));
            }
            "body" => {
                if let Some(body) = self.open.get(1)
                    && body.is_html("body")
                    && !in_template
                {
                    self.frameset_ok = false;
                    sink.add_attrs_if_missing(body.handle, attrs.to_vec());
                }
            }
            "frameset" => {
                let Some(body) = self.open.get(1).filter(|b| b.is_html("body")) else {
                    return Step::Done;
                };
                if !self.frameset_ok {
                    return Step::Done;
                }
                sink.remove_from_parent(body.handle);
                self.open.truncate(1);
                self.insert_html(sink, name, attrs.to_vec());
                self.mode = Mode::InFrameset;
            }
            _ if CLOSES_P.contains(&&**name) => {
                self.close_p_in_button_scope();
                self.insert_html(sink, name, attrs.to_vec());
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.close_p_in_button_scope();
                if self.current().is_some_and(|e| e.is_html_in(HEADINGS)) {
                    self.pop();
                }
                self.insert_html(sink, name, attrs.to_vec());
            }
            "pre" | "listing" => {
                self.close_p_in_button_scope();
                self.insert_html(sink, name, attrs.to_vec());
                self.skip_newline = true;
                self.frameset_ok = false;
            }
            "form" => {
                if self.form.is_some() && !in_template {
                    return Step::Done;
                }
                self.close_p_in_button_scope();
                let form = self.insert_html(sink, name, attrs.to_vec());
                if !in_template {
                    self.form = Some(form);
                }
            }
            "li" => {
                self.frameset_ok = false;
                self.close_list_item(&["li"]);
                self.close_p_in_button_scope();
                self.insert_html(sink, name, attrs.to_vec());
            }
            "dd" | "dt" => {
                self.frameset_ok = false;
                self.close_list_item(&["dd", "dt"]);
                self.close_p_in_button_scope();
                self.insert_html(sink, name, attrs.to_vec());
            }
            "plaintext" => {
                self.close_p_in_button_scope();
                self.insert_html(sink, name, attrs.to_vec());
                self.switch_to = Some(TextState::PlainText);
            }
            "button" => {
                if self.has_in_scope(&["button"], Scope::Default) {
                    self.generate_implied_end_tags(None);
                    self.pop_until_html(&["button"]);
                }
                self.reconstruct_formatting(sink);
                self.insert_html(sink, name, attrs.to_vec());
                self.frameset_ok = false;
            }
            "a" => {
                if let Some(a) = self.formatting_after_marker("a") {
                    if !self.adoption_agency(sink, name) {
                        self.any_other_end_tag(name);
                    }
                    self.remove_formatting(a);
                    if let Some(index) = self.open.iter().rposition(|e| e.handle == a) {
                        self.open.remove(index);
                    }
                }
                self.reconstruct_formatting(sink);
                self.insert_formatting(sink, name, attrs);
            }
            _ if FORMATTING.contains(&&**name) => {
                self.reconstruct_formatting(sink);
                self.insert_formatting(sink, name, attrs);
            }
            "nobr" => {
                self.reconstruct_formatting(sink);
                if self.has_in_scope(&["nobr"], Scope::Default) {
                    if !self.adoption_agency(sink, name) {
                        self.any_other_end_tag(name);
                    }
                    self.reconstruct_formatting(sink);
                }
                self.insert_formatting(sink, name, attrs);
            }
            "applet" | "marquee" | "object" => {
                self.reconstruct_formatting(sink);
                let element = self.insert_html(sink, name, attrs.to_vec());
                if self.is_current(element) {
                    self.formatting.push(Formatting::Marker);
                }
                self.frameset_ok = false;
            }
            "table" => {
                if self.quirks != QuirksMode::Quirks {
                    self.close_p_in_button_scope();
                }
                self.insert_html(sink, name, attrs.to_vec());
                self.frameset_ok = false;
                self.mode = Mode::InTable;
            }
            "area" | "br" | "embed" | "img" | "keygen" | "wbr" => {
                self.reconstruct_formatting(sink);
                self.insert_html_void(sink, name, attrs.to_vec());
                self.frameset_ok = false;
            }
            "input" => {
                if self.context_is_select() {
                    return Step::Done;
                }
                if self.has_in_scope(&["select"], Scope::Default) {
                    self.pop_until_html(&["select"]);
                }
                self.reconstruct_formatting(sink);
                let hidden = attr(attrs, "type").is_some_and(|t| t.eq_ignore_ascii_case("hidden"));
                self.insert_html_void(sink, name, attrs.to_vec());
                if !hidden {
                    self.frameset_ok = false;
                }
            }
            "param" | "source" | "track" => {
                self.insert_html_void(sink, name, attrs.to_vec());
            }
            "hr" => {
                self.close_p_in_button_scope();
                if self.has_in_scope(&["select"], Scope::Default) {
                    self.generate_implied_end_tags(None);
                }
                self.insert_html_void(sink, name, attrs.to_vec());
                self.frameset_ok = false;
            }
            "image" => {
                return self.in_body_start_tag(sink, &LocalName::from("img"), attrs, self_closing);
            }
            "textarea" => {
                self.skip_newline = true;
                self.frameset_ok = false;
                return self.insert_text_element(sink, name, attrs, TextState::RcData);
            }
            "xmp" => {
                self.close_p_in_button_scope();
                self.reconstruct_formatting(sink);
                self.frameset_ok = false;
                return self.insert_text_element(sink, name, attrs, TextState::RawText);
            }
            "iframe" => {
                self.frameset_ok = false;
                return self.insert_text_element(sink, name, attrs, TextState::RawText);
            }
            "noembed" => return self.insert_text_element(sink, name, attrs, TextState::RawText),
            "noscript" if self.scripting => {
                return self.insert_text_element(sink, name, attrs, TextState::RawText);
            }
            "select" => {
                if self.context_is_select() {
                    return Step::Done;
                }
                if self.has_in_scope(&["select"], Scope::Default) {
                    self.pop_until_html(&["select"]);
                    return Step::Done;
                }
                self.reconstruct_formatting(sink);
                self.insert_html(sink, name, attrs.to_vec());
                self.frameset_ok = false;
            }
            "option" => {
                if self.has_in_scope(&["select"], Scope::Default) {
                    self.generate_implied_end_tags(Some("optgroup"));
                } else if self.current().is_some_and(|e| e.is_html("option")) {
                    self.pop();
                }
                self.reconstruct_formatting(sink);
                self.insert_html(sink, name, attrs.to_vec());
            }
            "optgroup" => {
                if self.has_in_scope(&["select"], Scope::Default) {
                    self.generate_implied_end_tags(None);
                } else if self.current().is_some_and(|e| e.is_html("option")) {
                    self.pop();
                }
                self.reconstruct_formatting(sink);
                self.insert_html(sink, name, attrs.to_vec());
            }
            "rb" | "rtc" => {
                if self.has_in_scope(&["ruby"], Scope::Default) {
                    self.generate_implied_end_tags(None);
                }
                self.insert_html(sink, name, attrs.to_vec());
            }
            "rp" | "rt" => {
                if self.has_in_scope(&["ruby"], Scope::Default) {
                    self.generate_implied_end_tags(Some("rtc"));
                }
                self.insert_html(sink, name, attrs.to_vec());
            }
            "math" | "svg" => {
                self.reconstruct_formatting(sink);
                let ns = if &**name == "math" {
                    Ns::MathMl
                } else {
                    Ns::Svg
                };
                self.insert_foreign(sink, ns, name, attrs, !self_closing);
            }
            "caption" | "col" | "colgroup" | "frame" | "head" | "tbody" | "td" | "tfoot" | "th"
            | "thead" | "tr" => {}
            _ => {
                self.reconstruct_formatting(sink);
                self.insert_html(sink, name, attrs.to_vec());
            }
        }
        Step::Done
    }

    fn in_body_end_tag<S: TreeSink<Handle = H>>(&mut self, sink: &mut S, name: &LocalName) -> Step {
        match &**name {
            "template" => return self.in_head(sink, &Token::EndTag { name: name.clone() }),
            "body" => {
                if self.has_in_scope(&["body"], Scope::Default) {
                    self.mode = Mode::AfterBody;
                }
            }
            "html" => {
                if self.has_in_scope(&["body"], Scope::Default) {
                    self.mode = Mode::AfterBody;
                    return Step::Reprocess;
                }
            }
            _ if BLOCK_END.contains(&&**name) => {
                if self.has_in_scope(&[name], Scope::Default) {
                    self.generate_implied_end_tags(None);
                    self.pop_until_html(&[name]);
                }
            }
            "form" => {
                if self.open.iter().any(|e| e.is_html("template")) {
                    if self.has_in_scope(&["form"], Scope::Default) {
                        self.generate_implied_end_tags(None);
                        self.pop_until_html(&["form"]);
                    }
                } else {
                    let Some(form) = self.form.take() else {
                        return Step::Done;
                    };
                    if !self.element_in_scope(form) {
                        return Step::Done;
                    }
                    self.generate_implied_end_tags(None);
                    if let Some(index) = self.open.iter().rposition(|e| e.handle == form) {
                        self.open.remove(index);
                    }
                }
            }
            "p" => {
                if !self.has_in_scope(&["p"], Scope::Button) {
                    self.insert_html(sink, name, Vec::new());
                }
                self.close_p();
            }
            "li" => {
                if self.has_in_scope(&["li"], Scope::ListItem) {
                    self.generate_implied_end_tags(Some("li"));
                    self.pop_until_html(&["li"]);
                }
            }
            "dd" | "dt" => {
                if self.has_in_scope(&[name], Scope::Default) {
                    self.generate_implied_end_tags(Some(name));
                    self.pop_until_html(&[name]);
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                if self.has_in_scope(HEADINGS, Scope::Default) {
                    self.generate_implied_end_tags(None);
                    self.pop_until_html(HEADINGS);
                }
            }
            _ if FORMATTING_END.contains(&&**name) => {
                if !self.adoption_agency(sink, name) {
                    self.any_other_end_tag(name);
                }
            }
            "applet" | "marquee" | "object" => {
                if self.has_in_scope(&[name], Scope::Default) {
                    self.generate_implied_end_tags(None);
                    self.pop_until_html(&[name]);
                    self.clear_formatting_to_marker();
                }
            }
            "br" => return self.in_body_start_tag(sink, name, &[], false),
            _ => self.any_other_end_tag(name),
        }
        Step::Done
    }

    /// Closes up to the nearest open HTML element called `name`, unless a
    /// special element comes first.
    fn any_other_end_tag(&mut self, name: &LocalName) {
        for index in (0..self.open.len()).rev() {
            let entry = &self.open[index];
            if entry.ns == Ns::Html && entry.name == *name {
                self.generate_implied_end_tags(Some(name));
                self.open.truncate(index);
                return;
            }
            if entry.is_special() {
                return;
            }
        }
    }

    /// For a new `li` (or `dd`/`dt`): closes an open one of `names` unless a
    /// special element other than `address`, `div` and `p` comes first.
    fn close_list_item(&mut self, names: &[&str]) {
        for index in (0..self.open.len()).rev() {
            let entry = &self.open[index];
            if entry.is_html_in(names) {
                let name = entry.name.clone();
                self.generate_implied_end_tags(Some(&name));
                self.pop_until_html(&[&name]);
                return;
            }
            if entry.is_special() && !entry.is_html_in(&["address", "div", "p"]) {
                return;
            }
        }
    }

    fn text<S: TreeSink<Handle = H>>(&mut self, sink: &mut S, token: &Token) -> Step {
        if let Token::Characters(text) = token {
            if let Some(element) = self.text_element
                && !text.is_empty()
            {
                sink.append(element, NodeOrText::Text(text));
            }
            return Step::Done;
        }
        // The tokenizer only ends a text element with its own end tag, or
        // the input ends inside it.
        if let Some(element) = self.text_element.take()
            && self.is_current(element)
        {
            // A script closed by its end tag is ready to run; one the input
            // ended inside never runs.
            let script = self.current().is_some_and(|open| open.is_html("script"));
            if script && matches!(token, Token::EndTag { .. }) {
                self.pending_script = Some(element);
            }
            self.pop();
        }
        self.mode = self.original_mode;
        if matches!(token, Token::Eof) {
            return Step::Reprocess;
        }
        Step::Done
    }

    fn in_table<S: TreeSink<Handle = H>>(&mut self, sink: &mut S, token: &Token) -> Step {
        match token {
            Token::Characters(_)
                if self.current().is_some_and(|e| {
                    e.is_html_in(&["table", "tbody", "template", "tfoot", "thead", "tr"])
                }) =>
            {
                self.pending_table_text.clear();
                self.pending_table_text_is_space = true;
                self.original_mode = self.mode;
                self.mode = Mode::InTableText;
                return Step::Reprocess;
            }
            Token::Comment(text) => {
                self.insert_comment(sink, text);
                return Step::Done;
            }
            Token::Doctype { .. } => return Step::Done,
            Token::StartTag { name, attrs, .. } => match &**name {
                "caption" => {
                    self.clear_stack_back_to(&["table", "template", "html"]);
                    self.formatting.push(Formatting::Marker);
                    self.insert_html(sink, name, attrs.clone());
                    self.mode = Mode::InCaption;
                    return Step::Done;
                }
                "colgroup" => {
                    self.clear_stack_back_to(&["table", "template", "html"]);
                    self.insert_html(sink, name, attrs.clone());
                    self.mode = Mode::InColumnGroup;
                    return Step::Done;
                }
                "col" => {
                    self.clear_stack_back_to(&["table", "template", "html"]);
                    self.insert_html(sink, &LocalName::from("colgroup"), Vec::new());
                    self.mode = Mode::InColumnGroup;
                    return Step::Reprocess;
                }
                "tbody" | "tfoot" | "thead" => {
                    self.clear_stack_back_to(&["table", "template", "html"]);
                    self.insert_html(sink, name, attrs.clone());
                    self.mode = Mode::InTableBody;
                    return Step::Done;
                }
                "td" | "th" | "tr" => {
                    self.clear_stack_back_to(&["table", "template", "html"]);
                    self.insert_html(sink, &LocalName::from("tbody"), Vec::new());
                    self.mode = Mode::InTableBody;
                    return Step::Reprocess;
                }
                "table" => {
                    if !self.has_in_scope(&["table"], Scope::Table) {
                        return Step::Done;
                    }
                    self.pop_until_html(&["table"]);
                    self.reset_insertion_mode();
                    return Step::Reprocess;
                }
                "style" | "script" | "template" => return self.in_head(sink, token),
                "input"
                    if attr(attrs, "type").is_some_and(|t| t.eq_ignore_ascii_case("hidden")) =>
                {
                    self.insert_html_void(sink, name, attrs.clone());
                    return Step::Done;
                }
                "form" => {
                    if self.form.is_some() || self.open.iter().any(|e| e.is_html("template")) {
                        return Step::Done;
                    }
                    let form = self.insert_html_void(sink, name, attrs.clone());
                    self.form = Some(form);
                    return Step::Done;
                }
                _ => {}
            },
            Token::EndTag { name } => match &**name {
                "table" => {
                    if self.has_in_scope(&["table"], Scope::Table) {
                        self.pop_until_html(&["table"]);
                        self.reset_insertion_mode();
                    }
                    return Step::Done;
                }
                "body" | "caption" | "col" | "colgroup" | "html" | "tbody" | "td" | "tfoot"
                | "th" | "thead" | "tr" => return Step::Done,
                "template" => return self.in_head(sink, token),
                _ => {}
            },
            Token::Eof => return self.in_body(sink, token),
            Token::Characters(_) => {}
        }
        self.foster_parent_in_body(sink, token)
    }

    /// In table's "anything else": in body rules, with stray content moved
    /// out in front of the table.
    fn foster_parent_in_body<S: TreeSink<Handle = H>>(
        &mut self,
        sink: &mut S,
        token: &Token,
    ) -> Step {
        self.foster_parenting = true;
        let step = self.in_body(sink, token);
        self.foster_parenting = false;
        step
    }

    fn in_table_text<S: TreeSink<Handle = H>>(&mut self, sink: &mut S, token: &Token) -> Step {
        if let Token::Characters(text) = token {
            for c in text.chars().filter(|&c| c != '\0') {
                self.pending_table_text.push(c);
                if !is_whitespace(c) {
                    self.pending_table_text_is_space = false;
                }
            }
            return Step::Done;
        }
        let pending = std::mem::take(&mut self.pending_table_text);
        if !pending.is_empty() {
            if self.pending_table_text_is_space {
                self.insert_text(sink, &pending);
            } else {
                self.foster_parent_in_body(sink, &Token::Characters(pending));
            }
        }
        self.mode = self.original_mode;
        Step::Reprocess
    }

    fn in_caption<S: TreeSink<Handle = H>>(&mut self, sink: &mut S, token: &Token) -> Step {
        let closes_caption = match token {
            Token::EndTag { name } if &**name == "caption" => {
                if self.close_caption() {
                    self.mode = Mode::InTable;
                }
                return Step::Done;
            }
            Token::StartTag { name, .. } => matches!(
                &**name,
                "caption" | "col" | "colgroup" | "tbody" | "td" | "tfoot" | "th" | "thead" | "tr"
            ),
            Token::EndTag { name } if &**name == "table" => true,
            Token::EndTag { name }
                if matches!(
                    &**name,
                    "body"
                        | "col"
                        | "colgroup"
                        | "html"
                        | "tbody"
                        | "td"
                        | "tfoot"
                        | "th"
                        | "thead"
                        | "tr"
                ) =>
            {
                return Step::Done;
            }
            _ => false,
        };
        if closes_caption {
            if self.close_caption() {
                self.mode = Mode::InTable;
                return Step::Reprocess;
            }
            return Step::Done;
        }
        self.in_body(sink, token)
    }

    /// Closes an open caption; false when there is none in table scope.
    fn close_caption(&mut self) -> bool {
        if !self.has_in_scope(&["caption"], Scope::Table) {
            return false;
        }
        self.generate_implied_end_tags(None);
        self.pop_until_html(&["caption"]);
        self.clear_formatting_to_marker();
        true
    }

    fn in_column_group<S: TreeSink<Handle = H>>(&mut self, sink: &mut S, token: &Token) -> Step {
        match token {
            Token::Characters(text) if is_all_whitespace(text) => {
                self.insert_text(sink, text);
                return Step::Done;
            }
            Token::Comment(text) => {
                self.insert_comment(sink, text);
                return Step::Done;
            }
            Token::Doctype { .. } => return Step::Done,
            Token::StartTag { name, attrs, .. } => match &**name {
                "html" => return self.in_body(sink, token),
                "col" => {
                    self.insert_html_void(sink, name, attrs.clone());
                    return Step::Done;
                }
                "template" => return self.in_head(sink, token),
                _ => {}
            },
            Token::EndTag { name } => match &**name {
                "colgroup" => {
                    if self.current().is_some_and(|e| e.is_html("colgroup")) {
                        self.pop();
                        self.mode = Mode::InTable;
                    }
                    return Step::Done;
                }
                "col" => return Step::Done,
                "template" => return self.in_head(sink, token),
                _ => {}
            },
            Token::Eof => return self.in_body(sink, token),
            Token::Characters(_) => {}
        }
        if !self.current().is_some_and(|e| e.is_html("colgroup")) {
            return Step::Done;
        }
        self.pop();
        self.mode = Mode::InTable;
        Step::Reprocess
    }

    fn in_table_body<S: TreeSink<Handle = H>>(&mut self, sink: &mut S, token: &Token) -> Step {
        const CONTEXT: &[&str] = &["tbody", "tfoot", "thead", "template", "html"];
        match token {
            Token::StartTag { name, attrs, .. } => match &**name {
                "tr" => {
                    self.clear_stack_back_to(CONTEXT);
                    self.insert_html(sink, name, attrs.clone());
                    self.mode = Mode::InRow;
                    return Step::Done;
                }
                "th" | "td" => {
                    self.clear_stack_back_to(CONTEXT);
                    self.insert_html(sink, &LocalName::from("tr"), Vec::new());
                    self.mode = Mode::InRow;
                    return Step::Reprocess;
                }
                "caption" | "col" | "colgroup" | "tbody" | "tfoot" | "thead" => {
                    return self.close_table_body();
                }
                _ => {}
            },
            Token::EndTag { name } => match &**name {
                "tbody" | "tfoot" | "thead" => {
                    if self.has_in_scope(&[name], Scope::Table) {
                        self.clear_stack_back_to(CONTEXT);
                        self.pop();
                        self.mode = Mode::InTable;
                    }
                    return Step::Done;
                }
                "table" => return self.close_table_body(),
                "body" | "caption" | "col" | "colgroup" | "html" | "td" | "th" | "tr" => {
                    return Step::Done;
                }
                _ => {}
            },
            _ => {}
        }
        self.in_table(sink, token)
    }

    /// Closes the open table section so the token can go to the table.
    fn close_table_body(&mut self) -> Step {
        if !self.has_in_scope(&["tbody", "thead", "tfoot"], Scope::Table) {
            return Step::Done;
        }
        self.clear_stack_back_to(&["tbody", "tfoot", "thead", "template", "html"]);
        self.pop();
        self.mode = Mode::InTable;
        Step::Reprocess
    }

    fn in_row<S: TreeSink<Handle = H>>(&mut self, sink: &mut S, token: &Token) -> Step {
        const CONTEXT: &[&str] = &["tr", "template", "html"];
        match token {
            Token::StartTag { name, attrs, .. } => match &**name {
                "th" | "td" => {
                    self.clear_stack_back_to(CONTEXT);
                    let cell = self.insert_html(sink, name, attrs.clone());
                    self.mode = Mode::InCell;
                    if self.is_current(cell) {
                        self.formatting.push(Formatting::Marker);
                    }
                    return Step::Done;
                }
                "caption" | "col" | "colgroup" | "tbody" | "tfoot" | "thead" | "tr" => {
                    return self.close_row();
                }
                _ => {}
            },
            Token::EndTag { name } => match &**name {
                "tr" => {
                    if self.has_in_scope(&["tr"], Scope::Table) {
                        self.clear_stack_back_to(CONTEXT);
                        self.pop();
                        self.mode = Mode::InTableBody;
                    }
                    return Step::Done;
                }
                "table" => return self.close_row(),
                "tbody" | "tfoot" | "thead" => {
                    if !self.has_in_scope(&[name], Scope::Table) {
                        return Step::Done;
                    }
                    return self.close_row();
                }
                "body" | "caption" | "col" | "colgroup" | "html" | "td" | "th" => {
                    return Step::Done;
                }
                _ => {}
            },
            _ => {}
        }
        self.in_table(sink, token)
    }

    /// Closes the open row so the token can go to the table section.
    fn close_row(&mut self) -> Step {
        if !self.has_in_scope(&["tr"], Scope::Table) {
            return Step::Done;
        }
        self.clear_stack_back_to(&["tr", "template", "html"]);
        self.pop();
        self.mode = Mode::InTableBody;
        Step::Reprocess
    }

    fn in_cell<S: TreeSink<Handle = H>>(&mut self, sink: &mut S, token: &Token) -> Step {
        match token {
            Token::EndTag { name } if matches!(&**name, "td" | "th") => {
                if self.has_in_scope(&[name], Scope::Table) {
                    self.generate_implied_end_tags(None);
                    self.pop_until_html(&[name]);
                    self.clear_formatting_to_marker();
                    self.mode = Mode::InRow;
                }
                return Step::Done;
            }
            Token::StartTag { name, .. }
                if matches!(
                    &**name,
                    "caption"
                        | "col"
                        | "colgroup"
                        | "tbody"
                        | "td"
                        | "tfoot"
                        | "th"
                        | "thead"
                        | "tr"
                ) =>
            {
                if !self.has_in_scope(&["td", "th"], Scope::Table) {
                    return Step::Done;
                }
                self.close_cell();
                return Step::Reprocess;
            }
            Token::EndTag { name }
                if matches!(&**name, "body" | "caption" | "col" | "colgroup" | "html") =>
            {
                return Step::Done;
            }
            Token::EndTag { name }
                if matches!(&**name, "table" | "tbody" | "tfoot" | "thead" | "tr") =>
            {
                if !self.has_in_scope(&[name], Scope::Table) {
                    return Step::Done;
                }
                self.close_cell();
                return Step::Reprocess;
            }
            _ => {}
        }
        self.in_body(sink, token)
    }

    fn close_cell(&mut self) {
        self.generate_implied_end_tags(None);
        self.pop_until_html(&["td", "th"]);
        self.clear_formatting_to_marker();
        self.mode = Mode::InRow;
    }

    fn in_template<S: TreeSink<Handle = H>>(&mut self, sink: &mut S, token: &Token) -> Step {
        let next = match token {
            Token::Characters(_) | Token::Comment(_) | Token::Doctype { .. } => {
                return self.in_body(sink, token);
            }
            Token::StartTag { name, .. } => match &**name {
                "base" | "basefont" | "bgsound" | "link" | "meta" | "noframes" | "script"
                | "style" | "template" | "title" => return self.in_head(sink, token),
                "caption" | "colgroup" | "tbody" | "tfoot" | "thead" => Mode::InTable,
                "col" => Mode::InColumnGroup,
                "tr" => Mode::InTableBody,
                "td" | "th" => Mode::InRow,
                _ => Mode::InBody,
            },
            Token::EndTag { name } if &**name == "template" => return self.in_head(sink, token),
            Token::EndTag { .. } => return Step::Done,
            Token::Eof => {
                if !self.open.iter().any(|e| e.is_html("template")) {
                    return Step::Done;
                }
                self.pop_until_html(&["template"]);
                self.clear_formatting_to_marker();
                self.template_modes.pop();
                self.reset_insertion_mode();
                return Step::Reprocess;
            }
        };
        self.template_modes.pop();
        self.template_modes.push(next);
        self.mode = next;
        Step::Reprocess
    }

    fn after_body<S: TreeSink<Handle = H>>(&mut self, sink: &mut S, token: &Token) -> Step {
        match token {
            Token::Characters(text) if is_all_whitespace(text) => self.in_body(sink, token),
            Token::Comment(text) => {
                if let Some(root) = self.open.first().map(|e| e.handle) {
                    self.append_comment_to(sink, root, text);
                }
                Step::Done
            }
            Token::Doctype { .. } | Token::Eof => Step::Done,
            Token::StartTag { name, .. } if &**name == "html" => self.in_body(sink, token),
            Token::EndTag { name } if &**name == "html" => {
                if self.context.is_none() {
                    self.mode = Mode::AfterAfterBody;
                }
                Step::Done
            }
            _ => {
                self.mode = Mode::InBody;
                Step::Reprocess
            }
        }
    }

    fn in_frameset<S: TreeSink<Handle = H>>(&mut self, sink: &mut S, token: &Token) -> Step {
        match token {
            Token::Characters(text) if is_all_whitespace(text) => self.insert_text(sink, text),
            Token::Comment(text) => self.insert_comment(sink, text),
            Token::StartTag { name, attrs, .. } => match &**name {
                "html" => return self.in_body(sink, token),
                "frameset" => {
                    self.insert_html(sink, name, attrs.clone());
                }
                "frame" => {
                    self.insert_html_void(sink, name, attrs.clone());
                }
                "noframes" => return self.in_head(sink, token),
                _ => {}
            },
            // The root `html` stays open: only a fragment gets here with it.
            Token::EndTag { name } if &**name == "frameset" && self.open.len() > 1 => {
                self.pop();
                if self.context.is_none() && !self.current().is_some_and(|e| e.is_html("frameset"))
                {
                    self.mode = Mode::AfterFrameset;
                }
            }
            _ => {}
        }
        Step::Done
    }

    fn after_frameset<S: TreeSink<Handle = H>>(&mut self, sink: &mut S, token: &Token) -> Step {
        match token {
            Token::Characters(text) if is_all_whitespace(text) => self.insert_text(sink, text),
            Token::Comment(text) => self.insert_comment(sink, text),
            Token::StartTag { name, .. } if &**name == "html" => return self.in_body(sink, token),
            Token::StartTag { name, .. } if &**name == "noframes" => {
                return self.in_head(sink, token);
            }
            Token::EndTag { name } if &**name == "html" => self.mode = Mode::AfterAfterFrameset,
            _ => {}
        }
        Step::Done
    }

    fn after_after_body<S: TreeSink<Handle = H>>(&mut self, sink: &mut S, token: &Token) -> Step {
        match token {
            Token::Comment(text) => {
                self.append_comment_to(sink, sink.document(), text);
                Step::Done
            }
            Token::Doctype { .. } => self.in_body(sink, token),
            Token::Characters(text) if is_all_whitespace(text) => self.in_body(sink, token),
            Token::StartTag { name, .. } if &**name == "html" => self.in_body(sink, token),
            Token::Eof => Step::Done,
            _ => {
                self.mode = Mode::InBody;
                Step::Reprocess
            }
        }
    }

    fn after_after_frameset<S: TreeSink<Handle = H>>(
        &mut self,
        sink: &mut S,
        token: &Token,
    ) -> Step {
        match token {
            Token::Comment(text) => {
                self.append_comment_to(sink, sink.document(), text);
                Step::Done
            }
            Token::Doctype { .. } => self.in_body(sink, token),
            Token::Characters(text) if is_all_whitespace(text) => self.in_body(sink, token),
            Token::StartTag { name, .. } if &**name == "html" => self.in_body(sink, token),
            Token::StartTag { name, .. } if &**name == "noframes" => self.in_head(sink, token),
            _ => Step::Done,
        }
    }

    /// The rules for tokens inside SVG or MathML.
    fn foreign_content<S: TreeSink<Handle = H>>(&mut self, sink: &mut S, token: &Token) -> Step {
        match token {
            Token::Characters(text) => {
                // NUL becomes U+FFFD, but only other text turns off
                // frameset-ok.
                if text.chars().any(|c| c != '\0' && !is_whitespace(c)) {
                    self.frameset_ok = false;
                }
                self.insert_text(sink, &text.replace('\0', "\u{FFFD}"));
                Step::Done
            }
            Token::Comment(text) => {
                self.insert_comment(sink, text);
                Step::Done
            }
            Token::Doctype { .. } | Token::Eof => Step::Done,
            Token::StartTag { name, attrs, .. }
                if FOREIGN_BREAKOUT.contains(&&**name)
                    || (&**name == "font"
                        && ["color", "face", "size"]
                            .iter()
                            .any(|a| attr(attrs, a).is_some())) =>
            {
                self.leave_foreign_content(sink, token)
            }
            Token::EndTag { name } if matches!(&**name, "br" | "p") => {
                self.leave_foreign_content(sink, token)
            }
            Token::StartTag {
                name,
                attrs,
                self_closing,
            } => {
                let ns = self.adjusted_current().map_or(Ns::Html, |node| node.ns);
                self.insert_foreign(sink, ns, name, attrs, !self_closing);
                Step::Done
            }
            Token::EndTag { name } => {
                let mut index = self.open.len().saturating_sub(1);
                loop {
                    if index == 0 {
                        return Step::Done;
                    }
                    if self.open[index].name.eq_ignore_ascii_case(name) {
                        self.open.truncate(index);
                        return Step::Done;
                    }
                    index -= 1;
                    if self.open[index].ns == Ns::Html {
                        return self.run_mode(self.mode, sink, token);
                    }
                }
            }
        }
    }

    /// An HTML tag inside foreign content: closes the foreign elements and
    /// runs the token by the HTML rules.
    fn leave_foreign_content<S: TreeSink<Handle = H>>(
        &mut self,
        sink: &mut S,
        token: &Token,
    ) -> Step {
        while let Some(node) = self.current()
            && !(node.ns == Ns::Html
                || node.is_mathml_text_integration()
                || node.is_html_integration())
        {
            self.open.pop();
        }
        self.run_mode(self.mode, sink, token)
    }

    // ----- Inserting nodes -----

    /// The standard's "appropriate place for inserting a node", into the
    /// current node or `target`, with foster parenting when it is on.
    fn place<S: TreeSink<Handle = H>>(&self, sink: &S, target: Option<&Open<H>>) -> Place<H> {
        let Some(target) = target.or_else(|| self.open.last()) else {
            return Place::Append(sink.document());
        };
        if self.foster_parenting && target.is_table_part() {
            let last_template = self.open.iter().rposition(|e| e.is_html("template"));
            let last_table = self.open.iter().rposition(|e| e.is_html("table"));
            match (last_template, last_table) {
                (Some(template), table) if table.is_none_or(|table| template > table) => {
                    return Place::Append(sink.template_contents(self.open[template].handle));
                }
                (_, None) => {
                    return Place::Append(self.open[0].handle);
                }
                (_, Some(table)) => {
                    let table_handle = self.open[table].handle;
                    if sink.parent(table_handle).is_some() {
                        return Place::Before(table_handle);
                    }
                    let previous = &self.open[table.saturating_sub(1)];
                    return self.place_inside(sink, previous);
                }
            }
        }
        self.place_inside(sink, target)
    }

    /// Appending to `target`, or to its contents when it is a template.
    fn place_inside<S: TreeSink<Handle = H>>(&self, sink: &S, target: &Open<H>) -> Place<H> {
        if target.is_html("template") {
            Place::Append(sink.template_contents(target.handle))
        } else {
            Place::Append(target.handle)
        }
    }

    fn insert_at<S: TreeSink<Handle = H>>(
        &self,
        sink: &mut S,
        place: Place<H>,
        child: NodeOrText<'_, H>,
    ) {
        match place {
            Place::Append(parent) => sink.append(parent, child),
            Place::Before(sibling) => sink.insert_before(sibling, child),
        }
    }

    fn insert_text<S: TreeSink<Handle = H>>(&mut self, sink: &mut S, text: &str) {
        if text.is_empty() {
            return;
        }
        let place = self.place(sink, None);
        let parent = match place {
            Place::Append(parent) => Some(parent),
            Place::Before(sibling) => sink.parent(sibling),
        };
        // Text never goes straight into the document.
        if parent == Some(sink.document()) {
            return;
        }
        self.insert_at(sink, place, NodeOrText::Text(text));
    }

    fn insert_comment<S: TreeSink<Handle = H>>(&mut self, sink: &mut S, text: &str) {
        let place = self.place(sink, None);
        let comment = sink.create_comment(text);
        self.insert_at(sink, place, NodeOrText::Node(comment));
    }

    fn append_comment_to<S: TreeSink<Handle = H>>(&mut self, sink: &mut S, parent: H, text: &str) {
        let comment = sink.create_comment(text);
        sink.append(parent, NodeOrText::Node(comment));
    }

    /// The `html` element, appended to the document and opened.
    fn insert_root<S: TreeSink<Handle = H>>(&mut self, sink: &mut S, attrs: Vec<Attribute>) {
        let html = sink.create_element(
            QualName::html(LocalName::from("html")),
            attrs,
            CreateFlags::default(),
        );
        sink.append(sink.document(), NodeOrText::Node(html));
        self.open.push(Open {
            handle: html,
            ns: Ns::Html,
            name: LocalName::from("html"),
            html_annotation: false,
        });
    }

    /// Creates an element and inserts it at the appropriate place, and
    /// opens it when `open` says so and the depth cap allows (AC-17).
    fn insert_element<S: TreeSink<Handle = H>>(
        &mut self,
        sink: &mut S,
        ns: Ns,
        name: &LocalName,
        attrs: Vec<Attribute>,
        open: bool,
    ) -> H {
        let html_annotation = ns == Ns::MathMl
            && &**name == "annotation-xml"
            && attr(&attrs, "encoding").is_some_and(|encoding| {
                encoding.eq_ignore_ascii_case("text/html")
                    || encoding.eq_ignore_ascii_case("application/xhtml+xml")
            });
        let place = self.place(sink, None);
        let flags = CreateFlags {
            template: ns == Ns::Html && &**name == "template",
        };
        let qual = QualName {
            prefix: None,
            ns: ns.atom(),
            local: name.clone(),
        };
        let element = sink.create_element(qual, attrs, flags);
        self.insert_at(sink, place, NodeOrText::Node(element));
        if open && self.open.len() < MAX_DEPTH {
            self.open.push(Open {
                handle: element,
                ns,
                name: name.clone(),
                html_annotation,
            });
        }
        element
    }

    /// Inserts an HTML element and opens it.
    fn insert_html<S: TreeSink<Handle = H>>(
        &mut self,
        sink: &mut S,
        name: &LocalName,
        attrs: Vec<Attribute>,
    ) -> H {
        self.insert_element(sink, Ns::Html, name, attrs, true)
    }

    /// Inserts an HTML element without opening it: void elements, which
    /// the standard inserts and pops at once.
    fn insert_html_void<S: TreeSink<Handle = H>>(
        &mut self,
        sink: &mut S,
        name: &LocalName,
        attrs: Vec<Attribute>,
    ) -> H {
        self.insert_element(sink, Ns::Html, name, attrs, false)
    }

    /// Inserts an SVG or MathML element with its names fixed up.
    fn insert_foreign<S: TreeSink<Handle = H>>(
        &mut self,
        sink: &mut S,
        ns: Ns,
        name: &LocalName,
        attrs: &[Attribute],
        open: bool,
    ) {
        let mut attrs = attrs.to_vec();
        let name = match ns {
            Ns::Svg => {
                for attr in &mut attrs {
                    if let Some(fixed) = SVG_ATTRIBUTE_NAMES
                        .iter()
                        .find(|n| n.eq_ignore_ascii_case(&attr.name.local))
                    {
                        attr.name.local = LocalName::from(*fixed);
                    }
                }
                SVG_TAG_NAMES
                    .iter()
                    .find(|n| n.eq_ignore_ascii_case(name))
                    .map_or_else(|| name.clone(), |fixed| LocalName::from(*fixed))
            }
            Ns::MathMl => {
                for attr in &mut attrs {
                    if &*attr.name.local == "definitionurl" {
                        attr.name.local = LocalName::from("definitionURL");
                    }
                }
                name.clone()
            }
            Ns::Html => name.clone(),
        };
        for attr in &mut attrs {
            adjust_foreign_attribute(&mut attr.name);
        }
        self.insert_element(sink, ns, &name, attrs, open);
    }

    /// Inserts an element whose content is raw text or RCDATA, and switches
    /// to `Text` mode until its end tag.
    fn insert_text_element<S: TreeSink<Handle = H>>(
        &mut self,
        sink: &mut S,
        name: &LocalName,
        attrs: &[Attribute],
        state: TextState,
    ) -> Step {
        let element = self.insert_html(sink, name, attrs.to_vec());
        self.text_element = Some(element);
        self.original_mode = self.mode;
        self.mode = Mode::Text;
        self.switch_to = Some(state);
        Step::Done
    }

    // ----- The stack of open elements -----

    fn current(&self) -> Option<&Open<H>> {
        self.open.last()
    }

    fn is_current(&self, handle: H) -> bool {
        self.open.last().is_some_and(|e| e.handle == handle)
    }

    /// The current node, or the fragment context while only the root is
    /// open.
    fn adjusted_current(&self) -> Option<&Open<H>> {
        match &self.context {
            Some(context) if self.open.len() == 1 => Some(context),
            _ => self.open.last(),
        }
    }

    fn context_is_select(&self) -> bool {
        self.context.as_ref().is_some_and(|c| c.is_html("select"))
    }

    fn pop(&mut self) {
        self.open.pop();
    }

    /// Pops up to and including the nearest HTML element named in `names`.
    /// Pops nothing when none is open.
    fn pop_until_html(&mut self, names: &[&str]) {
        if let Some(index) = self.open.iter().rposition(|e| e.is_html_in(names)) {
            self.open.truncate(index);
        }
    }

    fn clear_stack_back_to(&mut self, names: &[&str]) {
        while self.open.len() > 1 && !self.current().is_some_and(|e| e.is_html_in(names)) {
            self.open.pop();
        }
    }

    fn generate_implied_end_tags(&mut self, except: Option<&str>) {
        while let Some(current) = self.current()
            && current.is_html_in(IMPLIED_END)
            && except.is_none_or(|except| &*current.name != except)
        {
            self.open.pop();
        }
    }

    fn generate_implied_end_tags_thoroughly(&mut self) {
        while self
            .current()
            .is_some_and(|e| e.is_html_in(IMPLIED_END) || e.is_html_in(IMPLIED_END_TABLE))
        {
            self.open.pop();
        }
    }

    fn close_p(&mut self) {
        self.generate_implied_end_tags(Some("p"));
        self.pop_until_html(&["p"]);
    }

    fn close_p_in_button_scope(&mut self) {
        if self.has_in_scope(&["p"], Scope::Button) {
            self.close_p();
        }
    }

    fn is_scope_boundary(entry: &Open<H>, scope: Scope) -> bool {
        match scope {
            Scope::Table => entry.is_html_in(&["html", "table", "template"]),
            _ => {
                let default = match entry.ns {
                    Ns::Html => SCOPE_HTML.contains(&&*entry.name),
                    Ns::MathMl => {
                        entry.is_mathml_text_integration() || &*entry.name == "annotation-xml"
                    }
                    Ns::Svg => SVG_HTML_INTEGRATION.contains(&&*entry.name),
                };
                default
                    || (scope == Scope::ListItem && entry.is_html_in(&["ol", "ul"]))
                    || (scope == Scope::Button && entry.is_html("button"))
            }
        }
    }

    /// Whether an HTML element named in `names` is open in `scope`.
    fn has_in_scope(&self, names: &[&str], scope: Scope) -> bool {
        for entry in self.open.iter().rev() {
            if entry.is_html_in(names) {
                return true;
            }
            if Self::is_scope_boundary(entry, scope) {
                return false;
            }
        }
        false
    }

    /// Whether the element `handle` is open in the default scope.
    fn element_in_scope(&self, handle: H) -> bool {
        for entry in self.open.iter().rev() {
            if entry.handle == handle {
                return true;
            }
            if Self::is_scope_boundary(entry, Scope::Default) {
                return false;
            }
        }
        false
    }

    fn reset_insertion_mode(&mut self) {
        for index in (0..self.open.len()).rev() {
            let last = index == 0;
            let node = match &self.context {
                Some(context) if last => context,
                _ => &self.open[index],
            };
            if node.ns != Ns::Html {
                if last {
                    self.mode = Mode::InBody;
                    return;
                }
                continue;
            }
            self.mode = match &*node.name {
                "td" | "th" if !last => Mode::InCell,
                "tr" => Mode::InRow,
                "tbody" | "thead" | "tfoot" => Mode::InTableBody,
                "caption" => Mode::InCaption,
                "colgroup" => Mode::InColumnGroup,
                "table" => Mode::InTable,
                "template" => *self.template_modes.last().unwrap_or(&Mode::InBody),
                "head" if !last => Mode::InHead,
                "body" => Mode::InBody,
                "frameset" => Mode::InFrameset,
                "html" => {
                    if self.head.is_none() {
                        Mode::BeforeHead
                    } else {
                        Mode::AfterHead
                    }
                }
                _ if last => Mode::InBody,
                _ => continue,
            };
            return;
        }
        self.mode = Mode::InBody;
    }

    // ----- The list of active formatting elements -----

    fn formatting_index(&self, handle: H) -> Option<usize> {
        self.formatting.iter().rposition(
            |entry| matches!(entry, Formatting::Element { handle: h, .. } if *h == handle),
        )
    }

    fn remove_formatting(&mut self, handle: H) {
        if let Some(index) = self.formatting_index(handle) {
            self.formatting.remove(index);
        }
    }

    /// The last formatting element called `name` after the last marker.
    fn formatting_after_marker(&self, name: &str) -> Option<H> {
        for entry in self.formatting.iter().rev() {
            match entry {
                Formatting::Marker => return None,
                Formatting::Element { handle, tag } if &*tag.name == name => return Some(*handle),
                Formatting::Element { .. } => {}
            }
        }
        None
    }

    /// Inserts a formatting element and adds it to the list, keeping at
    /// most three identical ones after the last marker.
    fn insert_formatting<S: TreeSink<Handle = H>>(
        &mut self,
        sink: &mut S,
        name: &LocalName,
        attrs: &[Attribute],
    ) {
        let element = self.insert_html(sink, name, attrs.to_vec());
        if !self.is_current(element) {
            return;
        }
        let mut same = Vec::new();
        for (index, entry) in self.formatting.iter().enumerate().rev() {
            match entry {
                Formatting::Marker => break,
                Formatting::Element { tag, .. }
                    if tag.name == *name && same_attributes(&tag.attrs, attrs) =>
                {
                    same.push(index);
                }
                Formatting::Element { .. } => {}
            }
        }
        if same.len() >= 3
            && let Some(&earliest) = same.last()
        {
            self.formatting.remove(earliest);
        }
        self.formatting.push(Formatting::Element {
            handle: element,
            tag: Tag {
                name: name.clone(),
                attrs: attrs.to_vec(),
            },
        });
    }

    fn reconstruct_formatting<S: TreeSink<Handle = H>>(&mut self, sink: &mut S) {
        // At the depth cap nothing can be reopened anyway.
        if self.open.len() >= MAX_DEPTH {
            return;
        }
        let is_open = |builder: &Self, entry: &Formatting<H>| match entry {
            Formatting::Marker => true,
            Formatting::Element { handle, .. } => builder.open.iter().any(|e| e.handle == *handle),
        };
        let Some(last) = self.formatting.last() else {
            return;
        };
        if is_open(self, last) {
            return;
        }
        let mut start = self.formatting.len() - 1;
        while start > 0 && !is_open(self, &self.formatting[start - 1]) {
            start -= 1;
        }
        for index in start..self.formatting.len() {
            let Formatting::Element { tag, .. } = &self.formatting[index] else {
                continue;
            };
            let tag = tag.clone();
            let element = self.insert_html(sink, &tag.name, tag.attrs.clone());
            self.formatting[index] = Formatting::Element {
                handle: element,
                tag,
            };
        }
    }

    fn clear_formatting_to_marker(&mut self) {
        while let Some(entry) = self.formatting.pop() {
            if matches!(entry, Formatting::Marker) {
                return;
            }
        }
    }

    /// The adoption agency algorithm for an end tag (or an `a` or `nobr`
    /// start tag) called `subject`. False means "act as any other end tag".
    fn adoption_agency<S: TreeSink<Handle = H>>(
        &mut self,
        sink: &mut S,
        subject: &LocalName,
    ) -> bool {
        if let Some(current) = self.current()
            && current.ns == Ns::Html
            && current.name == *subject
            && self.formatting_index(current.handle).is_none()
        {
            self.open.pop();
            return true;
        }
        for _ in 0..8 {
            let Some(formatting) = self.formatting_after_marker(subject) else {
                return false;
            };
            let Some(formatting_open) = self.open.iter().rposition(|e| e.handle == formatting)
            else {
                self.remove_formatting(formatting);
                return true;
            };
            if !self.element_in_scope(formatting) {
                return true;
            }
            let Some(furthest) =
                (formatting_open + 1..self.open.len()).find(|&i| self.open[i].is_special())
            else {
                self.open.truncate(formatting_open);
                self.remove_formatting(formatting);
                return true;
            };
            let Some(common_ancestor) = formatting_open
                .checked_sub(1)
                .and_then(|i| self.open.get(i))
                .cloned()
            else {
                return true;
            };
            let furthest_block = self.open[furthest].handle;
            let mut bookmark = Bookmark::Replace(formatting);
            let mut node_index = furthest;
            let mut last_node = furthest_block;
            let mut inner = 0;
            loop {
                inner += 1;
                node_index -= 1;
                let node = self.open[node_index].handle;
                if node == formatting {
                    break;
                }
                if inner > 3
                    && let Some(index) = self.formatting_index(node)
                {
                    self.formatting.remove(index);
                }
                let Some(entry) = self.formatting_index(node) else {
                    self.open.remove(node_index);
                    continue;
                };
                let Formatting::Element { tag, .. } = &self.formatting[entry] else {
                    continue;
                };
                let tag = tag.clone();
                let copy = sink.create_element(
                    QualName::html(tag.name.clone()),
                    tag.attrs.clone(),
                    CreateFlags::default(),
                );
                self.formatting[entry] = Formatting::Element { handle: copy, tag };
                self.open[node_index].handle = copy;
                if last_node == furthest_block {
                    bookmark = Bookmark::InsertAfter(copy);
                }
                sink.remove_from_parent(last_node);
                sink.append(copy, NodeOrText::Node(last_node));
                last_node = copy;
            }
            sink.remove_from_parent(last_node);
            let place = self.place(sink, Some(&common_ancestor));
            self.insert_at(sink, place, NodeOrText::Node(last_node));

            let Some(Formatting::Element { tag, .. }) = self
                .formatting_index(formatting)
                .map(|i| self.formatting[i].clone())
            else {
                return true;
            };
            let copy = sink.create_element(
                QualName::html(tag.name.clone()),
                tag.attrs.clone(),
                CreateFlags::default(),
            );
            sink.reparent_children(furthest_block, copy);
            sink.append(furthest_block, NodeOrText::Node(copy));

            let entry = Formatting::Element {
                handle: copy,
                tag: tag.clone(),
            };
            match bookmark {
                Bookmark::Replace(_) => {
                    if let Some(index) = self.formatting_index(formatting) {
                        self.formatting[index] = entry;
                    }
                }
                Bookmark::InsertAfter(after) => {
                    if let Some(index) = self.formatting_index(after) {
                        self.formatting.insert(index + 1, entry);
                    }
                    self.remove_formatting(formatting);
                }
            }
            if let Some(index) = self.open.iter().rposition(|e| e.handle == formatting) {
                self.open.remove(index);
            }
            if let Some(index) = self.open.iter().rposition(|e| e.handle == furthest_block) {
                self.open.insert(
                    index + 1,
                    Open {
                        handle: copy,
                        ns: Ns::Html,
                        name: tag.name,
                        html_annotation: false,
                    },
                );
            }
        }
        true
    }

    fn set_quirks<S: TreeSink<Handle = H>>(&mut self, sink: &mut S, mode: QuirksMode) {
        self.quirks = mode;
        sink.set_quirks_mode(mode);
    }
}

fn start_tag(name: &LocalName, attrs: &[Attribute], self_closing: bool) -> Token {
    Token::StartTag {
        name: name.clone(),
        attrs: attrs.to_vec(),
        self_closing,
    }
}

/// Gives `xlink:`, `xml:` and `xmlns` attributes on foreign elements their
/// namespace and prefix.
fn adjust_foreign_attribute(name: &mut QualName) {
    let (prefix, local, ns): (Option<Prefix>, &str, Namespace) = match &*name.local {
        "xlink:actuate" | "xlink:arcrole" | "xlink:href" | "xlink:role" | "xlink:show"
        | "xlink:title" | "xlink:type" => (
            Some(Prefix::from("xlink")),
            &name.local[6..],
            ns!("http://www.w3.org/1999/xlink"),
        ),
        "xml:lang" | "xml:space" => (
            Some(Prefix::from("xml")),
            &name.local[4..],
            ns!("http://www.w3.org/XML/1998/namespace"),
        ),
        "xmlns" => (None, "xmlns", ns!("http://www.w3.org/2000/xmlns/")),
        "xmlns:xlink" => (
            Some(Prefix::from("xmlns")),
            "xlink",
            ns!("http://www.w3.org/2000/xmlns/"),
        ),
        _ => return,
    };
    *name = QualName {
        prefix,
        ns,
        local: LocalName::from(local),
    };
}

/// The document's mode from its doctype.
fn quirks_mode(
    force_quirks: bool,
    name: Option<&str>,
    public_id: Option<&str>,
    system_id: Option<&str>,
) -> QuirksMode {
    let public = public_id.map(str::to_ascii_lowercase);
    let system = system_id.map(str::to_ascii_lowercase);
    let public_starts = |prefixes: &[&str]| {
        public
            .as_deref()
            .is_some_and(|p| prefixes.iter().any(|x| p.starts_with(x)))
    };
    if force_quirks
        || name != Some("html")
        || public
            .as_deref()
            .is_some_and(|p| QUIRKS_PUBLIC_IDS.contains(&p))
        || system.as_deref() == Some(QUIRKS_SYSTEM_ID)
        || public_starts(QUIRKS_PUBLIC_PREFIXES)
        || (system.is_none() && public_starts(HTML4_PUBLIC_PREFIXES))
    {
        return QuirksMode::Quirks;
    }
    if public_starts(LIMITED_QUIRKS_PUBLIC_PREFIXES)
        || (system.is_some() && public_starts(HTML4_PUBLIC_PREFIXES))
    {
        return QuirksMode::LimitedQuirks;
    }
    QuirksMode::NoQuirks
}

#[cfg(test)]
mod html5lib;
