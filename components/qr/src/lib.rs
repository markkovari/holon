//! `qr` — turn a URL or a string into a scannable QR code, as SVG
//!
//! The `qrcode` crate does the hard part (segment encoding, Reed-Solomon error
//! correction, mask selection); we take its module grid and render it three
//! ways — a self-contained SVG, compact unicode blocks, and the raw matrix as
//! JSON. Rendering from the grid ourselves keeps the dependency to the encoder
//! only (no image/font features) and lets `svg` honor an arbitrary quiet zone.
//!
//! Pure compute, no host imports, no state.

#[allow(warnings)]
mod bindings;

use bindings::exports::qr::encode::encoder::{Ecc, Guest, QrError};
use qrcode::{Color, EcLevel, QrCode};

struct Component;

/// Encode into (module count per side, dark-flags row-major).
fn grid(data: &str, level: Ecc) -> Result<(usize, Vec<bool>), QrError> {
    let ec = match level {
        Ecc::Low => EcLevel::L,
        Ecc::Medium => EcLevel::M,
        Ecc::Quartile => EcLevel::Q,
        Ecc::High => EcLevel::H,
    };
    let code = QrCode::with_error_correction_level(data.as_bytes(), ec)
        .map_err(|e| QrError::TooLong(format!("{e:?}")))?;
    let width = code.width();
    let modules = code.to_colors().into_iter().map(|c| c == Color::Dark).collect();
    Ok((width, modules))
}

impl Guest for Component {
    fn svg(data: String, level: Ecc, quiet_zone: u32) -> Result<String, QrError> {
        let (w, m) = grid(&data, level)?;
        let qz = quiet_zone as usize;
        let dim = w + 2 * qz;
        // One <path> of unit squares for the dark modules over a white ground;
        // viewBox makes it scale to any rendered size.
        let mut path = String::new();
        for y in 0..w {
            for x in 0..w {
                if m[y * w + x] {
                    path.push_str(&format!("M{} {}h1v1h-1z", x + qz, y + qz));
                }
            }
        }
        Ok(format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {dim} {dim}\" \
             shape-rendering=\"crispEdges\"><rect width=\"{dim}\" height=\"{dim}\" \
             fill=\"#fff\"/><path fill=\"#000\" d=\"{path}\"/></svg>"
        ))
    }

    fn unicode(data: String, level: Ecc) -> Result<String, QrError> {
        let (w, m) = grid(&data, level)?;
        let dark = |x: i64, y: i64| -> bool {
            x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < w && m[y as usize * w + x as usize]
        };
        // One character encodes two vertical modules. Add a 1-cell light border.
        let mut out = String::new();
        let mut y = -1i64;
        while y < w as i64 + 1 {
            for x in -1..w as i64 + 1 {
                out.push(match (dark(x, y), dark(x, y + 1)) {
                    (true, true) => '█',
                    (true, false) => '▀',
                    (false, true) => '▄',
                    (false, false) => ' ',
                });
            }
            out.push('\n');
            y += 2;
        }
        Ok(out)
    }

    fn matrix(data: String, level: Ecc) -> Result<String, QrError> {
        let (w, m) = grid(&data, level)?;
        let mut out = format!("{{\"size\":{w},\"modules\":[");
        for y in 0..w {
            if y > 0 {
                out.push(',');
            }
            out.push('[');
            for x in 0..w {
                if x > 0 {
                    out.push(',');
                }
                out.push_str(if m[y * w + x] { "true" } else { "false" });
            }
            out.push(']');
        }
        out.push_str("]}");
        Ok(out)
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_is_wellformed_and_scalable() {
        let s = <Component as Guest>::svg("https://example.com".into(), Ecc::Medium, 4).unwrap();
        assert!(s.starts_with("<svg"));
        assert!(s.contains("viewBox=\"0 0"));
        assert!(s.contains("<path fill=\"#000\""));
        assert!(s.ends_with("</svg>"));
    }

    #[test]
    fn matrix_is_square_and_nonempty() {
        let j = <Component as Guest>::matrix("hi".into(), Ecc::Low).unwrap();
        assert!(j.starts_with("{\"size\":"));
        // at least one dark module
        assert!(j.contains("true"));
    }

    #[test]
    fn unicode_renders_blocks() {
        let u = <Component as Guest>::unicode("hi".into(), Ecc::Low).unwrap();
        assert!(u.contains('█') || u.contains('▀') || u.contains('▄'));
    }

    #[test]
    fn higher_ecc_is_denser() {
        // same data, more recovery -> at least as many modules per side
        let low = <Component as Guest>::matrix("payload".into(), Ecc::Low).unwrap();
        let high = <Component as Guest>::matrix("payload".into(), Ecc::High).unwrap();
        let size = |s: &str| s[s.find(':').unwrap() + 1..s.find(',').unwrap()].parse::<usize>().unwrap();
        assert!(size(&high) >= size(&low));
    }

    #[test]
    fn too_long_input_errors() {
        let huge = "x".repeat(8000);
        assert!(matches!(
            <Component as Guest>::svg(huge, Ecc::High, 2),
            Err(QrError::TooLong(_))
        ));
    }
}
