//! `cache` — reference implementation of `cache:store`.
//!
//! TTL-aware byte cache over `wasi:keyvalue`. The store has no native expiry,
//! so each entry is stored as an 8-byte big-endian expiry epoch followed by the
//! raw value. `0` expiry means "never". Reads past expiry are misses and delete
//! the entry lazily.

#[allow(warnings)]
mod bindings;

use bindings::cache::store::source;
use bindings::cache::store::sink;
use bindings::exports::cache::store::cache::{CacheError, Guest};
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::keyvalue::store;

struct Component;

const BUCKET: &str = "default";

fn now() -> u64 {
    wall_clock::now().seconds
}

/// Namespace + sanitize a key to NATS-legal kv chars.
fn ckey(key: &str) -> String {
    let mut out = String::with_capacity(key.len() + 2);
    out.push_str("c_");
    for b in key.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'/' | b'=' => out.push(b as char),
            _ => out.push_str(&format!("_{b:02X}")),
        }
    }
    out
}

fn open() -> Result<store::Bucket, CacheError> {
    store::open(BUCKET).map_err(|e| CacheError::BackendUnavailable(format!("open: {e:?}")))
}

/// Read an entry, returning (expiry, value) if present. Does not check freshness.
fn raw_get(bucket: &store::Bucket, k: &str) -> Result<Option<(u64, Vec<u8>)>, CacheError> {
    match bucket.get(k) {
        Ok(Some(bytes)) if bytes.len() >= 8 => {
            let mut e = [0u8; 8];
            e.copy_from_slice(&bytes[..8]);
            Ok(Some((u64::from_be_bytes(e), bytes[8..].to_vec())))
        }
        Ok(Some(_)) => Ok(None), // corrupt/too-short -> miss
        Ok(None) => Ok(None),
        Err(e) => Err(CacheError::BackendUnavailable(format!("get: {e:?}"))),
    }
}

/// Fresh value for `key`, deleting it lazily if expired.
fn fresh(key: &str) -> Result<Option<Vec<u8>>, CacheError> {
    let bucket = open()?;
    let k = ckey(key);
    match raw_get(&bucket, &k)? {
        Some((exp, val)) => {
            if exp != 0 && exp <= now() {
                let _ = bucket.delete(&k);
                Ok(None)
            } else {
                Ok(Some(val))
            }
        }
        None => Ok(None),
    }
}

impl Guest for Component {
    fn get(key: String) -> Result<Option<Vec<u8>>, CacheError> {
        fresh(&key)
    }

    fn peek(key: String) -> Result<Option<Vec<u8>>, CacheError> {
        fresh(&key)
    }

    fn set(key: String, value: Vec<u8>, ttl_seconds: u64) -> Result<(), CacheError> {
        let bucket = open()?;
        let exp = if ttl_seconds == 0 { 0 } else { now() + ttl_seconds };
        let mut buf = Vec::with_capacity(8 + value.len());
        buf.extend_from_slice(&exp.to_be_bytes());
        buf.extend_from_slice(&value);
        bucket
            .set(&ckey(&key), &buf)
            .map_err(|e| CacheError::BackendUnavailable(format!("set: {e:?}")))
    }

    fn delete(key: String) -> Result<(), CacheError> {
        let bucket = open()?;
        bucket
            .delete(&ckey(&key))
            .map_err(|e| CacheError::BackendUnavailable(format!("delete: {e:?}")))
    }

    fn invalidate(key: String) -> Result<(), CacheError> {
        Self::delete(key)
    }

    fn invalidate_prefix(prefix: String) -> Result<u32, CacheError> {
        let bucket = open()?;
        let want = ckey(&prefix); // namespaced prefix to match stored keys
        let mut removed = 0u32;
        let mut cursor: Option<u64> = None;
        // Page through all keys; delete those under the namespaced prefix.
        loop {
            let page = bucket
                .list_keys(cursor)
                .map_err(|e| CacheError::BackendUnavailable(format!("list: {e:?}")))?;
            for k in &page.keys {
                if k.starts_with(&want) {
                    let _ = bucket.delete(k);
                    removed += 1;
                }
            }
            match page.cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        Ok(removed)
    }

    fn ttl(key: String) -> Result<Option<u64>, CacheError> {
        let bucket = open()?;
        match raw_get(&bucket, &ckey(&key))? {
            Some((0, _)) => Ok(Some(0)), // stored with no expiry
            Some((exp, _)) if exp > now() => Ok(Some(exp - now())),
            _ => Ok(None), // absent or expired
        }
    }

    // ---- strategies ----

    fn get_through(key: String, ttl_seconds: u64) -> Result<Option<Vec<u8>>, CacheError> {
        // hit?
        if let Some(v) = fresh(&key)? {
            return Ok(Some(v));
        }
        // miss -> load from the backing source, then populate.
        match source::load(&key).map_err(CacheError::SourceFailed)? {
            Some(v) => {
                Self::set(key, v.clone(), ttl_seconds)?;
                Ok(Some(v))
            }
            None => Ok(None),
        }
    }

    fn put_through(key: String, value: Vec<u8>, ttl_seconds: u64) -> Result<(), CacheError> {
        // write the backing store first; only cache if it succeeded, so the
        // cache never holds a value the source rejected.
        sink::store(&key, &value).map_err(CacheError::SourceFailed)?;
        Self::set(key, value, ttl_seconds)
    }

    fn put_behind(key: String, value: Vec<u8>, ttl_seconds: u64) -> Result<(), CacheError> {
        // cache immediately, and record a pending-flush marker keyed by the
        // cache key so `flush` can find and drain it later.
        Self::set(key.clone(), value, ttl_seconds)?;
        let bucket = open()?;
        bucket
            .set(&wb_marker(&key), key.as_bytes())
            .map_err(|e| CacheError::BackendUnavailable(format!("wb mark: {e:?}")))
    }

    fn flush() -> Result<u32, CacheError> {
        let bucket = open()?;
        let mut flushed = 0u32;
        let mut cursor: Option<u64> = None;
        loop {
            let page = bucket
                .list_keys(cursor)
                .map_err(|e| CacheError::BackendUnavailable(format!("list: {e:?}")))?;
            for stored in &page.keys {
                if !stored.starts_with(WB_PREFIX) {
                    continue;
                }
                // recover the original cache key from the marker value.
                let Ok(Some(raw)) = bucket.get(stored) else { continue };
                let Ok(orig) = String::from_utf8(raw) else { continue };
                // current cached value for that key (skip if expired/gone).
                if let Some(val) = fresh(&orig)? {
                    match sink::store(&orig, &val) {
                        Ok(()) => {
                            let _ = bucket.delete(stored); // drained
                            flushed += 1;
                        }
                        // retain the marker for the next flush on failure.
                        Err(_) => {}
                    }
                } else {
                    let _ = bucket.delete(stored); // value gone; drop the marker
                }
            }
            match page.cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        Ok(flushed)
    }
}

const WB_PREFIX: &str = "wb_";

/// Write-behind marker key for a cache key (sanitized, distinct namespace).
fn wb_marker(key: &str) -> String {
    let mut out = String::from(WB_PREFIX);
    for b in key.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'/' | b'=' => out.push(b as char),
            _ => out.push_str(&format!("_{b:02X}")),
        }
    }
    out
}

bindings::export!(Component with_types_in bindings);
