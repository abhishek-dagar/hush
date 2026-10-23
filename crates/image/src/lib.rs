//! Image decoding in the content process (spec 0004, child 0005, Images).
//!
//! Each format is an [`ImageDecoder`] over its crate: `png`, `zune-jpeg`,
//! `gif` (the first frame, on its logical screen) and `image-webp` (lossy,
//! lossless, and the first frame of an animation). The format is picked by
//! its magic bytes, never by `Content-Type`. Output is premultiplied RGBA8
//! in sRGB, the layout `AddImage` sends. Every size is checked against
//! [`Limits`] from the headers, before any pixel buffer is allocated.

#![forbid(unsafe_code)]

mod gif;
mod ico;
mod jpeg;
mod png;
mod webp;

/// The largest image side, in pixels (spec 0002, Security model).
pub const MAX_SIDE: u32 = 16_384;
/// The most pixels one image may have.
pub const MAX_PIXELS: u64 = 256 * 1024 * 1024;

/// How large an image may decode to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The largest width or height, in pixels.
    pub max_side: u32,
    /// The most pixels in total.
    pub max_pixels: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_side: MAX_SIDE,
            max_pixels: MAX_PIXELS,
        }
    }
}

impl Limits {
    /// Checks a size from an image's headers.
    pub fn check(&self, width: u32, height: u32) -> Result<(), ImageError> {
        if width == 0 || height == 0 {
            return Err(ImageError::Malformed("an empty image".into()));
        }
        if width > self.max_side
            || height > self.max_side
            || u64::from(width) * u64::from(height) > self.max_pixels
        {
            return Err(ImageError::TooLarge { width, height });
        }
        Ok(())
    }
}

/// A decoded image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoded {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Premultiplied RGBA8 in sRGB, rows top to bottom with no padding.
    pub pixels: Vec<u8>,
}

/// Why an image did not decode.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ImageError {
    /// The bytes are not in any format the engine reads.
    #[error("not an image format the engine reads")]
    Unsupported,
    /// The image is over [`Limits`].
    #[error("the image is too large: {width} by {height}")]
    TooLarge {
        /// Its width, from the headers.
        width: u32,
        /// Its height.
        height: u32,
    },
    /// The data is broken.
    #[error("the image data is broken: {0}")]
    Malformed(String),
}

/// One image format.
pub trait ImageDecoder {
    /// Whether `bytes` start like this format.
    fn sniff(&self, bytes: &[u8]) -> bool;

    /// Decodes `bytes`, refusing anything over `limits`.
    fn decode(&self, bytes: &[u8], limits: &Limits) -> Result<Decoded, ImageError>;
}

/// Every format the engine reads, in sniffing order.
const DECODERS: [&dyn ImageDecoder; 4] = [&png::Png, &jpeg::Jpeg, &gif::Gif, &webp::WebP];

/// Decodes `bytes` with the decoder whose magic bytes they start with.
pub fn decode(bytes: &[u8], limits: &Limits) -> Result<Decoded, ImageError> {
    DECODERS
        .iter()
        .find(|decoder| decoder.sniff(bytes))
        .ok_or(ImageError::Unsupported)?
        .decode(bytes, limits)
}

/// The largest favicon side the browser is sent (spec 0004, AC-37).
pub const MAX_ICON_SIDE: u32 = 256;

/// A decoded favicon: straight alpha RGBA8, at most [`MAX_ICON_SIDE`] a
/// side, the layout window icons take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Icon {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Straight alpha RGBA8, rows top to bottom with no padding.
    pub rgba: Vec<u8>,
}

/// Decodes a favicon (AC-37): an ICO file's entry nearest 32 px (ties to
/// the larger), or any other format [`decode`] reads, scaled down to at
/// most [`MAX_ICON_SIDE`] a side.
pub fn decode_icon(bytes: &[u8], limits: &Limits) -> Result<Icon, ImageError> {
    let (width, height, rgba) = if ico::sniff(bytes) {
        ico::decode(bytes, limits)?
    } else {
        let decoded = decode(bytes, limits)?;
        let mut pixels = decoded.pixels;
        unpremultiply(&mut pixels);
        (decoded.width, decoded.height, pixels)
    };
    Ok(fit(width, height, &rgba, MAX_ICON_SIDE))
}

/// Scales RGBA8 down, nearest neighbour, so neither side is over `max`.
fn fit(width: u32, height: u32, rgba: &[u8], max: u32) -> Icon {
    if width <= max && height <= max {
        return Icon {
            width,
            height,
            rgba: rgba.to_vec(),
        };
    }
    let scale = f64::from(max) / f64::from(width.max(height));
    let to = |side: u32| ((f64::from(side) * scale).round() as u32).clamp(1, max);
    let (out_w, out_h) = (to(width), to(height));
    let mut out = Vec::with_capacity(out_w as usize * out_h as usize * 4);
    for y in 0..out_h {
        let src_y = (u64::from(y) * u64::from(height) / u64::from(out_h)) as usize;
        for x in 0..out_w {
            let src_x = (u64::from(x) * u64::from(width) / u64::from(out_w)) as usize;
            let at = (src_y * width as usize + src_x) * 4;
            out.extend_from_slice(&rgba[at..at + 4]);
        }
    }
    Icon {
        width: out_w,
        height: out_h,
        rgba: out,
    }
}

/// Makes premultiplied RGBA8 straight alpha again, in place.
fn unpremultiply(pixels: &mut [u8]) {
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = u16::from(pixel[3]);
        if alpha == 255 || alpha == 0 {
            continue;
        }
        for channel in &mut pixel[..3] {
            *channel = ((u16::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
        }
    }
}

/// Makes straight alpha RGBA8 premultiplied, in place.
fn premultiply(pixels: &mut [u8]) {
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = u16::from(pixel[3]);
        if alpha == 255 {
            continue;
        }
        for channel in &mut pixel[..3] {
            // Rounded: (c * a + 127) / 255.
            *channel = ((u16::from(*channel) * alpha + 127) / 255) as u8;
        }
    }
}

/// RGBA8 from rows of `channels` bytes per pixel: 1 gray, 2 gray and
/// alpha, 3 RGB or 4 RGBA. `None` for any other count.
fn to_rgba(pixels: &[u8], channels: usize) -> Option<Vec<u8>> {
    let rgba = match channels {
        1 => pixels.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        2 => pixels
            .chunks_exact(2)
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        3 => pixels
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        4 => pixels.to_vec(),
        _ => return None,
    };
    Some(rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premultiplying_scales_color_by_alpha_and_keeps_opaque_pixels() {
        let mut pixels = [200, 100, 50, 255, 200, 100, 50, 128, 255, 255, 255, 0];
        premultiply(&mut pixels);
        assert_eq!(pixels, [200, 100, 50, 255, 100, 50, 25, 128, 0, 0, 0, 0]);
    }

    #[test]
    fn channels_expand_to_rgba() {
        assert_eq!(to_rgba(&[9], 1), Some(vec![9, 9, 9, 255]));
        assert_eq!(to_rgba(&[9, 3], 2), Some(vec![9, 9, 9, 3]));
        assert_eq!(to_rgba(&[1, 2, 3], 3), Some(vec![1, 2, 3, 255]));
        assert_eq!(to_rgba(&[1, 2, 3, 4, 5], 5), None);
    }

    #[test]
    fn sizes_over_the_limits_are_refused_before_decoding() {
        let limits = Limits::default();
        assert_eq!(limits.check(16_384, 16_384), Ok(()));
        assert_eq!(
            limits.check(16_385, 1),
            Err(ImageError::TooLarge {
                width: 16_385,
                height: 1
            })
        );
        let tight = Limits {
            max_side: 100,
            max_pixels: 50,
        };
        assert!(matches!(
            tight.check(10, 10),
            Err(ImageError::TooLarge { .. })
        ));
        assert!(matches!(limits.check(0, 5), Err(ImageError::Malformed(_))));
    }

    #[test]
    fn unpremultiplying_undoes_premultiplying() {
        let mut pixels = [100, 50, 25, 128, 7, 7, 7, 0, 9, 8, 7, 255];
        unpremultiply(&mut pixels);
        assert_eq!(pixels, [199, 100, 50, 128, 7, 7, 7, 0, 9, 8, 7, 255]);
    }

    #[test]
    fn large_icons_scale_down_keeping_their_shape() {
        let rgba: Vec<u8> = (0..512u32 * 128)
            .flat_map(|i| [(i % 512 / 2) as u8, 0, 0, 255])
            .collect();
        let icon = fit(512, 128, &rgba, MAX_ICON_SIDE);
        assert_eq!((icon.width, icon.height), (256, 64));
        assert_eq!(icon.rgba.len(), 256 * 64 * 4);
        // Nearest neighbour: output x 10 reads source x 20.
        assert_eq!(icon.rgba[10 * 4], 10);
        let small = fit(16, 16, &[1; 16 * 16 * 4], MAX_ICON_SIDE);
        assert_eq!((small.width, small.height), (16, 16));
    }

    #[test]
    fn unknown_bytes_are_unsupported() {
        assert_eq!(
            decode(b"<html>not an image", &Limits::default()),
            Err(ImageError::Unsupported)
        );
        assert_eq!(
            decode(&[], &Limits::default()),
            Err(ImageError::Unsupported)
        );
    }
}
