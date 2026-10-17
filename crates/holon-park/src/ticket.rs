//! Content-addressed ticket ids (like `holon_vcs::patch::patch_hash`): re-parking
//! the identical call is detectable without a lookup, so a caller that crashed
//! after `park` and retried cannot double-park.
//!
//! Only `session` and `correlation` are hashed; `description` and `deadline`
//! may be reworded on retry without being a different call.

use sha2::{Digest, Sha256};

use crate::model::Effect;

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

/// A commit's id (ADR-0110): the hash of the session and the effects exactly as listed, so the
/// identical commit again is the same batch and a retry cannot make a second one. Reordering the
/// effects is a different commit, because their order is part of what they mean.
pub fn batch_id(session: &str, effects: &[Effect]) -> String {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"holon-park/batch/v1\n");
    field(&mut buf, session);
    buf.extend_from_slice(&serde_json::to_vec(effects).expect("effects always serialize"));
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
    fn a_commit_is_its_session_and_its_effects_in_order() {
        use crate::model::{Effect, WakeEffect};
        let w = |c: &str| {
            Effect::Wake(WakeEffect {
                correlation: c.into(),
                answer: crate::model::CallResult { ok: true, body: vec![], detail: None },
            })
        };
        let ab = vec![w("a"), w("b")];
        assert_eq!(batch_id("s", &ab), batch_id("s", &ab.clone()));
        assert_ne!(batch_id("s", &ab), batch_id("s2", &ab));
        assert_ne!(batch_id("s", &ab), batch_id("s", &[w("b"), w("a")]), "order is meaning");
        assert_ne!(batch_id("s", &ab), batch_id("s", &[w("a")]));
    }

    #[test]
    fn no_ambiguity_at_the_field_boundary() {
        // Without a length prefix, ("s1x", "1") and ("s1", "x1") would collide.
        assert_ne!(ticket_id("s1x", "1"), ticket_id("s1", "x1"));
    }
}
