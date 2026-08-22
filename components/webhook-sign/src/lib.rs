//! `webhook-sign` — sign an outbound webhook the way the major providers do — HMAC-SHA256 over a timestamped payload
//!
//! The SEND side that matches `webhook:ingest`'s verify side: it produces the
//! same HMAC-SHA256 signature headers that `webhook:ingest` checks, so a
//! service that both emits and consumes webhooks uses a matched pair (sign here,
//! verify there). Two header styles:
//!   - Stripe: `t={ts},v1={hex}` where the MAC is taken over `{ts}.{body}`.
//!   - GitHub: `sha256={hex}` where the MAC is taken over the raw body.
//!
//! Pure crypto + clock. The signing secret is supplied by the caller (store it
//! in `secrets:vault`); `wasi:clocks/wall-clock` only provides the timestamp.

#[allow(warnings)]
mod bindings;

use hmac::{Hmac, Mac};

use bindings::exports::webhook::sign::signer::{Guest, Scheme, SignError, Signature};
use bindings::wasi::clocks::wall_clock;

type HmacSha256 = Hmac<sha2::Sha256>;

struct Component;

/// HMAC-SHA256 of `msg` under `secret`, rendered as lowercase hex.
fn hmac_hex(secret: &str, msg: &[u8]) -> String {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("hmac accepts any key length");
    mac.update(msg);
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Constant-time comparison of two hex strings: length-check, then
/// XOR-accumulate over every byte so the time taken does not depend on where
/// the first mismatch is.
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn now() -> u64 {
    wall_clock::now().seconds
}

impl Guest for Component {
    fn sign(body: Vec<u8>, secret: String, scheme: Scheme) -> Result<Signature, SignError> {
        Self::sign_at(body, secret, scheme, now())
    }

    fn sign_at(
        body: Vec<u8>,
        secret: String,
        scheme: Scheme,
        timestamp: u64,
    ) -> Result<Signature, SignError> {
        match scheme {
            Scheme::Stripe => {
                // signed payload = `{timestamp}.` ++ body
                let mut signed = format!("{timestamp}.").into_bytes();
                signed.extend_from_slice(&body);
                let hex = hmac_hex(&secret, &signed);
                Ok(Signature {
                    header: format!("t={timestamp},v1={hex}"),
                    timestamp,
                })
            }
            Scheme::Github => {
                let hex = hmac_hex(&secret, &body);
                Ok(Signature {
                    header: format!("sha256={hex}"),
                    timestamp: 0,
                })
            }
        }
    }

    fn verify(
        body: Vec<u8>,
        header: String,
        secret: String,
        scheme: Scheme,
        tolerance_seconds: u64,
    ) -> Result<(), SignError> {
        match scheme {
            Scheme::Stripe => {
                // header = `t={ts},v1={hex}` — split on ',', then '='.
                let mut t: Option<u64> = None;
                let mut v1: Option<String> = None;
                for part in header.split(',') {
                    let (k, val) = part
                        .split_once('=')
                        .ok_or(SignError::MalformedSignature)?;
                    match k.trim() {
                        "t" => t = Some(val.trim().parse().map_err(|_| SignError::MalformedSignature)?),
                        "v1" => v1 = Some(val.trim().to_string()),
                        _ => {}
                    }
                }
                let t = t.ok_or(SignError::MalformedSignature)?;
                let v1 = v1.ok_or(SignError::MalformedSignature)?;

                // optional replay window check.
                if tolerance_seconds > 0 {
                    let now = now();
                    let delta = if now >= t { now - t } else { t - now };
                    if delta > tolerance_seconds {
                        return Err(SignError::TimestampOutOfTolerance);
                    }
                }

                let mut signed = format!("{t}.").into_bytes();
                signed.extend_from_slice(&body);
                let expected = hmac_hex(&secret, &signed);
                if ct_eq(&expected, &v1) {
                    Ok(())
                } else {
                    Err(SignError::SignatureMismatch)
                }
            }
            Scheme::Github => {
                // header = `sha256={hex}` — no timestamp check.
                let hex = header
                    .strip_prefix("sha256=")
                    .ok_or(SignError::MalformedSignature)?;
                let expected = hmac_hex(&secret, &body);
                if ct_eq(&expected, hex) {
                    Ok(())
                } else {
                    Err(SignError::SignatureMismatch)
                }
            }
        }
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    /// The comparison a signature check hangs on.
    ///
    /// It must be constant-time in the CONTENT — no early return on the first
    /// differing byte — because an attacker who can time the check otherwise
    /// recovers a valid signature one byte at a time. Timing cannot be asserted
    /// here; what can be asserted is the shape that makes it possible: every
    /// byte is folded into one accumulator, so equal-length inputs always do
    /// the same amount of work.
    #[test]
    fn ct_eq_agrees_with_equality_on_every_edge() {
        assert!(ct_eq("", ""));
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"), "a difference in the LAST byte");
        assert!(!ct_eq("abc", "bbc"), "a difference in the FIRST byte");
        assert!(!ct_eq("abc", "abcd"), "a prefix is not a match");
        assert!(!ct_eq("abcd", "abc"), "nor is a superset");
        assert!(!ct_eq("", "a"));
        // Case matters: hex signatures are compared as written.
        assert!(!ct_eq("ABC", "abc"));
    }

    /// A signature is hex of a fixed width, and the width is what a caller
    /// checks a truncated header against.
    #[test]
    fn hmac_hex_is_lowercase_hex_of_a_fixed_width() {
        let sig = hmac_hex("key", b"payload");
        assert_eq!(sig.len(), 64, "sha256 is 32 bytes, 64 hex chars");
        assert!(sig.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        // Deterministic, and sensitive to both inputs.
        assert_eq!(sig, hmac_hex("key", b"payload"));
        assert_ne!(sig, hmac_hex("key2", b"payload"));
        assert_ne!(sig, hmac_hex("key", b"payload2"));
        // A one-bit change in the body changes the signature.
        assert_ne!(hmac_hex("k", b"a"), hmac_hex("k", b"b"));
    }
}
