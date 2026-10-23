//! PNG, through the `png` crate.

use std::io::Cursor;

use png::{ColorType, Transformations};

use crate::{Decoded, ImageDecoder, ImageError, Limits};

const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

pub(crate) struct Png;

impl ImageDecoder for Png {
    fn sniff(&self, bytes: &[u8]) -> bool {
        bytes.starts_with(SIGNATURE)
    }

    fn decode(&self, bytes: &[u8], limits: &Limits) -> Result<Decoded, ImageError> {
        let broken = |err: png::DecodingError| ImageError::Malformed(err.to_string());
        let mut decoder = png::Decoder::new_with_limits(
            Cursor::new(bytes),
            png::Limits {
                bytes: usize::try_from(limits.max_pixels.saturating_mul(4)).unwrap_or(usize::MAX),
            },
        );
        // Palettes and low bit depths expand, 16 bits strip to 8, and an
        // alpha channel is always added: gray and alpha, or RGBA.
        decoder
            .set_transformations(Transformations::normalize_to_color8() | Transformations::ALPHA);
        let mut reader = decoder.read_info().map_err(broken)?;
        let (width, height) = (reader.info().width, reader.info().height);
        limits.check(width, height)?;
        let size = reader
            .output_buffer_size()
            .ok_or(ImageError::TooLarge { width, height })?;
        let mut buffer = vec![0; size];
        let frame = reader.next_frame(&mut buffer).map_err(broken)?;
        buffer.truncate(frame.buffer_size());
        let channels = match frame.color_type {
            ColorType::GrayscaleAlpha => 2,
            ColorType::Rgba => 4,
            other => {
                return Err(ImageError::Malformed(format!(
                    "unexpected color type {other:?}"
                )));
            }
        };
        let mut pixels = crate::to_rgba(&buffer, channels)
            .ok_or_else(|| ImageError::Malformed("bad channel count".into()))?;
        crate::premultiply(&mut pixels);
        Ok(Decoded {
            width,
            height,
            pixels,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PNG of `width` by `height` from `rows` of `color` pixels, through
    /// the crate's own encoder.
    pub(crate) fn encode(width: u32, height: u32, color: png::ColorType, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(color);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(data).unwrap();
        writer.finish().unwrap();
        out
    }

    #[test]
    fn rgba_and_rgb_decode_premultiplied() {
        let png = encode(2, 1, ColorType::Rgba, &[255, 0, 0, 255, 0, 0, 255, 128]);
        let decoded = Png.decode(&png, &Limits::default()).unwrap();
        assert_eq!((decoded.width, decoded.height), (2, 1));
        assert_eq!(decoded.pixels, [255, 0, 0, 255, 0, 0, 128, 128]);

        let png = encode(1, 1, ColorType::Rgb, &[10, 20, 30]);
        let decoded = Png.decode(&png, &Limits::default()).unwrap();
        assert_eq!(decoded.pixels, [10, 20, 30, 255]);

        let png = encode(1, 1, ColorType::Grayscale, &[7]);
        let decoded = crate::decode(&png, &Limits::default()).unwrap();
        assert_eq!(decoded.pixels, [7, 7, 7, 255]);
    }

    #[test]
    fn oversized_and_truncated_files_fail() {
        let png = encode(4, 4, ColorType::Rgb, &[0; 48]);
        let tight = Limits {
            max_side: 3,
            max_pixels: 100,
        };
        assert_eq!(
            Png.decode(&png, &tight),
            Err(ImageError::TooLarge {
                width: 4,
                height: 4
            })
        );
        assert!(matches!(
            Png.decode(&png[..png.len() / 2], &Limits::default()),
            Err(ImageError::Malformed(_))
        ));
    }
}
