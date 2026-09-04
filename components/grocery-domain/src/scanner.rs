use serde_json::json;
use crate::bindings::barcode::read::reader::{self as barcode, ReadError};
use crate::bindings::wasi::http::types::IncomingRequest;
use crate::read_body;
use crate::store::load_products;
use crate::types::Outcome;

// Built-in barcode fixture PNGs from components/barcode-read/fixtures
pub static FIXTURE_EAN13: &[u8] = include_bytes!("../../barcode-read/fixtures/ean13.png");
pub static FIXTURE_EAN8: &[u8] = include_bytes!("../../barcode-read/fixtures/ean8.png");
pub static FIXTURE_UPCA: &[u8] = include_bytes!("../../barcode-read/fixtures/upca.png");
pub static FIXTURE_CODE128: &[u8] = include_bytes!("../../barcode-read/fixtures/code128.png");
pub static FIXTURE_CODE128_LETTERS: &[u8] = include_bytes!("../../barcode-read/fixtures/code128-letters.png");
pub static FIXTURE_EAN13_LEADING_ZERO: &[u8] = include_bytes!("../../barcode-read/fixtures/ean13-leading-zero.png");

pub fn get_fixture(filename: &str) -> Option<&'static [u8]> {
    match filename {
        "ean13.png" => Some(FIXTURE_EAN13),
        "ean8.png" => Some(FIXTURE_EAN8),
        "upca.png" => Some(FIXTURE_UPCA),
        "code128.png" => Some(FIXTURE_CODE128),
        "code128-letters.png" => Some(FIXTURE_CODE128_LETTERS),
        "ean13-leading-zero.png" => Some(FIXTURE_EAN13_LEADING_ZERO),
        _ => None,
    }
}

/// POST /api/scan — REAL WebAssembly Barcode Decoding (barcode:read/reader)
pub fn scan_barcode(request: &IncomingRequest) -> Outcome {
    let image_bytes = match read_body(request) {
        Ok(b) if !b.is_empty() => b,
        _ => return Outcome::Err(400, "Request body is empty or not readable".into()),
    };

    // Invoke pure compute WASI decoder:
    match barcode::decode_png(&image_bytes) {
        Ok(symbol) => {
            let products = load_products();
            let product = products.iter().find(|p| p.barcode == symbol.text).cloned();
            Outcome::Json(200, json!({
                "barcode": {
                    "text": symbol.text,
                    "symbology": symbol.symbology,
                },
                "product": product,
            }).to_string())
        }
        Err(ReadError::NotFound) => {
            Outcome::Err(404, "No barcode detected in image. Ensure the barcode is clear and steady.".into())
        }
        Err(ReadError::BadImage(msg)) => {
            Outcome::Err(400, format!("Invalid PNG image data: {msg}"))
        }
    }
}
