//! `--kv-cache-ms` — a per-node read cache whose only correctness argument is a
//! clock.
//!
//! ## Why this shape, and not the one ADR-0059 rejected
//!
//! The read mirror kept a node's copy of a bucket fresh with a NATS `watch()`. It
//! lost by 2.3× for a structural reason: the watch delivers every write to every
//! node, so upkeep scales with the FLEET'S write rate, and the benchmark component
//! wrote on every request.
//!
//! ADR-0062 measured a real application instead: 264 reads per write, and the
//! hottest key in the run took 399 697 reads against 1 write. At that ratio the
//! expensive half of the mirror — knowing immediately when someone else writes —
//! is buying protection against something that almost never happens, and paying
//! for it on every write in the fleet.
//!
//! So this has **no coherence protocol at all**. An entry lives for `ttl` and then
//! it is gone. There is no watch, no invalidation message, no subscription, and
//! nothing whose cost scales with anyone else's write rate. The staleness bound is
//! the TTL and nothing else.
//!
//! ## What that costs, stated plainly
//!
//! **A write on another node is invisible here until the entry expires.** Within
//! one node writes invalidate their own key immediately, so read-your-own-writes
//! holds locally; across nodes it does not, and a read can be up to `ttl` stale.
//!
//! That is a real semantic change and it is why this is **off by default**. It is
//! sound for an app whose reads tolerate a bounded lag and unsound for one that
//! does not, and no amount of tuning changes which of those an app is.
//!
//! `increment` is never served from cache and always invalidates: a cached
//! read-modify-write is a lost update rather than a stale read, which is the same
//! exclusion the mirror made and the one line of it worth keeping.

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

pub struct CacheKv {
    inner: Arc<dyn KvBackend>,
    ttl: Duration,
    entries: Mutex<HashMap<String, Entry>>,
    hits: Mutex<(u64, u64)>,
}

/// ponytail: one flat map behind one mutex, cleared wholesale when it grows past
/// this. ADR-0062 measured a working set of 1 951 keys for a whole app, so the cap
/// is ~50 apps' worth and a clear should be rare. An LRU when a real deployment
/// says otherwise — and it will say so through this counter, not through a guess.
const MAX_ENTRIES: usize = 100_000;

impl CacheKv {
    pub fn wrap(inner: Arc<dyn KvBackend>, ttl_ms: u64) -> Arc<Self> {
        Arc::new(Self {
            inner,
            ttl: Duration::from_millis(ttl_ms),
            entries: Mutex::new(HashMap::new()),
            hits: Mutex::new((0, 0)),
        })
    }

    pub fn report(&self) -> String {
        let (h, m) = *self.hits.lock().unwrap();
        format!(
            "comp-host --kv-cache-ms {}: {h} hits, {m} misses ({:.1}% served), {} entries held",
            self.ttl.as_millis(),
            100.0 * h as f64 / (h + m).max(1) as f64,
            self.entries.lock().unwrap().len()
        )
    }

    fn fresh(&self, id: &str) -> Option<Option<Vec<u8>>> {
        let entries = self.entries.lock().unwrap();
        let (at, v) = entries.get(id)?;
        (at.elapsed() < self.ttl).then(|| v.clone())
    }

    fn store(&self, id: String, v: Option<Vec<u8>>) {
        let mut entries = self.entries.lock().unwrap();
        if entries.len() >= MAX_ENTRIES {
            entries.clear();
        }
        entries.insert(id, (Instant::now(), v));
    }

    /// A write makes this node's copy wrong, so it goes. Immediately, and before
    /// the write is reported as done — a window where the old value is still
    /// servable would break read-your-own-writes on the node that did the writing,
    /// which is the one guarantee this design does keep.
    fn drop_key(&self, id: &str) {
        self.entries.lock().unwrap().remove(id);
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
            self.hits.lock().unwrap().0 += 1;
            return Ok(v);
        }
        self.hits.lock().unwrap().1 += 1;
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

    /// **Never served from cache**, and this is the line that makes the cache safe
    /// to turn on at all.
    ///
    /// This is the read half of a compare-and-set: its whole job is to report the
    /// revision the store is actually at. Answering it from a copy would hand back a
    /// revision that was true once, the guarded write would then be built on it, and
    /// the guard would agree with itself about state that no longer exists — which
    /// is precisely the lost update ADR-0065 measured.
    ///
    /// A cached plain `get` can still be stale, and that is the documented,
    /// TTL-bounded trade (ADR-0064). It can no longer cost a write.
    /// It also REFRESHES the cache, which is what makes a losing writer able to
    /// make progress.
    ///
    /// Without it, a component that re-reads through the cache to build its retry
    /// keeps offering the same stale revision, is refused again, and cannot
    /// converge until the entry expires — the lost update becomes a failed request
    /// for up to the TTL, which is better and still wrong. This read is
    /// authoritative and already in hand, so putting it in the cache costs nothing
    /// and replaces exactly the entry that was causing the refusals.
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
        assert_eq!(c.get(&b, "k").unwrap().as_deref(), Some(&b"v2"[..]), "stale after its own write");
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
}
