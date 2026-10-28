//! Provider pool, token buckets, and circuit breaking for the smart cascading router.

pub const NANOS_PER_SEC: u64 = 1_000_000_000;
pub const NANOS_PER_MIN: u64 = 60 * NANOS_PER_SEC;

#[derive(Debug, Clone)]
pub struct TokenBucket {
    pub rpm: u32,
    pub tokens: f64,
    pub last_refill_ns: u64,
}

impl TokenBucket {
    pub fn new(rpm: u32, now_ns: u64) -> Self {
        Self {
            rpm,
            tokens: rpm as f64,
            last_refill_ns: now_ns,
        }
    }

    pub fn unmetered() -> Self {
        Self {
            rpm: 0,
            tokens: f64::MAX,
            last_refill_ns: 0,
        }
    }

    pub fn try_acquire(&mut self, now_ns: u64) -> bool {
        if self.rpm == 0 {
            return true;
        }
        self.refill(now_ns);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    fn refill(&mut self, now_ns: u64) {
        if self.rpm == 0 || now_ns <= self.last_refill_ns {
            return;
        }
        let elapsed_ns = now_ns.saturating_sub(self.last_refill_ns);
        let refill_rate_per_ns = (self.rpm as f64) / (NANOS_PER_MIN as f64);
        let added_tokens = (elapsed_ns as f64) * refill_rate_per_ns;
        self.tokens = (self.tokens + added_tokens).min(self.rpm as f64);
        self.last_refill_ns = now_ns;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CircuitState {
    Closed,
    OpenUntil(u64),
}

#[derive(Debug, Clone)]
pub struct CircuitBreaker {
    pub state: CircuitState,
    pub consecutive_failures: u32,
    pub failure_threshold: u32,
    pub cooldown_ns: u64,
}

impl CircuitBreaker {
    pub fn new(failure_threshold: u32, cooldown_secs: u64) -> Self {
        Self {
            state: CircuitState::Closed,
            consecutive_failures: 0,
            failure_threshold: failure_threshold.max(1),
            cooldown_ns: cooldown_secs * NANOS_PER_SEC,
        }
    }

    pub fn can_attempt(&mut self, now_ns: u64) -> bool {
        match self.state {
            CircuitState::Closed => true,
            CircuitState::OpenUntil(until) => {
                if now_ns >= until {
                    self.state = CircuitState::Closed;
                    self.consecutive_failures = 0;
                    true
                } else {
                    false
                }
            }
        }
    }

    pub fn record_success(&mut self) {
        self.state = CircuitState::Closed;
        self.consecutive_failures = 0;
    }

    pub fn record_failure(&mut self, now_ns: u64) {
        self.consecutive_failures += 1;
        if self.consecutive_failures >= self.failure_threshold {
            self.state = CircuitState::OpenUntil(now_ns + self.cooldown_ns);
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub id: String,
    pub tier: u8, // 1 = Free, 2 = Sub-cent, 3 = Frontier
    pub base_url: String,
    pub default_model: String,
    pub secret_key_name: Option<String>,
    pub bucket: TokenBucket,
    pub breaker: CircuitBreaker,
}

#[derive(Debug, Clone)]
pub struct RouterPool {
    pub providers: Vec<ProviderConfig>,
    pub rr_counter: usize,
}

impl RouterPool {
    pub fn new(providers: Vec<ProviderConfig>) -> Self {
        Self {
            providers,
            rr_counter: 0,
        }
    }

    /// Selects healthy candidates in order for a given tier.
    /// Rotates starting position using round-robin to evenly spread load.
    pub fn candidates_for_tier(&mut self, tier: u8, now_ns: u64) -> Vec<usize> {
        let matching_indices: Vec<usize> = self
            .providers
            .iter()
            .enumerate()
            .filter(|(_, p)| p.tier == tier)
            .map(|(i, _)| i)
            .collect();

        if matching_indices.is_empty() {
            return Vec::new();
        }

        let n = matching_indices.len();
        let offset = self.rr_counter % n;
        self.rr_counter = self.rr_counter.wrapping_add(1);

        let mut available = Vec::new();
        for i in 0..n {
            let idx = matching_indices[(offset + i) % n];
            let p = &mut self.providers[idx];
            if p.breaker.can_attempt(now_ns) && p.bucket.try_acquire(now_ns) {
                available.push(idx);
            }
        }
        available
    }

    /// Complete plan: candidates from Tier 1 (Free), then Tier 2, then Tier 3.
    pub fn plan_invocation(&mut self, now_ns: u64) -> Vec<usize> {
        let mut plan = Vec::new();
        // 1. All available in Tier 1
        plan.extend(self.candidates_for_tier(1, now_ns));
        // 2. Fallbacks in Tier 2
        plan.extend(self.candidates_for_tier(2, now_ns));
        // 3. Last resort in Tier 3
        plan.extend(self.candidates_for_tier(3, now_ns));
        plan
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_bucket_drains_and_refills() {
        let mut b = TokenBucket::new(15, 0); // 15 RPM
        // Can acquire 15 tokens immediately
        for _ in 0..15 {
            assert!(b.try_acquire(0));
        }
        // 16th fails at time 0
        assert!(!b.try_acquire(0));

        // After 4 seconds (1/15th of 60s), 1 token should refill
        let four_secs_ns = 4 * NANOS_PER_SEC;
        assert!(b.try_acquire(four_secs_ns));
        assert!(!b.try_acquire(four_secs_ns));
    }

    #[test]
    fn circuit_breaker_trips_and_recovers() {
        let mut cb = CircuitBreaker::new(2, 60); // 2 failures -> 60s cooldown
        assert!(cb.can_attempt(0));

        // Failure 1
        cb.record_failure(0);
        assert!(cb.can_attempt(0));

        // Failure 2 -> Trips circuit
        cb.record_failure(0);
        assert!(!cb.can_attempt(10 * NANOS_PER_SEC));
        assert!(!cb.can_attempt(59 * NANOS_PER_SEC));

        // After cooldown (60s), allows probe
        assert!(cb.can_attempt(60 * NANOS_PER_SEC));
        cb.record_success();
        assert!(cb.can_attempt(61 * NANOS_PER_SEC));
    }

    #[test]
    fn router_pool_bubbles_within_tier_and_escalates() {
        let now = 1_000_000_000;
        let p_gemini = ProviderConfig {
            id: "gemini".into(),
            tier: 1,
            base_url: "https://generativelanguage.googleapis.com".into(),
            default_model: "gemini-2.5-flash".into(),
            secret_key_name: Some("gemini-api-key".into()),
            bucket: TokenBucket::new(15, now),
            breaker: CircuitBreaker::new(2, 60),
        };
        let p_groq = ProviderConfig {
            id: "groq".into(),
            tier: 1,
            base_url: "https://api.groq.com".into(),
            default_model: "llama-3.3-70b-versatile".into(),
            secret_key_name: Some("groq-api-key".into()),
            bucket: TokenBucket::new(30, now),
            breaker: CircuitBreaker::new(2, 60),
        };
        let p_deepseek = ProviderConfig {
            id: "deepseek".into(),
            tier: 2,
            base_url: "https://api.deepseek.com".into(),
            default_model: "deepseek-chat".into(),
            secret_key_name: Some("deepseek-api-key".into()),
            bucket: TokenBucket::unmetered(),
            breaker: CircuitBreaker::new(3, 60),
        };

        let mut pool = RouterPool::new(vec![p_gemini, p_groq, p_deepseek]);

        // Initial plan includes Tier 1 providers first, then Tier 2
        let plan = pool.plan_invocation(now);
        assert_eq!(plan.len(), 3);
        assert!(plan.contains(&0)); // gemini
        assert!(plan.contains(&1)); // groq
        assert_eq!(plan[2], 2);     // deepseek is last

        // If both Tier 1 providers trip (circuit open):
        pool.providers[0].breaker.record_failure(now);
        pool.providers[0].breaker.record_failure(now);
        pool.providers[1].breaker.record_failure(now);
        pool.providers[1].breaker.record_failure(now);

        // Next plan escalates directly to Tier 2 (DeepSeek)
        let fallback_plan = pool.plan_invocation(now);
        assert_eq!(fallback_plan, vec![2]);
    }
}
