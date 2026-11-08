pub fn token_bucket(
  state : Bucket,
  now_ms : UInt64,
  capacity : Double,
  refill_per_sec : Double,
  cost : Double
) -> (Decision, Bucket) {
  let tokens = if state.updated_ms == 0UL {
    capacity
  } else {
    let elapsed_ms = if now_ms > state.updated_ms {
      now_ms - state.updated_ms
    } else {
      0UL
    }
    let elapsed_s = elapsed_ms.to_double() / 1000.0
    let refilled = state.tokens + elapsed_s * refill_per_sec
    if refilled < capacity { refilled } else { capacity }
  }

  if tokens + 1.0e-9 >= cost {
    let left = tokens - cost
    (
      { allowed: true, retry_after_ms: 0UL, remaining: left },
      { tokens: left, updated_ms: now_ms },
    )
  } else {
    let need = cost - tokens
    let retry : UInt64 = if refill_per_sec > 0.0 {
      let raw = need / refill_per_sec * 1000.0
      let truncated = raw.to_int64()
      let ceil_i = if raw - truncated.to_double() > 0.0 {
        truncated + 1L
      } else {
        truncated
      }
      ceil_i.reinterpret_as_uint64()
    } else {
      0xFFFF_FFFF_FFFF_FFFFUL
    }
    (
      { allowed: false, retry_after_ms: retry, remaining: tokens },
      { tokens, updated_ms: now_ms },
    )
  }
}

pub fn gcra(
  tat_ms : UInt64,
  now_ms : UInt64,
  period_ms : UInt64,
  burst : UInt,
  cost : UInt
) -> (Decision, UInt64) {
  let inc = cost.to_uint64() * period_ms
  let limit = burst.to_uint64() * period_ms
  let tat = if tat_ms == 0UL { now_ms } else { tat_ms }
  let allow_at = if tat > limit { tat - limit } else { 0UL }

  if now_ms >= allow_at {
    let base = if tat > now_ms { tat } else { now_ms }
    let new_tat = base + inc
    ({ allowed: true, retry_after_ms: 0UL, remaining: 0.0 }, new_tat)
  } else {
    let retry = allow_at - now_ms
    ({ allowed: false, retry_after_ms: retry, remaining: 0.0 }, tat)
  }
}
