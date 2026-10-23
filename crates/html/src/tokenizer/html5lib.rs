//! The `html5lib-tests` tokenizer suite (spec 0004, AC-15).
//!
//! Every case in `tests/html5lib-tests/tokenizer/*.test` runs in each
//! initial state it lists, twice: with the whole input in one push, and one
//! character per push, so streaming is checked on the same cases. Tokens
//! are compared; the suite's error codes are not, since the tokenizer only
//! counts parse errors (child 0002, Tokenizer).
//!
//! Lone surrogates (only in `doubleEscaped` cases) cannot be in a Rust
//! string, and the decoder turns them into U+FFFD before the tokenizer ever
//! sees them, so both the input and the expected output map them to U+FFFD.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use super::{State, Token, Tokenizer};
use crate::decode::Decoder;

/// A token in a form both sides can build and compare.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Doctype {
        name: Option<String>,
        public_id: Option<String>,
        system_id: Option<String>,
        correct: bool,
    },
    Start {
        name: String,
        attrs: BTreeMap<String, String>,
        self_closing: bool,
    },
    End(String),
    Comment(String),
    Chars(String),
}

/// Appends `tok`, merging it into a text run before it.
fn push_merged(out: &mut Vec<Tok>, tok: Tok) {
    if let (Some(Tok::Chars(text)), Tok::Chars(more)) = (out.last_mut(), &tok) {
        text.push_str(more);
        return;
    }
    out.push(tok);
}

fn from_token(token: Token) -> Option<Tok> {
    Some(match token {
        Token::Doctype {
            name,
            public_id,
            system_id,
            force_quirks,
        } => Tok::Doctype {
            name,
            public_id,
            system_id,
            correct: !force_quirks,
        },
        Token::StartTag {
            name,
            attrs,
            self_closing,
        } => Tok::Start {
            name: name.to_string(),
            attrs: attrs
                .into_iter()
                .map(|attr| (attr.name.local.to_string(), attr.value))
                .collect(),
            self_closing,
        },
        Token::EndTag { name } => Tok::End(name.to_string()),
        Token::Comment(text) => Tok::Comment(text),
        Token::Characters(text) => Tok::Chars(text),
        Token::Eof => return None,
    })
}

/// `\uXXXX` sequences to characters, for `doubleEscaped` cases.
fn unescape(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let code_at = |i: usize| -> Option<u32> {
        if chars.get(i) != Some(&'\\') || chars.get(i + 1) != Some(&'u') {
            return None;
        }
        let hex: String = chars.get(i + 2..i + 6)?.iter().collect();
        u32::from_str_radix(&hex, 16).ok()
    };
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let Some(code) = code_at(i) else {
            out.push(chars[i]);
            i += 1;
            continue;
        };
        i += 6;
        if (0xD800..0xDC00).contains(&code)
            && let Some(low) = code_at(i).filter(|low| (0xDC00..0xE000).contains(low))
        {
            i += 6;
            let pair = 0x1_0000 + ((code - 0xD800) << 10) + (low - 0xDC00);
            out.push(char::from_u32(pair).unwrap_or('\u{FFFD}'));
            continue;
        }
        out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
    }
    out
}

fn string(value: &Value, escaped: bool) -> Option<String> {
    let text = value.as_str()?;
    Some(if escaped {
        unescape(text)
    } else {
        text.to_owned()
    })
}

fn expected_tokens(output: &[Value], escaped: bool) -> Option<Vec<Tok>> {
    let mut out = Vec::new();
    for token in output {
        let parts = token.as_array()?;
        let kind = parts.first()?.as_str()?;
        let text = |i: usize| parts.get(i).and_then(|v| string(v, escaped));
        let tok = match kind {
            "DOCTYPE" => Tok::Doctype {
                name: text(1),
                public_id: text(2),
                system_id: text(3),
                correct: parts.get(4)?.as_bool()?,
            },
            "StartTag" => Tok::Start {
                name: text(1)?,
                attrs: parts
                    .get(2)?
                    .as_object()?
                    .iter()
                    .map(|(name, value)| {
                        let name = if escaped {
                            unescape(name)
                        } else {
                            name.clone()
                        };
                        Some((name, string(value, escaped)?))
                    })
                    .collect::<Option<_>>()?,
                self_closing: parts.get(3).and_then(Value::as_bool).unwrap_or(false),
            },
            "EndTag" => Tok::End(text(1)?),
            "Comment" => Tok::Comment(text(1)?),
            "Character" => Tok::Chars(text(1)?),
            _ => return None,
        };
        push_merged(&mut out, tok);
    }
    Some(out)
}

fn initial_state(name: &str) -> Option<State> {
    Some(match name {
        "Data state" => State::Data,
        "PLAINTEXT state" => State::PlainText,
        "RCDATA state" => State::RcData,
        "RAWTEXT state" => State::RawText,
        "Script data state" => State::ScriptData,
        "CDATA section state" => State::CdataSection,
        _ => return None,
    })
}

/// Tokenizes `input` from `state`, in pieces of `chunk` characters (all at
/// once when `None`).
fn run(input: &str, state: State, last_start_tag: Option<&str>, chunk: Option<usize>) -> Vec<Tok> {
    let mut tokenizer = Tokenizer::new();
    tokenizer.state = state;
    tokenizer.last_start_tag = last_start_tag.map(str::to_owned);
    let chars: Vec<char> = input.chars().collect();
    let pieces: Vec<String> = match chunk {
        Some(size) => chars.chunks(size).map(|c| c.iter().collect()).collect(),
        None => vec![input.to_owned()],
    };
    let mut out = Vec::new();
    let mut drain = |tokenizer: &mut Tokenizer| {
        while let Some(token) = tokenizer.next_token() {
            if let Some(tok) = from_token(token) {
                push_merged(&mut out, tok);
            }
        }
    };
    for piece in &pieces {
        tokenizer.push(piece);
        drain(&mut tokenizer);
    }
    tokenizer.set_eof();
    drain(&mut tokenizer);
    out
}

#[test]
fn every_tokenizer_case_passes() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/html5lib-tests/tokenizer");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", dir.display()))
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "test"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no .test files in {}", dir.display());

    let mut runs = 0;
    let mut failures = Vec::new();
    for path in &files {
        let file = path.file_name().map(|n| n.to_string_lossy().into_owned());
        let file = file.unwrap_or_default();
        let json: Value = serde_json::from_str(
            &std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{file}: {e}")),
        )
        .unwrap_or_else(|e| panic!("{file}: {e}"));
        // `xmlViolationTests` are for an XML compatible mode browsers do
        // not have; only `tests` apply.
        let Some(cases) = json.get("tests").and_then(Value::as_array) else {
            continue;
        };
        for case in cases {
            let description = case["description"].as_str().unwrap_or("?");
            let escaped = case["doubleEscaped"].as_bool().unwrap_or(false);
            let Some(input) = string(&case["input"], escaped) else {
                failures.push(format!("{file}: {description}: bad input"));
                continue;
            };
            // The input stream is preprocessed text, as the decoder makes it.
            let input = Decoder::new(None).preprocess(&input);
            let Some(expected) = case["output"]
                .as_array()
                .and_then(|output| expected_tokens(output, escaped))
            else {
                failures.push(format!("{file}: {description}: bad output"));
                continue;
            };
            let states: Vec<&str> = match case["initialStates"].as_array() {
                Some(states) => states.iter().filter_map(Value::as_str).collect(),
                None => vec!["Data state"],
            };
            let last_start_tag = case["lastStartTag"].as_str();
            for name in states {
                let Some(state) = initial_state(name) else {
                    failures.push(format!("{file}: {description}: unknown state {name}"));
                    continue;
                };
                for chunk in [None, Some(1)] {
                    runs += 1;
                    let got = run(&input, state, last_start_tag, chunk);
                    if got != expected {
                        let how = if chunk.is_some() {
                            "char by char"
                        } else {
                            "whole"
                        };
                        failures.push(format!(
                            "{file}: {description} ({name}, {how}):\n  input    {input:?}\n  expected {expected:?}\n  got      {got:?}"
                        ));
                    }
                }
            }
        }
    }
    assert!(runs > 10_000, "only {runs} runs; is the suite vendored?");
    assert!(
        failures.is_empty(),
        "{} of {runs} runs failed:\n{}",
        failures.len(),
        failures
            .iter()
            .take(40)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
