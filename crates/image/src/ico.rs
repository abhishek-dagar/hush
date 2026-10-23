//! ICO, read by hand for favicons (spec 0004, child 0005, Favicon, AC-37).
//!
//! The directory lists each entry's size; the one nearest [`TARGET`] px
//! (ties to the larger) is tried first. An entry is a PNG (by signature)
//! or a BMP DIB without its file header: 1, 4, 8, 24 or 32 bits per pixel,
//! rows bottom up, then a 1 bit AND mask that makes pixels transparent.

use crate::{ImageDecoder, ImageError, Limits, png::Png};

/// The size the favicon is wanted at, in pixels.
pub(crate) const TARGET: u32 = 32;

const HEADER: usize = 6;
const ENTRY: usize = 16;
const DIB_HEADER: usize = 40;

/// Whether `bytes` start like an icon file (reserved 0, type 1).
pub(crate) fn sniff(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0, 0, 1, 0])
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn broken(what: &str) -> ImageError {
    ImageError::Malformed(format!("icon: {what}"))
}

/// One directory entry: its listed size and where its data is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    size: u32,
    offset: usize,
    len: usize,
}

fn directory(bytes: &[u8]) -> Result<Vec<Entry>, ImageError> {
    let count = u16_at(bytes, 4).ok_or_else(|| broken("no header"))?;
    (0..usize::from(count))
        .map(|i| {
            let at = HEADER + i * ENTRY;
            let side = |b: u8| if b == 0 { 256 } else { u32::from(b) };
            let width = side(*bytes.get(at).ok_or_else(|| broken("short directory"))?);
            let height = side(*bytes.get(at + 1).ok_or_else(|| broken("short directory"))?);
            let len = u32_at(bytes, at + 8).ok_or_else(|| broken("short directory"))?;
            let offset = u32_at(bytes, at + 12).ok_or_else(|| broken("short directory"))?;
            Ok(Entry {
                size: width.max(height),
                offset: offset as usize,
                len: len as usize,
            })
        })
        .collect()
}

/// Entries nearest [`TARGET`] first, the larger of two equally near.
fn by_preference(mut entries: Vec<Entry>) -> Vec<Entry> {
    entries.sort_by_key(|entry| (entry.size.abs_diff(TARGET), std::cmp::Reverse(entry.size)));
    entries
}

/// The preferred entry that decodes, as straight alpha RGBA8.
pub(crate) fn decode(bytes: &[u8], limits: &Limits) -> Result<(u32, u32, Vec<u8>), ImageError> {
    let mut last = broken("no entries");
    for entry in by_preference(directory(bytes)?) {
        let data = entry
            .offset
            .checked_add(entry.len)
            .and_then(|end| bytes.get(entry.offset..end));
        let Some(data) = data else {
            last = broken("an entry outside the file");
            continue;
        };
        let decoded = if Png.sniff(data) {
            Png.decode(data, limits).map(|decoded| {
                let mut pixels = decoded.pixels;
                crate::unpremultiply(&mut pixels);
                (decoded.width, decoded.height, pixels)
            })
        } else {
            dib(data, limits)
        };
        match decoded {
            Ok(decoded) => return Ok(decoded),
            Err(err) => last = err,
        }
    }
    Err(last)
}

/// A BMP DIB as an icon stores it: the header's height counts the color
/// rows and the mask rows together.
fn dib(data: &[u8], limits: &Limits) -> Result<(u32, u32, Vec<u8>), ImageError> {
    let header = u32_at(data, 0).ok_or_else(|| broken("no bitmap header"))? as usize;
    if header < DIB_HEADER {
        return Err(broken("an old bitmap header"));
    }
    let width = u32_at(data, 4).ok_or_else(|| broken("no width"))? as i32;
    let height = u32_at(data, 8).ok_or_else(|| broken("no height"))? as i32;
    let bits = u16_at(data, 14).ok_or_else(|| broken("no depth"))?;
    let compression = u32_at(data, 16).ok_or_else(|| broken("no compression"))?;
    let colors_used = u32_at(data, 32).ok_or_else(|| broken("no palette size"))?;
    // BI_RGB, or BI_BITFIELDS at 32 bits with the usual BGRA masks.
    if compression != 0 && !(compression == 3 && bits == 32) {
        return Err(broken("compressed bitmaps are not read"));
    }
    let width = width.unsigned_abs();
    let height = height.unsigned_abs() / 2;
    limits.check(width, height)?;
    let (w, h) = (width as usize, height as usize);

    let palette_len = match bits {
        1 | 4 | 8 if colors_used == 0 => 1usize << bits,
        1 | 4 | 8 => (colors_used as usize).min(1 << bits),
        24 | 32 => 0,
        _ => return Err(broken("an unsupported bit depth")),
    };
    // BI_BITFIELDS puts three masks after the header.
    let masks = if compression == 3 { 12 } else { 0 };
    let palette_at = header + masks;
    let palette = data
        .get(palette_at..palette_at + palette_len * 4)
        .ok_or_else(|| broken("a short palette"))?;
    let stride = (w * usize::from(bits)).div_ceil(32) * 4;
    let mask_stride = w.div_ceil(32) * 4;
    let pixels_at = palette_at + palette.len();
    let color = data
        .get(pixels_at..pixels_at + stride * h)
        .ok_or_else(|| broken("short pixel rows"))?;
    // Some 32 bit icons leave the mask out.
    let mask = data.get(pixels_at + stride * h..pixels_at + stride * h + mask_stride * h);

    let mut rgba = vec![0; w * h * 4];
    let mut any_alpha = false;
    for y in 0..h {
        // Rows are stored bottom up.
        let row = &color[(h - 1 - y) * stride..][..stride];
        for x in 0..w {
            let [b, g, r, a] = match bits {
                24 => [row[x * 3], row[x * 3 + 1], row[x * 3 + 2], 255],
                32 => [row[x * 4], row[x * 4 + 1], row[x * 4 + 2], row[x * 4 + 3]],
                _ => {
                    let per_byte = 8 / usize::from(bits);
                    let byte = row[x / per_byte];
                    let shift = 8 - usize::from(bits) * (x % per_byte + 1);
                    let index = usize::from((byte >> shift) & ((1u16 << bits) - 1) as u8);
                    match palette.get(index * 4..index * 4 + 3) {
                        Some(entry) => [entry[0], entry[1], entry[2], 255],
                        None => [0, 0, 0, 255],
                    }
                }
            };
            any_alpha |= bits == 32 && a != 0;
            rgba[(y * w + x) * 4..][..4].copy_from_slice(&[r, g, b, a]);
        }
    }
    // A 32 bit icon with real alpha ignores the mask; any other is opaque
    // where the mask (if there is one) is clear.
    if bits == 32 && any_alpha {
        return Ok((width, height, rgba));
    }
    for y in 0..h {
        for x in 0..w {
            let pixel = &mut rgba[(y * w + x) * 4..][..4];
            let masked = mask.is_some_and(|mask| {
                mask[(h - 1 - y) * mask_stride + x / 8] & (0x80 >> (x % 8)) != 0
            });
            if masked {
                pixel.copy_from_slice(&[0, 0, 0, 0]);
            } else {
                pixel[3] = 255;
            }
        }
    }
    Ok((width, height, rgba))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An icon file around `images`, each `(listed size, data)`.
    pub(crate) fn icon(images: &[(u8, Vec<u8>)]) -> Vec<u8> {
        let mut out = vec![0, 0, 1, 0];
        out.extend_from_slice(&(images.len() as u16).to_le_bytes());
        let mut offset = HEADER + ENTRY * images.len();
        for (size, data) in images {
            out.extend_from_slice(&[*size, *size, 0, 0, 1, 0, 32, 0]);
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(offset as u32).to_le_bytes());
            offset += data.len();
        }
        for (_, data) in images {
            out.extend_from_slice(data);
        }
        out
    }

    /// A DIB of `width` by `height` at `bits`, with `palette`, color rows
    /// (bottom up, already padded) and mask rows.
    pub(crate) fn dib_bytes(
        width: u32,
        height: u32,
        bits: u16,
        palette: &[[u8; 4]],
        color: &[u8],
        mask: &[u8],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&width.to_le_bytes());
        out.extend_from_slice(&(height * 2).to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&bits.to_le_bytes());
        out.extend_from_slice(&[0; 4]); // BI_RGB
        out.extend_from_slice(&[0; 12]);
        out.extend_from_slice(&(palette.len() as u32).to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        for entry in palette {
            out.extend_from_slice(entry);
        }
        out.extend_from_slice(color);
        out.extend_from_slice(mask);
        out
    }

    #[test]
    fn the_entry_nearest_32_wins_and_ties_go_larger() {
        let entry = |size| Entry {
            size,
            offset: 0,
            len: 0,
        };
        let order = by_preference(vec![entry(16), entry(256), entry(48), entry(16), entry(32)]);
        let sizes: Vec<u32> = order.iter().map(|e| e.size).collect();
        assert_eq!(sizes, [32, 48, 16, 16, 256]);
        let tie = by_preference(vec![entry(24), entry(40)]);
        assert_eq!(tie[0].size, 40);
    }

    #[test]
    fn a_paletted_bitmap_reads_bottom_up_with_its_mask() {
        // 2 by 2 at 1 bit: palette 0 black, 1 red (BGRx). Bottom row first.
        let palette = [[0, 0, 0, 0], [0, 0, 255, 0]];
        // Bottom row: red, black. Top row: black, red. Padded to 4 bytes.
        let color = [0b1000_0000, 0, 0, 0, 0b0100_0000, 0, 0, 0];
        // Bottom row: second pixel transparent. Top row: none.
        let mask = [0b0100_0000, 0, 0, 0, 0, 0, 0, 0];
        let data = dib_bytes(2, 2, 1, &palette, &color, &mask);
        let (w, h, rgba) = dib(&data, &Limits::default()).unwrap();
        assert_eq!((w, h), (2, 2));
        assert_eq!(
            rgba,
            [
                0, 0, 0, 255, 255, 0, 0, 255, // top: black, red
                255, 0, 0, 255, 0, 0, 0, 0, // bottom: red, masked
            ]
        );
    }

    #[test]
    fn four_eight_and_24_bit_bitmaps_read() {
        let palette: Vec<[u8; 4]> = (0..16).map(|i| [i * 10, 0, 0, 0]).collect();
        // 1 by 1 at 4 bits: index 3 in the high nibble.
        let data = dib_bytes(1, 1, 4, &palette, &[0x30, 0, 0, 0], &[0, 0, 0, 0]);
        assert_eq!(dib(&data, &Limits::default()).unwrap().2, [0, 0, 30, 255]);
        let data = dib_bytes(1, 1, 8, &palette, &[5, 0, 0, 0], &[0, 0, 0, 0]);
        assert_eq!(dib(&data, &Limits::default()).unwrap().2, [0, 0, 50, 255]);
        let data = dib_bytes(1, 1, 24, &[], &[1, 2, 3, 0], &[0x80, 0, 0, 0]);
        assert_eq!(dib(&data, &Limits::default()).unwrap().2, [0, 0, 0, 0]);
    }

    #[test]
    fn thirty_two_bit_alpha_beats_the_mask_unless_it_is_all_zero() {
        let with_alpha = dib_bytes(1, 1, 32, &[], &[10, 20, 30, 128], &[0x80, 0, 0, 0]);
        assert_eq!(
            dib(&with_alpha, &Limits::default()).unwrap().2,
            [30, 20, 10, 128]
        );
        let no_alpha = dib_bytes(1, 1, 32, &[], &[10, 20, 30, 0], &[0, 0, 0, 0]);
        assert_eq!(
            dib(&no_alpha, &Limits::default()).unwrap().2,
            [30, 20, 10, 255]
        );
    }

    #[test]
    fn a_broken_entry_falls_back_to_the_next() {
        let good = dib_bytes(1, 1, 24, &[], &[1, 2, 3, 0], &[0, 0, 0, 0]);
        let file = icon(&[(32, vec![1, 2, 3]), (16, good)]);
        assert!(sniff(&file));
        let (w, h, rgba) = decode(&file, &Limits::default()).unwrap();
        assert_eq!((w, h, rgba), (1, 1, vec![3, 2, 1, 255]));
        assert!(decode(&icon(&[]), &Limits::default()).is_err());
        assert!(decode(&[0, 0, 1, 0, 5], &Limits::default()).is_err());
    }
}
