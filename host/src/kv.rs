//! Swappable key-value backends for the host's `wasi:keyvalue` implementation.
//!
//! The guest (the composed vet-domain wasm) calls `wasi:keyvalue/store` +
//! `atomics`; the host satisfies them, and WHICH durable store backs them is a
//! deployment choice — `--kv memory|redis|sqlite` — not a component change. Same
//! wasm bytes, different `KvBackend`.
//!
//! All methods are SYNCHRONOUS (the bindgen store trait is sync); redis uses the
//! blocking client, which is fine in the per-request handler.
//!
//! Keys are namespaced `{bucket}\x1f{key}` for the flat stores (redis) so named
//! buckets don't collide; sqlite uses a `(bucket, key)` primary key.
//!
//! A `bucket` here is a `BucketId`, which only a `Scope` can mint. That type is
//! the ADR-0012 fix: nothing a guest says can reach this file.

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::{Context, Result};

use comp_lattice::nats::servers;

use crate::tenant::BucketId;

/// A named-bucket key-value store. Errors surface as anyhow; the caller maps
/// them to the wasi:keyvalue `error` variant.
pub trait KvBackend: Send + Sync {
    /// Can every replica of an app see this store, wherever the replica runs?
    ///
    /// Deliberately a method on the implementation and NOT a match on a backend
    /// name somewhere central. "Shared" is a property of what a backend *is*, so a
    /// new one declares it here and nothing else has to be edited to know.
    ///
    /// There is no default. A backend that forgot to answer would be guessed at,
    /// and both guesses are wrong in expensive ways: `true` places an app somewhere
    /// it silently diverges, `false` refuses a deployment that would have been fine.
    /// The compiler asking is cheaper than either.
    fn shared(&self) -> bool;

    fn get(&self, bucket: &BucketId, key: &str) -> Result<Option<Vec<u8>>>;
    fn set(&self, bucket: &BucketId, key: &str, value: &[u8]) -> Result<()>;
    fn delete(&self, bucket: &BucketId, key: &str) -> Result<()>;
    fn exists(&self, bucket: &BucketId, key: &str) -> Result<bool>;
    fn list_keys(&self, bucket: &BucketId) -> Result<Vec<String>>;
    /// Atomic increment of an integer stored as a decimal string. Returns the
    /// new value. (redis INCRBY; in-memory read-modify-write; sqlite transactional.)
    fn increment(&self, bucket: &BucketId, key: &str, delta: u64) -> Result<u64>;

    /// The value together with the revision it is at, or `None` if absent.
    ///
    /// The read half of a compare-and-set. Every backend keeps a revision per key
    /// and bumps it on every write, `set` included — a revision that only moved for
    /// guarded writes would let a plain `set` slip past a guard silently.
    fn get_revision(&self, bucket: &BucketId, key: &str) -> Result<Option<(u64, Vec<u8>)>>;

    /// Write only if the key is still at `expected`. `expected == 0` means "must not
    /// exist yet", so a create and an update are the same call.
    ///
    /// This exists because ADR-0065 measured a lost update:
    /// `record-store::update` enforced its revision guard by reading, comparing and
    /// writing over three separate calls, which is not a guard at all once anything
    /// — another node, or a cache — changes the value in between. The comparison has
    /// to happen where the data is, and this is the smallest primitive that puts it
    /// there.
    ///
    /// No default implementation, for the same reason `shared` has none: a backend
    /// that quietly emulated this with get-then-set would compile, pass, and lose
    /// writes.
    fn set_if_revision(
        &self,
        bucket: &BucketId,
        key: &str,
        value: &[u8],
        expected: u64,
    ) -> Result<Cas>;
}

/// What a revision-guarded write did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cas {
    /// It landed. Carries the revision it landed at.
    Committed(u64),
    /// It did not. Carries the revision the store actually holds — 0 when the key
    /// is absent — so a caller can re-read and retry without a second round trip
    /// to find out what it missed.
    Conflict(u64),
}

// ---- in-memory (default) -------------------------------------------------

/// A value and the revision it is at. Every write bumps it, including a plain
/// `set` — see `get_revision` on the trait for why that matters.
type Versioned = (u64, Vec<u8>);

#[derive(Default)]
pub struct MemoryKv {
    buckets: Mutex<HashMap<String, HashMap<String, Versioned>>>,
}

impl KvBackend for MemoryKv {
    /// One process's heap. Nothing outside it can see this.
    fn shared(&self) -> bool {
        false
    }

    fn get(&self, bucket: &BucketId, key: &str) -> Result<Option<Vec<u8>>> {
        let bucket = bucket.as_str();
        Ok(crate::sync::held(&self.buckets)
            .get(bucket)
            .and_then(|b| b.get(key))
            .map(|(_, v)| v.clone()))
    }
    fn set(&self, bucket: &BucketId, key: &str, value: &[u8]) -> Result<()> {
        let bucket = bucket.as_str();
        let mut g = crate::sync::held(&self.buckets);
        let b = g.entry(bucket.into()).or_default();
        let rev = b.get(key).map(|(r, _)| *r).unwrap_or(0) + 1;
        b.insert(key.into(), (rev, value.to_vec()));
        Ok(())
    }
    fn delete(&self, bucket: &BucketId, key: &str) -> Result<()> {
        let bucket = bucket.as_str();
        if let Some(b) = crate::sync::held(&self.buckets).get_mut(bucket) {
            b.remove(key);
        }
        Ok(())
    }
    fn exists(&self, bucket: &BucketId, key: &str) -> Result<bool> {
        let bucket = bucket.as_str();
        Ok(crate::sync::held(&self.buckets)
            .get(bucket)
            .map(|b| b.contains_key(key))
            .unwrap_or(false))
    }
    fn list_keys(&self, bucket: &BucketId) -> Result<Vec<String>> {
        let bucket = bucket.as_str();
        Ok(crate::sync::held(&self.buckets)
            .get(bucket)
            .map(|b| b.keys().cloned().collect())
            .unwrap_or_default())
    }
    fn increment(&self, bucket: &BucketId, key: &str, delta: u64) -> Result<u64> {
        let bucket = bucket.as_str();
        let mut g = crate::sync::held(&self.buckets);
        let b = g.entry(bucket.into()).or_default();
        let (rev, cur) = b
            .get(key)
            .map(|(r, v)| {
                (*r, std::str::from_utf8(v).ok().and_then(|s| s.parse().ok()).unwrap_or(0u64))
            })
            .unwrap_or((0, 0));
        let next = cur.saturating_add(delta);
        b.insert(key.into(), (rev + 1, next.to_string().into_bytes()));
        Ok(next)
    }

    fn get_revision(&self, bucket: &BucketId, key: &str) -> Result<Option<Versioned>> {
        let bucket = bucket.as_str();
        Ok(crate::sync::held(&self.buckets).get(bucket).and_then(|b| b.get(key)).cloned())
    }

    /// Atomic because the compare and the write happen under one lock. That is the
    /// whole difference from the three-call version this replaces: nothing can land
    /// between the two halves, because nothing else can hold this mutex.
    fn set_if_revision(
        &self,
        bucket: &BucketId,
        key: &str,
        value: &[u8],
        expected: u64,
    ) -> Result<Cas> {
        let bucket = bucket.as_str();
        let mut g = crate::sync::held(&self.buckets);
        let b = g.entry(bucket.into()).or_default();
        let current = b.get(key).map(|(r, _)| *r).unwrap_or(0);
        if current != expected {
            return Ok(Cas::Conflict(current));
        }
        let next = current + 1;
        b.insert(key.into(), (next, value.to_vec()));
        Ok(Cas::Committed(next))
    }
}

// ---- redis ----------------------------------------------------------------
// Flat keyspace; bucket+key joined with a unit separator. list_keys uses SCAN
// over the `{bucket}\x1f*` prefix. A single shared blocking connection guarded
// by a mutex (the per-request handler is brief).

const SEP: char = '\u{1f}';

pub struct RedisKv {
    conn: Mutex<redis::Connection>,
}

impl RedisKv {
    pub fn connect(url: &str) -> Result<Self> {
        let client = redis::Client::open(url).context("redis client")?;
        let conn = client.get_connection().context("redis connect")?;
        Ok(Self { conn: Mutex::new(conn) })
    }
    fn k(bucket: &str, key: &str) -> String {
        format!("{bucket}{SEP}{key}")
    }

    /// Where a key's revision lives. Prefixed rather than suffixed so it cannot be
    /// caught by `list_keys`, which scans `{bucket}{SEP}*`.
    fn rev_k(bucket: &str, key: &str) -> String {
        format!("__rev{SEP}{bucket}{SEP}{key}")
    }
}

impl KvBackend for RedisKv {
    /// One server every node dials. Shared — though see ADR-0023 on why shared and
    /// isolated are different properties, and this only claims the first.
    fn shared(&self) -> bool {
        true
    }

    fn get(&self, bucket: &BucketId, key: &str) -> Result<Option<Vec<u8>>> {
        let bucket = bucket.as_str();
        use redis::Commands;
        let mut c = crate::sync::held(&self.conn);
        let v: Option<Vec<u8>> = c.get(Self::k(bucket, key)).context("redis get")?;
        Ok(v)
    }
    fn set(&self, bucket: &BucketId, key: &str, value: &[u8]) -> Result<()> {
        let bucket = bucket.as_str();
        let mut c = crate::sync::held(&self.conn);
        // The revision moves for a plain `set` too, or a guarded write could land
        // on top of one and never know.
        redis::pipe()
            .atomic()
            .set(Self::k(bucket, key), value)
            .ignore()
            .incr(Self::rev_k(bucket, key), 1)
            .ignore()
            .query::<()>(&mut c)
            .context("redis set")?;
        Ok(())
    }
    fn delete(&self, bucket: &BucketId, key: &str) -> Result<()> {
        let bucket = bucket.as_str();
        use redis::Commands;
        let mut c = crate::sync::held(&self.conn);
        // The revision goes with it: a lingering one would make the next create
        // look like an update of something that is not there.
        c.del::<_, ()>(&[Self::k(bucket, key), Self::rev_k(bucket, key)][..])
            .context("redis del")?;
        Ok(())
    }
    fn exists(&self, bucket: &BucketId, key: &str) -> Result<bool> {
        let bucket = bucket.as_str();
        use redis::Commands;
        let mut c = crate::sync::held(&self.conn);
        c.exists(Self::k(bucket, key)).context("redis exists")
    }
    fn list_keys(&self, bucket: &BucketId) -> Result<Vec<String>> {
        let bucket = bucket.as_str();
        use redis::Commands;
        let mut c = crate::sync::held(&self.conn);
        let prefix = format!("{bucket}{SEP}");
        let pattern = format!("{prefix}*");
        let keys: Vec<String> = c.scan_match(pattern).context("redis scan")?.collect();
        Ok(keys.into_iter().map(|k| k.trim_start_matches(&prefix).to_string()).collect())
    }
    fn increment(&self, bucket: &BucketId, key: &str, delta: u64) -> Result<u64> {
        let bucket = bucket.as_str();
        let mut c = crate::sync::held(&self.conn);
        let (next,): (i64,) = redis::pipe()
            .atomic()
            .incr(Self::k(bucket, key), delta as i64)
            .incr(Self::rev_k(bucket, key), 1)
            .ignore()
            .query(&mut c)
            .context("redis incrby")?;
        Ok(next as u64)
    }

    fn get_revision(&self, bucket: &BucketId, key: &str) -> Result<Option<Versioned>> {
        let bucket = bucket.as_str();
        use redis::Commands;
        let mut c = crate::sync::held(&self.conn);
        // One MGET so the value and its revision come from the same instant. Two
        // round trips could straddle a write and hand back a revision that does not
        // belong to the value beside it.
        let (v, rev): (Option<Vec<u8>>, Option<i64>) =
            c.mget(&[Self::k(bucket, key), Self::rev_k(bucket, key)][..]).context("redis mget")?;
        Ok(v.map(|v| (rev.unwrap_or(0).max(0) as u64, v)))
    }

    /// Atomic because Redis runs a script to completion with nothing interleaved,
    /// so the compare and the write are one operation server-side. A pipeline would
    /// not do: `MULTI` queues commands but cannot branch on what it read.
    fn set_if_revision(
        &self,
        bucket: &BucketId,
        key: &str,
        value: &[u8],
        expected: u64,
    ) -> Result<Cas> {
        let bucket = bucket.as_str();
        let script = redis::Script::new(
            r#"
            local cur = tonumber(redis.call('GET', KEYS[2]) or '0')
            if redis.call('EXISTS', KEYS[1]) == 0 then cur = 0 end
            if cur ~= tonumber(ARGV[2]) then return {0, cur} end
            local nxt = cur + 1
            redis.call('SET', KEYS[1], ARGV[1])
            redis.call('SET', KEYS[2], nxt)
            return {1, nxt}
            "#,
        );
        let mut c = crate::sync::held(&self.conn);
        let (ok, rev): (i64, i64) = script
            .key(Self::k(bucket, key))
            .key(Self::rev_k(bucket, key))
            .arg(value)
            .arg(expected.to_string())
            .invoke(&mut c)
            .context("redis cas script")?;
        Ok(if ok == 1 { Cas::Committed(rev as u64) } else { Cas::Conflict(rev.max(0) as u64) })
    }
}

/// Base interval for the compare-and-set retry backoff, in milliseconds. The same
/// 5ms wasmCloud's NATS keyvalue provider uses, for the same reason.
const CAS_BACKOFF_MS: u64 = 5;

/// What an unconfigured `--kv-replicas` aims for. Three is the smallest number
/// that survives losing a server, because quorum needs a majority (ADR-0067).
pub const DEFAULT_REPLICAS: usize = 3;

// ---- nats (JetStream KV) --------------------------------------------------
//
// The only backend on this list where TWO REPLICAS OF ONE APP SEE ONE STORE.
//
// That is not a nicety, it is the difference between a rate limiter that rate-limits
// and one that does not. `memory` and `sqlite` are node-local: spread an app over two
// nodes and each replica gets its own store under the same bucket name, so a counter
// counts wrong, a session vanishes on every other request, and a failover moves the
// placement without moving the data. Measured, and it is why this came back.
//
// It runs on `async-nats` — the same client the lattice agent uses, because the sync
// one cannot unify `rand` with it. `KvBackend` is a sync trait called from sync
// bindgen imports, so each method bridges with `block_in_place` + `Handle::block_on`.
// That is legal on the multi-threaded runtime (`#[tokio::main]`'s default): it tells
// tokio this worker is about to block so the others are not starved.
//
// ponytail: block_in_place per call; the principled fix is async bindgen imports and
// an async KvBackend, which is a refactor touching every impl in main.rs. Do it when
// something other than this needs it.

use async_nats::jetstream::kv::Store as JsStore;

pub struct NatsKv {
    handle: tokio::runtime::Handle,
    js: async_nats::jetstream::Context,
    /// How many copies of each bucket JetStream keeps.
    ///
    /// One is the default and one is a single disk holding every tenant's data.
    /// ADR-0035 measured this fleet surviving the loss of a HOST; nothing has ever
    /// measured it surviving the loss of the STORE, and with one replica it does
    /// not. Three is the smallest number that tolerates losing a server, because
    /// quorum needs a majority.
    replicas: usize,
    /// Keyed by the bucket id the caller passes, NOT by the derived JetStream
    /// bucket name — so the hot path is a borrowed lookup with no allocation.
    /// `RwLock` because after the first request per bucket this is read-only, and
    /// a `Mutex` here serialised every keyvalue operation on the node: it was a
    /// third of guest-call time under load (ADR-0057).
    stores: std::sync::RwLock<HashMap<String, JsStore>>,
}

impl NatsKv {
    /// `url` may be a comma-separated LIST, and in a replicated deployment it
    /// should be.
    ///
    /// A client given one address does discover the rest of the cluster — NATS
    /// servers advertise their peers in the INFO they send — and it will fail over
    /// to them. But that only works once it has connected to something. A host
    /// starting while its single listed server is the one that is down cannot
    /// bootstrap at all, and that is precisely the moment it matters.
    pub async fn connect(url: &str, replicas: usize) -> Result<Self> {
        let urls = servers(url);
        let client = async_nats::connect(urls.clone())
            .await
            .with_context(|| format!("connecting to NATS at {}", urls.join(", ")))?;
        Ok(Self {
            handle: tokio::runtime::Handle::current(),
            js: async_nats::jetstream::new(client),
            stores: std::sync::RwLock::new(HashMap::new()),
            replicas,
        })
    }

    /// NATS KV bucket names have a restricted charset.
    fn bucket_name(bucket: &str) -> String {
        let mut s: String = bucket
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .collect();
        if s.is_empty() {
            s.push('x');
        }
        s
    }

    /// Hex-escape a guest key into a NATS-KV-legal token. Guest keys are arbitrary
    /// bytes; KV keys are not.
    fn safe_key(key: &str) -> String {
        let mut out = String::with_capacity(key.len());
        for b in key.bytes() {
            match b {
                b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'=' | b'.' => {
                    out.push(b as char)
                }
                _ => out.push_str(&format!("_{b:02X}")),
            }
        }
        out
    }

    fn store_for(&self, bucket: &str) -> Result<JsStore> {
        if let Some(s) = crate::sync::reading(&self.stores).get(bucket) {
            return Ok(s.clone());
        }
        let name = Self::bucket_name(bucket);
        let store = self.block(async {
            match self.js.get_key_value(&name).await {
                Ok(s) => Ok(s),
                Err(_) => {
                    // Applies to buckets created FROM NOW ON. An existing bucket
                    // keeps whatever it was made with — `nats stream update` or a
                    // restore with `REPLICAS=` changes those.
                    let want = if self.replicas == 0 { DEFAULT_REPLICAS } else { self.replicas };
                    let cfg = |n| async_nats::jetstream::kv::Config {
                        bucket: name.clone(),
                        history: 1,
                        num_replicas: n,
                        ..Default::default()
                    };
                    match self.js.create_key_value(cfg(want)).await {
                        Ok(s) => Ok(s),
                        // Asking for more copies than the cluster can hold is the
                        // one failure worth surviving: it is what a single-server
                        // NATS says, and refusing to start there would break every
                        // single-node deployment. Only the AUTOMATIC choice falls
                        // back — an operator who typed a number gets the error,
                        // because they asked for something specific and did not
                        // get it.
                        Err(e) if self.replicas == 0 && want > 1 => {
                            eprintln!(
                                "comp-host: WARNING this NATS cannot hold {want} copies of \
                                 {name} ({e}), falling back to ONE. That is a single disk \
                                 holding this data: `just backup` is the floor, and a NATS \
                                 cluster of 3 with --kv-replicas 3 is the fix."
                            );
                            self.js.create_key_value(cfg(1)).await.map_err(anyhow::Error::from)
                        }
                        Err(e) => Err(anyhow::Error::from(e)),
                    }
                }
            }
        })?;
        crate::sync::writing(&self.stores).insert(bucket.to_string(), store.clone());
        Ok(store)
    }

    fn block<F: std::future::Future>(&self, fut: F) -> F::Output {
        tokio::task::block_in_place(|| self.handle.block_on(fut))
    }
}

impl KvBackend for NatsKv {
    /// A JetStream bucket, reachable from anywhere on the cluster.
    fn shared(&self) -> bool {
        true
    }

    fn get(&self, bucket: &BucketId, key: &str) -> Result<Option<Vec<u8>>> {
        let s = self.store_for(bucket.as_str())?;
        let k = Self::safe_key(key);
        Ok(self.block(async move { s.get(&k).await })?.map(|b| b.to_vec()))
    }

    fn set(&self, bucket: &BucketId, key: &str, value: &[u8]) -> Result<()> {
        let s = self.store_for(bucket.as_str())?;
        let (k, v) = (Self::safe_key(key), value.to_vec());
        self.block(async move { s.put(&k, v.into()).await }).context("nats kv put")?;
        Ok(())
    }

    fn delete(&self, bucket: &BucketId, key: &str) -> Result<()> {
        let s = self.store_for(bucket.as_str())?;
        let k = Self::safe_key(key);
        self.block(async move { s.delete(&k).await }).context("nats kv delete")?;
        Ok(())
    }

    fn exists(&self, bucket: &BucketId, key: &str) -> Result<bool> {
        Ok(self.get(bucket, key)?.is_some())
    }

    fn list_keys(&self, bucket: &BucketId) -> Result<Vec<String>> {
        use futures::StreamExt;
        let s = self.store_for(bucket.as_str())?;
        // The listing's failure is REPORTED, not turned into an empty list.
        //
        // This was `Err(_) => {}` returning `Ok(vec![])`, so a bucket that could not
        // be listed and a bucket with nothing in it were the same answer. Nine
        // components list keys, and every one of them would have read "empty" and
        // acted on it — the same silent-truncation shape `guestio.rs` lints for on
        // the guest side of the wire, on the host side of it.
        let keys = self.block(async move {
            let mut out = Vec::new();
            match s.keys().await {
                Ok(mut stream) => {
                    while let Some(k) = stream.next().await {
                        if let Ok(k) = k {
                            // Returned exactly as stored, NOT decoded. `safe_key`
                            // leaves `_` literal and also uses it to introduce an
                            // escape, so the encoding is not reversible: decoding
                            // turned `rec_orgs_01KZS…` into `rec_orgs\x01KZS…`,
                            // and every record key is that shape because a ULID
                            // starts with digits. Nine components list keys, and
                            // all of them were being handed corrupted names on this
                            // backend (ADR-0068).
                            //
                            // A key made only of the characters `safe_key` passes
                            // through — which is every key the components in this
                            // repo build, since they sanitize their own segments —
                            // is byte-identical here. One containing bytes that had
                            // to be escaped comes back in escaped form, which is a
                            // wart and is not corruption. Making it truly reversible
                            // means changing `safe_key`, and that renames every key
                            // already written.
                            out.push(k);
                        }
                    }
                }
                Err(e) => return Err(e),
            }
            Ok(out)
        });
        keys.context("nats kv list-keys")
    }

    /// Genuinely atomic ACROSS NODES, which no other backend here manages.
    ///
    /// JetStream KV gives every entry a revision and `update` fails if the revision
    /// moved, so this is a compare-and-swap retry loop rather than the
    /// read-modify-write the old synchronous client did. Two replicas of one app
    /// incrementing the same counter cannot lose an update — which is the entire
    /// reason a spread deployment can hold state at all.
    ///
    /// `wasi:keyvalue` still exposes no CAS to the GUEST, so a guest doing
    /// read-then-write across two calls is racy whatever this does (ADR-0008).
    fn increment(&self, bucket: &BucketId, key: &str, delta: u64) -> Result<u64> {
        let s = self.store_for(bucket.as_str())?;
        let k = Self::safe_key(key);
        self.block(async move {
            for attempt in 0..32u32 {
                // Exponential backoff between attempts, which wasmCloud's own NATS
                // KV provider does and this did not: an immediate retry loop turns
                // contention on one key into a thundering herd, where every loser
                // re-reads and re-collides at full speed. First attempt is
                // immediate; the rest wait 5ms, 10ms, 20ms… (ADR-0069).
                if attempt > 0 {
                    let backoff = CAS_BACKOFF_MS.saturating_mul(1 << attempt.min(6));
                    tokio::time::sleep(std::time::Duration::from_millis(backoff)).await;
                }
                let current = s.entry(&k).await.ok().flatten();
                let (n, rev) = match &current {
                    Some(e) if !e.value.is_empty() => (
                        String::from_utf8_lossy(&e.value).trim().parse::<u64>().unwrap_or(0),
                        e.revision,
                    ),
                    _ => (0, 0),
                };
                let next = n.saturating_add(delta);
                let bytes: bytes::Bytes = next.to_string().into_bytes().into();
                let ok = if rev == 0 {
                    // `create` fails if someone else got there first, which is the
                    // CAS for the absent case.
                    s.create(&k, bytes).await.is_ok()
                } else {
                    s.update(&k, bytes, rev).await.is_ok()
                };
                if ok {
                    return Ok(next);
                }
            }
            anyhow::bail!("nats kv increment: 32 CAS attempts all lost the race")
        })
    }

    fn get_revision(&self, bucket: &BucketId, key: &str) -> Result<Option<Versioned>> {
        let s = self.store_for(bucket.as_str())?;
        let k = Self::safe_key(key);
        let entry = self.block(async move { s.entry(&k).await }).context("nats kv entry")?;
        // An empty value is a tombstone, matching what `increment` above has always
        // assumed. Nothing here stores an empty value on purpose — record-store
        // writes JSON — and treating one as present would hand a caller a revision
        // for a key that `get` reports as absent.
        Ok(entry.filter(|e| !e.value.is_empty()).map(|e| (e.revision, e.value.to_vec())))
    }

    /// The only implementation here that is atomic ACROSS MACHINES, which is the
    /// reason this whole primitive exists (ADR-0065).
    ///
    /// The comparison happens inside JetStream, against the sequence it assigned —
    /// so it holds no matter how many nodes are writing, what any of them cached, or
    /// how stale the caller's copy is. A caller with an old revision gets a
    /// `Conflict` and re-reads; it can never overwrite what it did not see.
    fn set_if_revision(
        &self,
        bucket: &BucketId,
        key: &str,
        value: &[u8],
        expected: u64,
    ) -> Result<Cas> {
        let s = self.store_for(bucket.as_str())?;
        let k = Self::safe_key(key);
        let v: bytes::Bytes = value.to_vec().into();
        self.block(async move {
            let entry = s.entry(&k).await.context("nats kv entry")?;
            // What the CALLER should have seen: 0 for absent or tombstoned, the
            // sequence otherwise. The tombstone's own revision is kept separately
            // because JetStream still needs it to guard the write.
            let (visible, guard) = match &entry {
                Some(e) if !e.value.is_empty() => (e.revision, Some(e.revision)),
                Some(e) => (0, Some(e.revision)),
                None => (0, None),
            };
            if visible != expected {
                return Ok(Cas::Conflict(visible));
            }
            // The two calls have different error types and this only cares whether
            // the write landed, so both collapse to the revision or nothing.
            let landed: Option<u64> = match guard {
                Some(rev) => s.update(&k, v, rev).await.ok(),
                // No entry at all: `create` is the CAS for the absent case — it
                // fails if another writer created it in the meantime.
                None => s.create(&k, v).await.ok(),
            };
            match landed {
                Some(rev) => Ok(Cas::Committed(rev)),
                // JetStream refused, which means the revision moved between the
                // read above and the write. Report what it is now rather than
                // guessing: the caller re-reads and retries.
                None => {
                    let now = s.entry(&k).await.ok().flatten();
                    Ok(Cas::Conflict(
                        now.filter(|e| !e.value.is_empty()).map(|e| e.revision).unwrap_or(0),
                    ))
                }
            }
        })
    }
}

// There is deliberately no `unescape` here any more. `safe_key` is not injective
// — `_` is both a literal and the escape introducer — so no decoder can be right,
// and the one that used to live here silently corrupted every key it was given
// (ADR-0068). The tests below pin the ambiguity so nobody adds it back.

// ---- sqlite ---------------------------------------------------------------
// One file, one table, `(bucket, key)` as the primary key. The point of it is the
// thing `memory` can never do: survive a restart — and `Restart=always` in a
// systemd unit means restarts are routine, not exceptional (docs/SELFHOST.md).
//
// Chosen over an embedded pure-Rust store for one property that matters at 3am:
// you can open the file with the `sqlite3` CLI and look. That is worth more in a
// fleet you maintain alone than a cleaner dependency tree.

pub struct SqliteKv {
    // rusqlite's Connection is Send but not Sync, and the trait needs both, so one
    // connection behind a mutex.
    //
    // ponytail: serializes every kv call. SQLite serializes writes anyway, and the
    // per-request handler is brief; if reads ever dominate, the upgrade is a small
    // read pool (WAL already allows concurrent readers).
    conn: Mutex<rusqlite::Connection>,
}

impl SqliteKv {
    pub fn open(path: &str) -> Result<Self> {
        if let Some(dir) = std::path::Path::new(path).parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)
                    .with_context(|| format!("creating {}", dir.display()))?;
            }
        }
        let conn = rusqlite::Connection::open(path)
            .with_context(|| format!("opening sqlite at {path}"))?;
        // WAL survives an unclean shutdown and lets readers run during a write —
        // which is what a `Restart=always` service wants. `synchronous=NORMAL` is
        // the usual WAL pairing: durable across process death, and it does not
        // fsync on every commit.
        conn.pragma_update(None, "journal_mode", "WAL").context("journal_mode=WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL").context("synchronous")?;
        conn.busy_timeout(std::time::Duration::from_secs(5)).context("busy_timeout")?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS kv (
                 bucket TEXT NOT NULL,
                 key    TEXT NOT NULL,
                 value  BLOB NOT NULL,
                 PRIMARY KEY (bucket, key)
             ) WITHOUT ROWID",
            [],
        )
        .context("creating the kv table")?;
        // Added to databases that predate revisions. `ALTER TABLE` has no
        // `IF NOT EXISTS` in SQLite, so the error on a second run is the check —
        // and it is the only thing that can fail here for a benign reason.
        let _ = conn.execute("ALTER TABLE kv ADD COLUMN rev INTEGER NOT NULL DEFAULT 0", []);
        Ok(Self { conn: Mutex::new(conn) })
    }

    /// Where a systemd unit's data belongs, with no configuration.
    ///
    /// `StateDirectory=` makes systemd export `STATE_DIRECTORY`, and under
    /// `DynamicUser=yes` that path is private to the app's transient uid. So the
    /// default needs no flag on a box, and falls back to the working directory for
    /// a local run.
    pub fn default_path() -> String {
        match std::env::var("STATE_DIRECTORY") {
            // systemd may hand over a colon-separated list; the first is ours.
            Ok(dirs) if !dirs.is_empty() => {
                let first = dirs.split(':').next().unwrap_or(&dirs);
                format!("{first}/kv.db")
            }
            _ => "comp-kv.db".to_string(),
        }
    }
}

impl KvBackend for SqliteKv {
    /// One file on one node's disk. Two replicas would be two files with one name,
    /// which is the whole of ADR-0027.
    fn shared(&self) -> bool {
        false
    }

    fn get(&self, bucket: &BucketId, key: &str) -> Result<Option<Vec<u8>>> {
        let bucket = bucket.as_str();
        let conn = crate::sync::held(&self.conn);
        let mut q = conn.prepare_cached("SELECT value FROM kv WHERE bucket = ?1 AND key = ?2")?;
        let mut rows = q.query(rusqlite::params![bucket, key])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    fn set(&self, bucket: &BucketId, key: &str, value: &[u8]) -> Result<()> {
        let bucket = bucket.as_str();
        let conn = crate::sync::held(&self.conn);
        // The revision moves for a plain `set` too, or a guarded write could land
        // on top of one and never know.
        conn.prepare_cached(
            "INSERT INTO kv (bucket, key, value, rev) VALUES (?1, ?2, ?3, 1)
             ON CONFLICT(bucket, key) DO UPDATE SET value = excluded.value, rev = kv.rev + 1",
        )?
        .execute(rusqlite::params![bucket, key, value])?;
        Ok(())
    }

    fn delete(&self, bucket: &BucketId, key: &str) -> Result<()> {
        let bucket = bucket.as_str();
        let conn = crate::sync::held(&self.conn);
        conn.prepare_cached("DELETE FROM kv WHERE bucket = ?1 AND key = ?2")?
            .execute(rusqlite::params![bucket, key])?;
        Ok(())
    }

    fn exists(&self, bucket: &BucketId, key: &str) -> Result<bool> {
        let bucket = bucket.as_str();
        let conn = crate::sync::held(&self.conn);
        let mut q = conn.prepare_cached("SELECT 1 FROM kv WHERE bucket = ?1 AND key = ?2")?;
        Ok(q.exists(rusqlite::params![bucket, key])?)
    }

    fn list_keys(&self, bucket: &BucketId) -> Result<Vec<String>> {
        let bucket = bucket.as_str();
        let conn = crate::sync::held(&self.conn);
        let mut q = conn.prepare_cached("SELECT key FROM kv WHERE bucket = ?1 ORDER BY key")?;
        let rows = q.query_map(rusqlite::params![bucket], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Genuinely atomic, unlike the memory and NATS backends' read-modify-write:
    /// the read and the write happen in one IMMEDIATE transaction, so two
    /// concurrent increments cannot both read the same starting value.
    ///
    /// This is as far as atomicity can go here — `wasi:keyvalue` exposes no CAS, so
    /// a GUEST doing read-then-write across two calls is still racy whatever the
    /// backend does (ADR-0008).
    fn increment(&self, bucket: &BucketId, key: &str, delta: u64) -> Result<u64> {
        let bucket = bucket.as_str();
        let mut conn = crate::sync::held(&self.conn);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current: Option<Vec<u8>> = tx
            .prepare_cached("SELECT value FROM kv WHERE bucket = ?1 AND key = ?2")?
            .query_row(rusqlite::params![bucket, key], |r| r.get(0))
            .ok();
        // Same representation the other backends use: a decimal string, so a counter
        // written by one backend reads back under another.
        let n: u64 = match &current {
            Some(v) => String::from_utf8_lossy(v).trim().parse().unwrap_or(0),
            None => 0,
        };
        let next = n.saturating_add(delta);
        tx.prepare_cached(
            "INSERT INTO kv (bucket, key, value, rev) VALUES (?1, ?2, ?3, 1)
             ON CONFLICT(bucket, key) DO UPDATE SET value = excluded.value, rev = kv.rev + 1",
        )?
        .execute(rusqlite::params![bucket, key, next.to_string().as_bytes()])?;
        tx.commit()?;
        Ok(next)
    }

    fn get_revision(&self, bucket: &BucketId, key: &str) -> Result<Option<Versioned>> {
        let bucket = bucket.as_str();
        let conn = crate::sync::held(&self.conn);
        let mut q =
            conn.prepare_cached("SELECT rev, value FROM kv WHERE bucket = ?1 AND key = ?2")?;
        let mut rows = q.query(rusqlite::params![bucket, key])?;
        match rows.next()? {
            Some(row) => Ok(Some((row.get::<_, i64>(0)?.max(0) as u64, row.get(1)?))),
            None => Ok(None),
        }
    }

    /// Atomic: the compare and the write are one IMMEDIATE transaction, so nothing
    /// can land between them. Node-local like the rest of this backend — which is
    /// enough, because a store only one process can reach has only that process
    /// racing against itself.
    fn set_if_revision(
        &self,
        bucket: &BucketId,
        key: &str,
        value: &[u8],
        expected: u64,
    ) -> Result<Cas> {
        let bucket = bucket.as_str();
        let mut conn = crate::sync::held(&self.conn);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current: u64 = tx
            .prepare_cached("SELECT rev FROM kv WHERE bucket = ?1 AND key = ?2")?
            .query_row(rusqlite::params![bucket, key], |r| r.get::<_, i64>(0))
            .map(|r| r.max(0) as u64)
            .unwrap_or(0);
        if current != expected {
            return Ok(Cas::Conflict(current));
        }
        let next = current + 1;
        tx.prepare_cached(
            "INSERT INTO kv (bucket, key, value, rev) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(bucket, key) DO UPDATE SET value = excluded.value, rev = excluded.rev",
        )?
        .execute(rusqlite::params![bucket, key, value, next as i64])?;
        tx.commit()?;
        Ok(Cas::Committed(next))
    }
}

/// Build the backend named by `--kv`.
/// The backend a lattice node picks when nobody says otherwise.
///
/// A lattice node needs a store every replica can see (ADR-0027), and this is the
/// only implementation of one that ships today. It is a DEFAULT, not a
/// requirement: `KvBackend::shared` is the interface, `--kv` selects an
/// implementation, and nothing above this line knows the word "nats". A second
/// shared backend needs a `shared() -> true` and a line in `build`.
pub const DEFAULT_SHARED: &str = "nats";

pub async fn build(
    kind: &str,
    redis_url: &str,
    nats_url: &str,
    sqlite_path: &str,
    replicas: usize,
) -> Result<std::sync::Arc<dyn KvBackend>> {
    use std::sync::Arc;
    match kind {
        "memory" => Ok(Arc::new(MemoryKv::default())),
        "redis" => Ok(Arc::new(RedisKv::connect(redis_url)?)),
        "sqlite" => Ok(Arc::new(SqliteKv::open(sqlite_path)?)),
        "nats" => Ok(Arc::new(NatsKv::connect(nats_url, replicas).await?)),
        other => anyhow::bail!("unknown --kv backend: {other} (use memory|redis|nats|sqlite)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reason `list_keys` hands back stored names rather than decoded ones.
    ///
    /// Two DIFFERENT keys encode to the same NATS key, so a decoder cannot know
    /// which one it is looking at. It used to guess, and it guessed wrong on the
    /// shape every record key has.
    #[test]
    fn the_nats_key_encoding_cannot_be_reversed() {
        let literal = "rec_orgs_01KZS84N77";
        // A key that genuinely contains the byte 0x01 escapes to the same thing.
        let escaped = "rec_orgs\u{1}KZS84N77";
        assert_eq!(
            NatsKv::safe_key(literal),
            NatsKv::safe_key(escaped),
            "if these ever differ the encoding became reversible and list_keys can decode again"
        );
    }

    /// Backends are handed a host-built id, never a guest string — that is the
    /// whole point of the type. Tests reach for the same constructor.
    fn bkt(name: &str) -> BucketId {
        BucketId::for_test(name)
    }

    /// A unique scratch path per test, cleaned up on drop — including the `-wal` and
    /// `-shm` files WAL mode creates, which a naive cleanup leaves behind.
    struct Scratch(String);
    impl Scratch {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "comp-kv-test-{}-{}-{:?}.db",
                tag,
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            Scratch(p.to_string_lossy().to_string())
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{}", self.0, suffix));
            }
        }
    }

    #[test]
    fn sqlite_roundtrips_every_operation() {
        let s = Scratch::new("ops");
        let kv = SqliteKv::open(&s.0).expect("open");

        assert_eq!(kv.get(&bkt("b"), "missing").unwrap(), None);
        assert!(!kv.exists(&bkt("b"), "missing").unwrap());

        kv.set(&bkt("b"), "k", b"v1").unwrap();
        assert_eq!(kv.get(&bkt("b"), "k").unwrap().as_deref(), Some(&b"v1"[..]));
        assert!(kv.exists(&bkt("b"), "k").unwrap());

        // set is an upsert, not an insert — a second write must replace, not fail.
        kv.set(&bkt("b"), "k", b"v2").unwrap();
        assert_eq!(kv.get(&bkt("b"), "k").unwrap().as_deref(), Some(&b"v2"[..]));

        kv.set(&bkt("b"), "a", b"x").unwrap();
        assert_eq!(kv.list_keys(&bkt("b")).unwrap(), vec!["a".to_string(), "k".to_string()]);

        kv.delete(&bkt("b"), "k").unwrap();
        assert_eq!(kv.get(&bkt("b"), "k").unwrap(), None);
        // Deleting something absent is not an error — the guest may retry.
        kv.delete(&bkt("b"), "k").unwrap();
    }

    /// THE test. It is the entire reason this backend exists: `Restart=always` in a
    /// systemd unit means restarts are routine, and `--kv memory` cannot survive one.
    #[test]
    fn data_survives_a_restart() {
        let s = Scratch::new("restart");
        {
            let kv = SqliteKv::open(&s.0).expect("first start");
            kv.set(&bkt("orders"), "42", b"paid").unwrap();
            kv.increment(&bkt("stats"), "count", 7).unwrap();
        } // the process "dies" here — connection dropped, nothing flushed by hand

        let kv = SqliteKv::open(&s.0).expect("second start");
        assert_eq!(kv.get(&bkt("orders"), "42").unwrap().as_deref(), Some(&b"paid"[..]));
        assert_eq!(kv.get(&bkt("stats"), "count").unwrap().as_deref(), Some(&b"7"[..]));
        assert_eq!(kv.list_keys(&bkt("orders")).unwrap(), vec!["42".to_string()]);
    }

    #[test]
    fn buckets_do_not_leak_into_each_other() {
        // The same key in two buckets is two values — the property the platform's
        // whole isolation story rests on, here enforced by a composite primary key.
        let s = Scratch::new("buckets");
        let kv = SqliteKv::open(&s.0).unwrap();
        kv.set(&bkt("alice"), "secret", b"hers").unwrap();
        kv.set(&bkt("bob"), "secret", b"his").unwrap();
        assert_eq!(kv.get(&bkt("alice"), "secret").unwrap().as_deref(), Some(&b"hers"[..]));
        assert_eq!(kv.get(&bkt("bob"), "secret").unwrap().as_deref(), Some(&b"his"[..]));
        assert_eq!(kv.list_keys(&bkt("alice")).unwrap(), vec!["secret".to_string()]);
        kv.delete(&bkt("alice"), "secret").unwrap();
        assert_eq!(kv.get(&bkt("bob"), "secret").unwrap().as_deref(), Some(&b"his"[..]));
    }

    #[test]
    fn increment_counts_from_nothing_and_is_atomic_under_threads() {
        let s = Scratch::new("incr");
        let kv = std::sync::Arc::new(SqliteKv::open(&s.0).unwrap());
        assert_eq!(kv.increment(&bkt("c"), "n", 1).unwrap(), 1, "absent key starts at zero");
        assert_eq!(kv.increment(&bkt("c"), "n", 4).unwrap(), 5);

        // The claim this backend makes over memory/nats, which do read-modify-write:
        // concurrent increments cannot lose an update. 8 threads x 50 = 400.
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let kv = kv.clone();
                std::thread::spawn(move || {
                    for _ in 0..50 {
                        kv.increment(&bkt("c"), "concurrent", 1).unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(
            kv.get(&bkt("c"), "concurrent").unwrap().as_deref(),
            Some(&b"400"[..]),
            "an increment was lost — the transaction is not doing its job"
        );
    }

    #[test]
    fn a_counter_is_a_decimal_string_so_backends_agree() {
        // Same representation as redis INCRBY and the in-memory store, so a value
        // written under one backend reads back under another.
        let s = Scratch::new("repr");
        let kv = SqliteKv::open(&s.0).unwrap();
        kv.set(&bkt("c"), "n", b"41").unwrap();
        assert_eq!(kv.increment(&bkt("c"), "n", 1).unwrap(), 42);
        assert_eq!(kv.get(&bkt("c"), "n").unwrap().as_deref(), Some(&b"42"[..]));
    }

    #[test]
    fn the_default_path_follows_systemd() {
        // `StateDirectory=` makes systemd export this, and under DynamicUser it is a
        // private directory — so a unit needs no --sqlite-path at all.
        std::env::set_var("STATE_DIRECTORY", "/var/lib/private/comp/gate");
        assert_eq!(SqliteKv::default_path(), "/var/lib/private/comp/gate/kv.db");
        // systemd may pass a colon-separated list; the first entry is ours.
        std::env::set_var("STATE_DIRECTORY", "/var/lib/one:/var/lib/two");
        assert_eq!(SqliteKv::default_path(), "/var/lib/one/kv.db");
        std::env::remove_var("STATE_DIRECTORY");
        assert_eq!(SqliteKv::default_path(), "comp-kv.db");
    }

    #[tokio::test]
    async fn build_names_sqlite_and_rejects_nonsense() {
        let s = Scratch::new("build");
        assert!(build("sqlite", "", "", &s.0, 1).await.is_ok());
        // `dyn KvBackend` is not Debug, so match rather than unwrap_err.
        let err = match build("postgres", "", "", &s.0, 1).await {
            Err(e) => e.to_string(),
            Ok(_) => panic!("postgres is not a backend"),
        };
        assert!(err.contains("memory|redis|nats|sqlite"), "{err}");
    }

    /// Which backends every replica of an app can see.
    ///
    /// Getting this wrong in either direction is expensive: `true` for a
    /// node-local store places an app somewhere it silently diverges (the bug
    /// ADR-0027 is about), and `false` for a shared one refuses a deployment that
    /// would have been fine.
    #[test]
    fn a_backend_declares_whether_it_is_shared() {
        // Asked of the implementation, not of a name. A new backend answers here
        // and the reconciler learns it without anything central being edited.
        let s = Scratch::new("shared");
        assert!(!MemoryKv::default().shared(), "one process's heap is not shared");
        assert!(!SqliteKv::open(&s.0).unwrap().shared(), "one file per node is not one store");
    }

    /// This test used to assert that escaping round-trips, and it passed — because
    /// every example it chose dodged the one shape that breaks. `a_b` has a single
    /// character after the underscore, so the decoder left it alone; `rec_orgs_01K`
    /// has two hex digits, and the decoder ate them. That is the shape of every
    /// record key in the platform (ADR-0068).
    ///
    /// So the property is narrower than it looked: a key built from characters NATS
    /// already accepts is stored verbatim, which is what makes `list_keys` handing
    /// back stored names correct for every component in this repo.
    #[test]
    fn a_key_of_legal_characters_is_stored_verbatim() {
        for key in ["plain", "a_b", "rec_orgs_01KZS84N77", "idx_orgs", "sess-abc.123", "="] {
            assert_eq!(NatsKv::safe_key(key), key, "{key:?} was altered on the way in");
        }
    }

    /// And a key that needs escaping comes back in escaped form rather than wrong.
    #[test]
    fn a_key_needing_escapes_is_escaped_not_corrupted() {
        assert_eq!(NatsKv::safe_key("with space"), "with_20space");
        assert_eq!(NatsKv::safe_key("a/b"), "a_2Fb");
        // The wart, said out loud: `list_keys` hands this back as `with_20space`.
        // Not the original, and not something else's name either.
    }
}
