//! `iban` — reference implementation of `iban:validate/validator`.
//!
//! Normalize an IBAN (strip spaces, upper-case), check its length against the
//! country's rule (for known countries), and verify the ISO 7064 mod-97
//! checksum: move the first four characters to the end, map each letter to two
//! digits (A=10 … Z=35), and require the whole number ≡ 1 (mod 97). Pure compute.

#[allow(warnings)]
mod bindings;

use bindings::exports::iban::validate::validator::{Guest, IbanError, IbanInfo};

struct Component;

/// Country code -> total IBAN length (the common registry entries).
const LENGTHS: &[(&str, u32)] = &[
    ("AD", 24), ("AE", 23), ("AT", 20), ("BE", 16), ("BG", 22), ("BH", 22), ("BR", 29), ("CH", 21),
    ("CY", 28), ("CZ", 24), ("DE", 22), ("DK", 18), ("EE", 20), ("ES", 24), ("FI", 18), ("FO", 18),
    ("FR", 27), ("GB", 22), ("GE", 22), ("GI", 23), ("GL", 18), ("GR", 27), ("HR", 21), ("HU", 28),
    ("IE", 22), ("IL", 23), ("IS", 26), ("IT", 27), ("KW", 30), ("LB", 28), ("LI", 21), ("LT", 20),
    ("LU", 20), ("LV", 21), ("MC", 27), ("MD", 24), ("ME", 22), ("MK", 19), ("MT", 31), ("MU", 30),
    ("NL", 18), ("NO", 15), ("PL", 28), ("PT", 25), ("QA", 29), ("RO", 24), ("RS", 22), ("SA", 24),
    ("SE", 24), ("SI", 19), ("SK", 24), ("SM", 27), ("TN", 24), ("TR", 26), ("UA", 29), ("VA", 22),
];

fn expected_length(country: &str) -> Option<u32> {
    LENGTHS.iter().find(|(c, _)| *c == country).map(|(_, n)| *n)
}

/// Fold a run of decimal digits mod 97 (streaming, so no bignum needed).
fn mod97_step(rem: u32, digit: u32) -> u32 {
    (rem * 10 + digit) % 97
}

impl Guest for Component {
    fn validate(iban: String) -> Result<IbanInfo, IbanError> {
        // normalize: drop ASCII whitespace, upper-case.
        let norm: String = iban.chars().filter(|c| !c.is_whitespace()).map(|c| c.to_ascii_uppercase()).collect();
        if norm.len() < 5 {
            return Err(IbanError::TooShort);
        }
        let bytes = norm.as_bytes();
        let country = &norm[0..2];
        if !bytes[0].is_ascii_uppercase() || !bytes[1].is_ascii_uppercase() {
            return Err(IbanError::BadCountry(country.to_string()));
        }
        if !bytes[2].is_ascii_digit() || !bytes[3].is_ascii_digit() {
            return Err(IbanError::BadCheck); // check digits must be numeric
        }
        // only letters + digits allowed anywhere.
        for &b in bytes {
            if !b.is_ascii_alphanumeric() {
                return Err(IbanError::BadChar((b as char).to_string()));
            }
        }
        if let Some(exp) = expected_length(country) {
            if norm.len() as u32 != exp {
                return Err(IbanError::BadLength((norm.len() as u32, exp)));
            }
        }

        // mod-97: move the first 4 chars to the end, expand letters to two
        // digits, fold mod 97 as we go.
        let mut rem = 0u32;
        let feed = |rem: &mut u32, c: u8| {
            if c.is_ascii_digit() {
                *rem = mod97_step(*rem, (c - b'0') as u32);
            } else {
                let v = (c - b'A' + 10) as u32; // 10..=35
                *rem = mod97_step(*rem, v / 10);
                *rem = mod97_step(*rem, v % 10);
            }
        };
        for &c in &bytes[4..] {
            feed(&mut rem, c);
        }
        for &c in &bytes[0..4] {
            feed(&mut rem, c);
        }
        if rem != 1 {
            return Err(IbanError::BadCheck);
        }

        // group in fours for display.
        let formatted = norm
            .as_bytes()
            .chunks(4)
            .map(|c| std::str::from_utf8(c).unwrap_or(""))
            .collect::<Vec<_>>()
            .join(" ");

        Ok(IbanInfo {
            country: country.to_string(),
            check_digits: norm[2..4].to_string(),
            bban: norm[4..].to_string(),
            formatted,
            length: norm.len() as u32,
        })
    }
}

bindings::export!(Component with_types_in bindings);
