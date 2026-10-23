//! The tokenizer: text to tokens, one state machine step at a time
//! (spec 0004, child 0002, Tokenizer).
//!
//! Every state of the standard's tokenization section, as one `match` over
//! [`State`]. Input arrives in pieces, so every state can stop where the
//! input ends and pick up again on the next push; nothing is lost and
//! nothing waits longer than the token it is in the middle of. Parse errors
//! are counted and logged at `trace`, never surfaced: the tokenizer always
//! produces something.
//!
//! Text is batched: characters collect into one run, handed out as a single
//! [`Token::Characters`] before the next other token, or when the input
//! runs out, so a page shows the text it has while the rest loads.

use std::collections::VecDeque;

use base::atoms::{LocalName, QualName};

use crate::Attribute;
use crate::entities;

/// What the tokenizer hands the tree builder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Token {
    /// A `<!DOCTYPE>`. A part the source left out is `None`, which is not
    /// the same as empty for quirks mode.
    Doctype {
        name: Option<String>,
        public_id: Option<String>,
        system_id: Option<String>,
        force_quirks: bool,
    },
    /// A start tag, attributes in source order, duplicates dropped.
    StartTag {
        name: LocalName,
        attrs: Vec<Attribute>,
        self_closing: bool,
    },
    /// An end tag. Its attributes are dropped, as the standard does.
    EndTag { name: LocalName },
    /// A comment's text.
    Comment(String),
    /// A run of text.
    Characters(String),
    /// The end of the input.
    Eof,
}

/// The states the tree builder can switch the tokenizer to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextState {
    /// Normal markup. The tokenizer returns to it by itself after a text
    /// element's end tag; only a fragment's context starts elsewhere.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "only the suite's fragment cases start in data state explicitly"
        )
    )]
    Data,
    /// Text with character references but no tags, up to the matching end
    /// tag (`<title>`, `<textarea>`).
    RcData,
    /// Text with neither, up to the matching end tag (`<style>`, `<xmp>`,
    /// `<noscript>` with scripting on).
    RawText,
    /// Script text, which has its own escape rules for `<!--`.
    ScriptData,
    /// Everything to the end is text (`<plaintext>`).
    PlainText,
}

/// The standard's tokenizer states, by their names there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Data,
    RcData,
    RawText,
    ScriptData,
    PlainText,
    TagOpen,
    EndTagOpen,
    TagName,
    RcDataLessThanSign,
    RcDataEndTagOpen,
    RcDataEndTagName,
    RawTextLessThanSign,
    RawTextEndTagOpen,
    RawTextEndTagName,
    ScriptDataLessThanSign,
    ScriptDataEndTagOpen,
    ScriptDataEndTagName,
    ScriptDataEscapeStart,
    ScriptDataEscapeStartDash,
    ScriptDataEscaped,
    ScriptDataEscapedDash,
    ScriptDataEscapedDashDash,
    ScriptDataEscapedLessThanSign,
    ScriptDataEscapedEndTagOpen,
    ScriptDataEscapedEndTagName,
    ScriptDataDoubleEscapeStart,
    ScriptDataDoubleEscaped,
    ScriptDataDoubleEscapedDash,
    ScriptDataDoubleEscapedDashDash,
    ScriptDataDoubleEscapedLessThanSign,
    ScriptDataDoubleEscapeEnd,
    BeforeAttributeName,
    AttributeName,
    AfterAttributeName,
    BeforeAttributeValue,
    AttributeValueDoubleQuoted,
    AttributeValueSingleQuoted,
    AttributeValueUnquoted,
    AfterAttributeValueQuoted,
    SelfClosingStartTag,
    BogusComment,
    MarkupDeclarationOpen,
    CommentStart,
    CommentStartDash,
    Comment,
    CommentLessThanSign,
    CommentLessThanSignBang,
    CommentLessThanSignBangDash,
    CommentLessThanSignBangDashDash,
    CommentEndDash,
    CommentEnd,
    CommentEndBang,
    Doctype,
    BeforeDoctypeName,
    DoctypeName,
    AfterDoctypeName,
    AfterDoctypePublicKeyword,
    BeforeDoctypePublicIdentifier,
    DoctypePublicIdentifierDoubleQuoted,
    DoctypePublicIdentifierSingleQuoted,
    AfterDoctypePublicIdentifier,
    BetweenDoctypePublicAndSystemIdentifiers,
    AfterDoctypeSystemKeyword,
    BeforeDoctypeSystemIdentifier,
    DoctypeSystemIdentifierDoubleQuoted,
    DoctypeSystemIdentifierSingleQuoted,
    AfterDoctypeSystemIdentifier,
    BogusDoctype,
    CdataSection,
    CdataSectionBracket,
    CdataSectionEnd,
    CharacterReference,
    NamedCharacterReference,
    AmbiguousAmpersand,
    NumericCharacterReference,
    HexadecimalCharacterReferenceStart,
    DecimalCharacterReferenceStart,
    HexadecimalCharacterReference,
    DecimalCharacterReference,
    NumericCharacterReferenceEnd,
}

impl From<TextState> for State {
    fn from(state: TextState) -> Self {
        match state {
            TextState::Data => Self::Data,
            TextState::RcData => Self::RcData,
            TextState::RawText => Self::RawText,
            TextState::ScriptData => Self::ScriptData,
            TextState::PlainText => Self::PlainText,
        }
    }
}

/// The three text states whose end tag states work alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EndTagText {
    RcData,
    RawText,
    ScriptData,
    ScriptDataEscaped,
}

impl EndTagText {
    fn text_state(self) -> State {
        match self {
            Self::RcData => State::RcData,
            Self::RawText => State::RawText,
            Self::ScriptData => State::ScriptData,
            Self::ScriptDataEscaped => State::ScriptDataEscaped,
        }
    }

    fn name_state(self) -> State {
        match self {
            Self::RcData => State::RcDataEndTagName,
            Self::RawText => State::RawTextEndTagName,
            Self::ScriptData => State::ScriptDataEndTagName,
            Self::ScriptDataEscaped => State::ScriptDataEscapedEndTagName,
        }
    }
}

/// A tag being read.
#[derive(Debug, Default)]
struct TagBuilder {
    end: bool,
    name: String,
    attrs: Vec<(String, String)>,
    self_closing: bool,
}

/// A doctype being read.
#[derive(Debug, Default)]
struct DoctypeBuilder {
    name: Option<String>,
    public_id: Option<String>,
    system_id: Option<String>,
    force_quirks: bool,
}

/// What the next input position holds.
enum Input {
    Char(char),
    Eof,
    /// The input ran out but more may come.
    Wait,
}

/// Whether the input starts with a word.
enum Lookahead {
    Yes,
    No,
    /// The input so far is a prefix of the word; more is needed to tell.
    Wait,
}

/// Whether `step` can run again or needs more input.
enum Step {
    Continue,
    Wait,
}

const fn is_space(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\x0C' | ' ')
}

pub(crate) struct Tokenizer {
    input: Vec<char>,
    pos: usize,
    eof: bool,
    state: State,
    /// Tokens ready to hand out; the run of text before them comes first.
    ready: VecDeque<Token>,
    /// Text read but not yet handed out.
    text: String,
    tag: TagBuilder,
    attr_name: String,
    attr_value: String,
    comment: String,
    doctype: DoctypeBuilder,
    /// The standard's temporary buffer.
    buffer: String,
    /// Where a character reference returns to.
    return_state: State,
    /// The numeric character reference being read.
    char_ref_code: u32,
    /// The last start tag's name, which ends RCDATA, RAWTEXT and script
    /// data.
    last_start_tag: Option<String>,
    /// Whether `<![CDATA[` opens a CDATA section, which it does only in
    /// foreign content; the tree builder keeps it current.
    cdata_allowed: bool,
    errors: u32,
    eof_sent: bool,
}

impl Tokenizer {
    pub(crate) fn new() -> Self {
        Self {
            input: Vec::new(),
            pos: 0,
            eof: false,
            state: State::Data,
            ready: VecDeque::new(),
            text: String::new(),
            tag: TagBuilder::default(),
            attr_name: String::new(),
            attr_value: String::new(),
            comment: String::new(),
            doctype: DoctypeBuilder::default(),
            buffer: String::new(),
            return_state: State::Data,
            char_ref_code: 0,
            last_start_tag: None,
            cdata_allowed: false,
            errors: 0,
            eof_sent: false,
        }
    }

    /// Adds decoded text to the end of the input.
    pub(crate) fn push(&mut self, text: &str) {
        // Drop what was consumed before growing, so the buffer holds only
        // the token in progress plus the new text.
        if self.pos > 0 {
            self.input.drain(..self.pos);
            self.pos = 0;
        }
        self.input.extend(text.chars());
    }

    /// No more input will come.
    pub(crate) fn set_eof(&mut self) {
        self.eof = true;
    }

    /// The tree builder's switch after a start tag (`<style>` and so on).
    pub(crate) fn switch_to(&mut self, state: TextState) {
        self.state = state.into();
    }

    /// Lets `<![CDATA[` open a CDATA section, which the standard allows
    /// only while the adjusted current node is not an HTML element.
    pub(crate) fn set_cdata_allowed(&mut self, allowed: bool) {
        self.cdata_allowed = allowed;
    }

    /// How many parse errors the input had so far.
    pub(crate) fn errors(&self) -> u32 {
        self.errors
    }

    /// The next token, or `None` when the input ran out partway and more
    /// is needed.
    pub(crate) fn next_token(&mut self) -> Option<Token> {
        loop {
            if let Some(token) = self.ready.pop_front() {
                return Some(token);
            }
            if self.eof_sent {
                return None;
            }
            if let Step::Wait = self.step() {
                // Hand out the text read so far, so it shows while the rest
                // loads; it is complete, whatever comes next.
                if self.text.is_empty() {
                    return None;
                }
                return Some(Token::Characters(std::mem::take(&mut self.text)));
            }
        }
    }

    // Input.

    fn consume(&mut self) -> Input {
        match self.input.get(self.pos) {
            Some(&c) => {
                self.pos += 1;
                Input::Char(c)
            }
            None if self.eof => Input::Eof,
            None => Input::Wait,
        }
    }

    /// The standard's "reconsume in `state`": the character just consumed
    /// is read again there. The end of the input was never consumed.
    fn reconsume(&mut self, c: Option<char>, state: State) {
        if c.is_some() {
            self.pos = self.pos.saturating_sub(1);
        }
        self.state = state;
    }

    fn remaining(&self) -> &[char] {
        self.input.get(self.pos..).unwrap_or_default()
    }

    /// Whether the input at the current position starts with `word`.
    fn lookahead(&self, word: &str, ignore_case: bool) -> Lookahead {
        let rest = self.remaining();
        let same = |w: char, r: char| {
            if ignore_case {
                w.eq_ignore_ascii_case(&r)
            } else {
                w == r
            }
        };
        let len = word.chars().count();
        if !word.chars().zip(rest).all(|(w, &r)| same(w, r)) {
            return Lookahead::No;
        }
        if rest.len() >= len {
            Lookahead::Yes
        } else if self.eof {
            Lookahead::No
        } else {
            Lookahead::Wait
        }
    }

    // Output.

    fn error(&mut self, what: &'static str) {
        self.errors = self.errors.saturating_add(1);
        tracing::trace!(error = what, "html parse error");
    }

    fn emit_char(&mut self, c: char) {
        self.text.push(c);
    }

    fn emit_str(&mut self, s: &str) {
        self.text.push_str(s);
    }

    /// Hands out `token` after the text run before it.
    fn emit(&mut self, token: Token) {
        if !self.text.is_empty() {
            self.ready
                .push_back(Token::Characters(std::mem::take(&mut self.text)));
        }
        self.ready.push_back(token);
    }

    fn emit_eof(&mut self) {
        self.emit(Token::Eof);
        self.eof_sent = true;
    }

    fn emit_comment(&mut self) {
        let comment = std::mem::take(&mut self.comment);
        self.emit(Token::Comment(comment));
    }

    fn emit_doctype(&mut self) {
        let doctype = std::mem::take(&mut self.doctype);
        self.emit(Token::Doctype {
            name: doctype.name,
            public_id: doctype.public_id,
            system_id: doctype.system_id,
            force_quirks: doctype.force_quirks,
        });
    }

    fn new_tag(&mut self, end: bool) {
        self.tag = TagBuilder {
            end,
            ..TagBuilder::default()
        };
        self.attr_name.clear();
        self.attr_value.clear();
    }

    fn start_attribute(&mut self) {
        self.finish_attribute();
        self.attr_name.clear();
        self.attr_value.clear();
    }

    /// Ends the attribute in progress. A name already on the tag keeps its
    /// first value; the repeat is dropped.
    fn finish_attribute(&mut self) {
        if self.attr_name.is_empty() {
            return;
        }
        let name = std::mem::take(&mut self.attr_name);
        let value = std::mem::take(&mut self.attr_value);
        if self.tag.attrs.iter().any(|(existing, _)| *existing == name) {
            self.error("duplicate-attribute");
            return;
        }
        self.tag.attrs.push((name, value));
    }

    fn emit_tag(&mut self) {
        self.finish_attribute();
        let tag = std::mem::take(&mut self.tag);
        let name = LocalName::from(tag.name.as_str());
        if tag.end {
            if !tag.attrs.is_empty() {
                self.error("end-tag-with-attributes");
            }
            if tag.self_closing {
                self.error("end-tag-with-trailing-solidus");
            }
            self.emit(Token::EndTag { name });
            return;
        }
        self.last_start_tag = Some(tag.name);
        self.emit(Token::StartTag {
            name,
            attrs: tag
                .attrs
                .into_iter()
                .map(|(name, value)| Attribute {
                    name: QualName::attribute(LocalName::from(name)),
                    value,
                })
                .collect(),
            self_closing: tag.self_closing,
        });
    }

    /// Whether the end tag being read closes the element the last start tag
    /// opened.
    fn is_appropriate_end_tag(&self) -> bool {
        self.tag.end && self.last_start_tag.as_deref() == Some(self.tag.name.as_str())
    }

    fn in_attribute(&self) -> bool {
        matches!(
            self.return_state,
            State::AttributeValueDoubleQuoted
                | State::AttributeValueSingleQuoted
                | State::AttributeValueUnquoted
        )
    }

    /// The standard's "flush code points consumed as a character
    /// reference": the temporary buffer goes to the attribute value or out
    /// as text.
    fn flush_char_ref(&mut self) {
        let buffer = std::mem::take(&mut self.buffer);
        if self.in_attribute() {
            self.attr_value.push_str(&buffer);
        } else {
            self.emit_str(&buffer);
        }
    }

    // The state machine.

    /// Runs one state.
    fn step(&mut self) -> Step {
        // States that look ahead, or read nothing, before consuming.
        match self.state {
            State::MarkupDeclarationOpen => return self.markup_declaration_open(),
            State::NamedCharacterReference => return self.named_character_reference(),
            State::NumericCharacterReferenceEnd => {
                self.numeric_character_reference_end();
                return Step::Continue;
            }
            State::AfterDoctypeName => {
                if let Some(step) = self.after_doctype_keyword() {
                    return step;
                }
            }
            _ => {}
        }
        let c = match self.consume() {
            Input::Wait => return Step::Wait,
            Input::Eof => None,
            Input::Char(c) => Some(c),
        };
        match self.state {
            State::Data
            | State::RcData
            | State::RawText
            | State::ScriptData
            | State::PlainText
            | State::CdataSection
            | State::CdataSectionBracket
            | State::CdataSectionEnd => self.text_states(c),
            State::TagOpen
            | State::EndTagOpen
            | State::TagName
            | State::BeforeAttributeName
            | State::AttributeName
            | State::AfterAttributeName
            | State::BeforeAttributeValue
            | State::AttributeValueDoubleQuoted
            | State::AttributeValueSingleQuoted
            | State::AttributeValueUnquoted
            | State::AfterAttributeValueQuoted
            | State::SelfClosingStartTag => self.tag_states(c),
            State::RcDataLessThanSign
            | State::RcDataEndTagOpen
            | State::RcDataEndTagName
            | State::RawTextLessThanSign
            | State::RawTextEndTagOpen
            | State::RawTextEndTagName
            | State::ScriptDataLessThanSign
            | State::ScriptDataEndTagOpen
            | State::ScriptDataEndTagName
            | State::ScriptDataEscapedEndTagOpen
            | State::ScriptDataEscapedEndTagName => self.end_tag_in_text_states(c),
            State::ScriptDataEscapeStart
            | State::ScriptDataEscapeStartDash
            | State::ScriptDataEscaped
            | State::ScriptDataEscapedDash
            | State::ScriptDataEscapedDashDash
            | State::ScriptDataEscapedLessThanSign
            | State::ScriptDataDoubleEscapeStart
            | State::ScriptDataDoubleEscaped
            | State::ScriptDataDoubleEscapedDash
            | State::ScriptDataDoubleEscapedDashDash
            | State::ScriptDataDoubleEscapedLessThanSign
            | State::ScriptDataDoubleEscapeEnd => self.script_escape_states(c),
            State::BogusComment
            | State::CommentStart
            | State::CommentStartDash
            | State::Comment
            | State::CommentLessThanSign
            | State::CommentLessThanSignBang
            | State::CommentLessThanSignBangDash
            | State::CommentLessThanSignBangDashDash
            | State::CommentEndDash
            | State::CommentEnd
            | State::CommentEndBang => self.comment_states(c),
            State::Doctype
            | State::BeforeDoctypeName
            | State::DoctypeName
            | State::AfterDoctypeName
            | State::AfterDoctypePublicKeyword
            | State::BeforeDoctypePublicIdentifier
            | State::DoctypePublicIdentifierDoubleQuoted
            | State::DoctypePublicIdentifierSingleQuoted
            | State::AfterDoctypePublicIdentifier
            | State::BetweenDoctypePublicAndSystemIdentifiers
            | State::AfterDoctypeSystemKeyword
            | State::BeforeDoctypeSystemIdentifier
            | State::DoctypeSystemIdentifierDoubleQuoted
            | State::DoctypeSystemIdentifierSingleQuoted
            | State::AfterDoctypeSystemIdentifier
            | State::BogusDoctype => self.doctype_states(c),
            State::CharacterReference
            | State::AmbiguousAmpersand
            | State::NumericCharacterReference
            | State::HexadecimalCharacterReferenceStart
            | State::DecimalCharacterReferenceStart
            | State::HexadecimalCharacterReference
            | State::DecimalCharacterReference => self.char_ref_states(c),
            // Handled before consuming, above.
            State::MarkupDeclarationOpen
            | State::NamedCharacterReference
            | State::NumericCharacterReferenceEnd => {}
        }
        Step::Continue
    }

    /// Data, RCDATA, RAWTEXT, script data, PLAINTEXT and CDATA sections.
    fn text_states(&mut self, c: Option<char>) {
        let state = self.state;
        let Some(c) = c else {
            if matches!(
                state,
                State::CdataSection | State::CdataSectionBracket | State::CdataSectionEnd
            ) {
                self.error("eof-in-cdata");
                match state {
                    State::CdataSectionBracket => self.emit_char(']'),
                    State::CdataSectionEnd => self.emit_str("]]"),
                    _ => {}
                }
            }
            self.emit_eof();
            return;
        };
        match (state, c) {
            (State::Data | State::RcData, '&') => {
                self.return_state = state;
                self.state = State::CharacterReference;
            }
            (State::Data, '<') => self.state = State::TagOpen,
            (State::RcData, '<') => self.state = State::RcDataLessThanSign,
            (State::RawText, '<') => self.state = State::RawTextLessThanSign,
            (State::ScriptData, '<') => self.state = State::ScriptDataLessThanSign,
            (State::Data, '\0') => {
                // Kept as is; the tree builder decides what NUL becomes.
                self.error("unexpected-null-character");
                self.emit_char('\0');
            }
            (State::RcData | State::RawText | State::ScriptData | State::PlainText, '\0') => {
                self.error("unexpected-null-character");
                self.emit_char('\u{FFFD}');
            }
            (State::CdataSection, ']') => self.state = State::CdataSectionBracket,
            (State::CdataSectionBracket, ']') => self.state = State::CdataSectionEnd,
            (State::CdataSectionBracket, _) => {
                self.emit_char(']');
                self.reconsume(Some(c), State::CdataSection);
            }
            (State::CdataSectionEnd, ']') => self.emit_char(']'),
            (State::CdataSectionEnd, '>') => self.state = State::Data,
            (State::CdataSectionEnd, _) => {
                self.emit_str("]]");
                self.reconsume(Some(c), State::CdataSection);
            }
            (_, c) => self.emit_char(c),
        }
    }

    /// From `<` through a start or end tag and its attributes.
    fn tag_states(&mut self, c: Option<char>) {
        let state = self.state;
        let Some(c) = c else {
            match state {
                State::TagOpen => {
                    self.error("eof-before-tag-name");
                    self.emit_char('<');
                }
                State::EndTagOpen => {
                    self.error("eof-before-tag-name");
                    self.emit_str("</");
                }
                State::BeforeAttributeName | State::AttributeName => {
                    self.reconsume(None, State::AfterAttributeName);
                    return;
                }
                State::BeforeAttributeValue => {
                    self.reconsume(None, State::AttributeValueUnquoted);
                    return;
                }
                // A tag cut off by the end of the input is dropped.
                _ => self.error("eof-in-tag"),
            }
            self.emit_eof();
            return;
        };
        match state {
            State::TagOpen => match c {
                '!' => self.state = State::MarkupDeclarationOpen,
                '/' => self.state = State::EndTagOpen,
                c if c.is_ascii_alphabetic() => {
                    self.new_tag(false);
                    self.reconsume(Some(c), State::TagName);
                }
                '?' => {
                    self.error("unexpected-question-mark-instead-of-tag-name");
                    self.comment.clear();
                    self.reconsume(Some(c), State::BogusComment);
                }
                _ => {
                    self.error("invalid-first-character-of-tag-name");
                    self.emit_char('<');
                    self.reconsume(Some(c), State::Data);
                }
            },
            State::EndTagOpen => match c {
                c if c.is_ascii_alphabetic() => {
                    self.new_tag(true);
                    self.reconsume(Some(c), State::TagName);
                }
                '>' => {
                    self.error("missing-end-tag-name");
                    self.state = State::Data;
                }
                _ => {
                    self.error("invalid-first-character-of-tag-name");
                    self.comment.clear();
                    self.reconsume(Some(c), State::BogusComment);
                }
            },
            State::TagName => match c {
                c if is_space(c) => self.state = State::BeforeAttributeName,
                '/' => self.state = State::SelfClosingStartTag,
                '>' => {
                    self.state = State::Data;
                    self.emit_tag();
                }
                '\0' => {
                    self.error("unexpected-null-character");
                    self.tag.name.push('\u{FFFD}');
                }
                c => self.tag.name.push(c.to_ascii_lowercase()),
            },
            State::BeforeAttributeName => match c {
                c if is_space(c) => {}
                '/' | '>' => self.reconsume(Some(c), State::AfterAttributeName),
                '=' => {
                    self.error("unexpected-equals-sign-before-attribute-name");
                    self.start_attribute();
                    self.attr_name.push('=');
                    self.state = State::AttributeName;
                }
                _ => {
                    self.start_attribute();
                    self.reconsume(Some(c), State::AttributeName);
                }
            },
            State::AttributeName => match c {
                c if is_space(c) || c == '/' || c == '>' => {
                    self.reconsume(Some(c), State::AfterAttributeName);
                }
                '=' => self.state = State::BeforeAttributeValue,
                '\0' => {
                    self.error("unexpected-null-character");
                    self.attr_name.push('\u{FFFD}');
                }
                '"' | '\'' | '<' => {
                    self.error("unexpected-character-in-attribute-name");
                    self.attr_name.push(c);
                }
                c => self.attr_name.push(c.to_ascii_lowercase()),
            },
            State::AfterAttributeName => match c {
                c if is_space(c) => {}
                '/' => self.state = State::SelfClosingStartTag,
                '=' => self.state = State::BeforeAttributeValue,
                '>' => {
                    self.state = State::Data;
                    self.emit_tag();
                }
                _ => {
                    self.start_attribute();
                    self.reconsume(Some(c), State::AttributeName);
                }
            },
            State::BeforeAttributeValue => match c {
                c if is_space(c) => {}
                '"' => self.state = State::AttributeValueDoubleQuoted,
                '\'' => self.state = State::AttributeValueSingleQuoted,
                '>' => {
                    self.error("missing-attribute-value");
                    self.state = State::Data;
                    self.emit_tag();
                }
                _ => self.reconsume(Some(c), State::AttributeValueUnquoted),
            },
            State::AttributeValueDoubleQuoted | State::AttributeValueSingleQuoted => {
                let quote = if state == State::AttributeValueDoubleQuoted {
                    '"'
                } else {
                    '\''
                };
                match c {
                    c if c == quote => self.state = State::AfterAttributeValueQuoted,
                    '&' => {
                        self.return_state = state;
                        self.state = State::CharacterReference;
                    }
                    '\0' => {
                        self.error("unexpected-null-character");
                        self.attr_value.push('\u{FFFD}');
                    }
                    c => self.attr_value.push(c),
                }
            }
            State::AttributeValueUnquoted => match c {
                c if is_space(c) => self.state = State::BeforeAttributeName,
                '&' => {
                    self.return_state = state;
                    self.state = State::CharacterReference;
                }
                '>' => {
                    self.state = State::Data;
                    self.emit_tag();
                }
                '\0' => {
                    self.error("unexpected-null-character");
                    self.attr_value.push('\u{FFFD}');
                }
                '"' | '\'' | '<' | '=' | '`' => {
                    self.error("unexpected-character-in-unquoted-attribute-value");
                    self.attr_value.push(c);
                }
                c => self.attr_value.push(c),
            },
            State::AfterAttributeValueQuoted => match c {
                c if is_space(c) => self.state = State::BeforeAttributeName,
                '/' => self.state = State::SelfClosingStartTag,
                '>' => {
                    self.state = State::Data;
                    self.emit_tag();
                }
                _ => {
                    self.error("missing-whitespace-between-attributes");
                    self.reconsume(Some(c), State::BeforeAttributeName);
                }
            },
            State::SelfClosingStartTag => match c {
                '>' => {
                    self.tag.self_closing = true;
                    self.state = State::Data;
                    self.emit_tag();
                }
                _ => {
                    self.error("unexpected-solidus-in-tag");
                    self.reconsume(Some(c), State::BeforeAttributeName);
                }
            },
            _ => {}
        }
    }

    /// `</name>` inside RCDATA, RAWTEXT and script data (escaped or not):
    /// only the end tag of the last start tag ends the text; anything else
    /// is text.
    fn end_tag_in_text_states(&mut self, c: Option<char>) {
        let (kind, step) = match self.state {
            State::RcDataLessThanSign => (EndTagText::RcData, 0),
            State::RcDataEndTagOpen => (EndTagText::RcData, 1),
            State::RcDataEndTagName => (EndTagText::RcData, 2),
            State::RawTextLessThanSign => (EndTagText::RawText, 0),
            State::RawTextEndTagOpen => (EndTagText::RawText, 1),
            State::RawTextEndTagName => (EndTagText::RawText, 2),
            State::ScriptDataLessThanSign => (EndTagText::ScriptData, 0),
            State::ScriptDataEndTagOpen => (EndTagText::ScriptData, 1),
            State::ScriptDataEndTagName => (EndTagText::ScriptData, 2),
            State::ScriptDataEscapedEndTagOpen => (EndTagText::ScriptDataEscaped, 1),
            _ => (EndTagText::ScriptDataEscaped, 2),
        };
        let text_state = kind.text_state();
        match step {
            // The less than sign state (the escaped one lives with the
            // other script escape states).
            0 => match c {
                Some('/') => {
                    self.buffer.clear();
                    self.state = match kind {
                        EndTagText::RcData => State::RcDataEndTagOpen,
                        EndTagText::RawText => State::RawTextEndTagOpen,
                        _ => State::ScriptDataEndTagOpen,
                    };
                }
                Some('!') if kind == EndTagText::ScriptData => {
                    self.state = State::ScriptDataEscapeStart;
                    self.emit_str("<!");
                }
                _ => {
                    self.emit_char('<');
                    self.reconsume(c, text_state);
                }
            },
            // The end tag open state.
            1 => match c {
                Some(c) if c.is_ascii_alphabetic() => {
                    self.new_tag(true);
                    self.reconsume(Some(c), kind.name_state());
                }
                _ => {
                    self.emit_str("</");
                    self.reconsume(c, text_state);
                }
            },
            // The end tag name state.
            _ => {
                let appropriate = self.is_appropriate_end_tag();
                match c {
                    Some(c) if is_space(c) && appropriate => {
                        self.state = State::BeforeAttributeName;
                    }
                    Some('/') if appropriate => self.state = State::SelfClosingStartTag,
                    Some('>') if appropriate => {
                        self.state = State::Data;
                        self.emit_tag();
                    }
                    Some(c) if c.is_ascii_alphabetic() => {
                        self.tag.name.push(c.to_ascii_lowercase());
                        self.buffer.push(c);
                    }
                    _ => {
                        self.emit_str("</");
                        let buffer = std::mem::take(&mut self.buffer);
                        self.emit_str(&buffer);
                        self.reconsume(c, text_state);
                    }
                }
            }
        }
    }

    /// `<!--` inside a script, and `<script>` inside that.
    fn script_escape_states(&mut self, c: Option<char>) {
        let state = self.state;
        let Some(c) = c else {
            match state {
                State::ScriptDataEscapeStart | State::ScriptDataEscapeStartDash => {
                    self.reconsume(None, State::ScriptData);
                }
                State::ScriptDataEscapedLessThanSign => {
                    self.emit_char('<');
                    self.reconsume(None, State::ScriptDataEscaped);
                }
                State::ScriptDataDoubleEscapeStart => {
                    self.reconsume(None, State::ScriptDataEscaped);
                }
                State::ScriptDataDoubleEscapedLessThanSign | State::ScriptDataDoubleEscapeEnd => {
                    self.reconsume(None, State::ScriptDataDoubleEscaped);
                }
                _ => {
                    self.error("eof-in-script-html-comment-like-text");
                    self.emit_eof();
                }
            }
            return;
        };
        match state {
            State::ScriptDataEscapeStart | State::ScriptDataEscapeStartDash => {
                if c == '-' {
                    self.state = if state == State::ScriptDataEscapeStart {
                        State::ScriptDataEscapeStartDash
                    } else {
                        State::ScriptDataEscapedDashDash
                    };
                    self.emit_char('-');
                } else {
                    self.reconsume(Some(c), State::ScriptData);
                }
            }
            State::ScriptDataEscaped
            | State::ScriptDataEscapedDash
            | State::ScriptDataEscapedDashDash => match c {
                '-' => {
                    self.state = match state {
                        State::ScriptDataEscaped => State::ScriptDataEscapedDash,
                        _ => State::ScriptDataEscapedDashDash,
                    };
                    self.emit_char('-');
                }
                '<' => self.state = State::ScriptDataEscapedLessThanSign,
                '>' if state == State::ScriptDataEscapedDashDash => {
                    self.state = State::ScriptData;
                    self.emit_char('>');
                }
                '\0' => {
                    self.error("unexpected-null-character");
                    self.state = State::ScriptDataEscaped;
                    self.emit_char('\u{FFFD}');
                }
                c => {
                    self.state = State::ScriptDataEscaped;
                    self.emit_char(c);
                }
            },
            State::ScriptDataEscapedLessThanSign => match c {
                '/' => {
                    self.buffer.clear();
                    self.state = State::ScriptDataEscapedEndTagOpen;
                }
                c if c.is_ascii_alphabetic() => {
                    self.buffer.clear();
                    self.emit_char('<');
                    self.reconsume(Some(c), State::ScriptDataDoubleEscapeStart);
                }
                _ => {
                    self.emit_char('<');
                    self.reconsume(Some(c), State::ScriptDataEscaped);
                }
            },
            State::ScriptDataDoubleEscapeStart | State::ScriptDataDoubleEscapeEnd => {
                let (inside, outside) = if state == State::ScriptDataDoubleEscapeStart {
                    (State::ScriptDataDoubleEscaped, State::ScriptDataEscaped)
                } else {
                    (State::ScriptDataEscaped, State::ScriptDataDoubleEscaped)
                };
                match c {
                    c if is_space(c) || c == '/' || c == '>' => {
                        self.state = if self.buffer == "script" {
                            inside
                        } else {
                            outside
                        };
                        self.emit_char(c);
                    }
                    c if c.is_ascii_alphabetic() => {
                        self.buffer.push(c.to_ascii_lowercase());
                        self.emit_char(c);
                    }
                    _ => self.reconsume(Some(c), outside),
                }
            }
            State::ScriptDataDoubleEscaped
            | State::ScriptDataDoubleEscapedDash
            | State::ScriptDataDoubleEscapedDashDash => match c {
                '-' => {
                    self.state = match state {
                        State::ScriptDataDoubleEscaped => State::ScriptDataDoubleEscapedDash,
                        _ => State::ScriptDataDoubleEscapedDashDash,
                    };
                    self.emit_char('-');
                }
                '<' => {
                    self.state = State::ScriptDataDoubleEscapedLessThanSign;
                    self.emit_char('<');
                }
                '>' if state == State::ScriptDataDoubleEscapedDashDash => {
                    self.state = State::ScriptData;
                    self.emit_char('>');
                }
                '\0' => {
                    self.error("unexpected-null-character");
                    self.state = State::ScriptDataDoubleEscaped;
                    self.emit_char('\u{FFFD}');
                }
                c => {
                    self.state = State::ScriptDataDoubleEscaped;
                    self.emit_char(c);
                }
            },
            State::ScriptDataDoubleEscapedLessThanSign => {
                if c == '/' {
                    self.buffer.clear();
                    self.state = State::ScriptDataDoubleEscapeEnd;
                    self.emit_char('/');
                } else {
                    self.reconsume(Some(c), State::ScriptDataDoubleEscaped);
                }
            }
            _ => {}
        }
    }

    /// After `<!`: a comment, a doctype, a CDATA section, or a bogus
    /// comment.
    fn markup_declaration_open(&mut self) -> Step {
        let words = [("--", false), ("DOCTYPE", true), ("[CDATA[", false)];
        let mut waiting = false;
        for (word, ignore_case) in words {
            match self.lookahead(word, ignore_case) {
                Lookahead::Yes => {
                    self.pos += word.len();
                    match word {
                        "--" => {
                            self.comment.clear();
                            self.state = State::CommentStart;
                        }
                        "DOCTYPE" => self.state = State::Doctype,
                        _ if self.cdata_allowed => self.state = State::CdataSection,
                        _ => {
                            self.error("cdata-in-html-content");
                            self.comment = "[CDATA[".to_owned();
                            self.state = State::BogusComment;
                        }
                    }
                    return Step::Continue;
                }
                Lookahead::Wait => waiting = true,
                Lookahead::No => {}
            }
        }
        if waiting {
            return Step::Wait;
        }
        self.error("incorrectly-opened-comment");
        self.comment.clear();
        self.state = State::BogusComment;
        Step::Continue
    }

    /// Comments, from `<!--` (or a bogus one from `<?` or `</` plus junk)
    /// to the `>` that ends it.
    fn comment_states(&mut self, c: Option<char>) {
        let state = self.state;
        let Some(c) = c else {
            match state {
                State::CommentLessThanSign
                | State::CommentLessThanSignBang
                | State::CommentStart => self.reconsume(None, State::Comment),
                State::CommentLessThanSignBangDash => self.reconsume(None, State::CommentEndDash),
                State::CommentLessThanSignBangDashDash => self.reconsume(None, State::CommentEnd),
                State::BogusComment => {
                    self.emit_comment();
                    self.emit_eof();
                }
                _ => {
                    self.error("eof-in-comment");
                    self.emit_comment();
                    self.emit_eof();
                }
            }
            return;
        };
        match state {
            State::BogusComment => match c {
                '>' => {
                    self.state = State::Data;
                    self.emit_comment();
                }
                '\0' => {
                    self.error("unexpected-null-character");
                    self.comment.push('\u{FFFD}');
                }
                c => self.comment.push(c),
            },
            State::CommentStart => match c {
                '-' => self.state = State::CommentStartDash,
                '>' => {
                    self.error("abrupt-closing-of-empty-comment");
                    self.state = State::Data;
                    self.emit_comment();
                }
                _ => self.reconsume(Some(c), State::Comment),
            },
            State::CommentStartDash => match c {
                '-' => self.state = State::CommentEnd,
                '>' => {
                    self.error("abrupt-closing-of-empty-comment");
                    self.state = State::Data;
                    self.emit_comment();
                }
                _ => {
                    self.comment.push('-');
                    self.reconsume(Some(c), State::Comment);
                }
            },
            State::Comment => match c {
                '<' => {
                    self.comment.push('<');
                    self.state = State::CommentLessThanSign;
                }
                '-' => self.state = State::CommentEndDash,
                '\0' => {
                    self.error("unexpected-null-character");
                    self.comment.push('\u{FFFD}');
                }
                c => self.comment.push(c),
            },
            State::CommentLessThanSign => match c {
                '!' => {
                    self.comment.push('!');
                    self.state = State::CommentLessThanSignBang;
                }
                '<' => self.comment.push('<'),
                _ => self.reconsume(Some(c), State::Comment),
            },
            State::CommentLessThanSignBang => match c {
                '-' => self.state = State::CommentLessThanSignBangDash,
                _ => self.reconsume(Some(c), State::Comment),
            },
            State::CommentLessThanSignBangDash => match c {
                '-' => self.state = State::CommentLessThanSignBangDashDash,
                _ => self.reconsume(Some(c), State::CommentEndDash),
            },
            State::CommentLessThanSignBangDashDash => {
                if c != '>' {
                    self.error("nested-comment");
                }
                self.reconsume(Some(c), State::CommentEnd);
            }
            State::CommentEndDash => match c {
                '-' => self.state = State::CommentEnd,
                _ => {
                    self.comment.push('-');
                    self.reconsume(Some(c), State::Comment);
                }
            },
            State::CommentEnd => match c {
                '>' => {
                    self.state = State::Data;
                    self.emit_comment();
                }
                '!' => self.state = State::CommentEndBang,
                '-' => self.comment.push('-'),
                _ => {
                    self.comment.push_str("--");
                    self.reconsume(Some(c), State::Comment);
                }
            },
            State::CommentEndBang => match c {
                '-' => {
                    self.comment.push_str("--!");
                    self.state = State::CommentEndDash;
                }
                '>' => {
                    self.error("incorrectly-closed-comment");
                    self.state = State::Data;
                    self.emit_comment();
                }
                _ => {
                    self.comment.push_str("--!");
                    self.reconsume(Some(c), State::Comment);
                }
            },
            _ => {}
        }
    }

    /// `<!DOCTYPE name PUBLIC "public id" "system id">` and its broken
    /// forms.
    fn doctype_states(&mut self, c: Option<char>) {
        let state = self.state;
        let Some(c) = c else {
            match state {
                State::Doctype | State::BeforeDoctypeName => {
                    self.doctype = DoctypeBuilder::default();
                }
                State::BogusDoctype => {
                    self.emit_doctype();
                    self.emit_eof();
                    return;
                }
                _ => {}
            }
            self.error("eof-in-doctype");
            self.doctype.force_quirks = true;
            self.emit_doctype();
            self.emit_eof();
            return;
        };
        match state {
            State::Doctype => match c {
                c if is_space(c) => self.state = State::BeforeDoctypeName,
                '>' => self.reconsume(Some(c), State::BeforeDoctypeName),
                _ => {
                    self.error("missing-whitespace-before-doctype-name");
                    self.reconsume(Some(c), State::BeforeDoctypeName);
                }
            },
            State::BeforeDoctypeName => match c {
                c if is_space(c) => {}
                '>' => {
                    self.error("missing-doctype-name");
                    self.doctype = DoctypeBuilder {
                        force_quirks: true,
                        ..DoctypeBuilder::default()
                    };
                    self.state = State::Data;
                    self.emit_doctype();
                }
                c => {
                    let first = if c == '\0' {
                        self.error("unexpected-null-character");
                        '\u{FFFD}'
                    } else {
                        c.to_ascii_lowercase()
                    };
                    self.doctype = DoctypeBuilder {
                        name: Some(first.to_string()),
                        ..DoctypeBuilder::default()
                    };
                    self.state = State::DoctypeName;
                }
            },
            State::DoctypeName => match c {
                c if is_space(c) => self.state = State::AfterDoctypeName,
                '>' => {
                    self.state = State::Data;
                    self.emit_doctype();
                }
                c => {
                    let c = if c == '\0' {
                        self.error("unexpected-null-character");
                        '\u{FFFD}'
                    } else {
                        c.to_ascii_lowercase()
                    };
                    self.doctype.name.get_or_insert_default().push(c);
                }
            },
            State::AfterDoctypeName => match c {
                c if is_space(c) => {}
                '>' => {
                    self.state = State::Data;
                    self.emit_doctype();
                }
                // `after_doctype_keyword` took every other character.
                _ => {}
            },
            State::AfterDoctypePublicKeyword
            | State::BeforeDoctypePublicIdentifier
            | State::AfterDoctypeSystemKeyword
            | State::BeforeDoctypeSystemIdentifier => {
                let public = matches!(
                    state,
                    State::AfterDoctypePublicKeyword | State::BeforeDoctypePublicIdentifier
                );
                let after_keyword = matches!(
                    state,
                    State::AfterDoctypePublicKeyword | State::AfterDoctypeSystemKeyword
                );
                match c {
                    c if is_space(c) => {
                        if after_keyword {
                            self.state = if public {
                                State::BeforeDoctypePublicIdentifier
                            } else {
                                State::BeforeDoctypeSystemIdentifier
                            };
                        }
                    }
                    '"' | '\'' => {
                        if after_keyword {
                            self.error("missing-whitespace-after-doctype-keyword");
                        }
                        self.start_doctype_id(public, c);
                    }
                    '>' => {
                        self.error("missing-doctype-identifier");
                        self.doctype.force_quirks = true;
                        self.state = State::Data;
                        self.emit_doctype();
                    }
                    _ => {
                        self.error("missing-quote-before-doctype-identifier");
                        self.doctype.force_quirks = true;
                        self.reconsume(Some(c), State::BogusDoctype);
                    }
                }
            }
            State::DoctypePublicIdentifierDoubleQuoted
            | State::DoctypePublicIdentifierSingleQuoted
            | State::DoctypeSystemIdentifierDoubleQuoted
            | State::DoctypeSystemIdentifierSingleQuoted => {
                let public = matches!(
                    state,
                    State::DoctypePublicIdentifierDoubleQuoted
                        | State::DoctypePublicIdentifierSingleQuoted
                );
                let quote = match state {
                    State::DoctypePublicIdentifierDoubleQuoted
                    | State::DoctypeSystemIdentifierDoubleQuoted => '"',
                    _ => '\'',
                };
                match c {
                    c if c == quote => {
                        self.state = if public {
                            State::AfterDoctypePublicIdentifier
                        } else {
                            State::AfterDoctypeSystemIdentifier
                        };
                    }
                    '>' => {
                        self.error("abrupt-doctype-identifier");
                        self.doctype.force_quirks = true;
                        self.state = State::Data;
                        self.emit_doctype();
                    }
                    c => {
                        let c = if c == '\0' {
                            self.error("unexpected-null-character");
                            '\u{FFFD}'
                        } else {
                            c
                        };
                        let id = if public {
                            &mut self.doctype.public_id
                        } else {
                            &mut self.doctype.system_id
                        };
                        id.get_or_insert_default().push(c);
                    }
                }
            }
            State::AfterDoctypePublicIdentifier
            | State::BetweenDoctypePublicAndSystemIdentifiers => {
                let after = state == State::AfterDoctypePublicIdentifier;
                match c {
                    c if is_space(c) => {
                        if after {
                            self.state = State::BetweenDoctypePublicAndSystemIdentifiers;
                        }
                    }
                    '>' => {
                        self.state = State::Data;
                        self.emit_doctype();
                    }
                    '"' | '\'' => {
                        if after {
                            self.error(
                                "missing-whitespace-between-doctype-public-and-system-identifiers",
                            );
                        }
                        self.start_doctype_id(false, c);
                    }
                    _ => {
                        self.error("missing-quote-before-doctype-system-identifier");
                        self.doctype.force_quirks = true;
                        self.reconsume(Some(c), State::BogusDoctype);
                    }
                }
            }
            State::AfterDoctypeSystemIdentifier => match c {
                c if is_space(c) => {}
                '>' => {
                    self.state = State::Data;
                    self.emit_doctype();
                }
                _ => {
                    // Unlike the other doctype errors, this one keeps
                    // standards mode.
                    self.error("unexpected-character-after-doctype-system-identifier");
                    self.reconsume(Some(c), State::BogusDoctype);
                }
            },
            State::BogusDoctype => match c {
                '>' => {
                    self.state = State::Data;
                    self.emit_doctype();
                }
                '\0' => self.error("unexpected-null-character"),
                _ => {}
            },
            _ => {}
        }
    }

    /// The after doctype name state's "anything else": `PUBLIC` or `SYSTEM`
    /// from the next character on, any case. `None` leaves space, `>` and
    /// the end of the input to `doctype_states`.
    fn after_doctype_keyword(&mut self) -> Option<Step> {
        let c = self.input.get(self.pos).copied()?;
        if is_space(c) || c == '>' {
            return None;
        }
        match (
            self.lookahead("PUBLIC", true),
            self.lookahead("SYSTEM", true),
        ) {
            (Lookahead::Yes, _) => {
                self.pos += 6;
                self.state = State::AfterDoctypePublicKeyword;
            }
            (_, Lookahead::Yes) => {
                self.pos += 6;
                self.state = State::AfterDoctypeSystemKeyword;
            }
            (Lookahead::Wait, _) | (_, Lookahead::Wait) => return Some(Step::Wait),
            _ => {
                // Nothing consumed, so the bogus state reads `c` again.
                self.error("invalid-character-sequence-after-doctype-name");
                self.doctype.force_quirks = true;
                self.state = State::BogusDoctype;
            }
        }
        Some(Step::Continue)
    }

    /// Starts an empty public or system identifier quoted by `quote`.
    fn start_doctype_id(&mut self, public: bool, quote: char) {
        let double = quote == '"';
        if public {
            self.doctype.public_id = Some(String::new());
            self.state = if double {
                State::DoctypePublicIdentifierDoubleQuoted
            } else {
                State::DoctypePublicIdentifierSingleQuoted
            };
        } else {
            self.doctype.system_id = Some(String::new());
            self.state = if double {
                State::DoctypeSystemIdentifierDoubleQuoted
            } else {
                State::DoctypeSystemIdentifierSingleQuoted
            };
        }
    }

    /// After `&`: a named or numeric reference, or a plain ampersand.
    fn char_ref_states(&mut self, c: Option<char>) {
        let state = self.state;
        let return_state = self.return_state;
        match state {
            State::CharacterReference => {
                self.buffer.clear();
                self.buffer.push('&');
                match c {
                    Some(c) if c.is_ascii_alphanumeric() => {
                        self.reconsume(Some(c), State::NamedCharacterReference);
                    }
                    Some('#') => {
                        self.buffer.push('#');
                        self.state = State::NumericCharacterReference;
                    }
                    _ => {
                        self.flush_char_ref();
                        self.reconsume(c, return_state);
                    }
                }
            }
            State::AmbiguousAmpersand => match c {
                Some(c) if c.is_ascii_alphanumeric() => {
                    if self.in_attribute() {
                        self.attr_value.push(c);
                    } else {
                        self.emit_char(c);
                    }
                }
                Some(';') => {
                    self.error("unknown-named-character-reference");
                    self.reconsume(c, return_state);
                }
                _ => self.reconsume(c, return_state),
            },
            State::NumericCharacterReference => {
                self.char_ref_code = 0;
                match c {
                    Some(x @ ('x' | 'X')) => {
                        self.buffer.push(x);
                        self.state = State::HexadecimalCharacterReferenceStart;
                    }
                    _ => self.reconsume(c, State::DecimalCharacterReferenceStart),
                }
            }
            State::HexadecimalCharacterReferenceStart | State::DecimalCharacterReferenceStart => {
                let (radix, digits) = if state == State::HexadecimalCharacterReferenceStart {
                    (16, State::HexadecimalCharacterReference)
                } else {
                    (10, State::DecimalCharacterReference)
                };
                match c {
                    Some(c) if c.is_digit(radix) => self.reconsume(Some(c), digits),
                    _ => {
                        self.error("absence-of-digits-in-numeric-character-reference");
                        self.flush_char_ref();
                        self.reconsume(c, return_state);
                    }
                }
            }
            State::HexadecimalCharacterReference | State::DecimalCharacterReference => {
                let radix = if state == State::HexadecimalCharacterReference {
                    16
                } else {
                    10
                };
                match c {
                    Some(c) if c.is_digit(radix) => {
                        let digit = c.to_digit(radix).unwrap_or_default();
                        self.char_ref_code = self
                            .char_ref_code
                            .saturating_mul(radix)
                            .saturating_add(digit);
                    }
                    Some(';') => self.state = State::NumericCharacterReferenceEnd,
                    _ => {
                        self.error("missing-semicolon-after-character-reference");
                        self.reconsume(c, State::NumericCharacterReferenceEnd);
                    }
                }
            }
            _ => {}
        }
    }

    /// The named reference state: the longest name in the table that the
    /// input starts with. Looks ahead instead of consuming one character
    /// at a time, so it waits until the name is complete.
    fn named_character_reference(&mut self) -> Step {
        let rest = self.remaining();
        let run = rest
            .iter()
            .take(entities::MAX_NAME)
            .take_while(|c| c.is_ascii_alphanumeric())
            .count();
        if run == rest.len() && run < entities::MAX_NAME && !self.eof {
            // The name may go on in the next chunk.
            return Step::Wait;
        }
        let with_semicolon = usize::from(rest.get(run) == Some(&';'));
        let candidate: String = rest.iter().take(run + with_semicolon).collect();
        let Some((name, replacement)) = entities::longest_match(&candidate) else {
            self.flush_char_ref();
            self.state = State::AmbiguousAmpersand;
            return Step::Continue;
        };
        let next = rest.get(name.len()).copied();
        self.pos += name.len();
        self.buffer.push_str(name);
        let return_state = self.return_state;
        if !name.ends_with(';') {
            // In attributes, a legacy reference without `;` followed by `=`
            // or an alphanumeric is left alone (`?a=1&copy=2`).
            if self.in_attribute() && next.is_some_and(|c| c == '=' || c.is_ascii_alphanumeric()) {
                self.flush_char_ref();
                self.state = return_state;
                return Step::Continue;
            }
            self.error("missing-semicolon-after-character-reference");
        }
        self.buffer.clear();
        self.buffer.push_str(replacement);
        self.flush_char_ref();
        self.state = return_state;
        Step::Continue
    }

    /// The numeric reference end state: the character a code stands for,
    /// with the standard's replacements.
    fn numeric_character_reference_end(&mut self) {
        let code = self.char_ref_code;
        let c = match code {
            0 => {
                self.error("null-character-reference");
                '\u{FFFD}'
            }
            0x11_0000.. => {
                self.error("character-reference-outside-unicode-range");
                '\u{FFFD}'
            }
            0xD800..=0xDFFF => {
                self.error("surrogate-character-reference");
                '\u{FFFD}'
            }
            code => {
                if is_noncharacter(code) {
                    self.error("noncharacter-character-reference");
                }
                let control = code == 0x0D
                    || (is_control(code) && !matches!(code, 0x09 | 0x0A | 0x0C | 0x20));
                if control {
                    self.error("control-character-reference");
                }
                c1_replacement(code)
                    .or_else(|| char::from_u32(code))
                    .unwrap_or('\u{FFFD}')
            }
        };
        self.buffer.clear();
        self.buffer.push(c);
        self.flush_char_ref();
        self.state = self.return_state;
    }
}

/// U+FDD0 to U+FDEF, and the last two code points of every plane.
const fn is_noncharacter(code: u32) -> bool {
    matches!(code, 0xFDD0..=0xFDEF) || (code & 0xFFFE) == 0xFFFE
}

/// C0 controls, DEL and C1 controls.
const fn is_control(code: u32) -> bool {
    matches!(code, 0x00..=0x1F | 0x7F..=0x9F)
}

/// What a numeric reference into the C1 range means: the character
/// windows-1252 puts there, since that is what pages meant.
fn c1_replacement(code: u32) -> Option<char> {
    const C1: [(u32, char); 27] = [
        (0x80, '\u{20AC}'),
        (0x82, '\u{201A}'),
        (0x83, '\u{0192}'),
        (0x84, '\u{201E}'),
        (0x85, '\u{2026}'),
        (0x86, '\u{2020}'),
        (0x87, '\u{2021}'),
        (0x88, '\u{02C6}'),
        (0x89, '\u{2030}'),
        (0x8A, '\u{0160}'),
        (0x8B, '\u{2039}'),
        (0x8C, '\u{0152}'),
        (0x8E, '\u{017D}'),
        (0x91, '\u{2018}'),
        (0x92, '\u{2019}'),
        (0x93, '\u{201C}'),
        (0x94, '\u{201D}'),
        (0x95, '\u{2022}'),
        (0x96, '\u{2013}'),
        (0x97, '\u{2014}'),
        (0x98, '\u{02DC}'),
        (0x99, '\u{2122}'),
        (0x9A, '\u{0161}'),
        (0x9B, '\u{203A}'),
        (0x9C, '\u{0153}'),
        (0x9E, '\u{017E}'),
        (0x9F, '\u{0178}'),
    ];
    C1.iter().find(|(c1, _)| *c1 == code).map(|(_, c)| *c)
}

#[cfg(test)]
mod html5lib;

#[cfg(test)]
mod tests {
    use super::*;

    /// Tokenizes `chunks` one push at a time, switching text states the
    /// way the tree builder does, and merges adjacent text runs.
    fn tokens(chunks: &[&str]) -> Vec<Token> {
        let mut tokenizer = Tokenizer::new();
        let mut out: Vec<Token> = Vec::new();
        let last = chunks.len();
        for (i, chunk) in chunks.iter().chain([&""]).enumerate() {
            tokenizer.push(chunk);
            if i == last {
                tokenizer.set_eof();
            }
            while let Some(token) = tokenizer.next_token() {
                match &token {
                    Token::StartTag { name, .. } if &**name == "style" => {
                        tokenizer.switch_to(TextState::RawText);
                    }
                    Token::StartTag { name, .. } if &**name == "script" => {
                        tokenizer.switch_to(TextState::ScriptData);
                    }
                    Token::StartTag { name, .. } if &**name == "title" => {
                        tokenizer.switch_to(TextState::RcData);
                    }
                    _ => {}
                }
                if let (Some(Token::Characters(text)), Token::Characters(more)) =
                    (out.last_mut(), &token)
                {
                    text.push_str(more);
                    continue;
                }
                out.push(token);
            }
        }
        out
    }

    fn text(s: &str) -> Token {
        Token::Characters(s.into())
    }

    fn start(name: &str, attrs: &[(&str, &str)]) -> Token {
        Token::StartTag {
            name: LocalName::from(name),
            attrs: attrs
                .iter()
                .map(|(n, v)| Attribute {
                    name: QualName::attribute(LocalName::from(*n)),
                    value: (*v).into(),
                })
                .collect(),
            self_closing: false,
        }
    }

    fn end(name: &str) -> Token {
        Token::EndTag {
            name: LocalName::from(name),
        }
    }

    #[test]
    fn tags_attributes_and_text() {
        assert_eq!(
            tokens(&[r#"<P CLASS="a" id=b data-x='c' hidden class=d>Hi &amp; bye</p>"#]),
            [
                start(
                    "p",
                    &[("class", "a"), ("id", "b"), ("data-x", "c"), ("hidden", "")]
                ),
                text("Hi & bye"),
                end("p"),
                Token::Eof
            ]
        );
    }

    #[test]
    fn any_split_gives_the_same_tokens() {
        let page = "<!DOCTYPE html PUBLIC 'p' \"s\"><!-- c --><p title=\"x &lt; y\">a&#169;b&copy &nbsp;c&notin;&noti</p>\
                    <style>p > a { }</style><title>T &amp; t</title>\
                    <script>a<!--<script>b</script>c-->d</script>&#x1F600;";
        let whole = tokens(&[page]);
        assert!(whole.contains(&text("p > a { }")));
        assert!(whole.contains(&text("T & t")));
        assert!(whole.contains(&text("a<!--<script>b</script>c-->d")));
        let chars: Vec<char> = page.chars().collect();
        for split in 1..chars.len() {
            let a: String = chars[..split].iter().collect();
            let b: String = chars[split..].iter().collect();
            assert_eq!(tokens(&[&a, &b]), whole, "split at {split}");
        }
    }

    #[test]
    fn comments_doctypes_and_references() {
        assert_eq!(
            tokens(&[
                "<!doctype HTML PUBLIC \"-//W3C//DTD\" 'sys'><!---->x<!-- a -- b -->&#x41;&#128;&bogus;"
            ]),
            [
                Token::Doctype {
                    name: Some("html".into()),
                    public_id: Some("-//W3C//DTD".into()),
                    system_id: Some("sys".into()),
                    force_quirks: false,
                },
                Token::Comment(String::new()),
                text("x"),
                Token::Comment(" a -- b ".into()),
                text("A\u{20AC}&bogus;"),
                Token::Eof
            ]
        );
    }

    #[test]
    fn a_broken_doctype_forces_quirks() {
        assert_eq!(
            tokens(&["<!DOCTYPE>"]),
            [
                Token::Doctype {
                    name: None,
                    public_id: None,
                    system_id: None,
                    force_quirks: true,
                },
                Token::Eof
            ]
        );
    }

    #[test]
    fn a_stray_less_than_and_a_cut_off_tag() {
        assert_eq!(
            tokens(&["a < b <3 </> c <di"]),
            [text("a < b <3  c "), Token::Eof]
        );
    }

    #[test]
    fn raw_text_ends_only_at_its_own_end_tag() {
        let mut tokenizer = Tokenizer::new();
        tokenizer.push("<style>if (a</b) {}</STYLE >x");
        tokenizer.set_eof();
        let first = tokenizer.next_token();
        assert_eq!(first, Some(start("style", &[])));
        tokenizer.switch_to(TextState::RawText);
        assert_eq!(tokenizer.next_token(), Some(text("if (a</b) {}")));
        assert_eq!(tokenizer.next_token(), Some(end("style")));
        assert_eq!(tokenizer.next_token(), Some(text("x")));
        assert_eq!(tokenizer.next_token(), Some(Token::Eof));
        assert_eq!(tokenizer.next_token(), None);
    }

    #[test]
    fn attribute_references_keep_legacy_query_strings() {
        assert_eq!(
            tokens(&["<a href='?a=1&copy=2&amp;b&not'>"]),
            [start("a", &[("href", "?a=1&copy=2&b\u{AC}")]), Token::Eof]
        );
    }

    #[test]
    fn nul_is_kept_in_data_and_replaced_elsewhere() {
        assert_eq!(
            tokens(&["a\0<p x='\0'><title>\0</title>"]),
            [
                text("a\0"),
                start("p", &[("x", "\u{FFFD}")]),
                start("title", &[]),
                text("\u{FFFD}"),
                end("title"),
                Token::Eof
            ]
        );
    }

    #[test]
    fn cdata_opens_only_when_allowed() {
        let mut tokenizer = Tokenizer::new();
        tokenizer.push("<![CDATA[a<b]]>");
        tokenizer.set_eof();
        assert_eq!(
            tokenizer.next_token(),
            Some(Token::Comment("[CDATA[a<b]]".into()))
        );

        let mut tokenizer = Tokenizer::new();
        tokenizer.set_cdata_allowed(true);
        tokenizer.push("<![CDATA[a<b]]>");
        tokenizer.set_eof();
        assert_eq!(tokenizer.next_token(), Some(text("a<b")));
        assert!(tokenizer.errors() == 0);
    }
}
