//! `guestfmt` — the two wire-format helpers components kept rewriting, and the bug that hid in one
//!
//! A percent escape encodes a BYTE. The broken shape (`out.push(b as char)`, in 7 of 28 hand-written
//! decoders) read it as a Unicode code point, so `caf%C3%A9` came back `cafÃ©`. ASCII is unaffected,
//! which is why it survived. The right shape collects bytes and decodes once at the end.
//!
//! ```text
//! input                       byte-correct     the broken shape
//! caf%C3%A9                   café             cafÃ©
//! M%C3%A1rk+K%C5%91v%C3%A1ri  Márk Kővári      MÃ¡rk KÅvÃ¡ri
//! ```
//!
//! Unlike [`guestio`](../guestio) these are plain functions: nothing here takes a bindings type.

/// Decode `application/x-www-form-urlencoded` text: `%XX` escapes and `+` for space.
///
/// Bytes are collected and decoded as UTF-8 once, at the end: one character is often several escapes.
///
/// Invalid UTF-8 becomes the replacement character rather than an error: a query string is
/// attacker-supplied and callers want a lossy-but-present value. A `%` not followed by two hex digits
/// is left alone (a literal `%` in user text is common).
///
/// The `+`-to-space pass runs FIRST and that ordering is load-bearing: `u8::from_str_radix` accepts a
/// leading sign, so `"+a"` parses as `+0x0a` and a one-pass decoder turned `%+a` into a newline, letting a
/// caller inject control bytes. Replacing `+` before parsing means no `+` reaches the radix call.
pub fn percent_decode(s: &str) -> String {
    let plus = s.replace('+', " ");
    let b = plus.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match (b[i], b.get(i + 1), b.get(i + 2)) {
            (b'%', Some(h), Some(l)) => {
                match u8::from_str_radix(core::str::from_utf8(&[*h, *l]).unwrap_or("zz"), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b[i]);
                        i += 1;
                    }
                }
            }
            _ => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Format Unix seconds as RFC 3339 UTC — `2026-08-29T18:12:02Z`.
///
/// Howard Hinnant's `civil_from_days`: shift the year to start in March so the leap day lands at the end
/// and month lengths become arithmetic. Always UTC, second precision; for offsets or milliseconds use a real date library.
pub fn rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let tod = secs % 86_400;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// Pull the token out of an `Authorization` header value.
///
/// Hand-written copies disagreed on what RFC 7235 and RFC 6750 specify:
///
///   * **The scheme name is case-insensitive** (RFC 7235 §2.1): matching the literal `"Bearer "` rejected a
///     legal `authorization: bearer <token>` as no credential, an unexplained 401.
///   * **Whitespace around the token is not part of it.**
///
/// Returns a borrow of the input, and `None` when the scheme is not Bearer, there is no token, or it is
/// empty (an empty credential is an absent one). The value only, not the header lookup, which needs a
/// bindings type and lives in `guestio::guest_bearer!`.
pub fn bearer_token(header_value: &str) -> Option<&str> {
    let (scheme, rest) = header_value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = rest.trim();
    (!token.is_empty()).then_some(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The case that separated the two shapes. Kept as a test because "ASCII still works" let the broken one survive.
    #[test]
    fn a_multi_byte_character_survives_decoding() {
        assert_eq!(percent_decode("caf%C3%A9"), "café");
        assert_eq!(percent_decode("M%C3%A1rk+K%C5%91v%C3%A1ri"), "Márk Kővári");
        assert_eq!(percent_decode("%E2%82%AC5"), "€5");
    }

    #[test]
    fn plus_is_a_space_and_2b_is_a_plus() {
        assert_eq!(percent_decode("a+b"), "a b");
        assert_eq!(percent_decode("a%2Bb"), "a+b");
    }

    /// A literal `%` that is not an escape is left alone rather than dropped.
    #[test]
    fn a_malformed_escape_is_kept_verbatim() {
        assert_eq!(percent_decode("100%25"), "100%");
        assert_eq!(percent_decode("tail%"), "tail%");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("50% off"), "50% off");
    }

    /// `from_str_radix` accepts a sign, so a one-pass decoder reads `%+a` as `+0x0a` and emits a newline.
    /// Found by fuzzing the two shapes against each other. Not about UTF-8: a caller injecting a control byte.
    #[test]
    fn a_signed_escape_is_not_a_number() {
        assert_eq!(percent_decode("%+a"), "% a");
        assert_eq!(percent_decode("%+A"), "% A");
        assert_eq!(percent_decode("x%+dy"), "x% dy");
        // `-` is rejected by the radix parse on its own, so it was never the dangerous one; asserted so the pair stays a pair.
        assert_eq!(percent_decode("%-a"), "%-a");
    }

    /// RFC 7235 §2.1 makes the scheme name case-insensitive.
    #[test]
    fn the_scheme_name_is_case_insensitive() {
        for header in ["Bearer abc", "bearer abc", "BEARER abc", "BeArEr abc"] {
            assert_eq!(bearer_token(header), Some("abc"), "{header:?}");
        }
    }

    /// Whitespace around a credential is not part of it.
    #[test]
    fn the_token_is_trimmed() {
        assert_eq!(bearer_token("Bearer  abc  "), Some("abc"));
        assert_eq!(bearer_token("Bearer \tabc"), Some("abc"));
    }

    /// An empty credential is an absent one: `strip_prefix` alone returns `Some("")`, which a caller reads as "a token was presented".
    #[test]
    fn an_empty_credential_is_not_a_credential() {
        assert_eq!(bearer_token("Bearer "), None);
        assert_eq!(bearer_token("Bearer    "), None);
    }

    #[test]
    fn another_scheme_is_not_a_bearer_token() {
        assert_eq!(bearer_token("Basic dXNlcjpwdw=="), None);
        assert_eq!(bearer_token("Token abc"), None);
        assert_eq!(bearer_token("abc"), None);
        assert_eq!(bearer_token(""), None);
        // Not a prefix match: the scheme is the whole first word.
        assert_eq!(bearer_token("Bearerish abc"), None);
    }

    #[test]
    fn rfc3339_matches_known_instants() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_000_000_000), "2001-09-09T01:46:40Z");
        // 2000-02-29: the leap year that the century rule makes an exception of.
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(1_788_028_139), "2026-08-29T18:28:59Z");
    }
}
