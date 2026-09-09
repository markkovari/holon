//! `luhn-checksum` — the mod-10 check digit used by credit-card numbers, IMEIs and
//! national ID schemes.
//!
//! `tests/luhn.rs` is the specification and is not writable from here. None of the
//! three functions below is implemented yet.
//!
//! Pure compute: digits in, a bool/digit/string out.
//!
//! ## The algorithm, right to left
//!
//! Starting from the rightmost digit, double every second digit; if doubling
//! pushes a digit past 9, subtract 9 (same as summing its own two digits). Sum
//! everything. A number is valid iff that sum is a multiple of 10 — its own last
//! digit IS the check digit, already included in the sum.
//!
//! To compute a check digit for a number that does not have one yet, the new
//! digit becomes the last position once appended, so it is the one being solved
//! for rather than summed.
//!
//! Three things a first attempt gets wrong:
//!
//!   * a non-digit character or an empty string is invalid input, not a panic —
//!     every function here returns `false`/`None`, never unwraps a parse;
//!   * the parity of "which digits get doubled" is counted from the RIGHT, not
//!     the left — `"79927398713"` and `"7992739871"` double different positions;
//!   * doubling `9` gives `18`, which folds to `9`, not `8` — fold by subtracting
//!     9, not by taking a modulo.

/// Is `digits` a valid Luhn number (its own last digit is the check digit)?
pub fn is_valid(digits: &str) -> bool {
    unimplemented!("digits: {digits:?}")
}

/// The check digit that would make `digits` valid once appended, or `None` if
/// `digits` is empty or contains a non-digit character.
pub fn checksum_digit(digits: &str) -> Option<u8> {
    unimplemented!("digits: {digits:?}")
}

/// `digits` with its check digit appended, or `None` on the same bad input as
/// [`checksum_digit`].
pub fn append_checksum(digits: &str) -> Option<String> {
    unimplemented!("digits: {digits:?}")
}

// ---- the component -----------------------------------------------------
//
// A mapping between the WIT types and the ones above. `tests/luhn.rs` judges the
// plain functions; this adds no behaviour, which is the only way that specification
// keeps covering what actually ships.

#[cfg(target_arch = "wasm32")]
#[allow(warnings)]
mod bindings;

#[cfg(target_arch = "wasm32")]
use bindings::exports::luhn::checksum::checksum as w;

#[cfg(target_arch = "wasm32")]
struct Component;

#[cfg(target_arch = "wasm32")]
impl w::Guest for Component {
    fn is_valid(digits: String) -> bool {
        crate::is_valid(&digits)
    }
    fn checksum_digit(digits: String) -> Option<u8> {
        crate::checksum_digit(&digits)
    }
    fn append_checksum(digits: String) -> Option<String> {
        crate::append_checksum(&digits)
    }
}

#[cfg(target_arch = "wasm32")]
bindings::export!(Component with_types_in bindings);
