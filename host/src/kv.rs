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
}

// ---- in-memory (default) -------------------------------------------------

#[derive(Default)]
pub struct MemoryKv {
    buckets: Mutex<HashMap<String, HashMap<String, Vec<u8>>>>,
}

impl KvBackend for MemoryKv {
    /// One process's heap. Nothing outside it can see this.
    fn shared(&self) -> bool {
        false
    }

    fn get(&self, bucket: &BucketId, key: &str) -> Result<Option<Vec<u8>>> {
        let bucket = bucket.as_str();
        Ok(self.buckets.lock().unwrap().get(bucket).and_then(|b| b.get(key)).cloned())
    }
    fn set(&self, bucket: &BucketId, key: &str, value: &[u8]) -> Result<()> {
        let bucket = bucket.as_str();
        self.buckets.lock().unwrap().entry(bucket.into()).or_default().insert(key.into(), value.to_vec());
        Ok(())
    }
    fn delete(&self, bucket: &BucketId, key: &str) -> Result<()> {
        let bucket = bucket.as_str();
        if let Some(b) = self.buckets.lock().unwrap().get_mut(bucket) {
            b.remove(key);
        }
        Ok(())
    }
    fn exists(&self, bucket: &BucketId, key: &str) -> Result<bool> {
        let bucket = bucket.as_str();
        Ok(self.buckets.lock().unwrap().get(bucket).map(|b| b.contains_key(key)).unwrap_or(false))
    }
    fn list_keys(&self, bucket: &BucketId) -> Result<Vec<String>> {
        let bucket = bucket.as_str();
        Ok(self.buckets.lock().unwrap().get(bucket).map(|b| b.keys().cloned().collect()).unwrap_or_default())
    }
    fn increment(&self, bucket: &BucketId, key: &str, delta: u64) -> Result<u64> {
        let bucket = bucket.as_str();
        let mut g = self.buckets.lock().unwrap();
        let b = g.entry(bucket.into()).or_default();
        let cur: u64 = b.get(key).and_then(|v| std::str::from_utf8(v).ok()).and_then(|s| s.parse().ok()).unwrap_or(0);
        let next = cur.saturating_add(delta);
        b.insert(key.into(), next.to_string().into_bytes());
        Ok(next)
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
        let mut c = self.conn.lock().unwrap();
        let v: Option<Vec<u8>> = c.get(Self::k(bucket, key)).context("redis get")?;
        Ok(v)
    }
    fn set(&self, bucket: &BucketId, key: &str, value: &[u8]) -> Result<()> {
        let bucket = bucket.as_str();
        use redis::Commands;
        let mut c = self.conn.lock().unwrap();
        c.set::<_, _, ()>(Self::k(bucket, key), value).context("redis set")?;
        Ok(())
    }
    fn delete(&self, bucket: &BucketId, key: &str) -> Result<()> {
        let bucket = bucket.as_str();
        use redis::Commands;
        let mut c = self.conn.lock().unwrap();
        c.del::<_, ()>(Self::k(bucket, key)).context("redis del")?;
        Ok(())
    }
    fn exists(&self, bucket: &BucketId, key: &str) -> Result<bool> {
        let bucket = bucket.as_str();
        use redis::Commands;
        let mut c = self.conn.lock().unwrap();
        Ok(c.exists(Self::k(bucket, key)).context("redis exists")?)
    }
    fn list_keys(&self, bucket: &BucketId) -> Result<Vec<String>> {
        let bucket = bucket.as_str();
        use redis::Commands;
        let mut c = self.conn.lock().unwrap();
        let prefix = format!("{bucket}{SEP}");
        let pattern = format!("{prefix}*");
        let keys: Vec<String> = c.scan_match(pattern).context("redis scan")?.collect();
        Ok(keys.into_iter().map(|k| k.trim_start_matches(&prefix).to_string()).collect())
    }
    fn increment(&self, bucket: &BucketId, key: &str, delta: u64) -> Result<u64> {
        let bucket = bucket.as_str();
        use redis::Commands;
        let mut c = self.conn.lock().unwrap();
        let next: i64 = c.incr(Self::k(bucket, key), delta as i64).context("redis incrby")?;
        Ok(next as u64)
    }
}

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
    /// Keyed by the bucket id the caller passes, NOT by the derived JetStream
    /// bucket name — so the hot path is a borrowed lookup with no allocation.
    /// `RwLock` because after the first request per bucket this is read-only, and
    /// a `Mutex` here serialised every keyvalue operation on the node: it was a
    /// third of guest-call time under load (ADR-0057).
    stores: std::sync::RwLock<HashMap<String, JsStore>>,
}

impl NatsKv {
    pub async fn connect(url: &str) -> Result<Self> {
        let client = async_nats::connect(url)
            .await
            .with_context(|| format!("connecting to NATS at {url}"))?;
        Ok(Self {
            handle: tokio::runtime::Handle::current(),
            js: async_nats::jetstream::new(client),
            stores: std::sync::RwLock::new(HashMap::new()),
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
        if let Some(s) = self.stores.read().unwrap().get(bucket) {
            return Ok(s.clone());
        }
        let name = Self::bucket_name(bucket);
        let store = self.block(async {
            match self.js.get_key_value(&name).await {
                Ok(s) => Ok(s),
                Err(_) => self
                    .js
                    .create_key_value(async_nats::jetstream::kv::Config {
                        bucket: name.clone(),
                        history: 1,
                        ..Default::default()
                    })
                    .await
                    .map_err(anyhow::Error::from),
            }
        })?;
        self.stores.write().unwrap().insert(bucket.to_string(), store.clone());
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
        let keys = self.block(async move {
            let mut out = Vec::new();
            match s.keys().await {
                Ok(mut stream) => {
                    while let Some(k) = stream.next().await {
                        if let Ok(k) = k {
                            out.push(unescape(&k));
                        }
                    }
                }
                Err(_) => {}
            }
            out
        });
        Ok(keys)
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
            for _ in 0..32 {
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
}

/// Reverse of `NatsKv::safe_key`.
fn unescape(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'_' && i + 2 < bytes.len() {
            if let Ok(b) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
            {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

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
        let conn = self.conn.lock().unwrap();
        let mut q = conn.prepare_cached("SELECT value FROM kv WHERE bucket = ?1 AND key = ?2")?;
        let mut rows = q.query(rusqlite::params![bucket, key])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    fn set(&self, bucket: &BucketId, key: &str, value: &[u8]) -> Result<()> {
        let bucket = bucket.as_str();
        let conn = self.conn.lock().unwrap();
        conn.prepare_cached(
            "INSERT INTO kv (bucket, key, value) VALUES (?1, ?2, ?3)
             ON CONFLICT(bucket, key) DO UPDATE SET value = excluded.value",
        )?
        .execute(rusqlite::params![bucket, key, value])?;
        Ok(())
    }

    fn delete(&self, bucket: &BucketId, key: &str) -> Result<()> {
        let bucket = bucket.as_str();
        let conn = self.conn.lock().unwrap();
        conn.prepare_cached("DELETE FROM kv WHERE bucket = ?1 AND key = ?2")?
            .execute(rusqlite::params![bucket, key])?;
        Ok(())
    }

    fn exists(&self, bucket: &BucketId, key: &str) -> Result<bool> {
        let bucket = bucket.as_str();
        let conn = self.conn.lock().unwrap();
        let mut q = conn
            .prepare_cached("SELECT 1 FROM kv WHERE bucket = ?1 AND key = ?2")?;
        Ok(q.exists(rusqlite::params![bucket, key])?)
    }

    fn list_keys(&self, bucket: &BucketId) -> Result<Vec<String>> {
        let bucket = bucket.as_str();
        let conn = self.conn.lock().unwrap();
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
        let mut conn = self.conn.lock().unwrap();
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
            "INSERT INTO kv (bucket, key, value) VALUES (?1, ?2, ?3)
             ON CONFLICT(bucket, key) DO UPDATE SET value = excluded.value",
        )?
        .execute(rusqlite::params![bucket, key, next.to_string().as_bytes()])?;
        tx.commit()?;
        Ok(next)
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
) -> Result<std::sync::Arc<dyn KvBackend>> {
    use std::sync::Arc;
    match kind {
        "memory" => Ok(Arc::new(MemoryKv::default())),
        "redis" => Ok(Arc::new(RedisKv::connect(redis_url)?)),
        "sqlite" => Ok(Arc::new(SqliteKv::open(sqlite_path)?)),
        "nats" => Ok(Arc::new(NatsKv::connect(nats_url).await?)),
        other => anyhow::bail!("unknown --kv backend: {other} (use memory|redis|nats|sqlite)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(build("sqlite", "", "", &s.0).await.is_ok());
        // `dyn KvBackend` is not Debug, so match rather than unwrap_err.
        let err = match build("postgres", "", "", &s.0).await {
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

    #[test]
    fn nats_keys_survive_a_round_trip_through_escaping() {
        // NATS KV keys are restricted to [-/_=.a-zA-Z0-9]; guest keys are arbitrary.
        // A key that does not come back is a record that silently disappears.
        for key in ["plain", "with space", "a/b", "sess:abc-123", "emoji-\u{1f600}", "a_b", "="] {
            let round = unescape(&NatsKv::safe_key(key));
            assert_eq!(round, key, "{key:?} did not survive escaping");
        }
    }
}
