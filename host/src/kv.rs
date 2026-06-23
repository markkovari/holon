//! Swappable key-value backends for the host's `wasi:keyvalue` implementation.
//!
//! The guest (the composed vet-domain wasm) calls `wasi:keyvalue/store` +
//! `atomics`; the host satisfies them, and WHICH durable store backs them is a
//! deployment choice — `--kv memory|redis|nats` — not a component change. Same
//! wasm bytes, different `KvBackend`.
//!
//! All methods are SYNCHRONOUS (the bindgen store trait is sync). redis uses the
//! blocking `redis` client; nats uses the synchronous `nats` JetStream KV client
//! — both fine in the per-request blocking handler.
//!
//! Keys are namespaced `{bucket}\x1f{key}` for the flat stores (redis) so named
//! buckets don't collide; NATS uses one JetStream KV bucket per `bucket` name.

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::{Context, Result};

/// A named-bucket key-value store. Errors surface as anyhow; the caller maps
/// them to the wasi:keyvalue `error` variant.
pub trait KvBackend: Send + Sync {
    fn get(&self, bucket: &str, key: &str) -> Result<Option<Vec<u8>>>;
    fn set(&self, bucket: &str, key: &str, value: &[u8]) -> Result<()>;
    fn delete(&self, bucket: &str, key: &str) -> Result<()>;
    fn exists(&self, bucket: &str, key: &str) -> Result<bool>;
    fn list_keys(&self, bucket: &str) -> Result<Vec<String>>;
    /// Atomic increment of an integer stored as a decimal string. Returns the
    /// new value. (redis INCRBY; in-memory + nats read-modify-write.)
    fn increment(&self, bucket: &str, key: &str, delta: u64) -> Result<u64>;
}

// ---- in-memory (default) -------------------------------------------------

#[derive(Default)]
pub struct MemoryKv {
    buckets: Mutex<HashMap<String, HashMap<String, Vec<u8>>>>,
}

impl KvBackend for MemoryKv {
    fn get(&self, bucket: &str, key: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.buckets.lock().unwrap().get(bucket).and_then(|b| b.get(key)).cloned())
    }
    fn set(&self, bucket: &str, key: &str, value: &[u8]) -> Result<()> {
        self.buckets.lock().unwrap().entry(bucket.into()).or_default().insert(key.into(), value.to_vec());
        Ok(())
    }
    fn delete(&self, bucket: &str, key: &str) -> Result<()> {
        if let Some(b) = self.buckets.lock().unwrap().get_mut(bucket) {
            b.remove(key);
        }
        Ok(())
    }
    fn exists(&self, bucket: &str, key: &str) -> Result<bool> {
        Ok(self.buckets.lock().unwrap().get(bucket).map(|b| b.contains_key(key)).unwrap_or(false))
    }
    fn list_keys(&self, bucket: &str) -> Result<Vec<String>> {
        Ok(self.buckets.lock().unwrap().get(bucket).map(|b| b.keys().cloned().collect()).unwrap_or_default())
    }
    fn increment(&self, bucket: &str, key: &str, delta: u64) -> Result<u64> {
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
    fn get(&self, bucket: &str, key: &str) -> Result<Option<Vec<u8>>> {
        use redis::Commands;
        let mut c = self.conn.lock().unwrap();
        let v: Option<Vec<u8>> = c.get(Self::k(bucket, key)).context("redis get")?;
        Ok(v)
    }
    fn set(&self, bucket: &str, key: &str, value: &[u8]) -> Result<()> {
        use redis::Commands;
        let mut c = self.conn.lock().unwrap();
        c.set::<_, _, ()>(Self::k(bucket, key), value).context("redis set")?;
        Ok(())
    }
    fn delete(&self, bucket: &str, key: &str) -> Result<()> {
        use redis::Commands;
        let mut c = self.conn.lock().unwrap();
        c.del::<_, ()>(Self::k(bucket, key)).context("redis del")?;
        Ok(())
    }
    fn exists(&self, bucket: &str, key: &str) -> Result<bool> {
        use redis::Commands;
        let mut c = self.conn.lock().unwrap();
        Ok(c.exists(Self::k(bucket, key)).context("redis exists")?)
    }
    fn list_keys(&self, bucket: &str) -> Result<Vec<String>> {
        use redis::Commands;
        let mut c = self.conn.lock().unwrap();
        let prefix = format!("{bucket}{SEP}");
        let pattern = format!("{prefix}*");
        let keys: Vec<String> = c.scan_match(pattern).context("redis scan")?.collect();
        Ok(keys.into_iter().map(|k| k.trim_start_matches(&prefix).to_string()).collect())
    }
    fn increment(&self, bucket: &str, key: &str, delta: u64) -> Result<u64> {
        use redis::Commands;
        let mut c = self.conn.lock().unwrap();
        let next: i64 = c.incr(Self::k(bucket, key), delta as i64).context("redis incrby")?;
        Ok(next as u64)
    }
}

// ---- nats (JetStream KV) --------------------------------------------------
// One JetStream KV bucket per `bucket` name (created on first use). NATS KV keys
// must match [-/_=\.a-zA-Z0-9]+, so arbitrary guest keys are hex-escaped.

pub struct NatsKv {
    ctx: nats::jetstream::JetStream,
    stores: Mutex<HashMap<String, nats::kv::Store>>,
}

impl NatsKv {
    pub fn connect(url: &str) -> Result<Self> {
        let nc = nats::connect(url).context("nats connect")?;
        let ctx = nats::jetstream::new(nc);
        Ok(Self { ctx, stores: Mutex::new(HashMap::new()) })
    }
    /// NATS KV bucket names also have a restricted charset; sanitize.
    fn bucket_name(bucket: &str) -> String {
        let mut s: String = bucket.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
        if s.is_empty() {
            s.push('x');
        }
        s
    }
    /// Hex-escape a guest key into a NATS-KV-legal token.
    fn safe_key(key: &str) -> String {
        let mut out = String::with_capacity(key.len());
        for b in key.bytes() {
            match b {
                b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'=' | b'.' => out.push(b as char),
                _ => out.push_str(&format!("_{b:02X}")),
            }
        }
        out
    }
    fn store_for(&self, bucket: &str) -> Result<nats::kv::Store> {
        let name = Self::bucket_name(bucket);
        let mut g = self.stores.lock().unwrap();
        if let Some(s) = g.get(&name) {
            return Ok(s.clone());
        }
        // bind to an existing bucket, or create it.
        let store = self
            .ctx
            .key_value(&name)
            .or_else(|_| {
                self.ctx.create_key_value(&nats::kv::Config { bucket: name.clone(), ..Default::default() })
            })
            .context("nats kv bucket")?;
        g.insert(name, store.clone());
        Ok(store)
    }
}

impl KvBackend for NatsKv {
    fn get(&self, bucket: &str, key: &str) -> Result<Option<Vec<u8>>> {
        let s = self.store_for(bucket)?;
        Ok(s.get(&Self::safe_key(key)).context("nats kv get")?)
    }
    fn set(&self, bucket: &str, key: &str, value: &[u8]) -> Result<()> {
        let s = self.store_for(bucket)?;
        s.put(&Self::safe_key(key), value.to_vec()).context("nats kv put")?;
        Ok(())
    }
    fn delete(&self, bucket: &str, key: &str) -> Result<()> {
        let s = self.store_for(bucket)?;
        s.delete(&Self::safe_key(key)).context("nats kv delete")?;
        Ok(())
    }
    fn exists(&self, bucket: &str, key: &str) -> Result<bool> {
        Ok(self.get(bucket, key)?.is_some())
    }
    fn list_keys(&self, bucket: &str) -> Result<Vec<String>> {
        let s = self.store_for(bucket)?;
        // NATS KV `keys()` returns the (escaped) keys; unescape back.
        let keys = s.keys().context("nats kv keys")?;
        Ok(keys.map(|k| unescape(&k)).collect())
    }
    fn increment(&self, bucket: &str, key: &str, delta: u64) -> Result<u64> {
        // no native atomic incr in the sync client — read-modify-write (the
        // host is the single writer for these counters in this demo).
        let cur = self.get(bucket, key)?
            .and_then(|v| String::from_utf8(v).ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        let next = cur.saturating_add(delta);
        self.set(bucket, key, next.to_string().as_bytes())?;
        Ok(next)
    }
}

/// Reverse of `NatsKv::safe_key`.
fn unescape(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'_' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16) {
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

/// Build the backend named by `--kv`.
pub fn build(kind: &str, redis_url: &str, nats_url: &str) -> Result<std::sync::Arc<dyn KvBackend>> {
    use std::sync::Arc;
    match kind {
        "memory" => Ok(Arc::new(MemoryKv::default())),
        "redis" => Ok(Arc::new(RedisKv::connect(redis_url)?)),
        "nats" => Ok(Arc::new(NatsKv::connect(nats_url)?)),
        other => anyhow::bail!("unknown --kv backend: {other} (use memory|redis|nats)"),
    }
}
