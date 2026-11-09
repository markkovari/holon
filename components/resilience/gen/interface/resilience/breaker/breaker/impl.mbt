///|
fn at_least_1(n : UInt) -> UInt {
  if n < 1U { 1U } else { n }
}

///|
fn mix(x : UInt64) -> UInt64 {
  let mut v = x + 0x9E3779B97F4A7C15UL
  v = (v ^ (v >> 30)) * 0xBF58476D1CE4E5B9UL
  v = (v ^ (v >> 27)) * 0x94D049BB133111EBUL
  v ^ (v >> 31)
}

///|
fn saturating_sub(a : UInt64, b : UInt64) -> UInt64 {
  if a >= b { a - b } else { 0UL }
}

///|
fn saturating_sub_u32(a : UInt, b : UInt) -> UInt {
  if a >= b { a - b } else { 0U }
}

///|
pub fn admit(state : Circuit, now_ms : UInt64, pol : Policy) -> (Admission, Circuit) {
  let mut c = state
  match c.state {
    CLOSED => {
      if pol.window_ms > 0UL && saturating_sub(now_ms, c.window_start_ms) >= pol.window_ms {
        c = { ..c, failures: 0U, window_start_ms: now_ms }
      }
      ({ admit: true, state: CLOSED, retry_after_ms: 0UL }, c)
    }
    OPEN => {
      let elapsed = saturating_sub(now_ms, c.changed_ms)
      if elapsed >= pol.open_ms {
        c = {
          ..c,
          state: HALF_OPEN,
          changed_ms: now_ms,
          successes: 0U,
          probes: 1U,
        }
        ({ admit: true, state: HALF_OPEN, retry_after_ms: 0UL }, c)
      } else {
        ({
          admit: false,
          state: OPEN,
          retry_after_ms: pol.open_ms - elapsed,
        }, c)
      }
    }
    HALF_OPEN => {
      if c.probes < at_least_1(pol.half_open_probes) {
        c = { ..c, probes: c.probes + 1U }
        ({ admit: true, state: HALF_OPEN, retry_after_ms: 0UL }, c)
      } else {
        ({ admit: false, state: HALF_OPEN, retry_after_ms: 0UL }, c)
      }
    }
  }
}

///|
pub fn observe(state : Circuit, now_ms : UInt64, pol : Policy, ok : Bool) -> Circuit {
  let mut c = state
  match (c.state, ok) {
    (CLOSED, true) => {
      c = { ..c, failures: 0U, window_start_ms: now_ms }
    }
    (CLOSED, false) => {
      if pol.window_ms > 0UL && saturating_sub(now_ms, c.window_start_ms) >= pol.window_ms {
        c = { ..c, failures: 0U, window_start_ms: now_ms }
      }
      if c.failures == 0U {
        c = { ..c, window_start_ms: now_ms }
      }
      let new_failures = c.failures + 1U
      if new_failures >= at_least_1(pol.failure_threshold) {
        c = {
          ..c,
          failures: new_failures,
          state: OPEN,
          changed_ms: now_ms,
          probes: 0U,
          successes: 0U,
        }
      } else {
        c = { ..c, failures: new_failures }
      }
    }
    (HALF_OPEN, true) => {
      let new_probes = saturating_sub_u32(c.probes, 1U)
      let new_successes = c.successes + 1U
      if new_successes >= at_least_1(pol.success_threshold) {
        c = {
          ..c,
          state: CLOSED,
          failures: 0U,
          successes: 0U,
          probes: 0U,
          window_start_ms: now_ms,
          changed_ms: now_ms,
        }
      } else {
        c = { ..c, probes: new_probes, successes: new_successes }
      }
    }
    (HALF_OPEN, false) => {
      c = {
        ..c,
        state: OPEN,
        changed_ms: now_ms,
        failures: at_least_1(pol.failure_threshold),
        successes: 0U,
        probes: 0U,
      }
    }
    (OPEN, _) => ()
  }
  c
}

///|
pub fn backoff(attempt : UInt, pol : RetryPolicy, seed : UInt64) -> UInt? {
  if attempt > at_least_1(pol.max_attempts) {
    return None
  }
  if attempt <= 1U {
    return Some(0U)
  }
  let cap : UInt64 = if pol.max_ms == 0U { 0xFFFFFFFFUL } else { pol.max_ms.to_uint64() }
  let base : UInt64 = pol.base_ms.to_uint64()
  let factor : UInt64 = pol.factor_pct.to_uint64()
  let mut delay = base
  let count = attempt.reinterpret_as_int() - 2
  for i = 0; i < count; i = i + 1 {
    if delay > 0xFFFFFFFFFFFFFFFFUL / factor {
      delay = 0xFFFFFFFFFFFFFFFFUL
    } else {
      delay = delay * factor / 100UL
    }
    if delay >= cap {
      delay = cap
      break
    }
  }
  let base_capped = if base < cap { base } else { cap }
  if delay > cap {
    delay = cap
  }
  if delay < base_capped {
    delay = base_capped
  }
  if !pol.jitter || delay <= base {
    return Some(delay.to_uint())
  }
  let span = delay - base + 1UL
  let mixed = mix(seed ^ (attempt.to_uint64() * 0x9E3779B97F4A7C15UL))
  let result = base + (mixed % span)
  Some(result.to_uint())
}
