//! `--kv-cache-ms` — a per-node read cache whose only correctness argument is a clock.
//!
//! No coherence protocol: an entry lives for `ttl` and is then gone — no watch, no
//! invalidation message, nothing whose cost scales with anyone else's write rate. The
//! read mirror ADR-0059 rejected paid for a NATS `watch()` on every write in the
//! fleet; ADR-0062 measured a real app at 264 reads per write.
//!
//! Cost: a write on another node is invisible here until the entry expires. Writes
//! invalidate their own key immediately, so read-your-own-writes holds locally but a
//! cross-node read can be up to `ttl` stale. That is why it is **off by default**.
//!
//! `increment` is never served from cache and always invalidates: a cached
//! read-modify-write is a lost update rather than a stale read.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::kv::{Cas, KvBackend};
use crate::tenant::BucketId;

/// A cached read. `None` is a cached MISS — "this key does not exist" is an answer
/// worth remembering, and a workload that probes absent keys would otherwise pay
/// full price for every one of them.
type Entry = (Instant, Option<Vec<u8>>);

struct CacheState {
    entries: HashMap<String, Entry>,
    order: std::collections::VecDeque<String>,
}

pub struct CacheKv {
    inner: Arc<dyn KvBackend>,
    ttl: Duration,
    max_entries: usize,
    state: Mutex<CacheState>,
    hits: Mutex<(u64, u64)>,
}

/// Bounded FIFO eviction avoids cache stampedes: when capacity is reached, the
/// oldest entry is evicted to make room rather than clearing the entire map wholesale
/// and creating a thundering herd against the underlying backend. ADR-0062 measured
/// a working set of 1 951 keys for a whole app, so the cap is ~50 apps' worth.
const MAX_ENTRIES: usize = 100_000;

impl CacheKv {
    pub fn wrap(inner: Arc<dyn KvBackend>, ttl_ms: u64) -> Arc<Self> {
        Self::with_capacity(inner, ttl_ms, MAX_ENTRIES)
    }

    pub fn with_capacity(inner: Arc<dyn KvBackend>, ttl_ms: u64, max_entries: usize) -> Arc<Self> {
        Arc::new(Self {
            inner,
            ttl: Duration::from_millis(ttl_ms),
            max_entries,
            state: Mutex::new(CacheState {
                entries: HashMap::new(),
                order: std::collections::VecDeque::new(),
            }),
            hits: Mutex::new((0, 0)),
        })
    }

    pub fn report(&self) -> String {
        let (h, m) = *crate::sync::held(&self.hits);
        format!(
            "comp-host --kv-cache-ms {}: {h} hits, {m} misses ({:.1}% served), {} entries held",
            self.ttl.as_millis(),
            100.0 * h as f64 / (h + m).max(1) as f64,
            crate::sync::held(&self.state).entries.len()
        )
    }

    fn fresh(&self, id: &str) -> Option<Option<Vec<u8>>> {
        let state = crate::sync::held(&self.state);
        let (at, v) = state.entries.get(id)?;
        (at.elapsed() < self.ttl).then(|| v.clone())
    }

    fn store(&self, id: String, v: Option<Vec<u8>>) {
        let mut state = crate::sync::held(&self.state);
        if !state.entries.contains_key(&id) {
            while state.entries.len() >= self.max_entries {
                if let Some(old_id) = state.order.pop_front() {
                    state.entries.remove(&old_id);
                } else {
                    break;
                }
            }
            state.order.push_back(id.clone());
        }
        state.entries.insert(id, (Instant::now(), v));
    }

    /// A write makes this node's copy wrong, so it goes. Immediately, and before
    /// the write is reported as done — a window where the old value is still
    /// servable would break read-your-own-writes on the node that did the writing,
    /// which is the one guarantee this design does keep.
    fn drop_key(&self, id: &str) {
        crate::sync::held(&self.state).entries.remove(id);
    }
}

fn id(bucket: &BucketId, key: &str) -> String {
    format!("{}\x1f{}", bucket.as_str(), key)
}

impl KvBackend for CacheKv {
    fn shared(&self) -> bool {
        self.inner.shared()
    }

    fn get(&self, bucket: &BucketId, key: &str) -> Result<Option<Vec<u8>>> {
        let id = id(bucket, key);
        if let Some(v) = self.fresh(&id) {
            crate::sync::held(&self.hits).0 += 1;
            return Ok(v);
        }
        crate::sync::held(&self.hits).1 += 1;
        let v = self.inner.get(bucket, key)?;
        self.store(id, v.clone());
        Ok(v)
    }

    fn set(&self, bucket: &BucketId, key: &str, value: &[u8]) -> Result<()> {
        self.drop_key(&id(bucket, key));
        self.inner.set(bucket, key, value)
    }

    fn delete(&self, bucket: &BucketId, key: &str) -> Result<()> {
        self.drop_key(&id(bucket, key));
        self.inner.delete(bucket, key)
    }

    /// Not served from cache. `exists` is a metadata question and a value cache
    /// that answered it would be asserting absence it never checked.
    fn exists(&self, bucket: &BucketId, key: &str) -> Result<bool> {
        self.inner.exists(bucket, key)
    }

    /// Never cached: a scan's answer changes with any write to any key in the
    /// bucket, so caching it needs bucket-level invalidation this design does not
    /// have. ADR-0062 measured `list_keys` at zero calls for this app anyway.
    fn list_keys(&self, bucket: &BucketId) -> Result<Vec<String>> {
        self.inner.list_keys(bucket)
    }

    fn increment(&self, bucket: &BucketId, key: &str, delta: u64) -> Result<u64> {
        self.drop_key(&id(bucket, key));
        self.inner.increment(bucket, key, delta)
    }

    /// **Never served from cache** — this is what makes the cache safe to turn on.
    /// It is the read half of a compare-and-set, so it must report the revision the
    /// store is actually at; a cached one would let the guard agree with itself about
    /// state that no longer exists, the lost update ADR-0065 measured.
    ///
    /// It also REFRESHES the cache: otherwise a losing writer that re-reads through
    /// the cache keeps offering the same stale revision and cannot converge until
    /// the entry expires.
    fn get_revision(&self, bucket: &BucketId, key: &str) -> Result<Option<(u64, Vec<u8>)>> {
        let got = self.inner.get_revision(bucket, key)?;
        self.store(id(bucket, key), got.as_ref().map(|(_, v)| v.clone()));
        Ok(got)
    }

    fn set_if_revision(
        &self,
        bucket: &BucketId,
        key: &str,
        value: &[u8],
        expected: u64,
    ) -> Result<Cas> {
        // Dropped whatever the outcome: a refused write means someone else moved
        // the key, so this node's copy is wrong either way.
        self.drop_key(&id(bucket, key));
        self.inner.set_if_revision(bucket, key, value, expected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kv::MemoryKv;

    fn cache(ttl_ms: u64) -> (Arc<CacheKv>, Arc<MemoryKv>) {
        let inner = Arc::new(MemoryKv::default());
        (CacheKv::wrap(inner.clone(), ttl_ms), inner)
    }

    #[test]
    fn a_repeat_read_is_served_without_touching_the_backend() {
        let (c, inner) = cache(60_000);
        let b = BucketId::for_test("b");
        inner.set(&b, "k", b"first").unwrap();
        assert_eq!(c.get(&b, "k").unwrap().as_deref(), Some(&b"first"[..]));
        // Behind the cache's back, as another node would.
        inner.set(&b, "k", b"second").unwrap();
        assert_eq!(
            c.get(&b, "k").unwrap().as_deref(),
            Some(&b"first"[..]),
            "within the TTL a neighbour's write is invisible — the documented cost"
        );
    }

    #[test]
    fn this_nodes_own_write_is_visible_to_this_nodes_next_read() {
        // The one guarantee kept. Without it a component could not read back what
        // it just stored, which no application tolerates.
        let (c, _) = cache(60_000);
        let b = BucketId::for_test("b");
        c.set(&b, "k", b"v1").unwrap();
        assert_eq!(c.get(&b, "k").unwrap().as_deref(), Some(&b"v1"[..]));
        c.set(&b, "k", b"v2").unwrap();
        assert_eq!(
            c.get(&b, "k").unwrap().as_deref(),
            Some(&b"v2"[..]),
            "stale after its own write"
        );
        c.delete(&b, "k").unwrap();
        assert_eq!(c.get(&b, "k").unwrap(), None, "a deleted key still read back");
    }

    #[test]
    fn an_entry_expires() {
        let (c, inner) = cache(1);
        let b = BucketId::for_test("b");
        inner.set(&b, "k", b"first").unwrap();
        let _ = c.get(&b, "k");
        inner.set(&b, "k", b"second").unwrap();
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(
            c.get(&b, "k").unwrap().as_deref(),
            Some(&b"second"[..]),
            "staleness must be bounded by the TTL and nothing else"
        );
    }

    #[test]
    fn increment_is_never_served_from_cache() {
        // The lost-update case. Three increments must be three, whatever is cached.
        let (c, _) = cache(60_000);
        let b = BucketId::for_test("b");
        assert_eq!(c.increment(&b, "n", 1).unwrap(), 1);
        assert_eq!(c.increment(&b, "n", 1).unwrap(), 2);
        assert_eq!(c.increment(&b, "n", 1).unwrap(), 3);
        assert_eq!(c.get(&b, "n").unwrap().as_deref(), Some(&b"3"[..]));
    }

    #[test]
    fn a_miss_is_cached_and_a_write_uncaches_it() {
        let (c, inner) = cache(60_000);
        let b = BucketId::for_test("b");
        assert_eq!(c.get(&b, "gone").unwrap(), None);
        inner.set(&b, "gone", b"appeared").unwrap();
        assert_eq!(c.get(&b, "gone").unwrap(), None, "a cached miss is an answer, and it holds");
        c.set(&b, "gone", b"mine").unwrap();
        assert_eq!(c.get(&b, "gone").unwrap().as_deref(), Some(&b"mine"[..]));
    }

    #[test]
    fn cache_eviction_evicts_oldest_without_clearing_hot_keys() {
        let inner = Arc::new(MemoryKv::default());
        let c = CacheKv::with_capacity(inner.clone(), 60_000, 3);
        let b = BucketId::for_test("b");

        // Seed 3 keys directly in inner store
        inner.set(&b, "k1", b"v1").unwrap();
        inner.set(&b, "k2", b"v2").unwrap();
        inner.set(&b, "k3", b"v3").unwrap();
        inner.set(&b, "k4", b"v4").unwrap();

        // Populate cache for k1, k2, k3
        assert_eq!(c.get(&b, "k1").unwrap().as_deref(), Some(&b"v1"[..]));
        assert_eq!(c.get(&b, "k2").unwrap().as_deref(), Some(&b"v2"[..]));
        assert_eq!(c.get(&b, "k3").unwrap().as_deref(), Some(&b"v3"[..]));

        // Mutate inner directly behind the cache's back
        inner.set(&b, "k1", b"v1_mutated").unwrap();
        inner.set(&b, "k2", b"v2_mutated").unwrap();
        inner.set(&b, "k3", b"v3_mutated").unwrap();

        // Accessing k4 pushes cache beyond capacity (cap 3) -> should evict k1 (oldest)
        assert_eq!(c.get(&b, "k4").unwrap().as_deref(), Some(&b"v4"[..]));

        // k2 and k3 MUST STILL BE HOT in cache (returning original v2 and v3, not mutated)
        assert_eq!(
            c.get(&b, "k2").unwrap().as_deref(),
            Some(&b"v2"[..]),
            "k2 must remain in cache without getting wiped"
        );
        assert_eq!(
            c.get(&b, "k3").unwrap().as_deref(),
            Some(&b"v3"[..]),
            "k3 must remain in cache without getting wiped"
        );

        // k1 was evicted when k4 was stored, so reading k1 hits inner and gets the mutated value
        assert_eq!(
            c.get(&b, "k1").unwrap().as_deref(),
            Some(&b"v1_mutated"[..]),
            "k1 should have been evicted and re-fetched"
        );
    }
}
