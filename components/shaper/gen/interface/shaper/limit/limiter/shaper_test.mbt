test "token bucket full initialization and consumption" {
  let initial : @limiter.Bucket = { tokens: 0.0, updated_ms: 0UL }
  let (d1, b1) = @limiter.token_bucket(initial, 1000UL, 10.0, 1.0, 4.0)
  assert_eq(d1.allowed, true)
  assert_eq(d1.retry_after_ms, 0UL)
  assert_eq(b1.tokens, 6.0)
  assert_eq(b1.updated_ms, 1000UL)

  // Spend again exceeding capacity
  let (d2, b2) = @limiter.token_bucket(b1, 1000UL, 10.0, 1.0, 8.0)
  assert_eq(d2.allowed, false)
  assert_eq(d2.retry_after_ms, 2000UL) // needs 2 tokens at 1/s
  assert_eq(b2.tokens, 6.0)

  // Refill after 3 seconds
  let (d3, b3) = @limiter.token_bucket(b2, 4000UL, 10.0, 1.0, 8.0)
  assert_eq(d3.allowed, true)
  assert_eq(d3.retry_after_ms, 0UL)
  assert_eq(b3.tokens, 1.0)
}

test "gcra burst and rate limit" {
  // period 100ms, burst 2, cost 1
  let (d1, tat1) = @limiter.gcra(0UL, 1000UL, 100UL, 2U, 1U)
  assert_eq(d1.allowed, true)
  assert_eq(tat1, 1100UL)

  let (d2, tat2) = @limiter.gcra(tat1, 1000UL, 100UL, 2U, 1U)
  assert_eq(d2.allowed, true)
  assert_eq(tat2, 1200UL)

  let (d3, tat3) = @limiter.gcra(tat2, 1000UL, 100UL, 2U, 1U)
  assert_eq(d3.allowed, true)
  assert_eq(tat3, 1300UL)

  // 4th request exceeds burst limit (1300 - 2*100 = 1100 > 1000)
  let (d4, tat4) = @limiter.gcra(tat3, 1000UL, 100UL, 2U, 1U)
  assert_eq(d4.allowed, false)
  assert_eq(d4.retry_after_ms, 100UL)
  assert_eq(tat4, 1300UL)
}
