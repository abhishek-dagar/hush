//! GIF, through the `gif` crate: the first frame only, composited onto the
//! logical screen (animation is a later feature).

use std::io::Cursor;
use std::num::NonZeroU64;

use crate::{Decoded, ImageDecoder, ImageError, Limits};

pub(crate) struct Gif;

impl ImageDecoder for Gif {
    fn sniff(&self, bytes: &[u8]) -> bool {
        bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")
    }

    fn decode(&self, bytes: &[u8], limits: &Limits) -> Result<Decoded, ImageError> {
        let broken = |err: gif::DecodingError| ImageError::Malformed(err.to_string());
        let mut options = gif::DecodeOptions::new();
        options.set_color_output(gif::ColorOutput::RGBA);
        if let Some(bytes) = NonZeroU64::new(limits.max_pixels.saturating_mul(4)) {
            options.set_memory_limit(gif::MemoryLimit::Bytes(bytes));
        }
        let mut decoder = options.read_info(Cursor::new(bytes)).map_err(broken)?;
        let screen = (u32::from(decoder.width()), u32::from(decoder.height()));
        let frame = decoder
            .read_next_frame()
            .map_err(broken)?
            .ok_or_else(|| ImageError::Malformed("no frames".into()))?;
        let frame_size = (u32::from(frame.width), u32::from(frame.height));
        // A file without a logical screen size shows its first frame as is.
        let (width, height, left, top) = if screen.0 == 0 || screen.1 == 0 {
            (frame_size.0, frame_size.1, 0, 0)
        } else {
            (
                screen.0,
                screen.1,
                u32::from(frame.left),
                u32::from(frame.top),
            )
        };
        limits.check(width, height)?;
        let mut pixels = vec![0u8; width as usize * height as usize * 4];
        let row_bytes = frame_size.0 as usize * 4;
        for (row, source) in frame.buffer.chunks_exact(row_bytes.max(1)).enumerate() {
            let y = top as usize + row;
            if y >= height as usize {
                break;
            }
            // The part of the row on the screen.
            let visible = (width.saturating_sub(left) as usize).min(frame_size.0 as usize);
            let start = (y * width as usize + left as usize) * 4;
            if let Some(target) = pixels.get_mut(start..start + visible * 4) {
                target.copy_from_slice(&source[..visible * 4]);
            }
        }
        crate::premultiply(&mut pixels);
        Ok(Decoded {
            width,
            height,
            pixels,
        })
    }
}
