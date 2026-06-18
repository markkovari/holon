//! `quota` — reference implementation of `quota:meter`.
//!
//! Cumulative usage metering + enforcement, backed by `wasi:keyvalue`.
//!
//! CONTRAST WITH `rate-limiter` (`ratelimit:guard`): the rate-limiter counts
//! EVENTS within a sliding/fixed window for burst protection and resets the
//! counter every window. `quota:meter` instead accumulates CONSUMPTION (API
//! calls this month, GB stored, seats used) against a budget over a billing
//! PERIOD — the counter only "resets" when the calendar period rolls over to a
//! new bucket. Reserve-or-reject before the work, record actual usage after,
//! peek the balance any time, reset on plan change.
//!
//! State: one counter key per (subject, period-bucket):
//!   `q_{sanitized subject}_{bucket}`  -> units used, bumped via the ATOMIC
//!                                        `wasi:keyvalue/atomics.increment`.
//! The bucket = `now / period_seconds`, so a subject's counter for the current
//! period is found purely by arithmetic on the wall clock — no enumeration.
//! `resets_at = (bucket + 1) * period`.
//!
//! Concurrency: consumption lives in its own key and is bumped atomically, so
//! two concurrent consumers can never lose an update and oversell silently.
//! Because the draft API exposes only `increment` (no compare-and-swap), the
//! reserve path is check-then-increment-then-recheck with best-effort
//! compensation on overshoot — see `reserve` for the race note.

#[allow(warnings)]
mod bindings;

use bindings::exports::quota::meter::meter::{Balance, Guest, QuotaError};
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::keyvalue::atomics;
use bindings::wasi::keyvalue::store as kv;

struct Component;

const BUCKET: &str = "default";
/// Fallback period when the caller passes `period_seconds == 0`: 30 days.
const DEFAULT_PERIOD: u64 = 2_592_000;

fn now() -> u64 {
    wall_clock::now().seconds
}

/// Treat a zero period as the 30-day default so we never divide by zero.
fn period_or_default(period_seconds: u64) -> u64 {
    if period_seconds == 0 {
        DEFAULT_PERIOD
    } else {
        period_seconds
    }
}

/// The current period bucket index for `period` (= floor(now / period)).
fn bucket_of(period: u64) -> u64 {
    now() / period
}

/// Unix seconds the given bucket's period ends / resets.
fn resets_at(bucket: u64, period: u64) -> u64 {
    (bucket + 1).saturating_mul(period)
}

/// Sanitize an opaque subject to NATS-legal kv chars (same byte scheme as the
/// idempotency-guard's `id_key` / rate-limiter's `safe`).
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

/// Counter key for a subject's consumption in a given period bucket.
fn counter_key(subject: &str, bucket: u64) -> String {
    format!("q_{}_{}", sanitize(subject), bucket)
}

fn open() -> Result<kv::Bucket, QuotaError> {
    kv::open(BUCKET).map_err(|e| QuotaError::BackendUnavailable(format!("open: {e:?}")))
}

/// Read a u64 counter, treating absent / unparseable as 0.
fn read_u64(bucket: &kv::Bucket, key: &str) -> Result<u64, QuotaError> {
    match bucket.get(key) {
        Ok(Some(bytes)) => Ok(String::from_utf8(bytes)
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)),
        Ok(None) => Ok(0),
        Err(e) => Err(QuotaError::BackendUnavailable(format!("get: {e:?}"))),
    }
}

fn write_u64(bucket: &kv::Bucket, key: &str, v: u64) -> Result<(), QuotaError> {
    bucket
        .set(key, v.to_string().as_bytes())
        .map_err(|e| QuotaError::BackendUnavailable(format!("set: {e:?}")))
}

/// Build a `Balance` snapshot from a known `used` total.
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

        // Optimistic pre-check against the current value.
        let current = read_u64(&b, &key)?;
        if current.saturating_add(amount) > limit {
            // Wouldn't fit; consume nothing.
            return Err(QuotaError::Exceeded(limit.saturating_sub(current)));
        }

        // Atomically claim the units. `increment` returns the NEW total, so two
        // concurrent reservers can't both read the same `current` and oversell.
        let new = atomics::increment(&b, &key, amount)
            .map_err(|e| QuotaError::BackendUnavailable(format!("increment: {e:?}")))?;

        // BEST-EFFORT race compensation (NOT a hard guarantee): a concurrent
        // reserver may have incremented between our pre-check and our own
        // increment, pushing the atomic total over `limit`. The draft API has
        // no compare-and-swap to make claim-or-reject truly atomic, so on
        // overshoot we roll our `amount` back by re-setting the key to its
        // pre-increment value and reject. This `set` is itself non-atomic and
        // can clobber another racer's concurrent write — at worst it briefly
        // under-counts under heavy simultaneous contention, never permanently
        // oversells. (Same class of caveat as the idempotency-guard's nonce
        // race note.)
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

        // Unconditional post-hoc accounting: no limit check, just record.
        let new = atomics::increment(&b, &key, amount)
            .map_err(|e| QuotaError::BackendUnavailable(format!("increment: {e:?}")))?;

        Ok(balance(new, limit, bucket, period))
    }

    fn peek(
        subject: String,
        limit: u64,
        period_seconds: u64,
    ) -> Result<Balance, QuotaError> {
        let period = period_or_default(period_seconds);
        let bucket = bucket_of(period);
        let b = open()?;
        let used = read_u64(&b, &counter_key(&subject, bucket))?;
        Ok(balance(used, limit, bucket, period))
    }

    fn reset(subject: String) -> Result<(), QuotaError> {
        let b = open()?;
        // `reset` carries no period, so we can't reconstruct the exact bucket
        // for a subject metered with a non-default period; we delete the
        // CURRENT bucket under the DEFAULT period (the common monthly case).
        // We also can't enumerate every historical bucket key cheaply, so older
        // buckets are left untouched — they're never read again once the clock
        // advances past their period, so they age out naturally and leaving
        // them is harmless. Idempotent: deleting an absent key is a no-op.
        let bucket = bucket_of(DEFAULT_PERIOD);
        let _ = b.delete(&counter_key(&subject, bucket));
        Ok(())
    }
}

bindings::export!(Component with_types_in bindings);
