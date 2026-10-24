//! Pixel comparison for WPT reftests (spec 0005, *Pixel comparison*).

use std::str::FromStr;

/// One channel of a fuzzy range: `n` means `n-n`, `a-b` a closed range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub min: u32,
    pub max: u32,
}

impl Range {
    pub fn new(value: u32) -> Self {
        Self {
            min: value,
            max: value,
        }
    }
}

impl FromStr for Range {
    type Err = String;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let text = text.trim();
        if let Some((a, b)) = text.split_once('-') {
            let min = a.trim().parse::<u32>().map_err(|_| text.to_owned())?;
            let max = b.trim().parse::<u32>().map_err(|_| text.to_owned())?;
            if min > max {
                return Err(text.to_owned());
            }
            Ok(Self { min, max })
        } else {
            let value = text.parse::<u32>().map_err(|_| text.to_owned())?;
            Ok(Self::new(value))
        }
    }
}

/// A fuzzy tolerance, optionally bound to one reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fuzzy {
    /// The reference it applies to, if named.
    pub ref_url: Option<String>,
    /// Allowed range for the largest channel difference.
    pub max_diff: Range,
    /// Allowed range for the count of differing pixels.
    pub total_pixels: Range,
}

impl Fuzzy {
    /// Parses `content` in either the named or short form.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        // Named form: [<ref url>:]maxDifference=<a>[-<b>];totalPixels=<c>[-<d>]
        let (ref_url, body) = if let Some((left, right)) = text.split_once(':') {
            // only treat as named when the right side looks like maxDifference=
            if right
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("maxdifference=")
            {
                (Some(left.trim().to_owned()), right.to_owned())
            } else {
                (None, text.to_owned())
            }
        } else {
            (None, text.to_owned())
        };
        let body = body.trim();
        // split on ';'
        let mut max_part: Option<String> = None;
        let mut total_part: Option<String> = None;
        for piece in body.split(';') {
            let piece = piece.trim();
            if piece.is_empty() {
                continue;
            }
            let lower = piece.to_ascii_lowercase();
            if let Some(rest) = lower.strip_prefix("maxdifference=") {
                // keep original casing of the number part (numbers only)
                max_part = Some(piece[piece.len() - rest.len()..].to_owned());
            } else if let Some(rest) = lower.strip_prefix("totalpixels=") {
                total_part = Some(piece[piece.len() - rest.len()..].to_owned());
            } else {
                // short form: <a>[-<b>];<c>[-<d>]
                if max_part.is_none() {
                    max_part = Some(piece.to_owned());
                } else {
                    total_part = Some(piece.to_owned());
                }
            }
        }
        let max_diff = max_part.ok_or_else(|| text.to_owned())?.parse::<Range>()?;
        let total_pixels = total_part
            .ok_or_else(|| text.to_owned())?
            .parse::<Range>()?;
        Ok(Self {
            ref_url,
            max_diff,
            total_pixels,
        })
    }
}

/// The result of comparing two images.
#[derive(Debug, Clone)]
pub struct CompareResult {
    pub equal: bool,
    pub message: String,
}

/// Compares two RGBA8 buffers of the same size.
pub fn compare(a: &[u8], b: &[u8], fuzzy: Option<&Fuzzy>) -> CompareResult {
    let mut max_diff: u32 = 0;
    let mut total_pixels: u32 = 0;
    let pixels = a.len() / 4;
    for i in 0..pixels {
        let base = i * 4;
        let mut diff: u32 = 0;
        for c in 0..4 {
            let d = a[base + c].abs_diff(b[base + c]) as u32;
            if d > diff {
                diff = d;
            }
        }
        if diff > 0 {
            total_pixels += 1;
            if diff > max_diff {
                max_diff = diff;
            }
        }
    }
    let equal = match fuzzy {
        None => total_pixels == 0,
        Some(f) => {
            max_diff >= f.max_diff.min
                && max_diff <= f.max_diff.max
                && total_pixels >= f.total_pixels.min
                && total_pixels <= f.total_pixels.max
        }
    };
    let message = if equal {
        if total_pixels == 0 {
            "equal".to_owned()
        } else {
            format!("{total_pixels} pixels differ, max {max_diff}")
        }
    } else if total_pixels == 0 {
        "expected a difference".to_owned()
    } else {
        format!("{total_pixels} pixels differ, max {max_diff}")
    };
    CompareResult { equal, message }
}

/// Builds a diff image: black where equal, solid red where different.
pub fn diff_image(a: &[u8], b: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; a.len()];
    for i in 0..(a.len() / 4) {
        let base = i * 4;
        let mut differs = false;
        for c in 0..4 {
            if a[base + c] != b[base + c] {
                differs = true;
                break;
            }
        }
        if differs {
            out[base] = 255;
            out[base + 1] = 0;
            out[base + 2] = 0;
            out[base + 3] = 255;
        } else {
            out[base + 3] = 255;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_buffers_are_equal() {
        let a = vec![10u8, 20, 30, 255, 0, 0, 0, 255];
        let r = compare(&a, &a, None);
        assert!(r.equal);
    }

    #[test]
    fn a_single_difference_fails_without_fuzzy() {
        let a = vec![0u8, 0, 0, 255];
        let b = vec![1u8, 0, 0, 255];
        let r = compare(&a, &b, None);
        assert!(!r.equal);
    }

    #[test]
    fn fuzzy_passes_inside_the_range() {
        let a = vec![0u8, 0, 0, 255];
        let b = vec![10u8, 0, 0, 255];
        let f = Fuzzy::parse("0-10;0-5").unwrap();
        let r = compare(&a, &b, Some(&f));
        assert!(r.equal);
    }

    #[test]
    fn fuzzy_fails_outside_the_range() {
        let a = vec![0u8, 0, 0, 255];
        let b = vec![10u8, 0, 0, 255];
        let f = Fuzzy::parse("0-9;0-5").unwrap();
        let r = compare(&a, &b, Some(&f));
        assert!(!r.equal);
    }

    #[test]
    fn named_form_parses() {
        let f = Fuzzy::parse("ref.html:maxDifference=0-2;totalPixels=0-10").unwrap();
        assert_eq!(f.ref_url.as_deref(), Some("ref.html"));
        assert_eq!(f.max_diff, Range { min: 0, max: 2 });
        assert_eq!(f.total_pixels, Range { min: 0, max: 10 });
    }
}
