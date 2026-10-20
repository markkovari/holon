//! Fixed-window failure counter: the count is an atomic increment; clearing it at a window boundary
//! is a rare, benign race that can briefly under-count.

#[allow(warnings)]
mod bindings;

use bindings::exports::ratelimit::guard::limiter::{Guest, LimitError};
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::config::store as config;
use bindings::wasi::keyvalue::atomics;
use bindings::wasi::keyvalue::store;

struct Component;

const BUCKET: &str = "default";

fn max_attempts() -> u64 {
    cfg_u64("max-attempts", 5)
}
fn lockout_window() -> u64 {
    cfg_u64("lockout-window", 300)
}
fn cfg_u64(key: &str, default: u64) -> u64 {
    match config::get(key) {
        Ok(Some(v)) => v.parse().unwrap_or(default),
        _ => default,
    }
}

fn now() -> u64 {
    wall_clock::now().seconds
}

fn safe(prefix: &str, key: &str) -> String {
    let mut out = String::with_capacity(key.len() + prefix.len());
    out.push_str(prefix);
    for b in key.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'/' | b'=' => out.push(b as char),
            _ => out.push_str(&format!("_{b:02X}")),
        }
    }
    out
}

fn count_key(key: &str) -> String {
    safe("rlc_", key)
}
fn window_key(key: &str) -> String {
    safe("rlw_", key)
}

fn open() -> Result<store::Bucket, LimitError> {
    store::open(BUCKET).map_err(|e| LimitError::BackendUnavailable(format!("open: {e:?}")))
}

fn read_u64(bucket: &store::Bucket, key: &str) -> Result<Option<u64>, LimitError> {
    match bucket.get(key) {
        Ok(Some(bytes)) => Ok(String::from_utf8(bytes).ok().and_then(|s| s.parse().ok())),
        Ok(None) => Ok(None),
        Err(e) => Err(LimitError::BackendUnavailable(format!("get: {e:?}"))),
    }
}

fn write_u64(bucket: &store::Bucket, key: &str, v: u64) -> Result<(), LimitError> {
    bucket
        .set(key, v.to_string().as_bytes())
        .map_err(|e| LimitError::BackendUnavailable(format!("set: {e:?}")))
}

fn current(bucket: &store::Bucket, key: &str, window: u64) -> Result<(u64, u64), LimitError> {
    let now = now();
    match read_u64(bucket, &window_key(key))? {
        Some(start) if now.saturating_sub(start) < window => {
            let count = read_u64(bucket, &count_key(key))?.unwrap_or(0);
            Ok((count, start))
        }
        _ => Ok((0, now)),
    }
}

impl Guest for Component {
    fn check(key: String) -> Result<u32, LimitError> {
        let max = max_attempts();
        if max == 0 {
            return Ok(u32::MAX); // disabled
        }
        let window = lockout_window();
        let bucket = open()?;
        let (count, start) = current(&bucket, &key, window)?;
        if count >= max {
            let retry = window.saturating_sub(now().saturating_sub(start));
            return Err(LimitError::Locked(retry as u32));
        }
        Ok((max - count) as u32)
    }

    fn record_failure(key: String) -> Result<(), LimitError> {
        let max = max_attempts();
        if max == 0 {
            return Ok(());
        }
        let window = lockout_window();
        let bucket = open()?;
        let ck = count_key(&key);
        let wk = window_key(&key);
        let now = now();

        match read_u64(&bucket, &wk)? {
            Some(start) if now.saturating_sub(start) < window => {}
            _ => {
                write_u64(&bucket, &wk, now)?;
                let _ = bucket.delete(&ck);
            }
        }

        atomics::increment(&bucket, &ck, 1)
            .map(|_| ())
            .map_err(|e| LimitError::BackendUnavailable(format!("increment: {e:?}")))
    }

    fn reset(key: String) -> Result<(), LimitError> {
        let bucket = open()?;
        let _ = bucket.delete(&count_key(&key));
        bucket
            .delete(&window_key(&key))
            .map_err(|e| LimitError::BackendUnavailable(format!("delete: {e:?}")))
    }
}

bindings::export!(Component with_types_in bindings);
