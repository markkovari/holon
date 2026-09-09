//! `luhn-checksum` — the mod-10 check digit used by credit-card numbers, IMEIs and
//! national ID schemes.
//!
//! `tests/luhn.rs` is the specification and is not writable from here.
//!
//! Pure compute: digits in, a bool/digit/string out.

/// Sums the digits of `digits`, doubling every second one counted from the
/// right. `double_rightmost` controls whether the rightmost digit itself is
/// among the doubled ones (used by `checksum_digit`, where an as-yet-unwritten
/// digit will land to the right of everything here). Returns `None` on empty
/// input or a non-digit byte.
fn folded_sum(digits: &str, double_rightmost: bool) -> Option<u32> {
    if digits.is_empty() {
        return None;
    }
    let mut sum: u32 = 0;
    for (i, b) in digits.bytes().rev().enumerate() {
        if !b.is_ascii_digit() {
            return None;
        }
        let d = (b - b'0') as u32;
        let doubles = (i % 2 == 0) == double_rightmost;
        sum += if doubles {
            let twice = d * 2;
            if twice > 9 { twice - 9 } else { twice }
        } else {
            d
        };
    }
    Some(sum)
}

/// Is `digits` a valid Luhn number (its own last digit is the check digit)?
pub fn is_valid(digits: &str) -> bool {
    // The rightmost digit is the check digit itself and is never doubled.
    matches!(folded_sum(digits, false), Some(sum) if sum % 10 == 0)
}

/// The check digit that would make `digits` valid once appended, or `None` if
/// `digits` is empty or contains a non-digit character.
pub fn checksum_digit(digits: &str) -> Option<u8> {
    // Once a digit is appended, every existing digit's doubling flips (the
    // rightmost existing digit is no longer the rightmost overall).
    let sum = folded_sum(digits, true)?;
    Some(((10 - (sum % 10) as u8) % 10) as u8)
}

/// `digits` with its check digit appended, or `None` on the same bad input as
/// [`checksum_digit`].
pub fn append_checksum(digits: &str) -> Option<String> {
    let check = checksum_digit(digits)?;
    Some(format!("{digits}{check}"))
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
