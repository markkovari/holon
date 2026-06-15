//! `rate-limiter` — reference implementation of `ratelimit:guard`.
//!
//! Fixed-window failure counter with lockout, backed by `wasi:keyvalue`.
//! State is two keys per limited identifier:
//!   `rlc_{key}`  -> the failure count (mutated via the ATOMIC `increment`)
//!   `rlw_{key}`  -> the window-start unix seconds
//! When `count` reaches `max-attempts` within `lockout-window` seconds, the key
//! is locked until the window expires; a new window starts once the old elapses.
//!
//! Concurrency: the count lives in its own key and is bumped with
//! `wasi:keyvalue/atomics.increment`, so concurrent `record-failure` calls never
//! lose an update (the bug a load/modify/save would have). The window reset
//! (clearing the counter when the window elapses) is a benign, rare race — at
//! worst it briefly under-counts right at a window boundary, never over-counts.
//!
//! Config (wasi:config/runtime):
//!   max-attempts     failures allowed per window before lockout (default 5; 0 = disabled)
//!   lockout-window   window / lockout duration, seconds          (default 300)

#[allow(warnings)]
mod bindings;

use bindings::exports::ratelimit::guard::limiter::{Guest, LimitError};
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::config::runtime as config;
use bindings::wasi::keyvalue::atomics;
use bindings::wasi::keyvalue::store;

struct Component;

const BUCKET: &str = "default";

// ---- config -------------------------------------------------------------

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

/// Sanitize an identifier to NATS-legal kv chars, with a per-purpose prefix.
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

/// Current (count, window_start) for `key`, treating an elapsed window as a
/// fresh zero count.
fn current(bucket: &store::Bucket, key: &str, window: u64) -> Result<(u64, u64), LimitError> {
    let now = now();
    match read_u64(bucket, &window_key(key))? {
        Some(start) if now.saturating_sub(start) < window => {
            let count = read_u64(bucket, &count_key(key))?.unwrap_or(0);
            Ok((count, start))
        }
        // absent or elapsed -> fresh window.
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

        // Start (or roll over) the window if none is active. Clearing the
        // counter here is the only non-atomic step; the increment below is what
        // must not lose updates, and it is atomic.
        match read_u64(&bucket, &wk)? {
            Some(start) if now.saturating_sub(start) < window => {}
            _ => {
                write_u64(&bucket, &wk, now)?;
                let _ = bucket.delete(&ck); // reset count for the new window
            }
        }

        // Atomic: concurrent failures all count, no lost updates.
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
