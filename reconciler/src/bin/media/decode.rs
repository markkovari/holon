//! Metadata and the green plane of a camera raw file or a JPEG original.

use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use image::{DynamicImage, ImageDecoder};
use serde_json::{json, Value};

use crate::sharpness::round;

/// What the decode stage hands the rest of the job.
pub(crate) struct Decoded {
    pub(crate) metadata: Value,
    /// Half-resolution green plane, linear 0–1, cropped to the picture and
    /// turned the way the picture is displayed — so a tile's position means
    /// the same thing as a Vision box on the developed image.
    pub(crate) green: Vec<f32>,
    pub(crate) gw: usize,
    pub(crate) gh: usize,
}

/// EXIF orientation 1–8 from rawler's enum.
pub(crate) fn exif_orientation(o: rawler::decoders::Orientation) -> u8 {
    use rawler::decoders::Orientation::*;
    match o {
        Normal | Unknown => 1,
        HorizontalFlip => 2,
        Rotate180 => 3,
        VerticalFlip => 4,
        Transpose => 5,
        Rotate90 => 6,
        Transverse => 7,
        Rotate270 => 8,
    }
}

/// Apply EXIF orientation `o` to a `w`×`h` plane. Returns the new plane and
/// its dimensions (swapped for 5–8).
fn orient_plane(p: &[f32], w: usize, h: usize, o: u8) -> (Vec<f32>, usize, usize) {
    if o <= 1 || o > 8 {
        return (p.to_vec(), w, h);
    }
    let (ow, oh) = if o >= 5 { (h, w) } else { (w, h) };
    let mut out = vec![0f32; p.len()];
    for y in 0..oh {
        for x in 0..ow {
            let (sx, sy) = match o {
                2 => (w - 1 - x, y),
                3 => (w - 1 - x, h - 1 - y),
                4 => (x, h - 1 - y),
                5 => (y, x),
                6 => (y, h - 1 - x),
                7 => (w - 1 - y, h - 1 - x),
                _ => (w - 1 - y, x), // 8
            };
            out[y * ow + x] = p[sy * w + sx];
        }
    }
    (out, ow, oh)
}

fn rational(r: &Option<rawler::formats::tiff::Rational>) -> Value {
    match r {
        Some(r) if r.d != 0 => json!(round(r.n as f64 / r.d as f64, 6)),
        _ => Value::Null,
    }
}

/// `2026:09:23 13:11:47` -> `2026-09-23T13:11:47`: the camera clock, no zone.
/// Anything else — blank, all-spaces, a stray multi-byte character — is null.
fn exif_datetime(s: &str) -> Option<String> {
    let (date, time) = s.trim().split_once(' ')?;
    let time = time.get(..8)?;
    // `d` is a digit, anything else must match exactly.
    let shaped = |v: &str, pat: &str| {
        v.len() == pat.len()
            && v.bytes()
                .zip(pat.bytes())
                .all(|(b, p)| if p == b'd' { b.is_ascii_digit() } else { b == p })
    };
    let ok = shaped(date, "dddd:dd:dd") && shaped(time, "dd:dd:dd") && !date.starts_with("0000");
    ok.then(|| format!("{}T{}", date.replace(':', "-"), time))
}

/// Metadata and the green plane of a camera raw file.
pub(crate) fn decode_raw(path: &Path) -> Result<Decoded> {
    use rawler::rawimage::{RawImageData, RawPhotometricInterpretation};
    let params = rawler::decoders::RawDecodeParams::default();
    let src = rawler::rawsource::RawSource::new(path).context("open raw")?;
    let decoder = rawler::get_decoder(&src)?;
    let meta = decoder.raw_metadata(&src, &params)?;
    let raw = decoder.raw_image(&src, &params, false)?;
    let RawPhotometricInterpretation::Cfa(cfa) = &raw.photometric else {
        bail!("not a colour-filter-array raw: {:?}", raw.photometric);
    };
    let RawImageData::Integer(data) = &raw.data else {
        bail!("floating-point raw data is not handled");
    };
    let w = raw.width;
    let white = *raw.whitelevel.0.first().unwrap_or(&65535) as f32;
    let black = raw.blacklevel.levels.first().map(|r| r.as_f32()).unwrap_or(0.0);

    // Only the picture: the sensor's masked borders are black, and a black
    // edge is the sharpest thing in any frame.
    let area = raw.crop_area.or(raw.active_area);
    let (x0, y0, cw, ch) = match area {
        Some(r) => (r.p.x & !1, r.p.y & !1, r.d.w, r.d.h),
        None => (0, 0, w, raw.height),
    };
    let (gw, gh) = (cw / 2, ch / 2);
    let mut green = vec![0f32; gw * gh];
    let range = (white - black).max(1.0);
    for gy in 0..gh {
        for gx in 0..gw {
            let (mut sum, mut n) = (0f32, 0f32);
            for dy in 0..2 {
                for dx in 0..2 {
                    let (y, x) = (y0 + gy * 2 + dy, x0 + gx * 2 + dx);
                    if cfa.cfa.color_at(y, x) == 1 {
                        sum += data[y * w + x] as f32;
                        n += 1.0;
                    }
                }
            }
            green[gy * gw + gx] = ((sum / n.max(1.0) - black) / range).clamp(0.0, 1.0);
        }
    }
    let o = exif_orientation(raw.orientation);
    let (green, gw, gh) = orient_plane(&green, gw, gh, o);
    let (pw, ph) = if o >= 5 { (ch, cw) } else { (cw, ch) };

    let ex = &meta.exif;
    let camera = format!("{} {}", meta.make, meta.model).trim().to_string();
    let metadata = json!({
        "camera": camera,
        "lens": ex.lens_model.clone().or_else(|| meta.lens.as_ref().map(|l| l.lens_name.clone())),
        "captured_at": ex.date_time_original.as_deref().and_then(exif_datetime),
        "exposure_s": rational(&ex.exposure_time),
        "fnumber": rational(&ex.fnumber),
        "focal_mm": rational(&ex.focal_length),
        "iso": ex.iso_speed_ratings.map(u32::from).or(ex.iso_speed),
        "width": pw,
        "height": ph,
    });
    Ok(Decoded { metadata, green, gw, gh })
}

/// A JPEG original: its EXIF (when it has any) is the metadata, and the
/// "green plane" is the G channel, linearised from sRGB.
pub(crate) fn decode_jpeg(path: &Path) -> Result<Decoded> {
    let img = open_upright(path)?.to_rgb8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    let green = img.pixels().map(|p| srgb_to_linear(p[1] as f32 / 255.0)).collect();
    let mut metadata = jpeg_metadata(path).unwrap_or_else(|e| {
        // No EXIF, or EXIF too broken to read: the picture is still a picture.
        eprintln!("comp-media: no readable EXIF in the JPEG ({e:#})");
        json!({
            "camera": null, "lens": null, "captured_at": null, "exposure_s": null,
            "fnumber": null, "focal_mm": null, "iso": null, "width": null, "height": null,
        })
    });
    metadata["width"] = json!(w);
    metadata["height"] = json!(h);
    Ok(Decoded { metadata, green, gw: w, gh: h })
}

/// The metadata fields a JPEG's EXIF gives, `width`/`height` left for the
/// caller (the upright decoded size, not whatever PixelXDimension claims).
/// The JPEG decoder that decodes the pixels also finds the APP1 segment.
fn jpeg_metadata(path: &Path) -> Result<Value> {
    let mut decoder = image::ImageReader::open(path)?.with_guessed_format()?.into_decoder()?;
    let chunk = decoder.exif_metadata()?.ok_or_else(|| anyhow!("no EXIF segment"))?;
    exif_chunk_metadata(&chunk)
}

/// Parse a raw EXIF chunk (a TIFF header and IFDs, optionally still behind
/// the `Exif\0\0` APP1 marker). kamadak-exif rather than rawler's TIFF
/// reader: rawler allocates whatever an entry's count claims before
/// reading it, and a 64 KB APP1 segment must not be able to ask for 32 GB.
/// A partly broken chunk still gives the fields that did parse.
fn exif_chunk_metadata(chunk: &[u8]) -> Result<Value> {
    use exif::{In, Tag};
    let tiff = chunk.strip_prefix(b"Exif\0\0").unwrap_or(chunk).to_vec();
    let ex = exif::Reader::new()
        .continue_on_error(true)
        .read_raw(tiff)
        .or_else(|e| e.distill_partial_result(|_| {}))
        .map_err(|e| anyhow!("EXIF: {e}"))?;
    let field = |t| ex.get_field(t, In::PRIMARY).map(|f| &f.value);
    let text = |t| match field(t) {
        Some(exif::Value::Ascii(v)) => {
            v.first().map(|s| String::from_utf8_lossy(s).trim().to_string())
        }
        _ => None,
    };
    let ratio = |t| match field(t) {
        Some(exif::Value::Rational(v)) => {
            v.first().filter(|r| r.denom != 0).map(|r| json!(round(r.to_f64(), 6)))
        }
        _ => None,
    };
    let uint = |t| field(t).and_then(|v| v.get_uint(0));
    let camera =
        compose_camera(&text(Tag::Make).unwrap_or_default(), &text(Tag::Model).unwrap_or_default());
    Ok(json!({
        "camera": camera,
        "lens": text(Tag::LensModel).filter(|s| !s.is_empty()),
        "captured_at": text(Tag::DateTimeOriginal).as_deref().and_then(exif_datetime),
        "exposure_s": ratio(Tag::ExposureTime),
        "fnumber": ratio(Tag::FNumber),
        "focal_mm": ratio(Tag::FocalLength),
        // ISOSpeedRatings, renamed PhotographicSensitivity in EXIF 2.3.
        "iso": uint(Tag::PhotographicSensitivity).or_else(|| uint(Tag::ISOSpeed)),
        "width": null,
        "height": null,
    }))
}

/// `Make` + `Model` the way the ARW path shows them: rawler's clean names
/// from its camera table when it knows the body ("SONY" + "ILCE-7RM5" is
/// "Sony ILCE-7RM5", as on the ARW), else the EXIF strings as written,
/// minus a repeated make ("Canon" + "Canon EOS R5" is "Canon EOS R5").
fn compose_camera(make: &str, model: &str) -> Option<String> {
    let (make, model) = (make.trim(), model.trim());
    let known = rawler::global_loader()
        .get_cameras()
        .iter()
        .filter(|((mk, md, _), _)| mk == make && md == model)
        .min_by_key(|((_, _, mode), _)| !mode.is_empty())
        .map(|(_, cam)| (cam.clean_make.as_str(), cam.clean_model.as_str()));
    let (make, model) = known.unwrap_or((make, model));
    let camera = if !make.is_empty() && model.to_lowercase().starts_with(&make.to_lowercase()) {
        model.to_string()
    } else {
        format!("{make} {model}").trim().to_string()
    };
    (!camera.is_empty()).then_some(camera)
}

/// Decode a JPEG and turn it the way its EXIF says — Core Image does the same
/// on the Apple path, and the plane and the renditions must agree with it.
pub(crate) fn open_upright(path: &Path) -> Result<DynamicImage> {
    let mut decoder = image::ImageReader::open(path)?.with_guessed_format()?.into_decoder()?;
    let orientation = decoder.orientation()?;
    let mut img = DynamicImage::from_decoder(decoder)?;
    img.apply_orientation(orientation);
    Ok(img)
}

fn srgb_to_linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod tests {
    use image::codecs::jpeg::JpegEncoder;
    use image::ImageEncoder;

    use super::*;

    /// Orientation 6 turns the plane a quarter clockwise: the source's
    /// bottom-left pixel becomes the top-left.
    #[test]
    fn orienting_a_plane_moves_pixels_the_way_exif_says() {
        // 3 wide, 2 high:  a b c / d e f
        let p = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        assert_eq!(orient_plane(&p, 3, 2, 6), (vec![4.0, 1.0, 5.0, 2.0, 6.0, 3.0], 2, 3));
        assert_eq!(orient_plane(&p, 3, 2, 8), (vec![3.0, 6.0, 2.0, 5.0, 1.0, 4.0], 2, 3));
        assert_eq!(orient_plane(&p, 3, 2, 3).0, vec![6.0, 5.0, 4.0, 3.0, 2.0, 1.0]);
        assert_eq!(orient_plane(&p, 3, 2, 1).0, p.to_vec());
    }

    #[test]
    fn exif_dates_become_iso_without_a_zone() {
        assert_eq!(exif_datetime("2026:09:23 13:11:47").as_deref(), Some("2026-09-23T13:11:47"));
        assert_eq!(exif_datetime("garbage"), None);
    }

    #[test]
    fn exif_datetime_rejects_what_is_not_a_camera_clock() {
        assert_eq!(
            exif_datetime("2026:09:23 13:11:47.123").as_deref(),
            Some("2026-09-23T13:11:47")
        );
        assert_eq!(exif_datetime("    :  :     :  :  "), None);
        assert_eq!(exif_datetime("0000:00:00 00:00:00"), None);
        assert_eq!(exif_datetime("2026:09:23 13:11:4\u{e9}"), None);
        assert_eq!(exif_datetime("2026-09-23 13:11:47"), None);
        assert_eq!(exif_datetime(""), None);
    }

    /// A TIFF value for the test EXIF writer.
    enum V {
        A(&'static str),
        R(u32, u32),
        S(u16),
    }

    /// (type, count, bytes) as TIFF stores them, little-endian.
    fn tiff_value(v: &V) -> (u16, u32, Vec<u8>) {
        match v {
            V::A(s) => (2, s.len() as u32 + 1, [s.as_bytes(), &[0]].concat()),
            V::R(n, d) => (5, 1, [n.to_le_bytes(), d.to_le_bytes()].concat()),
            V::S(x) => (3, 1, x.to_le_bytes().to_vec()),
        }
    }

    fn tiff_ifd(out: &mut Vec<u8>, data: &mut Vec<u8>, data_base: usize, entries: &[(u16, V)]) {
        out.extend((entries.len() as u16).to_le_bytes());
        for (tag, v) in entries {
            let (ty, count, bytes) = tiff_value(v);
            out.extend(tag.to_le_bytes());
            out.extend(ty.to_le_bytes());
            out.extend(count.to_le_bytes());
            if bytes.len() <= 4 {
                let mut inline = bytes.clone();
                inline.resize(4, 0);
                out.extend(inline);
            } else {
                out.extend(((data_base + data.len()) as u32).to_le_bytes());
                data.extend(&bytes);
                if data.len() % 2 == 1 {
                    data.push(0);
                }
            }
        }
        out.extend(0u32.to_le_bytes());
    }

    /// A minimal EXIF chunk: IFD0 (tags sorted, the Exif IFD pointer
    /// appended) and an Exif sub-IFD (tags sorted).
    fn tiff(ifd0: Vec<(u16, V)>, exif: Vec<(u16, V)>) -> Vec<u8> {
        let exif_at = 8 + 2 + 12 * (ifd0.len() + 1) + 4;
        let data_at = exif_at + 2 + 12 * exif.len() + 4;
        let mut ifd0 = ifd0;
        ifd0.push((0x8769, V::S(0))); // placeholder, patched below
        let mut out = b"II\x2a\x00".to_vec();
        out.extend(8u32.to_le_bytes());
        let mut data = Vec::new();
        tiff_ifd(&mut out, &mut data, data_at, &ifd0);
        // The pointer is a LONG holding the Exif IFD's offset.
        let ptr = 8 + 2 + 12 * (ifd0.len() - 1);
        out[ptr + 2..ptr + 4].copy_from_slice(&4u16.to_le_bytes());
        out[ptr + 8..ptr + 12].copy_from_slice(&(exif_at as u32).to_le_bytes());
        tiff_ifd(&mut out, &mut data, data_at, &exif);
        assert_eq!(out.len(), data_at);
        out.extend(data);
        out
    }

    /// A 16x8 JPEG on disk, with `exif` as its APP1 segment when given.
    /// A uniquely named temp file (created securely by `tempfile`, not a
    /// predictable name in the shared temp dir), deleted when the path drops.
    fn jpeg_file(name: &str, exif: Option<Vec<u8>>) -> tempfile::TempPath {
        let img = image::RgbImage::from_fn(16, 8, |x, y| {
            image::Rgb([(x * 16) as u8, (y * 32) as u8, 90])
        });
        let mut bytes = Vec::new();
        let mut enc = JpegEncoder::new_with_quality(&mut bytes, 90);
        if let Some(exif) = exif {
            enc.set_exif_metadata(exif).unwrap();
        }
        enc.write_image(img.as_raw(), 16, 8, image::ExtendedColorType::Rgb8).unwrap();
        let mut file = tempfile::Builder::new()
            .prefix(&format!("comp-media-test-{name}-"))
            .suffix(".jpg")
            .tempfile()
            .unwrap();
        std::io::Write::write_all(&mut file, &bytes).unwrap();
        file.into_temp_path()
    }

    #[test]
    fn a_jpeg_originals_exif_is_its_metadata() {
        let exif = tiff(
            vec![(0x010F, V::A("SONY")), (0x0110, V::A("ILCE-7RM5"))],
            vec![
                (0x829A, V::R(1, 250)),
                (0x829D, V::R(28, 10)),
                (0x8827, V::S(400)),
                (0x9003, V::A("2026:05:01 09:30:15")),
                (0x920A, V::R(500, 10)),
                (0xA434, V::A("FE 24-70mm F2.8 GM II")),
            ],
        );
        let path = jpeg_file("exif", Some(exif));
        let m = decode_jpeg(&path).unwrap().metadata;
        std::fs::remove_file(&path).ok();
        assert_eq!(
            m,
            json!({
                "camera": "Sony ILCE-7RM5", "lens": "FE 24-70mm F2.8 GM II",
                "captured_at": "2026-05-01T09:30:15", "exposure_s": 0.004, "fnumber": 2.8,
                "focal_mm": 50.0, "iso": 400, "width": 16, "height": 8,
            })
        );
    }

    #[test]
    fn a_missing_exif_tag_is_null_on_its_own() {
        // An unknown body keeps its EXIF strings; a model repeating the make
        // does not say it twice; only the tags present are filled.
        let exif = tiff(
            vec![(0x010F, V::A("Acme")), (0x0110, V::A("Acme Box 1")), (0x0112, V::S(6))],
            vec![(0x829D, V::R(8, 1))],
        );
        let path = jpeg_file("partial", Some(exif));
        let m = decode_jpeg(&path).unwrap().metadata;
        std::fs::remove_file(&path).ok();
        assert_eq!(
            m,
            json!({
                "camera": "Acme Box 1", "lens": null, "captured_at": null, "exposure_s": null,
                "fnumber": 8.0, "focal_mm": null, "iso": null,
                // Orientation 6: the upright picture is 8 wide.
                "width": 8, "height": 16,
            })
        );
    }

    #[test]
    fn a_jpeg_without_exif_has_only_its_size() {
        let path = jpeg_file("plain", None);
        let m = decode_jpeg(&path).unwrap().metadata;
        std::fs::remove_file(&path).ok();
        assert_eq!(
            m,
            json!({
                "camera": null, "lens": null, "captured_at": null, "exposure_s": null,
                "fnumber": null, "focal_mm": null, "iso": null, "width": 16, "height": 8,
            })
        );
    }

    #[test]
    fn broken_exif_is_no_metadata_not_a_failed_job() {
        let all_null = |v: &Value| v.as_object().unwrap().values().all(Value::is_null);
        for chunk in [&b"Exif\0\0II\x2a\x00\xff\xff\xff\x7f"[..], b"", b"not a tiff at all"] {
            if let Ok(v) = exif_chunk_metadata(chunk) {
                assert!(all_null(&v), "{v}");
            }
        }
        let mut truncated =
            tiff(vec![(0x010F, V::A("SONY"))], vec![(0x9003, V::A("2026:05:01 09:30:15"))]);
        truncated.truncate(truncated.len() - 12);
        let _ = exif_chunk_metadata(&truncated); // must not panic

        // An entry claiming four billion rationals is dropped without asking
        // for 32 GB; the fields around it still read.
        let mut huge = tiff(
            vec![(0x010F, V::A("Acme")), (0x0110, V::A("Box"))],
            vec![(0x829A, V::R(1, 250)), (0x829D, V::R(4, 1))],
        );
        let exif_at = 8 + 2 + 12 * 3 + 4;
        huge[exif_at + 2 + 4..exif_at + 2 + 8].copy_from_slice(&u32::MAX.to_le_bytes());
        let v = exif_chunk_metadata(&huge).unwrap();
        assert_eq!(
            (v["camera"].as_str(), v["exposure_s"].is_null(), v["fnumber"].as_f64()),
            (Some("Acme Box"), true, Some(4.0))
        );
    }
}
