//! JPEG, through `zune-jpeg`. JPEGs are opaque, so nothing is
//! premultiplied.

use std::io::Cursor;

use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

use crate::{Decoded, ImageDecoder, ImageError, Limits};

/// Start of image, then the first marker.
const SIGNATURE: &[u8] = &[0xFF, 0xD8, 0xFF];

pub(crate) struct Jpeg;

impl ImageDecoder for Jpeg {
    fn sniff(&self, bytes: &[u8]) -> bool {
        bytes.starts_with(SIGNATURE)
    }

    fn decode(&self, bytes: &[u8], limits: &Limits) -> Result<Decoded, ImageError> {
        let broken =
            |err: zune_jpeg::errors::DecodeErrors| ImageError::Malformed(format!("{err:?}"));
        let options = DecoderOptions::default()
            .jpeg_set_out_colorspace(ColorSpace::RGBA)
            .set_max_width(limits.max_side as usize)
            .set_max_height(limits.max_side as usize);
        let mut decoder = JpegDecoder::new_with_options(Cursor::new(bytes), options);
        decoder.decode_headers().map_err(broken)?;
        let info = decoder
            .info()
            .ok_or_else(|| ImageError::Malformed("no frame header".into()))?;
        let (width, height) = (u32::from(info.width), u32::from(info.height));
        limits.check(width, height)?;
        let decoded = decoder.decode().map_err(broken)?;
        let channels = decoded.len() / (width as usize * height as usize);
        let pixels = crate::to_rgba(&decoded, channels)
            .filter(|pixels| pixels.len() == width as usize * height as usize * 4)
            .ok_or_else(|| ImageError::Malformed("unexpected pixel layout".into()))?;
        Ok(Decoded {
            width,
            height,
            pixels,
        })
    }
}
