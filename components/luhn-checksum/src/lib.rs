//! `luhn-checksum` — the mod-10 check digit used by credit-card numbers, IMEIs and
/// `double_rightmost`: a check digit will still be appended to the right.
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
            if twice > 9 {
                twice - 9
            } else {
                twice
            }
        } else {
            d
        };
    }
    Some(sum)
}

pub fn is_valid(digits: &str) -> bool {
    // The rightmost digit is the check digit itself and is never doubled.
    matches!(folded_sum(digits, false), Some(sum) if sum % 10 == 0)
}

pub fn checksum_digit(digits: &str) -> Option<u8> {
    // Once a digit is appended, every existing digit's doubling flips.
    let sum = folded_sum(digits, true)?;
    Some((10 - (sum % 10) as u8) % 10)
}

pub fn append_checksum(digits: &str) -> Option<String> {
    let check = checksum_digit(digits)?;
    Some(format!("{digits}{check}"))
}

// Thin WIT mapping only, so tests/luhn.rs covers what ships.

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
