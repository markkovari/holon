//! `artifact-cache` — derived work, computed once and handed over.

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

/// Short: losing a race only duplicates work; expiring late blocks every branch on a dead producer.
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
    // A guest names a bucket from its allow-list; the host decides what it resolves to (ADR-0012).
    kv::open("default")
        .map_err(|e| CacheError::Unavailable(format!("opening the claim store: {e:?}")))
}

fn store_err(e: blobstore::BlobError) -> CacheError {
    match e {
        blobstore::BlobError::NotFound => CacheError::Invalid("not found".into()),
        blobstore::BlobError::BackendUnavailable(m) => CacheError::Unavailable(m),
    }
}

/// Length-prefixed, so ("ab", "c") and ("a", "bc") cannot collide.
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
    // 20 bytes names recomputable data, and matches a git object id's length in logs.
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

/// Derived, not random (no randomness here): it only stops a stale producer overwriting a fresh claim.
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

        // Simultaneous claims are the normal case, so this is guarded.
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
                return Ok(Outcome::Pending((deadline - now) * 1000));
            }
            // Expired: taking it is the recovery from a dead producer.
        }

        let deadline = now + claim_secs();
        let token = mint_token(&id, deadline);
        let value = format!("{deadline}:{token}");
        match cas::set(&b, &ck, value.as_bytes(), revision) {
            Ok(cas::Outcome::Committed(_)) => Ok(Outcome::Claimed(token)),
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
                // A second object: `blob:store` has no metadata beyond content type.
                let (producer, content_type, stored_at) = match blobstore::get(&c, &meta_key(&id)) {
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
            // A late producer may have been superseded; writing would overwrite its replacement's artifact.
            return Err(CacheError::NotYourClaim(format!(
                "the claim on {id} expired while you held it"
            )));
        }

        let c = container();
        // Bytes, metadata, then release: the reverse would advertise a missing artifact.
        blobstore::put(&c, &blob_key(&id), &bytes, &content_type).map_err(store_err)?;
        let meta = format!("{}\n{content_type}\n{}", claim, now());
        let _ = blobstore::put(&c, &meta_key(&id), meta.as_bytes(), "text/plain");
        // Dropped, not expired, so the next caller gets a hit instead of waiting.
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
            // Only the holder may release; a straggler would free its replacement's claim.
            Some((_, held)) if held == claim => cas::set(&b, &ck, b"", v.revision)
                .map(|_| ())
                .map_err(|e| CacheError::Unavailable(format!("{e:?}"))),
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

    #[test]
    fn reordering_the_inputs_is_a_different_artifact() {
        let ab = derive(&key("link", "1", &["a", "b"], ""));
        let ba = derive(&key("link", "1", &["b", "a"], ""));
        assert_ne!(ab, ba);
    }

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
        assert_ne!(token, mint_token(&id, 1_700_000_001));
    }

    #[test]
    fn a_claim_record_round_trips() {
        let (deadline, token) = parse_claim(b"1700000000:abc.def").unwrap();
        assert_eq!(deadline, 1_700_000_000);
        assert_eq!(token, "abc.def");
        assert!(parse_claim(b"nonsense").is_none());
        assert!(parse_claim(b"").is_none());
    }
}
