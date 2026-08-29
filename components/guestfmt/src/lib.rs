//! `guestfmt` — the two wire-format helpers components kept rewriting, and the bug that hid in one
//!
//! 28 percent-decoders across the pool, under six names, in two shapes that do not
//! agree. The disagreement is not stylistic:
//!
//! ```text
//! input                       byte-correct     what 7 components did
//! caf%C3%A9                   café             cafÃ©
//! M%C3%A1rk+K%C5%91v%C3%A1ri  Márk Kővári      MÃ¡rk KÅvÃ¡ri
//! ```
//!
//! A percent escape encodes a BYTE. The broken shape wrote `out.push(b as char)`,
//! which reads that byte as a Unicode code point — so every multi-byte UTF-8
//! sequence came back as its individual bytes reinterpreted as Latin-1. ASCII is
//! unaffected, which is why it survived: it is correct for exactly the inputs a
//! test written in English would use.
//!
//! The right shape collects bytes and decodes once at the end. That is what the
//! majority already did (13 of them, byte-identical), and it is what this is.
//!
//! Unlike [`guestio`](../guestio), these are plain functions. That crate has to be
//! macros because a bindings type cannot cross a crate boundary; nothing here takes
//! one, so the ordinary answer applies.

/// Decode `application/x-www-form-urlencoded` text: `%XX` escapes and `+` for space.
///
/// Bytes are collected and decoded as UTF-8 once, at the end — decoding each escape
/// on its own cannot work, because one character is often several escapes.
///
/// Invalid UTF-8 becomes the replacement character rather than an error. A query
/// string is attacker-supplied and this is the last step before a handler reads it:
/// refusing would turn a malformed parameter into a failed request, where every
/// caller here wants a lossy-but-present value.
///
/// A `%` with fewer than two hex digits after it, or non-hex digits, is left alone —
/// literal `%` in user text is common enough that dropping it is worse than keeping it.
///
/// The `+`-to-space pass runs FIRST, and that ordering is load-bearing rather than
/// stylistic. `u8::from_str_radix` accepts a leading sign, so `"+a"` parses as `+0x0a`
/// — and the seven components that decoded escapes in one pass turned `%+a` into a
/// newline, letting a caller inject control bytes through input this function leaves
/// as the literal text `% a`. Replacing `+` before parsing means no `+` ever reaches
/// the radix call.
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
/// Howard Hinnant's `civil_from_days`: shift the year to start in March so the
/// leap day lands at the end and the month-length table becomes arithmetic. No
/// lookup tables and no branches on leap years, which is why the same twenty lines
/// kept being copied instead of pulled in.
///
/// Always UTC and always second precision. A component that needs an offset or
/// milliseconds needs a real date library, not this.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The case that separated the two shapes. Kept as a test rather than a note,
    /// because "ASCII still works" is exactly what let the broken one survive.
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

    /// `from_str_radix` accepts a sign, so a one-pass decoder reads `%+a` as `+0x0a`
    /// and emits a newline. Found by fuzzing the two shapes against each other:
    /// 200 000 inputs, 699 divergences on ASCII alone, all of this form.
    ///
    /// This is the half that is not about UTF-8 — it is a caller injecting a control
    /// byte through text that should have stayed literal.
    #[test]
    fn a_signed_escape_is_not_a_number() {
        assert_eq!(percent_decode("%+a"), "% a");
        assert_eq!(percent_decode("%+A"), "% A");
        assert_eq!(percent_decode("x%+dy"), "x% dy");
        // `-` is rejected by the radix parse on its own (it will not fit a u8), so
        // it was never the dangerous one. Asserted so the pair stays a pair.
        assert_eq!(percent_decode("%-a"), "%-a");
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
