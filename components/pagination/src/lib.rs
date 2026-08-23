//! `pagination` — paginate a long list without drift — encode a sort position into an opaque cursor
//!
//! ## Keyset pagination
//! Offset/`LIMIT … OFFSET` paging silently skips or repeats rows when the
//! underlying set changes mid-scroll: insert a row before the offset and every
//! later page shifts by one. Keyset (a.k.a. cursor / seek) pagination avoids
//! this by remembering *where you were* rather than *how far in* you were. The
//! "where" is a [`Position`]: the value of the stable sort key at the page
//! boundary plus the unique id of that boundary row (the tiebreaker for rows
//! sharing a sort-key value), and the direction the cursor was issued for. The
//! next query becomes `WHERE (sort_key, id) > (:key, :id) ORDER BY sort_key, id
//! LIMIT :n` — stable under concurrent inserts and deletes.
//!
//! ## Tamper-evident opaque cursor
//! Clients must treat cursors as opaque and must not be able to forge or
//! silently corrupt one (which would let them page to arbitrary positions or
//! get confusing errors). So a cursor is:
//!
//! ```text
//!   cursor = base64url_nopad(payload) + "." + checksum_hex
//!   payload  = "f:{0|1}:{b64url(sort_key)}:{b64url(last_id)}"
//!   checksum = hex(  HMAC-SHA256(key, payload)[..8]  )
//! ```
//!
//! `decode` recomputes the HMAC over the decoded payload and compares it to the
//! supplied checksum in constant time (`hmac`'s `verify_slice`). Any mismatch —
//! forgery, bit-rot, or a cursor signed with a different key — yields
//! `invalid-cursor`. The payload base64url-encodes each field so the `:`
//! delimiter can never appear inside a field value.
//!
//! ## Config (wasi:config/store)
//!   cursor-secret   HMAC signing key. Defaults to a fixed development value;
//!                   PRODUCTION DEPLOYMENTS MUST SET THIS to a real secret,
//!                   otherwise cursors are trivially forgeable.
//!   max-page-size   upper bound for `clamp-limit` (default 100).

#[allow(warnings)]
mod bindings;

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use bindings::exports::paginate::cursor::cursors::{CursorError, Guest, PageInfo, Position};
use bindings::wasi::config::store as config;

struct Component;

type HmacSha256 = Hmac<Sha256>;

/// Number of leading HMAC bytes kept as the cursor checksum (16 hex chars).
const TAG_LEN: usize = 8;

// ---- config -------------------------------------------------------------

/// HMAC signing key. The fallback is a fixed, well-known development secret;
/// production MUST override it via config `cursor-secret`, since anyone who
/// knows the key can forge cursors.
fn signing_key() -> String {
    match config::get("cursor-secret") {
        Ok(Some(v)) if !v.is_empty() => v,
        _ => "paginate-default-secret".to_string(),
    }
}

fn max_page_size() -> u32 {
    match config::get("max-page-size") {
        Ok(Some(v)) => v.parse().unwrap_or(100),
        _ => 100,
    }
}

// ---- payload codec ------------------------------------------------------

/// Serialize a position into the canonical payload string. Each free-form
/// field is base64url-encoded so the `:` delimiter cannot collide with field
/// contents.
fn payload_of(pos: &Position) -> String {
    format!(
        "f:{}:{}:{}",
        if pos.forward { 1 } else { 0 },
        B64URL.encode(pos.sort_key.as_bytes()),
        B64URL.encode(pos.last_id.as_bytes()),
    )
}

/// Parse a payload string back into a position. Returns `None` on any
/// structural problem (wrong field count, bad base64, non-utf8, bad flag).
fn position_of(payload: &str) -> Option<Position> {
    let mut parts = payload.split(':');
    // tag, forward-flag, sort-key, last-id
    if parts.next()? != "f" {
        return None;
    }
    let forward = match parts.next()? {
        "1" => true,
        "0" => false,
        _ => return None,
    };
    let sort_key = decode_field(parts.next()?)?;
    let last_id = decode_field(parts.next()?)?;
    if parts.next().is_some() {
        return None; // trailing junk
    }
    Some(Position { sort_key, last_id, forward })
}

fn decode_field(b64: &str) -> Option<String> {
    let bytes = B64URL.decode(b64).ok()?;
    String::from_utf8(bytes).ok()
}

/// Hex-encode the first `TAG_LEN` bytes of HMAC-SHA256(key, payload).
fn checksum(key: &str, payload: &str) -> String {
    let mut mac =
        HmacSha256::new_from_slice(key.as_bytes()).expect("HMAC accepts keys of any length");
    mac.update(payload.as_bytes());
    let tag = mac.finalize().into_bytes();
    tag[..TAG_LEN].iter().map(|b| format!("{b:02x}")).collect()
}

/// Constant-time verify that `payload` carries `expected_hex` checksum under
/// `key`. Decodes the hex into bytes and uses `hmac`'s `verify_slice`.
fn verify(key: &str, payload: &str, expected_hex: &str) -> bool {
    let Some(expected) = hex_to_bytes(expected_hex) else {
        return false;
    };
    if expected.len() != TAG_LEN {
        return false;
    }
    let mut mac =
        HmacSha256::new_from_slice(key.as_bytes()).expect("HMAC accepts keys of any length");
    mac.update(payload.as_bytes());
    // `verify_slice` is constant-time and tolerates a truncated tag.
    mac.verify_truncated_left(&expected).is_ok()
}

fn hex_to_bytes(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for pair in bytes.chunks(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
    }
    Some(out)
}

// ---- guest --------------------------------------------------------------

impl Guest for Component {
    fn encode(pos: Position) -> String {
        let key = signing_key();
        let payload = payload_of(&pos);
        let sum = checksum(&key, &payload);
        format!("{}.{}", B64URL.encode(payload.as_bytes()), sum)
    }

    fn decode(cursor: String) -> Result<Position, CursorError> {
        // cursor = base64url(payload) "." checksum_hex
        let (b64_payload, sum) = cursor.split_once('.').ok_or(CursorError::InvalidCursor)?;
        let payload_bytes = B64URL.decode(b64_payload).map_err(|_| CursorError::InvalidCursor)?;
        let payload = String::from_utf8(payload_bytes).map_err(|_| CursorError::InvalidCursor)?;
        let key = signing_key();
        if !verify(&key, &payload, sum) {
            return Err(CursorError::InvalidCursor);
        }
        position_of(&payload).ok_or(CursorError::InvalidCursor)
    }

    fn clamp_limit(requested: u32) -> Result<u32, CursorError> {
        if requested == 0 {
            return Err(CursorError::BadLimit);
        }
        let max = max_page_size();
        Ok(requested.min(max))
    }

    fn build_page(
        first: Option<Position>,
        last: Option<Position>,
        more_before: bool,
        more_after: bool,
    ) -> PageInfo {
        let next_cursor = match (more_after, last) {
            (true, Some(mut pos)) => {
                pos.forward = true;
                Some(Self::encode(pos))
            }
            _ => None,
        };
        let prev_cursor = match (more_before, first) {
            (true, Some(mut pos)) => {
                pos.forward = false;
                Some(Self::encode(pos))
            }
            _ => None,
        };
        PageInfo { next_cursor, prev_cursor, has_next: more_after, has_prev: more_before }
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    /// A cursor is only opaque if it is also unforgeable. These pin that the
    /// tag depends on BOTH the key and the payload, so a client cannot page
    /// into someone else's results by editing the offset it was handed.
    #[test]
    fn a_cursor_tag_binds_the_key_and_the_payload() {
        let tag = checksum("k1", "offset:10");
        assert_eq!(tag, checksum("k1", "offset:10"), "deterministic");
        assert_ne!(tag, checksum("k2", "offset:10"), "a different key, a different tag");
        assert_ne!(tag, checksum("k1", "offset:11"), "a different payload, a different tag");
        assert_eq!(tag.len(), TAG_LEN * 2, "hex of a truncated tag");
        assert!(tag.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn verify_accepts_only_the_tag_this_key_produces() {
        let tag = checksum("secret", "page:2");
        assert!(verify("secret", "page:2", &tag));
        assert!(!verify("secret", "page:3", &tag), "the payload is bound");
        assert!(!verify("other", "page:2", &tag), "the key is bound");
    }

    /// Malformed tags must be REJECTED, not crash and not accidentally pass.
    ///
    /// A cursor arrives from a client, so every one of these is reachable by
    /// anyone with a URL bar: odd-length hex, non-hex characters, an empty
    /// tag, and a truncated one. The truncation case is the sharp one — the
    /// verifier tolerates a shortened tag by design, so "shorter" must still
    /// mean "wrong length" rather than "fewer bytes to guess".
    #[test]
    fn a_malformed_tag_is_refused_rather_than_trusted() {
        let tag = checksum("secret", "page:2");
        assert!(!verify("secret", "page:2", ""), "empty");
        assert!(!verify("secret", "page:2", "zz"), "not hex");
        assert!(!verify("secret", "page:2", &tag[..tag.len() - 1]), "odd length");
        assert!(!verify("secret", "page:2", &tag[..tag.len() - 2]), "truncated tag");
        assert!(!verify("secret", "page:2", &format!("{tag}00")), "over-long");
    }
}
