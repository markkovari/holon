//! Content-addressed ticket ids (like `holon_vcs::patch::patch_hash`): re-parking
//! the identical call is detectable without a lookup, so a caller that crashed
//! after `park` and retried cannot double-park.
//!
//! Only `session` and `correlation` are hashed; `description` and `deadline`
//! may be reworded on retry without being a different call.

use sha2::{Digest, Sha256};

fn field(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(s.len().to_string().as_bytes());
    buf.push(b':');
    buf.extend_from_slice(s.as_bytes());
    buf.push(b'\n');
}

pub fn ticket_id(session: &str, correlation: &str) -> String {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"holon-park/ticket/v1\n");
    field(&mut buf, session);
    field(&mut buf, correlation);
    hex::encode(Sha256::digest(&buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_session_and_correlation_is_the_same_ticket() {
        assert_eq!(ticket_id("s1", "req-1"), ticket_id("s1", "req-1"));
    }

    #[test]
    fn a_different_session_or_correlation_is_a_different_ticket() {
        assert_ne!(ticket_id("s1", "req-1"), ticket_id("s2", "req-1"));
        assert_ne!(ticket_id("s1", "req-1"), ticket_id("s1", "req-2"));
    }

    #[test]
    fn no_ambiguity_at_the_field_boundary() {
        // Without a length prefix, ("s1x", "1") and ("s1", "x1") would collide.
        assert_ne!(ticket_id("s1x", "1"), ticket_id("s1", "x1"));
    }
}
