//! A percent escape encodes a byte: decoding each as a code point turned `caf%C3%A9` into `cafÃ©`.

/// Lossy UTF-8: a query string is attacker-supplied. `+` is replaced first because `from_str_radix`
/// accepts a sign, so a one-pass decoder turned `%+a` into a newline.
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

/// Hinnant's `civil_from_days`; UTC, second precision only.
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

/// The scheme is case-insensitive (RFC 7235 §2.1); an empty token is an absent one.
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

    #[test]
    fn a_malformed_escape_is_kept_verbatim() {
        assert_eq!(percent_decode("100%25"), "100%");
        assert_eq!(percent_decode("tail%"), "tail%");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("50% off"), "50% off");
    }

    /// Found by fuzzing the two shapes against each other: a caller injecting a control byte.
    #[test]
    fn a_signed_escape_is_not_a_number() {
        assert_eq!(percent_decode("%+a"), "% a");
        assert_eq!(percent_decode("%+A"), "% A");
        assert_eq!(percent_decode("x%+dy"), "x% dy");
        assert_eq!(percent_decode("%-a"), "%-a");
    }

    #[test]
    fn the_scheme_name_is_case_insensitive() {
        for header in ["Bearer abc", "bearer abc", "BEARER abc", "BeArEr abc"] {
            assert_eq!(bearer_token(header), Some("abc"), "{header:?}");
        }
    }

    #[test]
    fn the_token_is_trimmed() {
        assert_eq!(bearer_token("Bearer  abc  "), Some("abc"));
        assert_eq!(bearer_token("Bearer \tabc"), Some("abc"));
    }

    /// `strip_prefix` alone returns `Some("")`, which a caller reads as a presented token.
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
        assert_eq!(bearer_token("Bearerish abc"), None);
    }

    #[test]
    fn rfc3339_matches_known_instants() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_000_000_000), "2001-09-09T01:46:40Z");
        // 2000-02-29: the century rule's exception.
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(1_788_028_139), "2026-08-29T18:28:59Z");
    }
}
