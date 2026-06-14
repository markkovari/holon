//! `rate-limiter` — reference implementation of `ratelimit:guard`.
//!
//! Fixed-window failure counter with lockout, backed by `wasi:keyvalue`.
//! State: `rl:{key}` -> JSON { count, window-start }. When `count` reaches
//! `max-attempts` within `lockout-window` seconds, the key is locked until the
//! window expires. A new window starts once the old one elapses.
//!
//! Config (wasi:config/runtime):
//!   max-attempts     failures allowed per window before lockout (default 5; 0 = disabled)
//!   lockout-window   window / lockout duration, seconds          (default 300)

#[allow(warnings)]
mod bindings;

use bindings::exports::ratelimit::guard::limiter::{Guest, LimitError};
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::config::runtime as config;
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

// ---- counter state ------------------------------------------------------

struct Counter {
    count: u64,
    window_start: u64,
}

fn now() -> u64 {
    wall_clock::now().seconds
}

fn rl_key(key: &str) -> String {
    // sanitize to NATS-legal kv chars (same scheme as auth-guard's kv::safe).
    let mut out = String::with_capacity(key.len() + 3);
    out.push_str("rl_");
    for b in key.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'/' | b'=' => out.push(b as char),
            _ => out.push_str(&format!("_{b:02X}")),
        }
    }
    out
}

fn open() -> Result<store::Bucket, LimitError> {
    store::open(BUCKET).map_err(|e| LimitError::BackendUnavailable(format!("open: {e:?}")))
}

fn load(bucket: &store::Bucket, key: &str) -> Result<Option<Counter>, LimitError> {
    match bucket.get(key) {
        Ok(Some(bytes)) => {
            let s = String::from_utf8(bytes)
                .map_err(|_| LimitError::BackendUnavailable("value not utf-8".into()))?;
            // stored as "count:window-start"
            let (c, w) = s
                .split_once(':')
                .ok_or_else(|| LimitError::BackendUnavailable("corrupt counter".into()))?;
            Ok(Some(Counter {
                count: c.parse().unwrap_or(0),
                window_start: w.parse().unwrap_or(0),
            }))
        }
        Ok(None) => Ok(None),
        Err(e) => Err(LimitError::BackendUnavailable(format!("get: {e:?}"))),
    }
}

fn save(bucket: &store::Bucket, key: &str, c: &Counter) -> Result<(), LimitError> {
    let body = format!("{}:{}", c.count, c.window_start);
    bucket
        .set(key, body.as_bytes())
        .map_err(|e| LimitError::BackendUnavailable(format!("set: {e:?}")))
}

/// Return the current counter for `key`, treating an elapsed window as reset.
fn current(bucket: &store::Bucket, key: &str, window: u64) -> Result<Counter, LimitError> {
    let now = now();
    match load(bucket, key)? {
        Some(c) if now.saturating_sub(c.window_start) < window => Ok(c),
        // absent or window elapsed -> fresh window.
        _ => Ok(Counter { count: 0, window_start: now }),
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
        let c = current(&bucket, &rl_key(&key), window)?;
        if c.count >= max {
            let retry = window.saturating_sub(now().saturating_sub(c.window_start));
            return Err(LimitError::Locked(retry as u32));
        }
        Ok((max - c.count) as u32)
    }

    fn record_failure(key: String) -> Result<(), LimitError> {
        let max = max_attempts();
        if max == 0 {
            return Ok(());
        }
        let window = lockout_window();
        let bucket = open()?;
        let k = rl_key(&key);
        let mut c = current(&bucket, &k, window)?;
        c.count += 1;
        save(&bucket, &k, &c)
    }

    fn reset(key: String) -> Result<(), LimitError> {
        let bucket = open()?;
        bucket
            .delete(&rl_key(&key))
            .map_err(|e| LimitError::BackendUnavailable(format!("delete: {e:?}")))
    }
}

bindings::export!(Component with_types_in bindings);
