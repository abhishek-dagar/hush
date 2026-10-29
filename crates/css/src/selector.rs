//! Selectors: parsing, specificity and right to left matching
//! (spec 0004, child 0003, Selectors, AC-19).
//!
//! The supported set is CSS2 era: type, universal, class, id, attribute
//! selectors with every operator, the four combinators, `:link`,
//! `:visited` (never matches), `:first-child`, `:last-child`, `:root` and
//! `:not()` of one compound, plus what the chrome CSS profile needs (spec
//! 0006, AC-2): the state pseudo classes `:hover`, `:active`, `:focus`,
//! `:focus-visible`, `:disabled` and `:enabled`, and the `::placeholder`
//! pseudo element on the subject compound. Anything else fails the parse,
//! which drops the whole rule.

use base::atoms::{Atom, LocalName, QualName};
use base::{local_name, ns};
use cssparser::{Parser, Token};

use crate::values::{ParseResult, invalid};
use crate::{MatchElement, PseudoState};

/// The most compounds one selector may have. Matching recurses once per
/// compound, so a longer selector is dropped like an unsupported one; real
/// sheets stay far below it.
const MAX_COMPOUNDS: usize = 64;

/// How many compound tests one match may make. Descendant combinators
/// backtrack, so a hostile `a a a … b` over a deep tree could otherwise
/// take exponential time; past the budget the selector does not match.
const MATCH_BUDGET: u32 = 64 * 1024;

/// How two compounds relate, read right to left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Combinator {
    /// Whitespace: any ancestor.
    Descendant,
    /// `>`: the parent.
    Child,
    /// `+`: the previous element sibling.
    NextSibling,
    /// `~`: any earlier element sibling.
    LaterSibling,
}

impl Combinator {
    /// Whether it relates siblings rather than ancestors.
    pub(crate) fn is_sibling(self) -> bool {
        matches!(self, Self::NextSibling | Self::LaterSibling)
    }
}

/// How an attribute selector compares the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttrOp {
    /// `=`.
    Equals,
    /// `~=`: one of the whitespace separated words.
    Includes,
    /// `|=`: equal, or starting with the value and a `-`.
    DashMatch,
    /// `^=`.
    Prefix,
    /// `$=`.
    Suffix,
    /// `*=`.
    Substring,
}

/// One simple selector.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Simple {
    /// A type selector. `lower` matches HTML elements case insensitively;
    /// `exact` is the name as written, for other namespaces.
    Type { lower: LocalName, exact: LocalName },
    /// `#id`.
    Id(Atom),
    /// `.class`.
    Class(Atom),
    /// `[name]`, or `[name op value]` with the `i` flag in `ignore_case`.
    Attribute {
        name: LocalName,
        check: Option<(AttrOp, String, bool)>,
    },
    /// `:link`: an `a` or `area` with an `href`.
    Link,
    /// `:visited`: never matches, so history cannot leak.
    Visited,
    /// `:first-child`.
    FirstChild,
    /// `:last-child`.
    LastChild,
    /// `:root`.
    Root,
    /// A pseudo class that matches an element state, such as `:hover`.
    State(PseudoState),
    /// `:enabled`: a form control that is not disabled (spec 0006, AC-16).
    Enabled,
    /// `:not(<selector list>)` (spec 0010, AC-14).
    Not(Vec<Selector>),
    /// `:is(<selector list>)` (spec 0010, AC-14).
    Is(Vec<Selector>),
    /// `:where(<selector list>)` (spec 0010, AC-14).
    Where(Vec<Selector>),
    /// `:has(<relative selector list>)` (spec 0010, AC-15).
    Has(Vec<RelativeSelector>),
}

/// A relative selector used inside `:has()`: an optional leading combinator
/// (`>`, `+`, `~`, or nothing for descendant) followed by a complex selector
/// whose subject compound is matched against the related element (spec 0010,
/// AC-15).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RelativeSelector {
    /// `None` means descendant; otherwise the combinator that relates the
    /// matched element back to the `:has()` subject.
    pub(crate) combinator: Option<Combinator>,
    /// The selector matched against the related element, walking up through
    /// its own combinators toward (but not past) the subject.
    pub(crate) selector: Selector,
}

/// Simple selectors that must all match one element. Empty is `*`.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Compound(pub(crate) Vec<Simple>);

/// A pseudo element a selector styles instead of the element itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PseudoElement {
    /// `::placeholder`: an `<input>`'s placeholder text (spec 0006, AC-2).
    Placeholder,
}

/// One complex selector, stored right to left: `compounds[0]` is the
/// subject, and `combinators[i]` joins `compounds[i]` to `compounds[i + 1]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Selector {
    pub(crate) compounds: Vec<Compound>,
    pub(crate) combinators: Vec<Combinator>,
    pub(crate) specificity: u32,
    /// The pseudo element after the subject compound, if any. Such a
    /// selector matches the element but styles only that pseudo element.
    pub(crate) pseudo_element: Option<PseudoElement>,
}

impl Selector {
    /// The packed specificity: ids, then classes, attributes and pseudo
    /// classes, then types, ten bits each.
    pub fn specificity(&self) -> u32 {
        self.specificity
    }

    /// The rightmost compound, which buckets the selector.
    pub(crate) fn subject(&self) -> &Compound {
        // A selector is never built without a compound.
        &self.compounds[0]
    }

    /// Whether matching this selector reads the element's siblings.
    pub(crate) fn reads_siblings(&self) -> bool {
        self.compounds.iter().any(Compound::reads_siblings)
    }
}

/// Parses a selector list. Any invalid or unsupported selector fails the
/// whole list.
/// Parses a selector list from text, as `querySelector` takes it (spec
/// 0004, child 0006). `None` when it does not parse or uses anything the
/// engine does not support, which scripts see as a `SyntaxError`.
pub fn parse_selectors(text: &str) -> Option<Vec<Selector>> {
    let mut parser = Parser::new(text);
    parser
        .parse_entirely(|input| parse_selector_list(input, false, false))
        .ok()
}

pub(crate) fn parse_selector_list(
    input: &mut Parser<'_>,
    in_not: bool,
    in_has: bool,
) -> ParseResult<Vec<Selector>> {
    input.parse_comma_separated(|input| parse_selector(input, in_not, in_has))
}

/// Parses a comma separated selector list forgivingly (spec 0010, AC-14):
/// each argument that fails to parse is dropped instead of failing the whole
/// list, used by `:is()` and `:where()`. An empty result matches nothing.
pub(crate) fn parse_selector_list_forgiving(
    input: &mut Parser<'_>,
    in_not: bool,
    in_has: bool,
) -> ParseResult<Vec<Selector>> {
    let list =
        input.parse_comma_separated_ignoring_errors(|input| parse_selector(input, in_not, in_has));
    Ok(list)
}

/// Parses a comma separated list of relative selectors, each an optional
/// leading combinator (`>`, `+`, `~`, or nothing for descendant) followed by
/// a complex selector (spec 0010, AC-15).
///
/// Parsed manually rather than with `parse_comma_separated` because the
/// closing `)` of the `:has()` block must be left for `parse_nested_block` to
/// consume; `parse_comma_separated` would reject it as trailing input.
pub(crate) fn parse_relative_selector_list(
    input: &mut Parser<'_>,
) -> ParseResult<Vec<RelativeSelector>> {
    let mut list = Vec::new();
    loop {
        input.skip_whitespace();
        let relative = parse_relative_selector(input)?;
        list.push(relative);
        input.skip_whitespace();
        let state = input.state();
        if matches!(input.next_including_whitespace(), Ok(Token::Comma)) {
            continue;
        }
        input.reset(&state);
        break;
    }
    Ok(list)
}

/// Parses one relative selector: an optional leading combinator then a
/// complex selector (spec 0010, AC-15).
fn parse_relative_selector(input: &mut Parser<'_>) -> ParseResult<RelativeSelector> {
    let state = input.state();
    let combinator = match input.next() {
        Ok(Token::Delim('>')) => Some(Combinator::Child),
        Ok(Token::Delim('+')) => Some(Combinator::NextSibling),
        Ok(Token::Delim('~')) => Some(Combinator::LaterSibling),
        Ok(_) => {
            input.reset(&state);
            None
        }
        Err(_) => return invalid(),
    };
    input.skip_whitespace();
    // Parse the complex selector: a subject compound, then zero or more
    // (combinator, compound) pairs. Pseudo elements are not allowed inside
    // `:has()` (spec 0010, AC-15).
    let mut compounds = Vec::new();
    let mut inner_combinators = Vec::new();
    loop {
        let (compound, pseudo_element) = parse_compound(input, false, true)?;
        if pseudo_element.is_some() {
            return invalid();
        }
        compounds.push(compound);
        let state = input.state();
        let next_combinator = match input.next_including_whitespace() {
            Ok(Token::WhiteSpace(_)) => Some(Combinator::Descendant),
            Ok(Token::Delim('>')) => Some(Combinator::Child),
            Ok(Token::Delim('+')) => Some(Combinator::NextSibling),
            Ok(Token::Delim('~')) => Some(Combinator::LaterSibling),
            Ok(_) => {
                input.reset(&state);
                None
            }
            Err(_) => None,
        };
        match next_combinator {
            Some(combinator) => {
                input.skip_whitespace();
                inner_combinators.push(combinator);
            }
            None => break,
        }
    }
    compounds.reverse();
    inner_combinators.reverse();
    let specificity = compounds
        .iter()
        .map(Compound::specificity)
        .fold(0, pack_add);
    let selector = Selector {
        compounds,
        combinators: inner_combinators,
        specificity,
        pseudo_element: None,
    };
    Ok(RelativeSelector {
        combinator,
        selector,
    })
}

fn parse_selector(input: &mut Parser<'_>, in_not: bool, in_has: bool) -> ParseResult<Selector> {
    let (first, mut pseudo_element) = parse_compound(input, in_not, in_has)?;
    let mut compounds = vec![first];
    let mut combinators = Vec::new();
    loop {
        let mut whitespace = false;
        let combinator = loop {
            let state = input.state();
            match input.next_including_whitespace() {
                Err(_) => break None,
                Ok(Token::WhiteSpace(_)) => whitespace = true,
                Ok(Token::Delim('>')) => break Some(Combinator::Child),
                Ok(Token::Delim('+')) => break Some(Combinator::NextSibling),
                Ok(Token::Delim('~')) => break Some(Combinator::LaterSibling),
                Ok(_) => {
                    input.reset(&state);
                    break whitespace.then_some(Combinator::Descendant);
                }
            }
        };
        let Some(combinator) = combinator else {
            if whitespace || input.is_exhausted() {
                break;
            }
            return invalid();
        };
        // A pseudo element ends the selector: it belongs on the subject.
        if pseudo_element.is_some() {
            return invalid();
        }
        input.skip_whitespace();
        combinators.push(combinator);
        let (compound, pseudo) = parse_compound(input, false, false)?;
        compounds.push(compound);
        pseudo_element = pseudo;
        if compounds.len() > MAX_COMPOUNDS {
            return invalid();
        }
    }
    compounds.reverse();
    combinators.reverse();
    // A pseudo element counts like a type selector.
    let specificity = compounds
        .iter()
        .map(Compound::specificity)
        .fold(u32::from(pseudo_element.is_some()), pack_add);
    Ok(Selector {
        compounds,
        combinators,
        specificity,
        pseudo_element,
    })
}

/// Parses one compound, and the pseudo element that may close it.
fn parse_compound(
    input: &mut Parser<'_>,
    in_not: bool,
    in_has: bool,
) -> ParseResult<(Compound, Option<PseudoElement>)> {
    let mut simple = Vec::new();
    let mut any = false;
    let state = input.state();
    match input.next_including_whitespace() {
        Ok(Token::Ident(name)) => {
            let exact = LocalName::from(&**name);
            let lower = LocalName::from(name.to_ascii_lowercase());
            simple.push(Simple::Type { lower, exact });
            any = true;
        }
        Ok(Token::Delim('*')) => any = true,
        _ => input.reset(&state),
    }
    // Namespace prefixes (`svg|rect`, `*|*`) are not supported.
    let state = input.state();
    if let Ok(Token::Delim('|')) = input.next_including_whitespace() {
        return invalid();
    }
    input.reset(&state);

    let mut pseudo_element = None;
    loop {
        let state = input.state();
        let token = match input.next_including_whitespace() {
            Ok(token) => token.clone(),
            Err(_) => break,
        };
        // Nothing may follow a pseudo element in its compound.
        if pseudo_element.is_some() {
            input.reset(&state);
            break;
        }
        match token {
            Token::IDHash(id) => simple.push(Simple::Id(Atom::from(&*id))),
            Token::Delim('.') => match input.next_including_whitespace()? {
                Token::Ident(class) => simple.push(Simple::Class(Atom::from(&**class))),
                _ => return invalid(),
            },
            Token::SquareBracketBlock => {
                simple.push(input.parse_nested_block(parse_attribute)?);
            }
            Token::Colon => {
                let pseudo = match input.next_including_whitespace()?.clone() {
                    Token::Colon if !in_not && !in_has => {
                        match input.next_including_whitespace()?.clone() {
                            Token::Ident(name) if name.eq_ignore_ascii_case("placeholder") => {
                                pseudo_element = Some(PseudoElement::Placeholder);
                                continue;
                            }
                            // `::before`, `::after`, `::marker` and the rest.
                            _ => return invalid(),
                        }
                    }
                    Token::Ident(name) => match name.to_ascii_lowercase().as_str() {
                        "link" | "any-link" => Simple::Link,
                        "visited" => Simple::Visited,
                        "first-child" => Simple::FirstChild,
                        "last-child" => Simple::LastChild,
                        "root" => Simple::Root,
                        "hover" => Simple::State(PseudoState::Hover),
                        "active" => Simple::State(PseudoState::Active),
                        "focus" => Simple::State(PseudoState::Focus),
                        "focus-visible" => Simple::State(PseudoState::FocusVisible),
                        "disabled" => Simple::State(PseudoState::Disabled),
                        "enabled" => Simple::Enabled,
                        // `:before` and friends are pseudo elements, which
                        // slice 1 drops; every other pseudo class is unsupported.
                        _ => return invalid(),
                    },
                    Token::Function(name) => match name.to_ascii_lowercase().as_str() {
                        "not" if !in_not => {
                            // `:not()` is strict: an invalid argument fails the whole
                            // selector, and combinators are not allowed inside (AC-14).
                            let list = input.parse_nested_block(|input| {
                                parse_selector_list(input, true, false)
                            })?;
                            for sel in &list {
                                if !sel.combinators.is_empty() || sel.compounds.len() != 1 {
                                    return invalid();
                                }
                            }
                            Simple::Not(list)
                        }
                        "is" | "where" => {
                            // `:is()`/`:where()` are forgiving: an invalid argument is
                            // dropped instead of failing the whole list (AC-14).
                            let list = input.parse_nested_block(|input| {
                                parse_selector_list_forgiving(input, false, false)
                            })?;
                            match name.to_ascii_lowercase().as_str() {
                                "is" => Simple::Is(list),
                                "where" => Simple::Where(list),
                                _ => unreachable!(),
                            }
                        }
                        "has" if !in_has => {
                            // `:has()` is strict: an invalid argument fails the whole
                            // selector, and nested `:has()` is invalid (AC-15).
                            let list = input
                                .parse_nested_block(|input| parse_relative_selector_list(input))?;
                            Simple::Has(list)
                        }
                        _ => return invalid(),
                    },
                    _ => return invalid(),
                };
                simple.push(pseudo);
            }
            _ => {
                input.reset(&state);
                break;
            }
        }
    }
    if simple.is_empty() && !any && pseudo_element.is_none() {
        return invalid();
    }
    Ok((Compound(simple), pseudo_element))
}

/// The inside of `[...]`.
fn parse_attribute(input: &mut Parser<'_>) -> ParseResult<Simple> {
    let name = LocalName::from(input.expect_ident()?.to_ascii_lowercase());
    if input.is_exhausted() {
        return Ok(Simple::Attribute { name, check: None });
    }
    let op = match *input.next()? {
        Token::Delim('=') => AttrOp::Equals,
        Token::IncludeMatch => AttrOp::Includes,
        Token::DashMatch => AttrOp::DashMatch,
        Token::PrefixMatch => AttrOp::Prefix,
        Token::SuffixMatch => AttrOp::Suffix,
        Token::SubstringMatch => AttrOp::Substring,
        _ => return invalid(),
    };
    let value = input.expect_ident_or_string()?.to_string();
    let ignore_case = match input.try_parse(|i| i.expect_ident_cloned()) {
        Ok(flag) if flag.eq_ignore_ascii_case("i") => true,
        Ok(flag) if flag.eq_ignore_ascii_case("s") => false,
        Ok(_) => return invalid(),
        Err(_) => false,
    };
    input.expect_exhausted()?;
    Ok(Simple::Attribute {
        name,
        check: Some((op, value, ignore_case)),
    })
}

/// Adds two packed specificities, each part saturating at 1023.
fn pack_add(a: u32, b: u32) -> u32 {
    let part = |v: u32, shift: u32| (v >> shift) & 0x3ff;
    let sum = |shift| (part(a, shift) + part(b, shift)).min(0x3ff) << shift;
    sum(20) | sum(10) | sum(0)
}

impl Compound {
    fn specificity(&self) -> u32 {
        self.0
            .iter()
            .map(|simple| match simple {
                Simple::Id(_) => 1 << 20,
                Simple::Type { .. } => 1,
                Simple::Not(list) | Simple::Is(list) => {
                    list.iter().map(Selector::specificity).fold(0, u32::max)
                }
                Simple::Has(list) => list
                    .iter()
                    .map(|relative| relative.selector.specificity())
                    .fold(0, u32::max),
                Simple::Where(_) => 0,
                _ => 1 << 10,
            })
            .fold(0, pack_add)
    }

    /// Whether matching it reads the element's siblings.
    fn reads_siblings(&self) -> bool {
        self.0.iter().any(|simple| match simple {
            Simple::FirstChild | Simple::LastChild => true,
            Simple::Not(list) | Simple::Is(list) | Simple::Where(list) => {
                list.iter().any(Selector::reads_siblings)
            }
            Simple::Has(list) => list.iter().any(|relative| {
                relative.selector.reads_siblings()
                    || relative.combinator.is_some_and(Combinator::is_sibling)
            }),
            _ => false,
        })
    }

    fn matches<E: MatchElement + Clone>(&self, element: &E) -> bool {
        self.0.iter().all(|simple| simple.matches(element))
    }
}

impl Simple {
    pub(crate) fn matches<E: MatchElement + Clone>(&self, element: &E) -> bool {
        let html = ns!("http://www.w3.org/1999/xhtml");
        match self {
            Self::Type { lower, exact } => {
                let name = element.name();
                if name.ns == html {
                    name.local == *lower
                } else {
                    name.local == *exact
                }
            }
            Self::Id(id) => element.id() == Some(id),
            Self::Class(class) => element.classes().contains(class),
            Self::Attribute { name, check } => {
                let Some(value) = element.attribute(&QualName::attribute(name.clone())) else {
                    return false;
                };
                match check {
                    None => true,
                    Some((op, expected, ignore_case)) => {
                        attribute_matches(*op, value, expected, *ignore_case)
                    }
                }
            }
            Self::Link => {
                let name = element.name();
                name.ns == html
                    && matches!(&*name.local, "a" | "area")
                    && element
                        .attribute(&QualName::attribute(local_name!("href")))
                        .is_some()
            }
            Self::Visited => false,
            Self::FirstChild => element.prev_sibling_element().is_none(),
            Self::LastChild => element.next_sibling_element().is_none(),
            Self::Root => element.is_root(),
            Self::State(state) => element.is_in_state(*state),
            Self::Enabled => {
                is_form_control(element.name()) && !element.is_in_state(PseudoState::Disabled)
            }
            Self::Not(list) => !list
                .iter()
                .any(|s| s.compounds.iter().all(|c| c.matches(element))),
            Self::Is(list) => list
                .iter()
                .any(|s| s.compounds.iter().all(|c| c.matches(element))),
            Self::Where(list) => list
                .iter()
                .any(|s| s.compounds.iter().all(|c| c.matches(element))),
            Self::Has(list) => list.iter().any(|relative| {
                let mut context = MatchContext::default();
                relative.matches_subject(element, &mut context)
            }),
        }
    }
}

/// Whether `name` is a control `:disabled` and `:enabled` apply to (spec
/// 0006, AC-16).
fn is_form_control(name: &QualName) -> bool {
    name.ns == ns!("http://www.w3.org/1999/xhtml")
        && matches!(&*name.local, "button" | "input" | "select" | "textarea")
}

fn attribute_matches(op: AttrOp, value: &str, expected: &str, ignore_case: bool) -> bool {
    let (value, expected) = if ignore_case {
        (value.to_ascii_lowercase(), expected.to_ascii_lowercase())
    } else {
        (value.to_owned(), expected.to_owned())
    };
    match op {
        AttrOp::Equals => value == expected,
        AttrOp::Includes => {
            !expected.is_empty()
                && !expected.contains(char::is_whitespace)
                && value.split_ascii_whitespace().any(|word| word == expected)
        }
        AttrOp::DashMatch => {
            value == expected
                || value
                    .strip_prefix(&expected)
                    .is_some_and(|rest| rest.starts_with('-'))
        }
        AttrOp::Prefix => !expected.is_empty() && value.starts_with(&expected),
        AttrOp::Suffix => !expected.is_empty() && value.ends_with(&expected),
        AttrOp::Substring => !expected.is_empty() && value.contains(&expected),
    }
}

/// What matching learned beyond the answer.
pub struct MatchContext<E> {
    /// Elements a sibling dependent part of a selector (`+`, `~`,
    /// `:first-child`, `:last-child`) was tested against. The caller sets
    /// `AFFECTED_BY_SIBLING_SELECTORS` on them (spec 0004, AC-21).
    pub sibling_dependent: Vec<E>,
}

impl<E> Default for MatchContext<E> {
    fn default() -> Self {
        Self {
            sibling_dependent: Vec::new(),
        }
    }
}

impl Selector {
    /// The pseudo element it styles, or `None` for the element itself.
    pub fn pseudo_element(&self) -> Option<PseudoElement> {
        self.pseudo_element
    }

    /// Whether the selector matches `element` itself. A selector for a
    /// pseudo element never does, as `querySelector` requires.
    pub fn matches<E: MatchElement + Clone>(
        &self,
        element: &E,
        context: &mut MatchContext<E>,
    ) -> bool {
        self.pseudo_element.is_none() && self.matches_compounds(element, context)
    }

    /// Whether the compounds match `element`, whatever the pseudo element:
    /// the stylist picks the pseudo element's rules itself.
    pub(crate) fn matches_compounds<E: MatchElement + Clone>(
        &self,
        element: &E,
        context: &mut MatchContext<E>,
    ) -> bool {
        let mut budget = MATCH_BUDGET;
        self.matches_from(0, element, context, &mut budget)
    }

    fn matches_from<E: MatchElement + Clone>(
        &self,
        index: usize,
        element: &E,
        context: &mut MatchContext<E>,
        budget: &mut u32,
    ) -> bool {
        let Some(left) = budget.checked_sub(1) else {
            return false;
        };
        *budget = left;
        let compound = &self.compounds[index];
        let next = self.combinators.get(index).copied();
        if compound.reads_siblings() || next.is_some_and(Combinator::is_sibling) {
            context.sibling_dependent.push(element.clone());
        }
        if !compound.matches(element) {
            return false;
        }
        let Some(combinator) = next else {
            return true;
        };
        let step = |e: &E| match combinator {
            Combinator::Descendant | Combinator::Child => e.parent_element(),
            Combinator::NextSibling | Combinator::LaterSibling => e.prev_sibling_element(),
        };
        let mut candidate = step(element);
        while let Some(current) = candidate {
            if self.matches_from(index + 1, &current, context, budget) {
                return true;
            }
            if matches!(combinator, Combinator::Child | Combinator::NextSibling) {
                return false;
            }
            candidate = step(&current);
        }
        false
    }
}

impl RelativeSelector {
    /// Whether the element related to `subject` by this relative selector
    /// exists (descendant, child, next sibling, or later siblings), matching
    /// the inner selector against it per spec 0010, AC-15.
    fn matches_subject<E: MatchElement + Clone>(
        &self,
        subject: &E,
        context: &mut MatchContext<E>,
    ) -> bool {
        let candidates: Vec<E> = match self.combinator {
            None | Some(Combinator::Descendant) => {
                // Descendant: every element in subject's subtree, in tree order.
                // The walk stays inside subject's subtree: it only follows
                // first-child / next-sibling links downward, so a following
                // sibling of the subject (or of an ancestor) is never seen.
                let mut out = Vec::new();
                let mut stack: Vec<E> = Vec::new();
                if let Some(child) = subject.first_child_element() {
                    stack.push(child);
                }
                while let Some(node) = stack.pop() {
                    out.push(node.clone());
                    // Collect children so they are visited in tree order.
                    let mut children: Vec<E> = Vec::new();
                    let mut child = node.first_child_element();
                    while let Some(ch) = child {
                        children.push(ch.clone());
                        child = ch.next_sibling_element();
                    }
                    for ch in children.into_iter().rev() {
                        stack.push(ch);
                    }
                }
                out
            }
            Some(Combinator::Child) => subject.first_child_element().into_iter().collect(),
            Some(Combinator::NextSibling) => subject.next_sibling_element().into_iter().collect(),
            Some(Combinator::LaterSibling) => {
                let mut out = Vec::new();
                let mut candidate = subject.next_sibling_element();
                while let Some(sibling) = candidate {
                    out.push(sibling.clone());
                    candidate = sibling.next_sibling_element();
                }
                out
            }
        };
        candidates
            .into_iter()
            .any(|candidate| self.selector.matches_compounds(&candidate, context))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn parse(text: &str) -> Option<Vec<Selector>> {
        let mut parser = Parser::new(text);
        parser
            .parse_entirely(|input| parse_selector_list(input, false, false))
            .ok()
    }

    #[test]
    fn specificity_counts_ids_classes_and_types() {
        let spec = |text: &str| parse(text).unwrap()[0].specificity();
        assert_eq!(spec("*"), 0);
        assert_eq!(spec("li"), 1);
        assert_eq!(spec("ul li"), 2);
        assert_eq!(spec("ul ol+li"), 3);
        assert_eq!(spec("h1 + *[rel=up]"), (1 << 10) + 1);
        assert_eq!(spec("ul ol li.red"), (1 << 10) + 3);
        assert_eq!(spec("li.red.level"), (2 << 10) + 1);
        assert_eq!(spec("#x34y"), 1 << 20);
        assert_eq!(spec("#s12:not(foo)"), (1 << 20) + 1);
        assert_eq!(spec("a:link"), (1 << 10) + 1);
        assert_eq!(spec("button:hover"), (1 << 10) + 1);
        assert_eq!(spec("input::placeholder"), 2);
        assert_eq!(spec(".tab:focus-visible"), 2 << 10);
    }

    #[test]
    fn is_where_not_specificity() {
        // `:is()` and `:not()` take the specificity of their most specific argument.
        let spec = |text: &str| parse(text).unwrap()[0].specificity();
        assert_eq!(spec("div:is(.foo, #bar)"), (1 << 20) + 1);
        assert_eq!(spec("div:is(.foo, .bar)"), (1 << 10) + 1);
        assert_eq!(spec("div:not(.foo)"), (1 << 10) + 1);
        // `:where()` adds zero specificity.
        assert_eq!(spec("div:where(.foo, #bar)"), 1);
        assert_eq!(spec("div:where(#bar)"), 1);

        // `:not()` is strict: an invalid argument fails the whole selector.
        assert!(parse("div:not(:not-a-real-pseudo)").is_none());
        assert!(parse("div:not(a b)").is_none());
        // Nested `:not()` inside `:not()` is invalid.
        assert!(parse("div:not(:not(.foo))").is_none());
        // `:not()` inside `:is()` is allowed.
        assert!(parse("div:is(:not(.foo))").is_some());
        // Combinators are not allowed inside `:not()`.
        assert!(parse("div:not(.foo .bar)").is_none());
    }

    #[test]
    fn is_where_are_forgiving() {
        // AC-14: an invalid argument inside :is()/:where() is dropped, not the whole list.
        assert!(parse("div:is(#t, :bogus())").is_some());
        let kept = parse("div:is(#t, :bogus())").unwrap();
        assert_eq!(kept[0].specificity(), (1 << 20) + 1);
        assert!(parse("div:where(.foo, :not-a-real-pseudo)").is_some());
    }

    #[test]
    fn has_specificity_and_restrictions() {
        // `:has()` takes the specificity of its most specific argument.
        let spec = |text: &str| parse(text).unwrap()[0].specificity();
        assert_eq!(spec("div:has(.foo, #bar)"), (1 << 20) + 1);
        assert_eq!(spec("div:has(.foo, .bar)"), (1 << 10) + 1);

        // Nested `:has()` inside `:has()` is invalid.
        assert!(parse("div:has(:has(.foo))").is_none());
        // `:has()` inside `:not()` is allowed (no combinators inside :not()).
        assert!(parse("div:not(:has(.foo))").is_some());
        // `:not()` inside `:has()` is allowed.
        assert!(parse("div:has(:not(.foo))").is_some());
    }

    #[test]
    fn placeholder_sits_on_the_subject_only() {
        // Spec 0006, AC-2
        let selector = &parse("input:focus::placeholder").unwrap()[0];
        assert_eq!(selector.pseudo_element(), Some(PseudoElement::Placeholder));
        assert_eq!(parse("input").unwrap()[0].pseudo_element(), None);
        assert!(parse("::placeholder").is_some());
        for bad in [
            "input::placeholder span",
            "input::placeholder > b",
            "input::placeholder:hover",
            ":not(::placeholder)",
            "input::before",
        ] {
            assert!(parse(bad).is_none(), "{bad} should be dropped");
        }
    }

    #[test]
    fn unsupported_selectors_fail_the_whole_list() {
        // AC-19
        for good in [
            "a",
            "*",
            "div > p",
            "h1 + p",
            "h1 ~ p",
            "ul  li",
            "[lang|=en]",
            "[href^='http' i]",
            "a:link, a:visited",
            ":not(.x)",
            ":root",
            "p:first-child:last-child",
            "a:hover",
            ".row:hover .label",
            ":not(:hover)",
            "button:active",
            "a:focus, a:focus-visible",
            "button:disabled + .x",
            ":not(:enabled)",
            "input::placeholder",
        ] {
            assert!(parse(good).is_some(), "{good} should parse");
        }
        for bad in [
            "p::before",
            "p:before",
            "li:nth-child(2)",
            "svg|rect",
            "a, b:checked",
            ":not(:not(a))",
            ":not(a b)",
            "div >",
            "#1x",
            "",
        ] {
            assert!(parse(bad).is_none(), "{bad} should be dropped");
        }
        let long = vec!["a"; MAX_COMPOUNDS + 1].join(" ");
        assert!(parse(&long).is_none(), "an overlong selector is dropped");
        assert!(parse(&vec!["a"; MAX_COMPOUNDS].join(" ")).is_some());
    }

    #[test]
    fn has_parsing_and_restrictions() {
        // Nested `:has()` inside `:has()` is invalid.
        assert!(parse("div:has(:has(.foo))").is_none());
        // `:has()` inside `:not()` is allowed.
        assert!(parse("div:not(:has(.foo))").is_some());
        // `:not()` inside `:has()` is allowed.
        assert!(parse("div:has(:not(.foo))").is_some());
        // `:has(.foo .bar)` is valid — descendant combinator inside `:has()`.
        assert!(parse("div:has(.foo .bar)").is_some());
    }

    #[test]
    fn has_relative_combinators_match_subject() {
        // AC-15: :has() with child, next-sibling and later-sibling combinators.
        use std::rc::Rc;

        use base::atoms::{Atom, QualName};
        use base::local_name;

        use super::{MatchElement, PseudoState};

        #[derive(Clone, Default)]
        struct Node {
            id: Option<Atom>,
            classes: Vec<Atom>,
            first_child: Option<Rc<Node>>,
            next_sibling: Option<Rc<Node>>,
        }

        impl Node {
            fn elem(id: &str) -> Rc<Self> {
                Rc::new(Self {
                    id: Some(Atom::from(id)),
                    ..Self::default()
                })
            }
            fn with_class(mut self: Rc<Self>, class: &str) -> Rc<Self> {
                Rc::get_mut(&mut self)
                    .unwrap()
                    .classes
                    .push(Atom::from(class));
                self
            }
        }

        impl MatchElement for Rc<Node> {
            fn name(&self) -> &QualName {
                static DIV: std::sync::LazyLock<QualName> =
                    std::sync::LazyLock::new(|| QualName::html(local_name!("div")));
                &DIV
            }
            fn id(&self) -> Option<&Atom> {
                self.id.as_ref()
            }
            fn classes(&self) -> &[Atom] {
                &self.classes
            }
            fn attribute(&self, _name: &QualName) -> Option<&str> {
                None
            }
            fn parent_element(&self) -> Option<Self> {
                None
            }
            fn first_child_element(&self) -> Option<Self> {
                self.first_child.as_ref().map(Rc::clone)
            }
            fn prev_sibling_element(&self) -> Option<Self> {
                None
            }
            fn next_sibling_element(&self) -> Option<Self> {
                self.next_sibling.as_ref().map(Rc::clone)
            }
            fn is_root(&self) -> bool {
                false
            }
            fn same_attributes(&self, _other: &Self) -> bool {
                true
            }
            fn same_state(&self, _other: &Self) -> bool {
                true
            }
            fn is_in_state(&self, _state: PseudoState) -> bool {
                false
            }
            fn has_shadow_root(&self) -> bool {
                false
            }
            fn is_affected_by_sibling_selectors(&self) -> bool {
                false
            }
        }

        // deep is a child of t; t is a child of h. Set child links while each
        // node is still unique, so no Rc is cloned before its own mutation.
        let deep = Node::with_class(Node::elem("deep"), "deep");
        let mut t = Node::elem("t");
        Rc::get_mut(&mut t).unwrap().first_child = Some(Rc::clone(&deep));
        let mut h = Node::elem("h");
        Rc::get_mut(&mut h).unwrap().first_child = Some(Rc::clone(&t));
        // div#g followed by span.sib
        let mut g = Node::elem("g");
        let sib = Node::with_class(Node::elem("sib"), "sib");
        Rc::get_mut(&mut g).unwrap().next_sibling = Some(Rc::clone(&sib));
        // div#a followed by span.far
        let mut a = Node::elem("a");
        let far = Node::with_class(Node::elem("far"), "far");
        Rc::get_mut(&mut a).unwrap().next_sibling = Some(Rc::clone(&far));

        // #h:has(> #t) matches #h because #h has a child #t.
        let sel = &parse("#h:has(> #t)").unwrap()[0];
        assert!(
            sel.matches_compounds(&h, &mut Default::default()),
            "child combinator :has(> #t)"
        );
        // #g:has(+ .sib) matches #g because #g's next sibling is .sib.
        let sel = &parse("#g:has(+ .sib)").unwrap()[0];
        assert!(
            sel.matches_compounds(&g, &mut Default::default()),
            "next sibling :has(+ .sib)"
        );
        // #a:has(~ .far) matches #a because #a has a later sibling .far.
        let sel = &parse("#a:has(~ .far)").unwrap()[0];
        assert!(
            sel.matches_compounds(&a, &mut Default::default()),
            "later sibling :has(~ .far)"
        );
        // A non-subject must not match.
        let sel = &parse("#h:has(> #t)").unwrap()[0];
        assert!(
            !sel.matches_compounds(&g, &mut Default::default()),
            "#g is not a :has(> #t) subject"
        );
        // Descendant form still works: #h:has(.deep) where #t has a child .deep.
        let sel = &parse("#h:has(.deep)").unwrap()[0];
        assert!(
            sel.matches_compounds(&h, &mut Default::default()),
            "descendant :has(.deep)"
        );
    }
}
