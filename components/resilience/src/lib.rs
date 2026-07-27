//! `resilience` — reference implementation of `resilience:breaker/breaker`.
//!
//! A circuit breaker and a backoff schedule, as pure functions. The caller owns
//! the state and passes it in; every function returns the state to persist. No
//! state, no clock, no host imports — `now-ms` and the jitter `seed` come in as
//! arguments, which is also what makes the state machine exhaustively testable.
//!
//! The state machine:
//!
//! ```text
//!                failures >= failure-threshold
//!        CLOSED ────────────────────────────────► OPEN
//!          ▲                                       │ cooldown open-ms elapsed
//!          │ successes >= success-threshold         ▼
//!          └──────────────── HALF-OPEN ◄────── (admit up to half-open-probes)
//!                               │
//!                               └── one probe fails ──► OPEN
//! ```
//!
//! `failure-threshold` counts CONSECUTIVE failures: a success in `closed` clears
//! the count, and `window-ms` bounds how long a partial streak is remembered, so
//! an upstream that fails once an hour never trips.
//!
//! ponytail: consecutive-with-staleness, not a sliding error *ratio*. It needs
//! one counter instead of a bucket ring and behaves identically for the failure
//! mode that matters (an upstream that is actually down). Upgrade to a ratio
//! (`sketch`-style bucket ring in the same record) only if a partially-degraded
//! upstream — say 30% errors, forever — must also trip.

#[allow(warnings)]
mod bindings;

use bindings::exports::resilience::breaker::breaker::{
    Admission, Circuit, CircuitState, Guest, Policy, RetryPolicy,
};

struct Component;

/// A zero threshold would mean "trip on nothing" / "never close"; clamp to 1.
fn at_least_1(n: u32) -> u32 {
    n.max(1)
}

impl Guest for Component {
    fn admit(state: Circuit, now_ms: u64, pol: Policy) -> (Admission, Circuit) {
        let mut c = state;
        match c.state {
            CircuitState::Closed => {
                // Forget a stale failure streak before admitting.
                if pol.window_ms > 0 && now_ms.saturating_sub(c.window_start_ms) >= pol.window_ms {
                    c.failures = 0;
                    c.window_start_ms = now_ms;
                }
                (Admission { admit: true, state: CircuitState::Closed, retry_after_ms: 0 }, c)
            }
            CircuitState::Open => {
                let elapsed = now_ms.saturating_sub(c.changed_ms);
                if elapsed >= pol.open_ms {
                    // Cooldown done: go half-open and spend the first probe.
                    c.state = CircuitState::HalfOpen;
                    c.changed_ms = now_ms;
                    c.successes = 0;
                    c.probes = 1;
                    (Admission { admit: true, state: CircuitState::HalfOpen, retry_after_ms: 0 }, c)
                } else {
                    // Fail fast — the upstream is not dialled at all.
                    (
                        Admission {
                            admit: false,
                            state: CircuitState::Open,
                            retry_after_ms: pol.open_ms - elapsed,
                        },
                        c,
                    )
                }
            }
            CircuitState::HalfOpen => {
                if c.probes < at_least_1(pol.half_open_probes) {
                    c.probes += 1;
                    (Admission { admit: true, state: CircuitState::HalfOpen, retry_after_ms: 0 }, c)
                } else {
                    // Probe budget spent; someone else's probe decides. No useful
                    // retry-after: the answer arrives when that probe reports.
                    (Admission { admit: false, state: CircuitState::HalfOpen, retry_after_ms: 0 }, c)
                }
            }
        }
    }

    fn observe(state: Circuit, now_ms: u64, pol: Policy, ok: bool) -> Circuit {
        let mut c = state;
        match (c.state, ok) {
            (CircuitState::Closed, true) => {
                c.failures = 0;
                c.window_start_ms = now_ms;
            }
            (CircuitState::Closed, false) => {
                if pol.window_ms > 0 && now_ms.saturating_sub(c.window_start_ms) >= pol.window_ms {
                    c.failures = 0;
                    c.window_start_ms = now_ms;
                }
                if c.failures == 0 {
                    c.window_start_ms = now_ms;
                }
                c.failures += 1;
                if c.failures >= at_least_1(pol.failure_threshold) {
                    c.state = CircuitState::Open;
                    c.changed_ms = now_ms;
                    c.probes = 0;
                    c.successes = 0;
                }
            }
            (CircuitState::HalfOpen, true) => {
                // The probe reported, so its slot is free again — `half-open-probes`
                // bounds CONCURRENT probes, not probes per episode.
                c.probes = c.probes.saturating_sub(1);
                c.successes += 1;
                if c.successes >= at_least_1(pol.success_threshold) {
                    // Recovered.
                    c.state = CircuitState::Closed;
                    c.failures = 0;
                    c.successes = 0;
                    c.probes = 0;
                    c.window_start_ms = now_ms;
                    c.changed_ms = now_ms;
                }
            }
            (CircuitState::HalfOpen, false) => {
                // One failed probe is enough — back to open, full cooldown.
                c.state = CircuitState::Open;
                c.changed_ms = now_ms;
                c.failures = at_least_1(pol.failure_threshold);
                c.successes = 0;
                c.probes = 0;
            }
            // An outcome reported while open means the caller ignored `admit`.
            // Nothing to learn from it; leave the cooldown running.
            (CircuitState::Open, _) => {}
        }
        c
    }

    fn backoff(attempt: u32, pol: RetryPolicy, seed: u64) -> Option<u32> {
        if attempt > at_least_1(pol.max_attempts) {
            return None; // out of attempts — stop retrying
        }
        if attempt <= 1 {
            return Some(0); // the first attempt does not wait
        }
        let cap = if pol.max_ms == 0 { u32::MAX } else { pol.max_ms } as u64;
        let base = pol.base_ms as u64;
        // base * (factor_pct/100)^(attempt-2), saturating, capped.
        let mut delay = base;
        for _ in 0..(attempt - 2) {
            delay = delay.saturating_mul(pol.factor_pct as u64) / 100;
            if delay >= cap {
                break;
            }
        }
        let delay = delay.min(cap).max(base.min(cap));
        if !pol.jitter || delay <= base {
            return Some(delay as u32);
        }
        // Decorrelated jitter: uniform in [base, delay]. Deterministic in `seed`
        // (splitmix64 mixed with the attempt) so this stays a pure function.
        let span = delay - base + 1;
        Some((base + mix(seed ^ (attempt as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)) % span) as u32)
    }
}

/// splitmix64 finalizer — cheap, well-distributed bit mixing.
fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    fn pol() -> Policy {
        Policy {
            failure_threshold: 3,
            window_ms: 10_000,
            open_ms: 1_000,
            half_open_probes: 1,
            success_threshold: 2,
        }
    }

    fn fresh() -> Circuit {
        Circuit {
            state: CircuitState::Closed,
            failures: 0,
            successes: 0,
            window_start_ms: 0,
            changed_ms: 0,
            probes: 0,
        }
    }

    fn admit(c: Circuit, t: u64) -> (Admission, Circuit) {
        <Component as Guest>::admit(c, t, pol())
    }
    fn observe(c: Circuit, t: u64, ok: bool) -> Circuit {
        <Component as Guest>::observe(c, t, pol(), ok)
    }

    #[test]
    fn trips_after_threshold_then_fails_fast() {
        let mut c = fresh();
        for t in [10, 20] {
            let (a, next) = admit(c, t);
            assert!(a.admit);
            c = observe(next, t, false);
            assert!(matches!(c.state, CircuitState::Closed), "2 failures < threshold 3");
        }
        let (a, next) = admit(c, 30);
        assert!(a.admit);
        c = observe(next, 30, false);
        assert!(matches!(c.state, CircuitState::Open), "3rd failure trips");

        // Fail fast for the whole cooldown, with an exact retry-after.
        let (a, _) = admit(c, 400);
        assert!(!a.admit);
        assert_eq!(a.retry_after_ms, 630, "open_ms 1000 - 370 elapsed");
    }

    #[test]
    fn success_clears_a_partial_streak() {
        let mut c = fresh();
        c = observe(c, 10, false);
        c = observe(c, 20, false);
        assert_eq!(c.failures, 2);
        c = observe(c, 30, true);
        assert_eq!(c.failures, 0, "a success clears the streak");
        c = observe(c, 40, false);
        c = observe(c, 50, false);
        assert!(matches!(c.state, CircuitState::Closed), "streak restarted, no trip");
    }

    #[test]
    fn stale_failures_are_forgotten() {
        let mut c = fresh();
        c = observe(c, 1_000, false);
        c = observe(c, 2_000, false);
        // Two failures, then a gap longer than window_ms: the streak is stale.
        c = observe(c, 2_000 + 10_001, false);
        assert!(matches!(c.state, CircuitState::Closed), "stale streak forgotten");
        assert_eq!(c.failures, 1);
    }

    #[test]
    fn half_open_probe_recovers_then_closes() {
        // Trip it.
        let mut c = fresh();
        for t in [10, 20, 30] {
            c = observe(c, t, false);
        }
        assert!(matches!(c.state, CircuitState::Open));

        // Cooldown elapsed -> one probe admitted, budget then spent.
        let (a, next) = admit(c, 1_030);
        assert!(a.admit && matches!(a.state, CircuitState::HalfOpen));
        c = next;
        let (a2, next) = admit(c, 1_031);
        assert!(!a2.admit, "half_open_probes = 1, budget spent");
        c = next;

        // success_threshold 2: one success is not enough, two close it.
        c = observe(c, 1_040, true);
        assert!(matches!(c.state, CircuitState::HalfOpen));
        let (a3, next) = admit(c, 1_050);
        assert!(a3.admit, "a fresh probe after a good one");
        c = observe(next, 1_060, true);
        assert!(matches!(c.state, CircuitState::Closed), "2 successes close it");
        assert_eq!(c.failures, 0);
    }

    #[test]
    fn a_failed_probe_reopens_with_full_cooldown() {
        let mut c = fresh();
        for t in [10, 20, 30] {
            c = observe(c, t, false);
        }
        let (_, next) = admit(c, 1_030);
        c = observe(next, 1_035, false);
        assert!(matches!(c.state, CircuitState::Open));
        let (a, _) = admit(c, 1_035);
        assert_eq!(a.retry_after_ms, 1_000, "cooldown restarts from the failed probe");
    }

    #[test]
    fn backoff_grows_caps_and_stops() {
        let p = RetryPolicy { max_attempts: 4, base_ms: 100, factor_pct: 200, max_ms: 500, jitter: false };
        let b = |n| <Component as Guest>::backoff(n, p, 7);
        assert_eq!(b(1), Some(0), "no wait before the first attempt");
        assert_eq!(b(2), Some(100));
        assert_eq!(b(3), Some(200));
        assert_eq!(b(4), Some(400));
        assert_eq!(b(5), None, "max_attempts 4 -> give up");

        let capped = RetryPolicy { max_ms: 250, ..p };
        assert_eq!(<Component as Guest>::backoff(4, capped, 7), Some(250), "capped");
    }

    #[test]
    fn jitter_stays_in_range_and_is_deterministic() {
        let p = RetryPolicy { max_attempts: 6, base_ms: 100, factor_pct: 200, max_ms: 10_000, jitter: true };
        for seed in 0..50u64 {
            for attempt in 2..=6u32 {
                let d = <Component as Guest>::backoff(attempt, p, seed).unwrap();
                let exact = <Component as Guest>::backoff(attempt, RetryPolicy { jitter: false, ..p }, seed).unwrap();
                assert!((100..=exact).contains(&d), "attempt {attempt} seed {seed}: {d} not in 100..={exact}");
            }
        }
        assert_eq!(
            <Component as Guest>::backoff(4, p, 42),
            <Component as Guest>::backoff(4, p, 42),
            "same seed -> same delay (pure)"
        );
    }
}
