//! A capability with REAL behavior, so its manifest version is not just a number.
//! The behavior grows by version, and `capman`'s conformance gate refuses a
//! manifest that claims a version the code does not actually implement:
//!   1.0.0 — lowercase; runs of non-alphanumerics collapse to one hyphen, trimmed.
//!   1.1.0 — additionally, common accented Latin letters fold to ASCII.

/// Turn text into a URL slug.
pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut pending_hyphen = false;
    for c in s.chars() {
        let c = fold(c);
        if c.is_ascii_alphanumeric() {
            if pending_hyphen && !out.is_empty() {
                out.push('-');
            }
            pending_hyphen = false;
            out.push(c.to_ascii_lowercase());
        } else {
            pending_hyphen = true;
        }
    }
    out
}

/// Fold an accented letter to ASCII. At 1.0.0 this is the identity — the 1.1.0
/// improvement is to make it actually fold, which is what the loop implements.
fn fold(c: char) -> char {
    c
}
