//! `barcode-read` — a barcode image into its digits and its symbology
//!
//! EAN-13, EAN-8, UPC-A and Code 128, decoded from a PNG. Pure compute: no
//! state, no host imports, no dataset, no model. A linear barcode is
//! one-dimensional — the height exists only so a scanner sweeping at an angle
//! still crosses the whole symbol — so this needs one line of pixels rather than
//! an image pipeline, which is why it fits in `wasm32-wasip2` as a real
//! component and not as another contract with nothing behind it.
//!
//! ## The ceiling, stated
//!
//! Orientation is handled by reading rows, then columns, each in both
//! directions: upright, upside down and both sideways. A label held at 30
//! degrees is NOT decoded. Fixing that means projecting scanlines at angles or
//! finding the symbol's own axis first, and neither is written here — a shop
//! app should ask the shopper to straighten up rather than get a wrong number.
//!
//! The decoder is in `scan`, and `tests/fixtures.rs` runs it against real
//! rendered barcodes rather than against its own encoder.

pub mod scan;
mod tables;

// ---- the component -----------------------------------------------------
//
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
            // The two failures are kept apart on purpose: "that is not a photo"
            // and "hold it steadier" are different things to tell a shopper, and
            // one string for both makes the app guess.
            Err(e) if e.starts_with("no barcode") => Err(ReadError::NotFound),
            Err(e) => Err(ReadError::BadImage(e)),
        }
    }
}

#[cfg(target_arch = "wasm32")]
bindings::export!(Component with_types_in bindings);
