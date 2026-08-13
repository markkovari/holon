//! `artifact-cache` — derived work, computed once and handed over.
//!
//! ## The hole in the obvious version
//!
//! A cache with `get` and `put` helps a swarm not at all in the generation where
//! it matters. Twenty branches start together, twenty look up the same chunk
//! index, twenty miss at the same instant, and twenty compute it. The cache
//! begins helping in generation two — after the expensive thing has been done
//! twenty times.
//!
//! So `lookup` answers three ways, not two: **hit**, **claimed** (it is absent and
//! you are the one computing it), **pending** (it is absent and somebody else
//! already is). One producer works; the rest either wait or go and do something
//! more useful. That is the whole point of the component.
//!
//! ## Claims are leases, not locks
//!
//! A producer that dies holding a lock wedges the key forever, and in a system
//! whose branches are *expected* to fail that is not an edge case. A claim
//! carries a deadline; once it passes, the next caller may take it. Nothing has
//! to notice the death or clean up after it — the same reasoning as
//! `lattice/src/lease.rs`, where the TTL *is* the liveness check.
//!
//! ## Why a lost claim is not a correctness problem
//!
//! Two producers racing on the same key both compute the same function of the
//! same inputs, so a duplicated effort costs money and never correctness. That is
//! what allows the claim to be advisory and the deadline to be short.

#[allow(warnings)]
mod bindings;

use bindings::blob::store::blobstore;
use bindings::comp::store::cas;
use bindings::exports::artifact::cache::store::{
    Artifact, ArtifactKey, CacheError, Guest, Outcome,
};
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::config::store as config;
use bindings::wasi::keyvalue::store as kv;

use sha2::{Digest, Sha256};

struct Component;

/// How long a producer gets before its claim is up for grabs.
///
/// Short on purpose. Losing a race duplicates work and never breaks anything, so
/// the cost of expiring too eagerly is small and bounded, while the cost of
/// expiring too late is every other branch blocked on a producer that died.
const DEFAULT_CLAIM_SECS: u64 = 120;

fn cfg(key: &str, default: &str) -> String {
    config::get(key).ok().flatten().filter(|s| !s.is_empty()).unwrap_or_else(|| default.to_string())
}

fn container() -> String {
    cfg("artifact-container", "artifacts")
}

fn claim_secs() -> u64 {
    cfg("artifact-claim-secs", "").parse().unwrap_or(DEFAULT_CLAIM_SECS)
}

fn now() -> u64 {
    wall_clock::now().seconds
}

fn bucket() -> Result<kv::Bucket, CacheError> {
    // The host names the bucket; a guest names one from its allow-list and never
    // chooses what that resolves to (ADR-0012).
    kv::open("default").map_err(|e| CacheError::Unavailable(format!("opening the claim store: {e:?}")))
}

fn store_err(e: blobstore::BlobError) -> CacheError {
    match e {
        blobstore::BlobError::NotFound => CacheError::Invalid("not found".into()),
        blobstore::BlobError::BackendUnavailable(m) => CacheError::Unavailable(m),
    }
}

/// The derivation. Length-prefixed, so no two different keys can serialise the
/// same way — `producer="ab", version="c"` and `producer="a", version="bc"` must
/// not collide, and a plain concatenation would let them.
fn derive(key: &ArtifactKey) -> String {
    fn field(h: &mut Sha256, s: &str) {
        h.update((s.len() as u64).to_le_bytes());
        h.update(s.as_bytes());
    }
    let mut h = Sha256::new();
    field(&mut h, &key.producer);
    field(&mut h, &key.version);
    h.update((key.inputs.len() as u64).to_le_bytes());
    for i in &key.inputs {
        field(&mut h, i);
    }
    field(&mut h, &key.params);
    let d = h.finalize();
    let mut s = String::with_capacity(40);
    // 20 bytes is plenty to name derived data that can always be recomputed, and
    // it keeps an id the same length as a git object id, which reads better when
    // the two sit beside each other in a log.
    for b in d.iter().take(20) {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        s.push(char::from_digit((b & 0xf) as u32, 16).unwrap());
    }
    s
}

fn blob_key(id: &str) -> String {
    format!("a/{id}")
}

fn meta_key(id: &str) -> String {
    format!("m/{id}")
}

fn claim_key(id: &str) -> String {
    format!("c/{id}")
}

/// `<deadline>:<token>` — a claim, as stored.
fn parse_claim(raw: &[u8]) -> Option<(u64, String)> {
    let s = core::str::from_utf8(raw).ok()?;
    let (deadline, token) = s.split_once(':')?;
    Some((deadline.parse().ok()?, token.to_string()))
}

/// A token nobody else will guess or accidentally reuse.
///
/// Derived rather than random: this component has no randomness of its own, and
/// the id plus the deadline plus the current second is unique enough for a token
/// whose only job is to stop a stale producer overwriting a fresh claim.
fn mint_token(id: &str, deadline: u64) -> String {
    let mut h = Sha256::new();
    h.update(id.as_bytes());
    h.update(deadline.to_le_bytes());
    let d = h.finalize();
    let mut s = String::new();
    for b in d.iter().take(8) {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        s.push(char::from_digit((b & 0xf) as u32, 16).unwrap());
    }
    format!("{id}.{s}")
}

fn id_of_token(token: &str) -> Result<String, CacheError> {
    token
        .split_once('.')
        .map(|(id, _)| id.to_string())
        .ok_or_else(|| CacheError::Invalid(format!("{token:?} is not a claim token")))
}

impl Guest for Component {
    fn derive_id(key: ArtifactKey) -> String {
        derive(&key)
    }

    fn lookup(key: ArtifactKey) -> Result<Outcome, CacheError> {
        let id = derive(&key);

        if let Some(a) = Self::get(id.clone())? {
            return Ok(Outcome::Hit(a));
        }

        // Absent. Try to become the producer — guarded, because two branches
        // claiming the same key at the same instant is the normal case here.
        let b = bucket()?;
        let ck = claim_key(&id);
        let current = cas::get(&b, &ck).map_err(|e| CacheError::Unavailable(format!("{e:?}")))?;
        let now = now();

        let (revision, held) = match &current {
            Some(v) => (v.revision, parse_claim(&v.value)),
            None => (0, None),
        };
        if let Some((deadline, _)) = &held {
            if *deadline > now {
                // Somebody is on it. Suggest waiting the remainder rather than a
                // fixed interval — a caller that polls faster than the work can
                // finish is just load.
                return Ok(Outcome::Pending((deadline - now) * 1000));
            }
            // Expired: the previous producer died or gave up. Taking it is the
            // recovery, and it needs no separate sweeper.
        }

        let deadline = now + claim_secs();
        let token = mint_token(&id, deadline);
        let value = format!("{deadline}:{token}");
        match cas::set(&b, &ck, value.as_bytes(), revision) {
            Ok(cas::Outcome::Committed(_)) => Ok(Outcome::Claimed(token)),
            // Lost the race by a hair. The winner is computing it; say so rather
            // than inventing a claim that would let both produce.
            Ok(cas::Outcome::Conflict(_)) => Ok(Outcome::Pending(claim_secs() * 1000 / 4)),
            Err(e) => Err(CacheError::Unavailable(format!("{e:?}"))),
        }
    }

    fn get(id: String) -> Result<Option<Artifact>, CacheError> {
        if id.is_empty() {
            return Err(CacheError::Invalid("empty artifact id".into()));
        }
        let c = container();
        match blobstore::get(&c, &blob_key(&id)) {
            Ok(bytes) => {
                // Metadata is a second object rather than a header on the first,
                // because `blob:store` has no metadata a caller can set beyond a
                // content type, and provenance is worth more than one field.
                let (producer, content_type, stored_at) =
                    match blobstore::get(&c, &meta_key(&id)) {
                        Ok(m) => {
                            let s = String::from_utf8_lossy(&m).to_string();
                            let mut parts = s.splitn(3, '\n');
                            (
                                parts.next().unwrap_or_default().to_string(),
                                parts.next().unwrap_or_default().to_string(),
                                parts.next().unwrap_or_default().parse().unwrap_or(0),
                            )
                        }
                        Err(_) => (String::new(), String::new(), 0),
                    };
                Ok(Some(Artifact { id, bytes, content_type, producer, stored_at }))
            }
            Err(blobstore::BlobError::NotFound) => Ok(None),
            Err(e) => Err(store_err(e)),
        }
    }

    fn put(claim: String, bytes: Vec<u8>, content_type: String) -> Result<String, CacheError> {
        let id = id_of_token(&claim)?;
        let b = bucket()?;
        let ck = claim_key(&id);

        let current = cas::get(&b, &ck).map_err(|e| CacheError::Unavailable(format!("{e:?}")))?;
        let Some(v) = &current else {
            return Err(CacheError::NotYourClaim(format!(
                "the claim on {id} is gone — it expired and somebody else took it"
            )));
        };
        let Some((deadline, held)) = parse_claim(&v.value) else {
            return Err(CacheError::Unavailable("the claim record is unreadable".into()));
        };
        if held != claim {
            return Err(CacheError::NotYourClaim(format!(
                "the claim on {id} belongs to another producer now"
            )));
        }
        if deadline <= now() {
            // Refused rather than written. A producer this late may have been
            // superseded, and letting it write would overwrite whatever the
            // producer that replaced it stored.
            return Err(CacheError::NotYourClaim(format!("the claim on {id} expired while you held it")));
        }

        let c = container();
        // Bytes first, then metadata, then release. A reader that arrives between
        // the two writes gets the artifact with empty provenance, which is
        // survivable; the reverse order would advertise an artifact that is not
        // there yet.
        blobstore::put(&c, &blob_key(&id), &bytes, &content_type).map_err(store_err)?;
        let meta = format!("{}\n{content_type}\n{}", claim, now());
        let _ = blobstore::put(&c, &meta_key(&id), meta.as_bytes(), "text/plain");
        // The claim is dropped, not expired: the artifact is there, so the next
        // caller should get a hit rather than be told to wait.
        let _ = cas::set(&b, &ck, b"", v.revision);
        Ok(id)
    }

    fn abandon(claim: String) -> Result<(), CacheError> {
        let id = id_of_token(&claim)?;
        let b = bucket()?;
        let ck = claim_key(&id);
        let current = cas::get(&b, &ck).map_err(|e| CacheError::Unavailable(format!("{e:?}")))?;
        let Some(v) = &current else { return Ok(()) };
        match parse_claim(&v.value) {
            // Only the holder may release it. Otherwise a straggler abandoning
            // late would free the claim of the producer that replaced it.
            Some((_, held)) if held == claim => {
                cas::set(&b, &ck, b"", v.revision)
                    .map(|_| ())
                    .map_err(|e| CacheError::Unavailable(format!("{e:?}")))
            }
            _ => Ok(()),
        }
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    fn key(producer: &str, version: &str, inputs: &[&str], params: &str) -> ArtifactKey {
        ArtifactKey {
            producer: producer.into(),
            version: version.into(),
            inputs: inputs.iter().map(|s| s.to_string()).collect(),
            params: params.into(),
        }
    }

    #[test]
    fn the_same_work_derives_the_same_id() {
        let a = derive(&key("chunker", "1", &["tree-abc"], "size=800"));
        let b = derive(&key("chunker", "1", &["tree-abc"], "size=800"));
        assert_eq!(a, b, "a cache that cannot recognise its own work is not a cache");
        assert_eq!(a.len(), 40);
    }

    /// The reason the version is IN the key. A producer that changed its rules
    /// makes artifacts that are not interchangeable with its old ones, and
    /// serving those would be worse than a miss.
    #[test]
    fn a_new_producer_version_is_a_different_artifact() {
        let old = derive(&key("chunker", "1", &["tree-abc"], ""));
        let new = derive(&key("chunker", "2", &["tree-abc"], ""));
        assert_ne!(old, new, "v2's output must not be served for a v1 request");
    }

    #[test]
    fn every_part_of_the_key_changes_the_id() {
        let base = derive(&key("chunker", "1", &["a"], "p"));
        for other in [
            key("embed", "1", &["a"], "p"),
            key("chunker", "1", &["b"], "p"),
            key("chunker", "1", &["a", "b"], "p"),
            key("chunker", "1", &["a"], "q"),
        ] {
            assert_ne!(base, derive(&other), "this input should have changed the id");
        }
    }

    /// Input ORDER matters for most producers, so it must matter here.
    #[test]
    fn reordering_the_inputs_is_a_different_artifact() {
        let ab = derive(&key("link", "1", &["a", "b"], ""));
        let ba = derive(&key("link", "1", &["b", "a"], ""));
        assert_ne!(ab, ba);
    }

    /// Fields are length-prefixed so no two distinct keys serialise identically.
    /// Concatenation would make these two the same artifact.
    #[test]
    fn fields_cannot_bleed_into_each_other() {
        let a = derive(&key("ab", "c", &[], ""));
        let b = derive(&key("a", "bc", &[], ""));
        assert_ne!(a, b, "a plain concatenation would have collided these");
    }

    #[test]
    fn a_token_names_the_artifact_it_claims() {
        let id = derive(&key("chunker", "1", &["x"], ""));
        let token = mint_token(&id, 1_700_000_000);
        assert_eq!(id_of_token(&token).unwrap(), id);
        assert!(id_of_token("no-dot-here").is_err());
        // Two deadlines on one id are two different claims, so a stale producer
        // cannot present an old token against a fresh claim.
        assert_ne!(token, mint_token(&id, 1_700_000_001));
    }

    #[test]
    fn a_claim_record_round_trips() {
        let (deadline, token) = parse_claim(b"1700000000:abc.def").unwrap();
        assert_eq!(deadline, 1_700_000_000);
        assert_eq!(token, "abc.def");
        assert!(parse_claim(b"nonsense").is_none());
        // An empty value is how a released claim is recorded, and must not parse
        // as a held one.
        assert!(parse_claim(b"").is_none());
    }
}
