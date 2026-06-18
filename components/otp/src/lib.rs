//! `otp` — reference implementation of `otp:totp` (RFC 4226 HOTP / RFC 6238 TOTP).
//!
//! Second-factor primitive over HMAC-SHA1, the algorithm Google Authenticator /
//! Authy expect:
//!   - HOTP (RFC 4226): code = dynamic-truncate(HMAC-SHA1(secret, counter)) mod 10^digits
//!   - TOTP (RFC 6238): HOTP with counter = unix-time / period
//!
//! Fully stateless and pure crypto + clock: the shared secret is always supplied
//! by the caller (store it in `secrets:vault`), so this component persists
//! nothing. `wasi:clocks/wall-clock` supplies "now"; `wasi:random/random`
//! supplies fresh secret + recovery-code entropy.

#[allow(warnings)]
mod bindings;

use hmac::{Hmac, Mac};

use bindings::exports::otp::totp::authenticator::{Guest, OtpError, Provisioned};
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::random::random::get_random_bytes;

type HmacSha1 = Hmac<sha1::Sha1>;

struct Component;

// ---- base32 (RFC 4648, A-Z 2-7, no padding) ----------------------------

const B32_ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// Encode bytes as unpadded RFC 4648 base32 (upper-case A-Z2-7).
fn base32_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(5) * 8);
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;
    for &b in data {
        buffer = (buffer << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            let idx = ((buffer >> bits) & 0x1f) as usize;
            out.push(B32_ALPHABET[idx] as char);
        }
    }
    if bits > 0 {
        // pad the trailing partial group on the right with zero bits.
        let idx = ((buffer << (5 - bits)) & 0x1f) as usize;
        out.push(B32_ALPHABET[idx] as char);
    }
    out
}

/// Map a base32 character to its 5-bit value. Accepts upper- and lower-case;
/// returns `None` for anything outside the A-Z2-7 alphabet (padding included).
fn base32_value(c: u8) -> Option<u32> {
    match c {
        b'A'..=b'Z' => Some((c - b'A') as u32),
        b'a'..=b'z' => Some((c - b'a') as u32),
        b'2'..=b'7' => Some((c - b'2' + 26) as u32),
        _ => None,
    }
}

/// Decode an unpadded RFC 4648 base32 string. Whitespace is ignored; any other
/// non-alphabet byte makes the whole input invalid (returns `None`).
fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 5 / 8);
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;
    for &c in s.as_bytes() {
        if c == b' ' || c == b'\t' || c == b'\r' || c == b'\n' {
            continue;
        }
        let v = base32_value(c)?;
        buffer = (buffer << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

/// Decode a secret string into raw bytes, mapping any base32 failure onto
/// `OtpError::BadSecret`.
fn decode_secret(secret: &str) -> Result<Vec<u8>, OtpError> {
    base32_decode(secret).ok_or(OtpError::BadSecret)
}

// ---- HOTP / TOTP core ----------------------------------------------------

/// Validate the requested digit count is within the supported 6..=8 range.
fn check_digits(digits: u8) -> Result<u32, OtpError> {
    if (6..=8).contains(&digits) {
        Ok(digits as u32)
    } else {
        Err(OtpError::BadDigits)
    }
}

/// RFC 4226 HOTP: HMAC-SHA1 over the 8-byte big-endian counter, dynamically
/// truncated to a `digits`-wide zero-padded decimal string.
fn hotp(secret_bytes: &[u8], counter: u64, digits: u8) -> Result<String, OtpError> {
    let digits = check_digits(digits)?;

    // HMAC-SHA1 accepts a key of any length, so this never errors.
    let mut mac = HmacSha1::new_from_slice(secret_bytes).unwrap();
    mac.update(&counter.to_be_bytes());
    let out = mac.finalize().into_bytes(); // 20 bytes

    // Dynamic truncation (RFC 4226 §5.3).
    let offset = (out[19] & 0x0f) as usize;
    let bin_code = ((out[offset] & 0x7f) as u32) << 24
        | (out[offset + 1] as u32) << 16
        | (out[offset + 2] as u32) << 8
        | (out[offset + 3] as u32);

    let modulo = 10u32.pow(digits);
    let code = bin_code % modulo;
    Ok(format!("{code:0width$}", width = digits as usize))
}

/// Resolve a caller-supplied period, treating 0 as the conventional 30 seconds.
fn period_or_default(period: u32) -> u64 {
    if period == 0 {
        30
    } else {
        period as u64
    }
}

fn now() -> u64 {
    wall_clock::now().seconds
}

/// Constant-time byte comparison (length-checked, XOR-accumulated) so code
/// verification does not leak via timing.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ---- minimal percent-encoding for the otpauth:// URI --------------------

/// Percent-encode the characters that would break an `otpauth://` label or
/// query value. Keeps unreserved chars + a few label-safe ones verbatim; this
/// is intentionally minimal but yields a valid URI.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

impl Guest for Component {
    fn provision(issuer: String, account: String) -> Result<Provisioned, OtpError> {
        // 20 random bytes -> 32 base32 chars (a 160-bit secret, matching SHA1).
        let raw = get_random_bytes(20);
        let secret = base32_encode(&raw);

        let issuer_enc = percent_encode(&issuer);
        let account_enc = percent_encode(&account);
        let uri = format!(
            "otpauth://totp/{issuer_enc}:{account_enc}?secret={secret}&issuer={issuer_enc}&algorithm=SHA1&digits=6&period=30"
        );

        Ok(Provisioned { secret, uri })
    }

    fn totp_at(
        secret: String,
        timestamp: u64,
        period: u32,
        digits: u8,
    ) -> Result<String, OtpError> {
        let bytes = decode_secret(&secret)?;
        let counter = timestamp / period_or_default(period);
        hotp(&bytes, counter, digits)
    }

    fn totp_now(secret: String) -> Result<String, OtpError> {
        Self::totp_at(secret, now(), 30, 6)
    }

    fn verify(
        secret: String,
        code: String,
        period: u32,
        digits: u8,
        skew: u32,
    ) -> Result<bool, OtpError> {
        let bytes = decode_secret(&secret)?;
        // Validate digits up front so a bad request fails before any compare.
        check_digits(digits)?;

        let step = period_or_default(period);
        let counter0 = now() / step;
        let skew = skew as u64;
        let lo = counter0.saturating_sub(skew);
        let hi = counter0.saturating_add(skew);

        let mut matched = false;
        let mut c = lo;
        loop {
            let candidate = hotp(&bytes, c, digits)?;
            // Constant-time per-candidate compare; do not early-return so the
            // loop's cost does not depend on which window matched.
            if constant_time_eq(candidate.as_bytes(), code.as_bytes()) {
                matched = true;
            }
            if c == hi {
                break;
            }
            c += 1;
        }
        Ok(matched)
    }

    fn hotp_at(secret: String, counter: u64, digits: u8) -> Result<String, OtpError> {
        let bytes = decode_secret(&secret)?;
        hotp(&bytes, counter, digits)
    }

    fn recovery_codes(count: u32) -> Vec<String> {
        let mut codes = Vec::with_capacity(count as usize);
        for _ in 0..count {
            // 5 random bytes -> exactly 8 base32 chars; render `xxxx-xxxx` lower-case.
            let raw = get_random_bytes(5);
            let b32 = base32_encode(&raw).to_lowercase();
            let bytes = b32.as_bytes();
            let code = format!(
                "{}-{}",
                std::str::from_utf8(&bytes[0..4]).unwrap(),
                std::str::from_utf8(&bytes[4..8]).unwrap()
            );
            codes.push(code);
        }
        codes
    }
}

bindings::export!(Component with_types_in bindings);
