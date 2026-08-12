//! `--kv-profile` — what a real application actually asks the store for.
//!
//! Every caching decision this platform has taken was measured on `gate-domain`,
//! a rate limiter that writes on every single request. That is the least
//! representative workload there is for a read cache, and it is why ADR-0059
//! rejected the read mirror: the mirror's upkeep scales with the fleet's WRITE
//! rate, so a write-per-request benchmark was always going to lose.
//!
//! What was never measured is what a real app does. This wraps whatever backend
//! `--kv` chose and counts, per operation: how many, how long, and — the number
//! that actually decides whether a read cache is worth building — how many reads
//! a perfect cache would have served.
//!
//! ## The hit-rate model, and what it is worth
//!
//! A key is "warm" once read, and any write to it makes it cold again. A read of a
//! warm key is one a cache with **infinite capacity and perfect invalidation**
//! would have served without touching the backend.
//!
//! That is an UPPER BOUND, deliberately. A real cache has a size limit, and across
//! several nodes it also has a coherence problem this single-process model ignores
//! entirely — a write on another node makes this node's entry stale, and nothing
//! here accounts for that. So a low number here is conclusive (do not build it) and
//! a high number is permission to try, not proof it will work.
//!
//! Off by default and not on the request path when off: without the flag the real
//! backend is handed out directly and this file allocates nothing.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;

use anyhow::Result;

use crate::kv::{Cas, KvBackend};
use crate::tenant::BucketId;

#[derive(Default, Clone, Copy)]
struct Op {
    calls: u64,
    nanos: u128,
}

impl Op {
    fn add(&mut self, d: std::time::Duration) {
        self.calls += 1;
        self.nanos += d.as_nanos();
    }

    fn mean_us(&self) -> f64 {
        if self.calls == 0 {
            0.0
        } else {
            self.nanos as f64 / self.calls as f64 / 1000.0
        }
    }
}

#[derive(Default)]
struct Stats {
    get: Op,
    set: Op,
    delete: Op,
    exists: Op,
    list_keys: Op,
    increment: Op,
    get_revision: Op,
    set_if_revision: Op,
    /// Reads of a key that was already warm — what a perfect cache would serve.
    warm_reads: u64,
    /// Distinct keys currently warm, for a sense of the working set a cache would
    /// have to hold to reach the hit rate above.
    warm: HashSet<String>,
    peak_warm: usize,
    /// Reads per key, and writes per key.
    ///
    /// A hit rate says a cache would work; it does not say WHAT to cache. A key
    /// read a million times and written once is a different proposition from a
    /// million keys read once each — the first is a small targeted cache with an
    /// easy invalidation story, the second needs the general mirror ADR-0059
    /// rejected. Only per-key counts tell them apart.
    reads_by_key: HashMap<String, u64>,
    writes_by_key: HashMap<String, u64>,
}

impl Stats {
    fn touch(&mut self, k: String) -> bool {
        *self.reads_by_key.entry(k.clone()).or_default() += 1;
        let hit = self.warm.contains(&k);
        if hit {
            self.warm_reads += 1;
        } else {
            self.warm.insert(k);
            self.peak_warm = self.peak_warm.max(self.warm.len());
        }
        hit
    }

    fn invalidate(&mut self, k: &str) {
        *self.writes_by_key.entry(k.to_string()).or_default() += 1;
        self.warm.remove(k);
    }
}

pub struct ProfileKv {
    inner: Arc<dyn KvBackend>,
    stats: Mutex<Stats>,
}

impl ProfileKv {
    pub fn wrap(inner: Arc<dyn KvBackend>) -> Arc<Self> {
        Arc::new(Self { inner, stats: Mutex::new(Stats::default()) })
    }

    /// One line per operation, then the two numbers this exists for.
    ///
    /// Printed on the way out rather than periodically: a benchmark's warm-up and
    /// its steady state have different mixes, and a running total is the honest
    /// summary of the whole run either way.
    pub fn report(&self) -> String {
        let s = self.stats.lock().unwrap();
        let reads = s.get.calls + s.exists.calls + s.list_keys.calls + s.get_revision.calls;
        let writes = s.set.calls + s.delete.calls + s.increment.calls + s.set_if_revision.calls;
        let total = reads + writes;
        let mut out = String::from("\ncomp-host --kv-profile: what the app asked the store for\n\n");
        out.push_str("  op            calls        mean us      share\n");
        for (name, op) in [
            ("get", s.get),
            ("set", s.set),
            ("delete", s.delete),
            ("exists", s.exists),
            ("list_keys", s.list_keys),
            ("increment", s.increment),
            ("get_rev", s.get_revision),
            ("cas_set", s.set_if_revision),
        ] {
            if op.calls == 0 {
                continue;
            }
            out.push_str(&format!(
                "  {name:<10} {:>8}      {:>8.1}    {:>6.1}%\n",
                op.calls,
                op.mean_us(),
                100.0 * op.calls as f64 / total.max(1) as f64
            ));
        }
        out.push_str(&format!(
            "\n  reads {reads}, writes {writes} — {:.1}% read\n",
            100.0 * reads as f64 / total.max(1) as f64
        ));
        // The decider. `get` only: `list_keys` is a scan, which no key-level cache
        // serves, and counting it as a hit would flatter the model.
        out.push_str(&format!(
            "  a perfect cache would have served {}/{} gets ({:.1}%), holding {} keys\n",
            s.warm_reads,
            s.get.calls,
            100.0 * s.warm_reads as f64 / s.get.calls.max(1) as f64,
            s.peak_warm
        ));
        out.push_str(
            "\n  Upper bound: infinite capacity, perfect invalidation, one process. A\n\
             \x20 multi-node cache also has to notice another node's writes, which this\n\
             \x20 does not model (ADR-0059).\n",
        );

        // Which keys, because the hit rate does not say what to cache.
        let mut hot: Vec<(&String, &u64)> = s.reads_by_key.iter().collect();
        hot.sort_by(|a, b| b.1.cmp(a.1));
        out.push_str("\n  hottest keys by read count\n");
        out.push_str("       reads   writes    share  key\n");
        for (k, n) in hot.iter().take(12) {
            out.push_str(&format!(
                "  {:>10}  {:>7}   {:>5.1}%  {}\n",
                n,
                s.writes_by_key.get(*k).copied().unwrap_or(0),
                100.0 * **n as f64 / s.get.calls.max(1) as f64,
                // The bucket prefix is the same for every key in a single-app run
                // and just eats the column.
                k.split('\x1f').next_back().unwrap_or(k)
            ));
        }
        let top: u64 = hot.iter().take(12).map(|(_, n)| **n).sum();
        out.push_str(&format!(
            "\n  those 12 keys are {:.1}% of all gets; {} distinct keys were read\n",
            100.0 * top as f64 / s.get.calls.max(1) as f64,
            s.reads_by_key.len()
        ));
        // A key never written during the run is one a cache can hold without ever
        // being wrong about it — the population a targeted cache would serve.
        let (never_written, nw_reads) = s
            .reads_by_key
            .iter()
            .filter(|(k, _)| !s.writes_by_key.contains_key(*k))
            .fold((0u64, 0u64), |(c, r), (_, n)| (c + 1, r + n));
        out.push_str(&format!(
            "  {} of them were never written: {:.1}% of all gets\n",
            never_written,
            100.0 * nw_reads as f64 / s.get.calls.max(1) as f64
        ));
        out
    }
}

/// Namespaced the way the flat backends do it, so two buckets with the same key
/// name are two entries here as well.
fn id(bucket: &BucketId, key: &str) -> String {
    format!("{}\x1f{}", bucket.as_str(), key)
}

/// Time the inner call, never change its answer. A profiler that alters what it
/// measures is worse than no profiler, so every method here is the real one with a
/// clock either side.
impl KvBackend for ProfileKv {
    fn shared(&self) -> bool {
        self.inner.shared()
    }

    fn get(&self, bucket: &BucketId, key: &str) -> Result<Option<Vec<u8>>> {
        let t = Instant::now();
        let r = self.inner.get(bucket, key);
        let d = t.elapsed();
        let mut s = self.stats.lock().unwrap();
        s.get.add(d);
        s.touch(id(bucket, key));
        r
    }

    fn set(&self, bucket: &BucketId, key: &str, value: &[u8]) -> Result<()> {
        let t = Instant::now();
        let r = self.inner.set(bucket, key, value);
        let d = t.elapsed();
        let mut s = self.stats.lock().unwrap();
        s.set.add(d);
        s.invalidate(&id(bucket, key));
        r
    }

    fn delete(&self, bucket: &BucketId, key: &str) -> Result<()> {
        let t = Instant::now();
        let r = self.inner.delete(bucket, key);
        let d = t.elapsed();
        let mut s = self.stats.lock().unwrap();
        s.delete.add(d);
        s.invalidate(&id(bucket, key));
        r
    }

    fn exists(&self, bucket: &BucketId, key: &str) -> Result<bool> {
        let t = Instant::now();
        let r = self.inner.exists(bucket, key);
        let d = t.elapsed();
        // Counted as a read but NOT as a cache hit: `exists` answers from metadata a
        // value cache does not necessarily hold.
        self.stats.lock().unwrap().exists.add(d);
        r
    }

    fn list_keys(&self, bucket: &BucketId) -> Result<Vec<String>> {
        let t = Instant::now();
        let r = self.inner.list_keys(bucket);
        let d = t.elapsed();
        self.stats.lock().unwrap().list_keys.add(d);
        r
    }

    fn increment(&self, bucket: &BucketId, key: &str, delta: u64) -> Result<u64> {
        let t = Instant::now();
        let r = self.inner.increment(bucket, key, delta);
        let d = t.elapsed();
        let mut s = self.stats.lock().unwrap();
        s.increment.add(d);
        // A read-modify-write, and the reason ADR-0059's mirror excluded it: a
        // cached read here is a LOST UPDATE, not a stale one.
        s.invalidate(&id(bucket, key));
        r
    }

    /// Counted as a read, and deliberately NOT as a cacheable one: it is the read
    /// half of a compare-and-set, which no cache may serve (ADR-0065). Counting it
    /// toward the hit rate would inflate a number whose whole purpose is to decide
    /// whether caching is worth it.
    fn get_revision(&self, bucket: &BucketId, key: &str) -> Result<Option<(u64, Vec<u8>)>> {
        let t = Instant::now();
        let r = self.inner.get_revision(bucket, key);
        let d = t.elapsed();
        self.stats.lock().unwrap().get_revision.add(d);
        r
    }

    fn set_if_revision(
        &self,
        bucket: &BucketId,
        key: &str,
        value: &[u8],
        expected: u64,
    ) -> Result<Cas> {
        let t = Instant::now();
        let r = self.inner.set_if_revision(bucket, key, value, expected);
        let d = t.elapsed();
        let mut s = self.stats.lock().unwrap();
        s.set_if_revision.add(d);
        s.invalidate(&id(bucket, key));
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kv::MemoryKv;

    fn wrapped() -> Arc<ProfileKv> {
        ProfileKv::wrap(Arc::new(MemoryKv::default()))
    }

    #[test]
    fn a_write_makes_a_key_cold_again() {
        let p = wrapped();
        let b = BucketId::for_test("b");
        p.set(&b, "k", b"1").unwrap();
        let _ = p.get(&b, "k"); // cold: first read
        let _ = p.get(&b, "k"); // warm: a cache would serve this
        p.set(&b, "k", b"2").unwrap();
        let _ = p.get(&b, "k"); // cold again — the write invalidated it

        let s = p.stats.lock().unwrap();
        assert_eq!(s.get.calls, 3);
        assert_eq!(s.warm_reads, 1, "only the repeat read with no write between it counts");
    }

    #[test]
    fn two_buckets_with_one_key_name_are_two_entries() {
        // Otherwise the model would report a cross-tenant hit — the one number that
        // must never be flattered, since ADR-0023 exists to make it impossible.
        let p = wrapped();
        let (a, b) = (BucketId::for_test("a"), BucketId::for_test("b"));
        let _ = p.get(&a, "k");
        let _ = p.get(&b, "k");
        assert_eq!(p.stats.lock().unwrap().warm_reads, 0, "a neighbour's key is not a hit");
    }

    #[test]
    fn increment_is_never_a_hit() {
        let p = wrapped();
        let b = BucketId::for_test("c");
        for _ in 0..3 {
            p.increment(&b, "n", 1).unwrap();
        }
        let s = p.stats.lock().unwrap();
        assert_eq!(s.increment.calls, 3);
        assert_eq!(s.warm_reads, 0, "a cached increment is a lost update, not a stale read");
    }
}
