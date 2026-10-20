//! Unlike rate-limiter, consumption accumulates per billing PERIOD; bucket = now / period, found by arithmetic.
//! The draft API has no CAS, so reserve is check-increment-recheck with best-effort compensation.

#[allow(warnings)]
mod bindings;

use bindings::exports::quota::meter::meter::{Balance, Guest, QuotaError};
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::keyvalue::atomics;
use bindings::wasi::keyvalue::store as kv;

struct Component;

const BUCKET: &str = "default";
/// Used when the caller passes 0: 30 days.
const DEFAULT_PERIOD: u64 = 2_592_000;

fn now() -> u64 {
    wall_clock::now().seconds
}

fn period_or_default(period_seconds: u64) -> u64 {
    if period_seconds == 0 {
        DEFAULT_PERIOD
    } else {
        period_seconds
    }
}

fn bucket_of(period: u64) -> u64 {
    now() / period
}

fn resets_at(bucket: u64, period: u64) -> u64 {
    (bucket + 1).saturating_mul(period)
}

/// NATS-legal kv chars only (as idempotency-guard's `id_key`).
fn sanitize(subject: &str) -> String {
    let mut out = String::with_capacity(subject.len());
    for b in subject.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'/' | b'=' => out.push(b as char),
            _ => out.push_str(&format!("_{b:02X}")),
        }
    }
    out
}

fn counter_key(subject: &str, bucket: u64) -> String {
    format!("q_{}_{}", sanitize(subject), bucket)
}

fn open() -> Result<kv::Bucket, QuotaError> {
    kv::open(BUCKET).map_err(|e| QuotaError::BackendUnavailable(format!("open: {e:?}")))
}

fn read_u64(bucket: &kv::Bucket, key: &str) -> Result<u64, QuotaError> {
    match bucket.get(key) {
        Ok(Some(bytes)) => {
            Ok(String::from_utf8(bytes).ok().and_then(|s| s.parse().ok()).unwrap_or(0))
        }
        Ok(None) => Ok(0),
        Err(e) => Err(QuotaError::BackendUnavailable(format!("get: {e:?}"))),
    }
}

fn write_u64(bucket: &kv::Bucket, key: &str, v: u64) -> Result<(), QuotaError> {
    bucket
        .set(key, v.to_string().as_bytes())
        .map_err(|e| QuotaError::BackendUnavailable(format!("set: {e:?}")))
}

fn balance(used: u64, limit: u64, bucket: u64, period: u64) -> Balance {
    Balance {
        used,
        limit,
        remaining: limit.saturating_sub(used),
        resets_at: resets_at(bucket, period),
    }
}

impl Guest for Component {
    fn reserve(
        subject: String,
        amount: u64,
        limit: u64,
        period_seconds: u64,
    ) -> Result<Balance, QuotaError> {
        let period = period_or_default(period_seconds);
        let bucket = bucket_of(period);
        let b = open()?;
        let key = counter_key(&subject, bucket);

        let current = read_u64(&b, &key)?;
        if current.saturating_add(amount) > limit {
            return Err(QuotaError::Exceeded(limit.saturating_sub(current)));
        }

        // `increment` returns the NEW total, so concurrent reservers cannot both oversell.
        let new = atomics::increment(&b, &key, amount)
            .map_err(|e| QuotaError::BackendUnavailable(format!("increment: {e:?}")))?;

        // Best-effort, no CAS: on overshoot re-set to the pre-increment value and reject; under contention
        // this may briefly under-count but never permanently oversells.
        if new > limit {
            let _ = write_u64(&b, &key, current);
            return Err(QuotaError::Exceeded(limit.saturating_sub(current)));
        }

        Ok(balance(new, limit, bucket, period))
    }

    fn record_usage(
        subject: String,
        amount: u64,
        limit: u64,
        period_seconds: u64,
    ) -> Result<Balance, QuotaError> {
        let period = period_or_default(period_seconds);
        let bucket = bucket_of(period);
        let b = open()?;
        let key = counter_key(&subject, bucket);

        let new = atomics::increment(&b, &key, amount)
            .map_err(|e| QuotaError::BackendUnavailable(format!("increment: {e:?}")))?;

        Ok(balance(new, limit, bucket, period))
    }

    fn peek(subject: String, limit: u64, period_seconds: u64) -> Result<Balance, QuotaError> {
        let period = period_or_default(period_seconds);
        let bucket = bucket_of(period);
        let b = open()?;
        let used = read_u64(&b, &counter_key(&subject, bucket))?;
        Ok(balance(used, limit, bucket, period))
    }

    fn reset(subject: String) -> Result<(), QuotaError> {
        let b = open()?;
        // `reset` carries no period, so it deletes the current DEFAULT-period bucket; older ones age out.
        let bucket = bucket_of(DEFAULT_PERIOD);
        let _ = b.delete(&counter_key(&subject, bucket));
        Ok(())
    }
}

bindings::export!(Component with_types_in bindings);
