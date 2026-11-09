fn pol() -> @breaker.Policy {
  {
    failure_threshold: 3U,
    window_ms: 10000UL,
    open_ms: 1000UL,
    half_open_probes: 1U,
    success_threshold: 2U,
  }
}

fn fresh() -> @breaker.Circuit {
  {
    state: @breaker.CircuitState::CLOSED,
    failures: 0U,
    successes: 0U,
    window_start_ms: 0UL,
    changed_ms: 0UL,
    probes: 0U,
  }
}

test "trips after threshold then fails fast" {
  let mut c = fresh()
  let p = pol()

  // 1st failure
  let (a1, next1) = @breaker.admit(c, 10UL, p)
  assert_eq(a1.admit, true)
  c = @breaker.observe(next1, 10UL, p, false)
  assert_eq(c.state.ordinal(), @breaker.CircuitState::CLOSED.ordinal())

  // 2nd failure
  let (a2, next2) = @breaker.admit(c, 20UL, p)
  assert_eq(a2.admit, true)
  c = @breaker.observe(next2, 20UL, p, false)
  assert_eq(c.state.ordinal(), @breaker.CircuitState::CLOSED.ordinal())

  // 3rd failure trips
  let (a3, next3) = @breaker.admit(c, 30UL, p)
  assert_eq(a3.admit, true)
  c = @breaker.observe(next3, 30UL, p, false)
  assert_eq(c.state.ordinal(), @breaker.CircuitState::OPEN.ordinal())

  // Fast fail at t=400 (cooldown 1000ms, elapsed 370ms -> retry_after 630ms)
  let (a4, _) = @breaker.admit(c, 400UL, p)
  assert_eq(a4.admit, false)
  assert_eq(a4.retry_after_ms, 630UL)
}

test "success clears a partial streak" {
  let mut c = fresh()
  let p = pol()
  c = @breaker.observe(c, 10UL, p, false)
  c = @breaker.observe(c, 20UL, p, false)
  assert_eq(c.failures, 2U)
  c = @breaker.observe(c, 30UL, p, true)
  assert_eq(c.failures, 0U)
  c = @breaker.observe(c, 40UL, p, false)
  c = @breaker.observe(c, 50UL, p, false)
  assert_eq(c.state.ordinal(), @breaker.CircuitState::CLOSED.ordinal())
}

test "half open probe recovers then closes" {
  let mut c = fresh()
  let p = pol()
  c = @breaker.observe(c, 10UL, p, false)
  c = @breaker.observe(c, 20UL, p, false)
  c = @breaker.observe(c, 30UL, p, false)
  assert_eq(c.state.ordinal(), @breaker.CircuitState::OPEN.ordinal())

  let (a, next) = @breaker.admit(c, 1030UL, p)
  assert_eq(a.admit, true)
  assert_eq(a.state.ordinal(), @breaker.CircuitState::HALF_OPEN.ordinal())
  c = next

  let (a2, next2) = @breaker.admit(c, 1031UL, p)
  assert_eq(a2.admit, false)
  c = next2

  // 1st success keeps it half-open
  c = @breaker.observe(c, 1040UL, p, true)
  assert_eq(c.state.ordinal(), @breaker.CircuitState::HALF_OPEN.ordinal())

  let (a3, next3) = @breaker.admit(c, 1050UL, p)
  assert_eq(a3.admit, true)

  // 2nd success closes it
  c = @breaker.observe(next3, 1060UL, p, true)
  assert_eq(c.state.ordinal(), @breaker.CircuitState::CLOSED.ordinal())
  assert_eq(c.failures, 0U)
}

test "backoff grows caps and stops" {
  let p : @breaker.RetryPolicy = {
    max_attempts: 4U,
    base_ms: 100U,
    factor_pct: 200U,
    max_ms: 500U,
    jitter: false,
  }
  assert_eq(@breaker.backoff(1U, p, 7UL), Some(0U))
  assert_eq(@breaker.backoff(2U, p, 7UL), Some(100U))
  assert_eq(@breaker.backoff(3U, p, 7UL), Some(200U))
  assert_eq(@breaker.backoff(4U, p, 7UL), Some(400U))
  assert_eq(@breaker.backoff(5U, p, 7UL), None)

  let capped : @breaker.RetryPolicy = { ..p, max_ms: 250U }
  assert_eq(@breaker.backoff(4U, capped, 7UL), Some(250U))
}

test "jitter stays in range and is deterministic" {
  let p : @breaker.RetryPolicy = {
    max_attempts: 6U,
    base_ms: 100U,
    factor_pct: 200U,
    max_ms: 10000U,
    jitter: true,
  }
  for seed = 0UL; seed < 50UL; seed = seed + 1UL {
    for attempt = 2U; attempt <= 6U; attempt = attempt + 1U {
      let d = match @breaker.backoff(attempt, p, seed) {
        Some(v) => v
        None => panic()
      }
      let exact = match @breaker.backoff(attempt, { ..p, jitter: false }, seed) {
        Some(v) => v
        None => panic()
      }
      assert_eq(d >= 100U && d <= exact, true)
    }
  }
  assert_eq(@breaker.backoff(4U, p, 42UL), @breaker.backoff(4U, p, 42UL))
}
