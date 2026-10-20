//! Keyset pagination: the cursor holds the boundary (sort_key, id), so mid-scroll inserts/deletes cannot skip or repeat rows.

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

const TAG_LEN: usize = 8;

/// The fallback is a well-known dev secret: production MUST set `cursor-secret` or cursors are forgeable.
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

/// base64url fields, so the `:` delimiter cannot collide.
fn payload_of(pos: &Position) -> String {
    format!(
        "f:{}:{}:{}",
        if pos.forward { 1 } else { 0 },
        B64URL.encode(pos.sort_key.as_bytes()),
        B64URL.encode(pos.last_id.as_bytes()),
    )
}

fn position_of(payload: &str) -> Option<Position> {
    let mut parts = payload.split(':');
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

fn checksum(key: &str, payload: &str) -> String {
    let mut mac =
        HmacSha256::new_from_slice(key.as_bytes()).expect("HMAC accepts keys of any length");
    mac.update(payload.as_bytes());
    let tag = mac.finalize().into_bytes();
    tag[..TAG_LEN].iter().map(|b| format!("{b:02x}")).collect()
}

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

impl Guest for Component {
    fn encode(pos: Position) -> String {
        let key = signing_key();
        let payload = payload_of(&pos);
        let sum = checksum(&key, &payload);
        format!("{}.{}", B64URL.encode(payload.as_bytes()), sum)
    }

    fn decode(cursor: String) -> Result<Position, CursorError> {
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

    /// The verifier tolerates a shortened tag by design, so "shorter" must still mean "wrong length".
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
