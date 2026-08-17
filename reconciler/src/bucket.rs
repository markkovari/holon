pub struct TokenBucket {
    capacity: u64,
    refill_per_sec: u64,
    tokens: u64,
    last_update_ms: u64,
}

impl TokenBucket {
    /// Create a new token bucket that starts full.
    pub fn new(capacity: u64, refill_per_sec: u64) -> Self {
        TokenBucket {
            capacity,
            refill_per_sec,
            tokens: capacity,
            last_update_ms: 0,
        }
    }

    /// Return current tokens plus refill since last update, capped at capacity.
    /// Does not consume tokens or advance the clock.
    pub fn available(&self, now_ms: u64) -> u64 {
        let elapsed_ms = now_ms.saturating_sub(self.last_update_ms);
        let refill = (self.refill_per_sec * elapsed_ms) / 1000;
        let total = self.tokens.saturating_add(refill);
        total.min(self.capacity)
    }

    /// Refill and advance clock to now_ms, then take n tokens if available.
    /// Returns true if n tokens were taken, false otherwise.
    pub fn try_take(&mut self, now_ms: u64, n: u64) -> bool {
        let elapsed_ms = now_ms.saturating_sub(self.last_update_ms);
        let refill = (self.refill_per_sec * elapsed_ms) / 1000;
        self.tokens = self.tokens.saturating_add(refill).min(self.capacity);
        self.last_update_ms = now_ms;

        if self.tokens >= n {
            self.tokens -= n;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_starts_full() {
        let bucket = TokenBucket::new(100, 10);
        assert_eq!(bucket.available(0), 100);
    }

    #[test]
    fn test_available_without_consuming() {
        let bucket = TokenBucket::new(100, 10);
        assert_eq!(bucket.available(0), 100);
        assert_eq!(bucket.available(0), 100); // Still 100, no consumption
    }

    #[test]
    fn test_try_take_success() {
        let mut bucket = TokenBucket::new(100, 10);
        assert!(bucket.try_take(0, 50));
        assert_eq!(bucket.available(0), 50);
    }

    #[test]
    fn test_try_take_failure() {
        let mut bucket = TokenBucket::new(100, 10);
        assert!(!bucket.try_take(0, 150));
        assert_eq!(bucket.available(0), 100); // Unchanged
    }

    #[test]
    fn test_refill_calculation() {
        let mut bucket = TokenBucket::new(100, 10);
        bucket.try_take(0, 50); // Now at 50 tokens
        assert_eq!(bucket.available(1000), 60); // 50 + (10 * 1000 / 1000) = 60
    }

    #[test]
    fn test_refill_capped_at_capacity() {
        let bucket = TokenBucket::new(100, 10);
        assert_eq!(bucket.available(10000), 100); // Would be 200, but capped at 100
    }

    #[test]
    fn test_time_backwards_no_refill() {
        let mut bucket = TokenBucket::new(100, 10);
        bucket.try_take(1000, 50); // Now at 50 tokens, last_update = 1000
        assert_eq!(bucket.available(500), 50); // Time went backwards, no refill
    }

    #[test]
    fn test_refill_floor_division() {
        let mut bucket = TokenBucket::new(100, 10);
        bucket.try_take(0, 100); // Now at 0 tokens
        assert_eq!(bucket.available(500), 5); // (10 * 500 / 1000) = 5 (floored)
    }

    #[test]
    fn test_multiple_operations() {
        let mut bucket = TokenBucket::new(100, 20);
        assert!(bucket.try_take(0, 30));
        assert_eq!(bucket.available(500), 80); // 70 + (20 * 500 / 1000) = 80
        assert!(bucket.try_take(500, 80));
        assert_eq!(bucket.available(500), 0);
        assert!(!bucket.try_take(500, 1));
        assert_eq!(bucket.available(1500), 20); // (20 * 1000 / 1000) = 20
    }
}