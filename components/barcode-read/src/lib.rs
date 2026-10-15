//! `barcode-read` — a barcode image into its digits and its symbology
//!
//! EAN-13, EAN-8, UPC-A and Code 128, decoded from a PNG. Pure compute: no state, no host
//! imports. A linear barcode is one-dimensional, so this needs one line of pixels rather
//! than an image pipeline, which is why it fits `wasm32-wasip2` as a real component.
//!
//! The ceiling: rows then columns, each in both directions (upright, upside down, both
//! sideways). A label held at 30 degrees is NOT decoded (that needs angled scanlines or
//! finding the symbol's own axis); a shop app should ask the shopper to straighten up
//! rather than get a wrong number.
//!
//! The decoder is in `scan`; `tests/fixtures.rs` runs it against real rendered barcodes,
//! not its own encoder.

pub mod scan;
mod tables;

// Gated on the target, as `components/card-identify` is and for the same
// reason: a `cdylib` carrying wit-bindgen's exports does not link natively, and
// `tests/fixtures.rs` judges the plain decoder.

#[cfg(target_arch = "wasm32")]
#[allow(warnings)]
mod bindings;

#[cfg(target_arch = "wasm32")]
use bindings::exports::barcode::read::reader::{Guest, ReadError, Symbol};

#[cfg(target_arch = "wasm32")]
struct Component;

#[cfg(target_arch = "wasm32")]
impl Guest for Component {
    fn decode_png(image: Vec<u8>) -> Result<Symbol, ReadError> {
        match scan::decode_png(&image) {
            Ok(s) => Ok(Symbol { text: s.text, symbology: s.symbology }),
            // Kept apart on purpose: "not a photo" and "hold it steadier" are different
            // things to tell a shopper.
            Err(e) if e.starts_with("no barcode") => Err(ReadError::NotFound),
            Err(e) => Err(ReadError::BadImage(e)),
        }
    }
}

#[cfg(target_arch = "wasm32")]
bindings::export!(Component with_types_in bindings);
