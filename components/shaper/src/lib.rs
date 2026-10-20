//! `shaper` — the arithmetic behind rate limiting — token bucket and GCRA, as pure functions whose state the caller holds
#[allow(warnings)]
mod bindings;

use bindings::exports::shaper::limit::limiter::{Bucket, Decision, Guest};

struct Component;

impl Guest for Component {
    fn token_bucket(
        state: Bucket,
        now_ms: u64,
        capacity: f64,
        refill_per_sec: f64,
        cost: f64,
    ) -> (Decision, Bucket) {
        // Uninitialized state starts full.
        let tokens = if state.updated_ms == 0 {
            capacity
        } else {
            let elapsed_s = now_ms.saturating_sub(state.updated_ms) as f64 / 1000.0;
            (state.tokens + elapsed_s * refill_per_sec).min(capacity)
        };

        if tokens + 1e-9 >= cost {
            let left = tokens - cost;
            (
                Decision { allowed: true, retry_after_ms: 0, remaining: left },
                Bucket { tokens: left, updated_ms: now_ms },
            )
        } else {
            let need = cost - tokens;
            let retry = if refill_per_sec > 0.0 {
                (need / refill_per_sec * 1000.0).ceil() as u64
            } else {
                u64::MAX
            };
            (
                Decision { allowed: false, retry_after_ms: retry, remaining: tokens },
                // Don't spend on a denial, but persist the refill.
                Bucket { tokens, updated_ms: now_ms },
            )
        }
    }

    fn gcra(tat_ms: u64, now_ms: u64, period_ms: u64, burst: u32, cost: u32) -> (Decision, u64) {
        let inc = cost as u64 * period_ms;
        let limit = burst as u64 * period_ms; // how far early a request may arrive
        let tat = if tat_ms == 0 { now_ms } else { tat_ms };
        let allow_at = tat.saturating_sub(limit); // earliest `now` that's accepted

        if now_ms >= allow_at {
            let new_tat = tat.max(now_ms) + inc;
            (Decision { allowed: true, retry_after_ms: 0, remaining: 0.0 }, new_tat)
        } else {
            (Decision { allowed: false, retry_after_ms: allow_at - now_ms, remaining: 0.0 }, tat)
        }
    }
}

bindings::export!(Component with_types_in bindings);
