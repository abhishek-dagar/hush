//! The WHATWG "prescan a byte stream to determine its encoding" algorithm:
//! finds a `<meta charset>` or `<meta http-equiv=content-type>` in the first
//! bytes of a document (spec 0004, child 0001, Encoding, AC-6).
//!
//! Written to follow the standard's steps one by one, so the step names in
//! the comments match the standard's.

use encoding_rs::{Encoding, UTF_8, UTF_16BE, UTF_16LE, WINDOWS_1252, X_USER_DEFINED};

/// How many bytes the prescan looks at.
pub(crate) const PRESCAN_BYTES: usize = 1024;

/// The encoding a `<meta>` in `bytes` declares, if any.
pub(crate) fn prescan(bytes: &[u8]) -> Option<&'static Encoding> {
    let bytes = bytes.get(..PRESCAN_BYTES).unwrap_or(bytes);
    let mut pos = 0;
    while pos < bytes.len() {
        let rest = bytes.get(pos..).unwrap_or_default();
        if rest.starts_with(b"<!--") {
            // The `-->` may share its dashes with the `<!--`.
            pos = find(bytes, pos + 2, b"-->")? + 2;
        } else if starts_with_ignore_case(rest, b"<meta")
            && rest.get(5).is_some_and(|&b| is_space(b) || b == b'/')
        {
            pos += 5;
            if let Some(encoding) = meta(bytes, &mut pos)? {
                return Some(encoding);
            }
        } else if rest.first() == Some(&b'<')
            && (rest.get(1).is_some_and(u8::is_ascii_alphabetic)
                || (rest.get(1) == Some(&b'/') && rest.get(2).is_some_and(u8::is_ascii_alphabetic)))
        {
            // Any other tag: skip its name, then its attributes.
            pos = bytes
                .iter()
                .skip(pos)
                .position(|&b| is_space(b) || b == b'>')
                .map(|offset| pos + offset)?;
            while attribute(bytes, &mut pos)?.is_some() {}
        } else if rest.starts_with(b"<!") || rest.starts_with(b"</") || rest.starts_with(b"<?") {
            pos = find(bytes, pos, b">")?;
        }
        // Next byte.
        pos += 1;
    }
    None
}

/// The steps after `<meta`: reads its attributes and decides. The outer
/// `None` means the input ran out; the inner one, no encoding here.
fn meta(bytes: &[u8], pos: &mut usize) -> Option<Option<&'static Encoding>> {
    let mut seen: Vec<Vec<u8>> = Vec::new();
    let mut got_pragma = false;
    let mut need_pragma: Option<bool> = None;
    let mut charset: Option<Option<&'static Encoding>> = None;
    while let Some((name, value)) = attribute(bytes, pos)? {
        if seen.contains(&name) {
            continue;
        }
        match name.as_slice() {
            b"http-equiv" if value == b"content-type" => got_pragma = true,
            b"content" => {
                if let Some(encoding) = from_content(&value)
                    && charset.is_none()
                {
                    charset = Some(Some(encoding));
                    need_pragma = Some(true);
                }
            }
            b"charset" if charset.is_none() => {
                charset = Some(Encoding::for_label(&value));
                need_pragma = Some(false);
            }
            _ => {}
        }
        seen.push(name);
    }
    // Processing.
    let Some(need_pragma) = need_pragma else {
        return Some(None);
    };
    if need_pragma && !got_pragma {
        return Some(None);
    }
    let Some(Some(encoding)) = charset else {
        return Some(None);
    };
    Some(Some(if encoding == UTF_16BE || encoding == UTF_16LE {
        UTF_8
    } else if encoding == X_USER_DEFINED {
        WINDOWS_1252
    } else {
        encoding
    }))
}

/// "Get an attribute": the next name and value, lowercased. The outer
/// `None` means the input ran out; the inner one, no more attributes.
fn attribute(bytes: &[u8], pos: &mut usize) -> Option<Option<(Vec<u8>, Vec<u8>)>> {
    let at = |pos: usize| bytes.get(pos).copied();
    while is_space(at(*pos)?) || at(*pos)? == b'/' {
        *pos += 1;
    }
    if at(*pos)? == b'>' {
        return Some(None);
    }
    let mut name = Vec::new();
    let mut value = Vec::new();
    // Name.
    loop {
        let b = at(*pos)?;
        if b == b'=' && !name.is_empty() {
            *pos += 1;
            break;
        }
        if is_space(b) {
            // Spaces: the name may still be followed by `=`.
            while is_space(at(*pos)?) {
                *pos += 1;
            }
            if at(*pos)? != b'=' {
                return Some(Some((name, value)));
            }
            *pos += 1;
            break;
        }
        if b == b'/' || b == b'>' {
            return Some(Some((name, value)));
        }
        name.push(b.to_ascii_lowercase());
        *pos += 1;
    }
    // Value.
    while is_space(at(*pos)?) {
        *pos += 1;
    }
    let b = at(*pos)?;
    if b == b'"' || b == b'\'' {
        loop {
            *pos += 1;
            let c = at(*pos)?;
            if c == b {
                *pos += 1;
                return Some(Some((name, value)));
            }
            value.push(c.to_ascii_lowercase());
        }
    }
    if b == b'>' {
        return Some(Some((name, value)));
    }
    loop {
        let c = at(*pos)?;
        if is_space(c) || c == b'>' {
            return Some(Some((name, value)));
        }
        value.push(c.to_ascii_lowercase());
        *pos += 1;
    }
}

/// "Extract a character encoding from a meta element", for the `content`
/// attribute of `<meta http-equiv=content-type>`.
fn from_content(content: &[u8]) -> Option<&'static Encoding> {
    let mut pos = 0;
    loop {
        pos = find_ignore_case(content, pos, b"charset")? + b"charset".len();
        while content.get(pos).is_some_and(|&b| is_space(b)) {
            pos += 1;
        }
        if content.get(pos) == Some(&b'=') {
            break;
        }
    }
    pos += 1;
    while content.get(pos).is_some_and(|&b| is_space(b)) {
        pos += 1;
    }
    let first = *content.get(pos)?;
    let label = if first == b'"' || first == b'\'' {
        let rest = content.get(pos + 1..)?;
        let end = rest.iter().position(|&b| b == first)?;
        rest.get(..end)?
    } else {
        let rest = content.get(pos..)?;
        let end = rest
            .iter()
            .position(|&b| is_space(b) || b == b';')
            .unwrap_or(rest.len());
        rest.get(..end)?
    };
    Encoding::for_label(label)
}

fn is_space(b: u8) -> bool {
    matches!(b, b'\t' | b'\n' | b'\x0C' | b'\r' | b' ')
}

fn starts_with_ignore_case(bytes: &[u8], prefix: &[u8]) -> bool {
    bytes
        .get(..prefix.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
}

/// Where `needle` next starts at or after `from`.
fn find(bytes: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    bytes
        .get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|offset| from + offset)
}

fn find_ignore_case(bytes: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    bytes
        .get(from..)?
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle))
        .map(|offset| from + offset)
}

#[cfg(test)]
mod tests {
    use encoding_rs::{EUC_JP, ISO_8859_2, SHIFT_JIS, WINDOWS_1251};

    use super::*;

    #[test]
    fn meta_charset() {
        assert_eq!(prescan(b"<meta charset=shift_jis>"), Some(SHIFT_JIS));
        assert_eq!(
            prescan(b"<!doctype html><META CHARSET='EUC-JP'>"),
            Some(EUC_JP)
        );
        assert_eq!(
            prescan(b"<head><meta charset=\"koi8-r\"/>"),
            Some(encoding_rs::KOI8_R)
        );
    }

    #[test]
    fn http_equiv_needs_the_pragma() {
        let page =
            b"<meta http-equiv=\"Content-Type\" content=\"text/html; charset=windows-1251\">";
        assert_eq!(prescan(page), Some(WINDOWS_1251));
        let reversed = b"<meta content='text/html;charset=iso-8859-2' http-equiv=content-type>";
        assert_eq!(prescan(reversed), Some(ISO_8859_2));
        // `content` without the pragma declares nothing.
        assert_eq!(
            prescan(b"<meta content=\"text/html; charset=koi8-r\">"),
            None
        );
    }

    #[test]
    fn utf16_and_x_user_defined_are_replaced() {
        assert_eq!(prescan(b"<meta charset=utf-16le>"), Some(UTF_8));
        assert_eq!(
            prescan(b"<meta charset=x-user-defined>"),
            Some(WINDOWS_1252)
        );
    }

    #[test]
    fn comments_and_other_tags_are_skipped() {
        assert_eq!(
            prescan(b"<!-- <meta charset=koi8-r> --><meta charset=big5>"),
            Some(encoding_rs::BIG5)
        );
        assert_eq!(
            prescan(b"<!--><meta charset=big5>"),
            Some(encoding_rs::BIG5)
        );
        assert_eq!(
            prescan(b"<title data-x=\"<meta charset=koi8-r>\">x</title><meta charset=gbk>"),
            Some(encoding_rs::GBK)
        );
        assert_eq!(prescan(b"<metal charset=koi8-r>"), None);
    }

    #[test]
    fn unknown_labels_and_late_metas_are_ignored() {
        assert_eq!(prescan(b"<meta charset=nonsense>"), None);
        let mut late = vec![b' '; PRESCAN_BYTES];
        late.extend_from_slice(b"<meta charset=koi8-r>");
        assert_eq!(prescan(&late), None);
        assert_eq!(prescan(b"<meta charset=koi8"), None);
    }
}
