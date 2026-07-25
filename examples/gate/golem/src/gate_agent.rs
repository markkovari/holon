//! The `gate` rate limiter as a REAL Golem agent — the exact-serialization
//! counterpart to the shared-store approximation in `gate-domain`.
//!
//! `#[agent_definition(mount = "/gate/{key}")]` makes **one durable worker per
//! key**. A Golem worker is single-threaded and its state is durable, so the
//! read-decide-write of `take` is inherently serialized — no compare-and-swap,
//! no revision retry, and it survives restarts. Fire N concurrent `take`s at one
//! key and EXACTLY `capacity` succeed (the shared-store CAS over-admits under the
//! same load — that's the whole point of GATE.md).

use golem_rust::{agent_definition, agent_implementation, endpoint};

/// A per-key token bucket. `capacity` tokens; `refill_per_sec` added on each
/// take from elapsed time (via the durable clock).
#[agent_definition(mount = "/gate/{key}")]
pub trait GateAgent {
    /// The constructor params identify the worker — one per `key`.
    fn new(key: String) -> Self;

    /// Spend one token if available (token bucket). Returns `{allowed, remaining}`.
    #[endpoint(post = "/take")]
    fn take(&mut self) -> String;

    /// GCRA smoothing to `RATE`/s with a `BURST` budget. Returns
    /// `{allowed, retry_after_ms}`.
    #[endpoint(post = "/throttle")]
    fn throttle(&mut self) -> String;

    /// Refill the bucket + clear the throttle (for demo replay).
    #[endpoint(post = "/reset")]
    fn reset(&mut self) -> String;
}

const CAPACITY: f64 = 10.0;
const REFILL_PER_SEC: f64 = 1.0;
const RATE_PER_SEC: u64 = 5; // GCRA target rate
const BURST: u64 = 2; // GCRA burst budget (cells)

struct GateImpl {
    _key: String,
    tokens: f64,
    updated_ms: u64,
    tat: u64, // GCRA theoretical arrival time
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[agent_implementation]
impl GateAgent for GateImpl {
    fn new(key: String) -> Self {
        Self { _key: key, tokens: CAPACITY, updated_ms: now_ms(), tat: 0 }
    }

    fn take(&mut self) -> String {
        // continuous refill from elapsed time (a durable clock read on Golem).
        let now = now_ms();
        let elapsed = now.saturating_sub(self.updated_ms) as f64 / 1000.0;
        self.tokens = (self.tokens + elapsed * REFILL_PER_SEC).min(CAPACITY);
        self.updated_ms = now;

        let allowed = self.tokens >= 1.0;
        if allowed {
            self.tokens -= 1.0;
        }
        format!("{{\"allowed\":{},\"remaining\":{:.2}}}", allowed, self.tokens)
    }

    fn throttle(&mut self) -> String {
        // GCRA over one theoretical-arrival-time timestamp (no queue, no timer).
        let now = now_ms();
        let period = 1000 / RATE_PER_SEC; // emission interval per cell (ms)
        let limit = BURST * period; // how far early a request may arrive
        let tat = if self.tat == 0 { now } else { self.tat };
        let allow_at = tat.saturating_sub(limit);
        let allowed = now >= allow_at;
        let retry = if allowed {
            self.tat = tat.max(now) + period; // advance by one cell
            0
        } else {
            allow_at - now
        };
        format!("{{\"allowed\":{},\"retry_after_ms\":{}}}", allowed, retry)
    }

    fn reset(&mut self) -> String {
        self.tokens = CAPACITY;
        self.updated_ms = now_ms();
        self.tat = 0;
        format!("{{\"capacity\":{},\"rate\":{},\"burst\":{}}}", CAPACITY, RATE_PER_SEC, BURST)
    }
}
