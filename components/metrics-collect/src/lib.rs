//! `metrics-collect` — reference implementation of `metrics:collect`.
//!
//! Atomic named counters backed by `wasi:keyvalue`. Each counter is two kv keys:
//!   `m/{key}`     — the count, mutated only via `atomics::increment` (race-free)
//!   `t/{key}`     — the last-updated unix-seconds stamp (best-effort, plain set)
//! `scan(prefix)` lists the `m/` keyspace, filters by prefix, and pairs each
//! count with its `t/` sibling. `rate` divides two counters, guarding div-by-0.

#[allow(warnings)]
mod bindings;

use bindings::exports::metrics::collect::collector::{Counter, Guest, MetricsError};
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::keyvalue::atomics;
use bindings::wasi::keyvalue::store as kv;

struct Component;

const BUCKET: &str = "default";
const COUNT: &str = "m/"; // count keyspace
const STAMP: &str = "t/"; // last-updated keyspace

fn open() -> Result<kv::Bucket, MetricsError> {
    kv::open(BUCKET).map_err(|e| MetricsError::BackendUnavailable(format!("open: {e:?}")))
}

fn now() -> u64 {
    wall_clock::now().seconds
}

fn count_key(key: &str) -> String {
    format!("{COUNT}{key}")
}
fn stamp_key(key: &str) -> String {
    format!("{STAMP}{key}")
}

fn read_u64(bucket: &kv::Bucket, key: &str) -> Result<u64, MetricsError> {
    match bucket.get(key) {
        Ok(Some(bytes)) => Ok(std::str::from_utf8(&bytes).ok().and_then(|s| s.parse().ok()).unwrap_or(0)),
        Ok(None) => Ok(0),
        Err(e) => Err(MetricsError::BackendUnavailable(format!("get: {e:?}"))),
    }
}

fn stamp(bucket: &kv::Bucket, key: &str) {
    // best-effort — a missing stamp only degrades `updated` to 0, never counts.
    let _ = bucket.set(&stamp_key(key), now().to_string().as_bytes());
}

impl Guest for Component {
    fn incr(key: String, by: u64) -> Result<u64, MetricsError> {
        let bucket = open()?;
        let new = atomics::increment(&bucket, &count_key(&key), by)
            .map_err(|e| MetricsError::BackendUnavailable(format!("increment: {e:?}")))?;
        stamp(&bucket, &key);
        Ok(new)
    }

    fn get(key: String) -> Result<u64, MetricsError> {
        let bucket = open()?;
        read_u64(&bucket, &count_key(&key))
    }

    fn scan(prefix: String) -> Result<Vec<Counter>, MetricsError> {
        let bucket = open()?;
        let mut out = Vec::new();
        let mut cursor: Option<u64> = None;
        loop {
            let page = bucket
                .list_keys(cursor)
                .map_err(|e| MetricsError::BackendUnavailable(format!("list-keys: {e:?}")))?;
            for k in &page.keys {
                let Some(bare) = k.strip_prefix(COUNT) else { continue };
                if !bare.starts_with(&prefix) {
                    continue;
                }
                let value = read_u64(&bucket, k)?;
                let updated = read_u64(&bucket, &stamp_key(bare))?;
                out.push(Counter { key: bare.to_string(), value, updated });
            }
            match page.cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        Ok(out)
    }

    fn rate(num_key: String, denom_key: String) -> Result<f64, MetricsError> {
        let bucket = open()?;
        let num = read_u64(&bucket, &count_key(&num_key))?;
        let denom = read_u64(&bucket, &count_key(&denom_key))?;
        if denom == 0 {
            return Ok(0.0);
        }
        Ok(num as f64 / denom as f64)
    }

    fn reset(key: String) -> Result<(), MetricsError> {
        let bucket = open()?;
        bucket
            .delete(&count_key(&key))
            .map_err(|e| MetricsError::BackendUnavailable(format!("delete: {e:?}")))?;
        let _ = bucket.delete(&stamp_key(&key));
        Ok(())
    }
}

bindings::export!(Component with_types_in bindings);
