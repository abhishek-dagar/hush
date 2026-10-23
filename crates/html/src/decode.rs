//! Bytes to text: picks the encoding, decodes as bytes stream in, and
//! normalizes newlines (spec 0004, child 0002, Pipeline).
//!
//! The encoding is picked in the order AC-6 gives: the byte order mark,
//! then the header charset, then a `<meta>` prescan of the first 1024
//! bytes, then `windows-1252`. Without a usable header charset the decoder
//! holds bytes until it has 1024 of them (or the body ends) so the prescan
//! sees them all.

use encoding_rs::{Encoding, WINDOWS_1252};

use crate::prescan::{PRESCAN_BYTES, prescan};

pub(crate) struct Decoder {
    state: State,
    /// The last chunk ended in a carriage return, already turned into a
    /// line feed, so a line feed starting the next chunk is dropped.
    after_cr: bool,
}

enum State {
    /// No header charset: the bytes so far, waiting for the prescan.
    Sniffing(Vec<u8>),
    /// The encoding is settled.
    Decoding(encoding_rs::Decoder),
}

impl Decoder {
    pub(crate) fn new(hint: Option<&str>) -> Self {
        let state = match hint.and_then(|label| Encoding::for_label(label.trim().as_bytes())) {
            // Sniffs a byte order mark first, which wins over `encoding`.
            Some(encoding) => State::Decoding(encoding.new_decoder()),
            None => State::Sniffing(Vec::new()),
        };
        Self {
            state,
            after_cr: false,
        }
    }

    /// The encoding's name; `windows-1252`, the fallback, until it is settled.
    pub(crate) fn encoding_name(&self) -> &'static str {
        match &self.state {
            State::Sniffing(_) => WINDOWS_1252.name(),
            State::Decoding(decoder) => decoder.encoding().name(),
        }
    }

    /// Decodes `bytes`; `last` flushes any partial character at the end.
    /// Malformed bytes become U+FFFD.
    pub(crate) fn decode(&mut self, bytes: &[u8], last: bool) -> String {
        match &mut self.state {
            State::Decoding(decoder) => {
                let decoded = decode_with(decoder, bytes, last);
                self.preprocess(&decoded)
            }
            State::Sniffing(held) => {
                held.extend_from_slice(bytes);
                let bom = Encoding::for_bom(held).map(|(encoding, _)| encoding);
                if bom.is_none() && held.len() < PRESCAN_BYTES && !last {
                    return String::new();
                }
                let encoding = bom.or_else(|| prescan(held)).unwrap_or(WINDOWS_1252);
                let held = std::mem::take(held);
                let mut decoder = encoding.new_decoder();
                let decoded = decode_with(&mut decoder, &held, last);
                self.state = State::Decoding(decoder);
                self.preprocess(&decoded)
            }
        }
    }

    /// Turns CRLF and lone CR into LF. NUL stays: the tokenizer decides
    /// per state what it becomes.
    pub(crate) fn preprocess(&mut self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        for c in text.chars() {
            let after_cr = std::mem::take(&mut self.after_cr);
            match c {
                '\r' => {
                    out.push('\n');
                    self.after_cr = true;
                }
                '\n' if after_cr => {}
                c => out.push(c),
            }
        }
        out
    }
}

/// Decodes `bytes` with `decoder`; malformed bytes become U+FFFD.
fn decode_with(decoder: &mut encoding_rs::Decoder, bytes: &[u8], last: bool) -> String {
    let capacity = decoder
        .max_utf8_buffer_length(bytes.len())
        .unwrap_or(bytes.len().saturating_mul(3).saturating_add(4));
    let mut decoded = String::with_capacity(capacity);
    let (_, _, _) = decoder.decode_to_string(bytes, &mut decoded, last);
    decoded
}
