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

    /// Spend one token if available. Returns JSON `{allowed, remaining}`.
    #[endpoint(post = "/take")]
    fn take(&mut self) -> String;

    /// Refill to capacity (for demo replay). Returns the capacity.
    #[endpoint(post = "/reset")]
    fn reset(&mut self) -> String;
}

const CAPACITY: f64 = 10.0;
const REFILL_PER_SEC: f64 = 1.0;

struct GateImpl {
    _key: String,
    tokens: f64,
    updated_ms: u64,
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
        Self { _key: key, tokens: CAPACITY, updated_ms: now_ms() }
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

    fn reset(&mut self) -> String {
        self.tokens = CAPACITY;
        self.updated_ms = now_ms();
        format!("{{\"capacity\":{}}}", CAPACITY)
    }
}
