//! `image-optimizer` — shrink a picture by re-encoding it at lower cost.
//!
//! Config (wasi:config/store):
//!   image-max-width   wider pictures are shrunk to this many pixels, default 1600
//!   image-quality     JPEG quality, 1-100, default 80

#[allow(warnings)]
mod bindings;

use std::io::Cursor;

use bindings::exports::media::image::optimizer::{Guest, ImageError};
use bindings::wasi::config::store as config;
use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader};

struct Component;

/// A decoded picture's bytes (3 a pixel for a JPEG, 4 for an RGBA PNG) must fit beside its
/// encoded input and the smaller output in the guest's memory.
const MAX_DECODED: u64 = 40_000_000;

fn setting(key: &str, default: u32) -> u32 {
    match config::get(key) {
        Ok(Some(v)) => v.trim().parse().unwrap_or(default),
        _ => default,
    }
}

fn optimize(img: &[u8], max_width: u32, quality: u8) -> Result<Vec<u8>, ImageError> {
    let bad = |e: image::ImageError| ImageError::NotAnImage(e.to_string());
    let reader = ImageReader::new(Cursor::new(img))
        .with_guessed_format()
        .map_err(|e| ImageError::NotAnImage(e.to_string()))?;
    let format = reader.format().ok_or_else(|| ImageError::NotAnImage("unknown format".into()))?;
    let decoder = reader.into_decoder().map_err(bad)?;
    let ((w, h), bytes) = (decoder.dimensions(), decoder.total_bytes());
    if bytes > MAX_DECODED {
        return Err(ImageError::TooLarge(format!(
            "{w}x{h} decodes to {bytes} bytes, over {MAX_DECODED}"
        )));
    }
    let decoded = DynamicImage::from_decoder(decoder).map_err(bad)?;
    // `thumbnail` averages areas in integers; `resize` would hold a float copy, 16 bytes a pixel.
    let resized = if w > max_width { decoded.thumbnail(max_width, u32::MAX) } else { decoded };
    let mut out = Cursor::new(Vec::new());
    match format {
        // The generic encoder ignores quality and writes at 75.
        ImageFormat::Jpeg => {
            resized.to_rgb8().write_with_encoder(JpegEncoder::new_with_quality(&mut out, quality))
        }
        f => resized.write_to(&mut out, f),
    }
    .map_err(bad)?;
    Ok(out.into_inner())
}

impl Guest for Component {
    fn optimize(img: Vec<u8>) -> Result<Vec<u8>, ImageError> {
        let quality = setting("image-quality", 80).clamp(1, 100) as u8;
        optimize(&img, setting("image-max-width", 1600).max(1), quality)
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, RgbImage};

    fn encoded(w: u32, h: u32, format: ImageFormat) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(RgbImage::from_fn(w, h, |x, y| image::Rgb([x as u8, y as u8, 7])))
            .write_to(&mut out, format)
            .unwrap();
        out.into_inner()
    }

    fn size(bytes: &[u8]) -> (u32, u32, ImageFormat) {
        let r = ImageReader::new(Cursor::new(bytes)).with_guessed_format().unwrap();
        let f = r.format().unwrap();
        let (w, h) = r.into_dimensions().unwrap();
        (w, h, f)
    }

    #[test]
    fn a_wide_picture_shrinks_to_the_maximum_and_keeps_its_format_and_aspect() {
        let out = optimize(&encoded(400, 100, ImageFormat::Png), 200, 80).unwrap();
        assert_eq!(size(&out), (200, 50, ImageFormat::Png));
        let out = optimize(&encoded(400, 300, ImageFormat::Jpeg), 100, 60).unwrap();
        assert_eq!(size(&out), (100, 75, ImageFormat::Jpeg));
    }

    #[test]
    fn a_narrow_picture_keeps_its_size() {
        let out = optimize(&encoded(50, 40, ImageFormat::Png), 200, 80).unwrap();
        assert_eq!(size(&out), (50, 40, ImageFormat::Png));
    }

    #[test]
    fn not_a_picture_and_too_many_pixels_are_refused_before_decoding() {
        assert!(matches!(optimize(b"hello", 200, 80), Err(ImageError::NotAnImage(_))));
        let huge = encoded(5000, 3000, ImageFormat::Png);
        assert!(matches!(optimize(&huge, 200, 80), Err(ImageError::TooLarge(_))));
    }
}
