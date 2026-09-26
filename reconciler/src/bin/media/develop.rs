//! Develop (CPU): the three renditions and a glance at exposure, without the
//! Swift helper.

use std::path::Path;

use anyhow::{anyhow, Result};
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ImageEncoder};
use serde_json::{json, Value};

use crate::decode::{exif_orientation, open_upright};
use crate::sharpness::round;

/// The web-share rendition must stay under this (a chat app's limit, and the
/// number the photoquest spec fixes).
const SHARE_MAX_BYTES: usize = 10 * 1024 * 1024;

/// (name, width, height, JPEG bytes).
pub(crate) type Rendition = (&'static str, u32, u32, Vec<u8>);

/// Long edge of each rendition.
pub(crate) const RENDITIONS: [(&str, u32); 3] = [("thumb", 512), ("share", 4096), ("ai", 1568)];

/// The developed picture, upright, and which developer made it.
pub(crate) fn develop_cpu(path: &Path, ext: &str) -> Result<(DynamicImage, &'static str)> {
    if ext != "arw" {
        return Ok((open_upright(path)?, "jpeg"));
    }
    let params = rawler::decoders::RawDecodeParams::default();
    let src = rawler::rawsource::RawSource::new(path)?;
    let decoder = rawler::get_decoder(&src)?;
    let raw = decoder.raw_image(&src, &params, false)?;
    let o = exif_orientation(raw.orientation);
    let developed = rawler::imgop::develop::RawDevelop::default()
        .develop_intermediate(&raw)
        .map_err(anyhow::Error::from)
        .and_then(|i| i.to_dynamic_image().ok_or_else(|| anyhow!("develop produced no image")));
    match developed {
        Ok(img) => {
            let mut img = DynamicImage::ImageRgb8(img.to_rgb8());
            if let Some(o) = image::metadata::Orientation::from_exif(o) {
                img.apply_orientation(o);
            }
            Ok((img, "rawler"))
        }
        Err(e) => {
            // The camera's own JPEG is a worse picture but a real one; say so.
            eprintln!("comp-media: rawler develop failed ({e}); using the embedded preview");
            let preview = decoder
                .preview_image(&src, &params)?
                .or(decoder.thumbnail_image(&src, &params)?)
                .ok_or_else(|| anyhow!("no embedded preview either"))?;
            Ok((preview, "embedded-preview"))
        }
    }
}

fn encode_jpeg(img: &DynamicImage, quality: u8) -> Result<Vec<u8>> {
    let rgb = img.to_rgb8();
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, quality).write_image(
        rgb.as_raw(),
        rgb.width(),
        rgb.height(),
        image::ExtendedColorType::Rgb8,
    )?;
    Ok(out)
}

fn fit(img: &DynamicImage, long_edge: u32, filter: FilterType) -> DynamicImage {
    let (w, h) = (img.width(), img.height());
    if w.max(h) <= long_edge {
        return img.clone();
    }
    let s = long_edge as f64 / w.max(h) as f64;
    let (nw, nh) = (((w as f64 * s).round() as u32).max(1), ((h as f64 * s).round() as u32).max(1));
    img.resize_exact(nw, nh, filter)
}

/// The three renditions as (name, width, height, jpeg bytes). The share copy
/// is shrunk from the full picture once and the others from it — resampling
/// 61 MP three times is the slow part of the CPU path.
pub(crate) fn renditions_cpu(full: &DynamicImage) -> Result<Vec<Rendition>> {
    let share = fit(full, 4096, FilterType::Triangle);
    let mut out = Vec::new();
    for (name, edge) in RENDITIONS {
        let img =
            if name == "share" { share.clone() } else { fit(&share, edge, FilterType::Lanczos3) };
        let mut bytes = encode_jpeg(&img, if name == "thumb" { 80 } else { 85 })?;
        // A busy high-ISO frame can blow past the share cap at q85.
        let mut q = 85;
        while name == "share" && bytes.len() >= SHARE_MAX_BYTES && q > 50 {
            q -= 10;
            bytes = encode_jpeg(&img, q)?;
        }
        out.push((name, img.width(), img.height(), bytes));
    }
    Ok(out)
}

/// Exposure at a glance, from the AI copy: mean luma, the share of pixels
/// crushed to black or blown to white, and mean HSV saturation.
pub(crate) fn colour(ai_jpeg: &[u8]) -> Result<Value> {
    let img = image::load_from_memory(ai_jpeg)?.to_rgb8();
    let n = (img.width() as f64 * img.height() as f64).max(1.0);
    let (mut luma, mut dark, mut bright, mut sat) = (0f64, 0f64, 0f64, 0f64);
    for p in img.pixels() {
        let (r, g, b) = (p[0] as f64 / 255.0, p[1] as f64 / 255.0, p[2] as f64 / 255.0);
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        luma += y;
        if y <= 2.0 / 255.0 {
            dark += 1.0;
        }
        if y >= 253.0 / 255.0 {
            bright += 1.0;
        }
        let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
        if mx > 0.0 {
            sat += (mx - mn) / mx;
        }
    }
    Ok(json!({
        "mean_luma": round(luma / n, 4),
        "clipped_shadows_pct": round(dark / n * 100.0, 3),
        "clipped_highlights_pct": round(bright / n * 100.0, 3),
        "saturation_mean": round(sat / n, 4),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_of_a_flat_grey_picture() {
        let img =
            DynamicImage::ImageRgb8(image::RgbImage::from_pixel(8, 8, image::Rgb([128, 128, 128])));
        let c = colour(&encode_jpeg(&img, 95).unwrap()).unwrap();
        assert!((c["mean_luma"].as_f64().unwrap() - 0.502).abs() < 0.01);
        assert_eq!(c["clipped_highlights_pct"].as_f64().unwrap(), 0.0);
        assert!(c["saturation_mean"].as_f64().unwrap() < 0.02);
    }
}
