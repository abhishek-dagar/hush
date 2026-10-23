//! WebP, through `image-webp`: lossy, lossless, and the first frame of an
//! animation.

use std::io::Cursor;

use crate::{Decoded, ImageDecoder, ImageError, Limits};

pub(crate) struct WebP;

impl ImageDecoder for WebP {
    fn sniff(&self, bytes: &[u8]) -> bool {
        bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP"
    }

    fn decode(&self, bytes: &[u8], limits: &Limits) -> Result<Decoded, ImageError> {
        let broken = |err: image_webp::DecodingError| ImageError::Malformed(err.to_string());
        let mut decoder = image_webp::WebPDecoder::new(Cursor::new(bytes)).map_err(broken)?;
        let (width, height) = decoder.dimensions();
        limits.check(width, height)?;
        decoder.set_memory_limit(
            usize::try_from(limits.max_pixels.saturating_mul(4)).unwrap_or(usize::MAX),
        );
        let size = decoder
            .output_buffer_size()
            .ok_or(ImageError::TooLarge { width, height })?;
        let mut buffer = vec![0; size];
        decoder.read_image(&mut buffer).map_err(broken)?;
        let channels = if decoder.has_alpha() { 4 } else { 3 };
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
