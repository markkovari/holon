//! Content-addressed blobs and compare-and-set pointers — the two storage
//! primitives the engine is written against.
//!
//! * [`BlobStore`]: immutable bytes named by their SHA-256. `put` is idempotent.
//!   Implemented by [`crate::mem::MemBlobs`] and, natively, a JetStream
//!   ObjectStore (`nats::NatsBlobs`).
//! * [`PointerStore`]: mutable names holding a hash, written ONLY by
//!   compare-and-set against the revision that was read. A pointer can hold
//!   `none` — a tombstone — and is never deleted: deleting a key resets its
//!   revision, and a CAS against a revision from the key's previous life would then
//!   match (ABA; the lesson of #284). Every value also names the op that wrote it
//!   ([`PointerValue::op`]), which is what lets recovery tell whether a logged
//!   intent landed (see [`crate::recovery`]).
//! * [`Kv`]: the byte-valued CAS map both [`KvPointers`] and
//!   [`crate::oplog::KvOpLog`] are built on, so the NATS adapter implements one
//!   trait (JetStream KV `create`/`update`) and the pointer and oplog algorithms
//!   are shared, and tested, in memory.
//!
//! Every trait method returns a `Send` future so an engine over these can be
//! driven from a multi-threaded runtime; the core itself has no runtime.

use std::future::Future;

use sha2::{Digest, Sha256};

use crate::error::{Result, VcsError};
use crate::model::{Hash, OpId};

/// A pointer's revision. Opaque: compared for equality only, never assumed to
/// increment by one (it is a JetStream stream sequence on NATS).
pub type Revision = u64;

/// A compare-and-set that did not land: the key is not at the expected revision.
/// `current` is the revision it is at (`None`: absent), when the store says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CasMismatch {
    pub current: Option<Revision>,
}

/// Immutable, content-addressed bytes.
pub trait BlobStore: Send + Sync {
    /// Store `bytes`; returns their lower-case hex SHA-256. Storing the same bytes
    /// twice is a no-op that returns the same hash.
    fn put(&self, bytes: Vec<u8>) -> impl Future<Output = Result<Hash>> + Send;
    /// The bytes named `hash`, or `None`.
    fn get(&self, hash: &str) -> impl Future<Output = Result<Option<Vec<u8>>>> + Send;
    /// Whether `hash` is stored. The default fetches; adapters with a cheaper
    /// metadata call override it.
    fn contains(&self, hash: &str) -> impl Future<Output = Result<bool>> + Send {
        async move { Ok(self.get(hash).await?.is_some()) }
    }
}

/// What a pointer holds: a hash (`None` = tombstone) and the op that wrote it.
///
/// `op` is `None` only for a pointer written before ops were named in values
/// (verify reports those as unexplained). The engine writes the id of the op
/// whose logged intent this write carries out — the op is appended BEFORE the
/// write, so the id is known — and a fence (a rewrite of the same value to bump
/// the revision) keeps the op it found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PointerValue {
    pub value: Option<Hash>,
    pub op: Option<OpId>,
}

impl PointerValue {
    pub fn new(value: Option<Hash>, op: OpId) -> Self {
        PointerValue { value, op: Some(op) }
    }
}

/// Mutable pointers to hashes, compare-and-set only.
pub trait PointerStore: Send + Sync {
    /// The pointer's value and revision; `None` when the key has never been
    /// written. Must never be served from a cache: this is the read a CAS is
    /// computed from.
    fn get(
        &self,
        key: &str,
    ) -> impl Future<Output = Result<Option<(PointerValue, Revision)>>> + Send;
    /// Write `value` only if the key is at `expected` (`None`: must not exist).
    /// `Ok(Err(_))` is a lost race — nothing was written — distinct from `Err`, a
    /// store failure.
    fn cas(
        &self,
        key: &str,
        expected: Option<Revision>,
        value: &PointerValue,
    ) -> impl Future<Output = Result<std::result::Result<Revision, CasMismatch>>> + Send;
}

/// A byte-valued map with per-key compare-and-set. Keys are what [`escape`]
/// produces joined with `/`, so they fit JetStream KV's key charset.
pub trait Kv: Send + Sync {
    fn get(&self, key: &str) -> impl Future<Output = Result<Option<(Vec<u8>, Revision)>>> + Send;
    fn cas(
        &self,
        key: &str,
        expected: Option<Revision>,
        value: Vec<u8>,
    ) -> impl Future<Output = Result<std::result::Result<Revision, CasMismatch>>> + Send;
}

/// [`PointerStore`] over any [`Kv`]. A value is `<hash>@<op>`, with `-` for the
/// tombstone's hash (never an empty value: some KV clients read an empty value
/// as absent). `<hash>` or `-` alone — no op — is read as `op: None`.
pub struct KvPointers<K> {
    kv: K,
}

impl<K> KvPointers<K> {
    pub fn new(kv: K) -> Self {
        KvPointers { kv }
    }
    pub fn kv(&self) -> &K {
        &self.kv
    }
}

const TOMBSTONE: &str = "-";

/// The byte encoding [`KvPointers`] stores.
pub fn encode_pointer(v: &PointerValue) -> Vec<u8> {
    let mut s = v.value.clone().unwrap_or_else(|| TOMBSTONE.to_string());
    if let Some(op) = v.op {
        s.push('@');
        s.push_str(&op.to_string());
    }
    s.into_bytes()
}

pub fn decode_pointer(key: &str, bytes: &[u8]) -> Result<PointerValue> {
    let s = std::str::from_utf8(bytes)
        .map_err(|_| VcsError::Storage(format!("pointer {key} holds non-UTF-8 bytes")))?;
    let (v, op) = match s.split_once('@') {
        Some((v, op)) => {
            let op = op
                .parse::<OpId>()
                .map_err(|_| VcsError::Storage(format!("pointer {key} holds {s:?}")))?;
            (v, Some(op))
        }
        None => (s, None),
    };
    let value = if v == TOMBSTONE {
        None
    } else if is_hash(v) {
        Some(v.to_string())
    } else {
        return Err(VcsError::Storage(format!("pointer {key} holds {s:?}, not a hash")));
    };
    Ok(PointerValue { value, op })
}

impl<K: Kv> PointerStore for KvPointers<K> {
    async fn get(&self, key: &str) -> Result<Option<(PointerValue, Revision)>> {
        let Some((bytes, rev)) = self.kv.get(key).await? else {
            return Ok(None);
        };
        Ok(Some((decode_pointer(key, &bytes)?, rev)))
    }

    async fn cas(
        &self,
        key: &str,
        expected: Option<Revision>,
        value: &PointerValue,
    ) -> Result<std::result::Result<Revision, CasMismatch>> {
        self.kv.cas(key, expected, encode_pointer(value)).await
    }
}

/// Lower-case hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> Hash {
    hex::encode(Sha256::digest(bytes))
}

/// Whether `s` is 64 lower-case hex characters.
pub fn is_hash(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Escape one key segment so it is safe in a JetStream KV key and cannot forge a
/// separator: `[A-Za-z0-9_-]` stays, every other byte (including `/`, `.` and `=`
/// itself) becomes `=XX`, upper-case hex. Injective, so two workspace ids never
/// share a key.
pub fn escape(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for b in segment.bytes() {
        if b.is_ascii_alphanumeric() || b == b'_' || b == b'-' {
            out.push(b as char);
        } else {
            out.push_str(&format!("={b:02X}"));
        }
    }
    if out.is_empty() {
        // An empty segment would make `a//b`; `=` alone is not an escape of anything.
        out.push('=');
    }
    out
}

/// The inverse of [`escape`]: `None` for a string `escape` cannot have produced.
pub fn unescape(segment: &str) -> Option<String> {
    if segment == "=" {
        return Some(String::new());
    }
    let b = segment.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'=' => {
                let hex = segment.get(i + 1..i + 3)?;
                if !hex.bytes().all(|c| c.is_ascii_digit() || (b'A'..=b'F').contains(&c)) {
                    return None;
                }
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            c if c.is_ascii_alphanumeric() || c == b'_' || c == b'-' => {
                out.push(c);
                i += 1;
            }
            _ => return None,
        }
    }
    String::from_utf8(out).ok()
}

// Shared stores: several engines (several agents' processes, in a test) over the
// same backends.
impl<T: BlobStore> BlobStore for std::sync::Arc<T> {
    fn put(&self, bytes: Vec<u8>) -> impl Future<Output = Result<Hash>> + Send {
        (**self).put(bytes)
    }
    fn get(&self, hash: &str) -> impl Future<Output = Result<Option<Vec<u8>>>> + Send {
        (**self).get(hash)
    }
    fn contains(&self, hash: &str) -> impl Future<Output = Result<bool>> + Send {
        (**self).contains(hash)
    }
}

impl<T: PointerStore> PointerStore for std::sync::Arc<T> {
    fn get(
        &self,
        key: &str,
    ) -> impl Future<Output = Result<Option<(PointerValue, Revision)>>> + Send {
        (**self).get(key)
    }
    fn cas(
        &self,
        key: &str,
        expected: Option<Revision>,
        value: &PointerValue,
    ) -> impl Future<Output = Result<std::result::Result<Revision, CasMismatch>>> + Send {
        (**self).cas(key, expected, value)
    }
}

impl<T: Kv> Kv for std::sync::Arc<T> {
    fn get(&self, key: &str) -> impl Future<Output = Result<Option<(Vec<u8>, Revision)>>> + Send {
        (**self).get(key)
    }
    fn cas(
        &self,
        key: &str,
        expected: Option<Revision>,
        value: Vec<u8>,
    ) -> impl Future<Output = Result<std::result::Result<Revision, CasMismatch>>> + Send {
        (**self).cas(key, expected, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_is_injective_and_kv_safe() {
        let cases = ["goal/42", "goal=2F42", "a.b", "", "ümlaut", "plain-_1"];
        let escaped: Vec<_> = cases.iter().map(|c| escape(c)).collect();
        for (i, a) in escaped.iter().enumerate() {
            assert!(a.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-=".contains(&b)), "{a}");
            for b in &escaped[i + 1..] {
                assert_ne!(a, b);
            }
        }
        assert_eq!(escape("plain-_1"), "plain-_1");
        assert_eq!(escape("goal/42"), "goal=2F42");
        for c in cases {
            assert_eq!(unescape(&escape(c)).as_deref(), Some(c), "{c:?}");
        }
        assert_eq!(unescape("a=2"), None);
        assert_eq!(unescape("a.b"), None);
        assert_eq!(unescape("=2f"), None, "escape writes upper-case hex only");
        assert_eq!(
            crate::oplog::workspace_of_head_key("oplog/goal=2F42/head").as_deref(),
            Some("goal/42")
        );
        assert_eq!(crate::oplog::workspace_of_head_key("oplog/goal=2F42/op/00000000000000000001"), None);
        assert_eq!(crate::oplog::workspace_of_head_key("oplog/goal/settled"), None);
    }

    #[test]
    fn pointer_encoding_round_trips() {
        let h = sha256_hex(b"x");
        for v in [
            PointerValue { value: Some(h.clone()), op: Some(7) },
            PointerValue { value: None, op: Some(1) },
            PointerValue { value: Some(h.clone()), op: None },
            PointerValue { value: None, op: None },
        ] {
            assert_eq!(decode_pointer("k", &encode_pointer(&v)).unwrap(), v);
        }
        assert!(decode_pointer("k", b"nothash@1").is_err());
        assert!(decode_pointer("k", format!("{h}@x").as_bytes()).is_err());
    }

    #[test]
    fn hash_shape() {
        assert!(is_hash(&sha256_hex(b"x")));
        assert!(!is_hash("ABC"));
        assert!(!is_hash(&sha256_hex(b"x").to_uppercase()));
    }
}
