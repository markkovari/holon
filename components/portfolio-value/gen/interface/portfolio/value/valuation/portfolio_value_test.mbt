let eur : String = "EUR"
let day_sec : UInt64 = 86400UL

fn buy(card : String, qty : UInt, unit : Int64, at : UInt64) -> @valuation.Event {
  {
    item_id: card,
    kind: ACQUIRED,
    quantity: qty,
    unit_minor: unit,
    currency: eur,
    at,
  }
}

fn sell(card : String, qty : UInt, unit : Int64, at : UInt64) -> @valuation.Event {
  {
    item_id: card,
    kind: DISPOSED,
    quantity: qty,
    unit_minor: unit,
    currency: eur,
    at,
  }
}

fn quote(card : String, unit : Int64, at : UInt64) -> @valuation.Quote {
  { item_id: card, unit_minor: unit, currency: eur, at }
}

test "a sale consumes the oldest lot first" {
  let events = [
    buy("base-4", 2U, 1000L, 100UL),
    buy("base-4", 1U, 4000L, 200UL),
    sell("base-4", 1U, 3000L, 300UL),
  ]
  match @valuation.value_at(events, [], 400UL) {
    Ok(v) => {
      assert_eq(v.realised_minor, 2000L) // sold 10.00 for 30.00
      assert_eq(v.cost_basis_minor, 5000L) // one 10.00 and one 40.00 still held
    }
    Err(_) => fail("valuation failed")
  }
}

test "a holding is valued at the latest quote at or before the instant" {
  let events = [buy("base-4", 1U, 1000L, 0UL)]
  let quotes = [quote("base-4", 5000L, 10UL * day_sec), quote("base-4", 9000L, 30UL * day_sec)]

  match @valuation.value_at(events, quotes, 5UL * day_sec) {
    Ok(early) => {
      assert_eq(early.market_value_minor, 1000L)
      assert_eq(early.unquoted, 1U)
    }
    Err(_) => fail("early failed")
  }

  match @valuation.value_at(events, quotes, 20UL * day_sec) {
    Ok(mid) => {
      assert_eq(mid.market_value_minor, 5000L)
      assert_eq(mid.unquoted, 0U)
      assert_eq(mid.unrealised_minor, 4000L)
    }
    Err(_) => fail("mid failed")
  }

  match @valuation.value_at(events, quotes, 40UL * day_sec) {
    Ok(late) => assert_eq(late.market_value_minor, 9000L)
    Err(_) => fail("late failed")
  }
}

test "unpriced holdings are carried at cost and counted" {
  let events = [buy("priced", 1U, 1000L, 0UL), buy("bulk-common", 40U, 5L, 0UL)]
  let quotes = [quote("priced", 8000L, 0UL)]
  match @valuation.value_at(events, quotes, day_sec) {
    Ok(v) => {
      assert_eq(v.unquoted, 40U)
      assert_eq(v.cost_basis_minor, 1200L)
      assert_eq(v.market_value_minor, 8200L)
    }
    Err(_) => fail("valuation failed")
  }
}

test "events out of order give the same answer" {
  let ordered = [
    buy("base-4", 2U, 1000L, 100UL),
    buy("base-4", 1U, 4000L, 200UL),
    sell("base-4", 1U, 3000L, 300UL),
  ]
  let shuffled = [
    ordered[2],
    ordered[0],
    ordered[1],
  ]
  let a = match @valuation.value_at(ordered, [], 400UL) {
    Ok(v) => v
    Err(_) => fail("ordered failed")
  }
  let b = match @valuation.value_at(shuffled, [], 400UL) {
    Ok(v) => v
    Err(_) => fail("shuffled failed")
  }
  assert_eq(a.realised_minor, b.realised_minor)
  assert_eq(a.cost_basis_minor, b.cost_basis_minor)
  assert_eq(a.market_value_minor, b.market_value_minor)
}

test "two currencies are refused" {
  let usd : @valuation.Event = {
    item_id: "base-4",
    kind: ACQUIRED,
    quantity: 1U,
    unit_minor: 1000L,
    currency: "USD",
    at: 200UL,
  }
  let events = [buy("base-4", 1U, 1000L, 100UL), usd]
  match @valuation.value_at(events, [], 300UL) {
    Err(MixedCurrency((expected, found))) => {
      assert_eq(expected, "EUR")
      assert_eq(found, "USD")
    }
    _ => fail("expected MixedCurrency")
  }
}

test "selling more than is held is refused" {
  let events = [buy("base-4", 1U, 1000L, 100UL), sell("base-4", 2U, 3000L, 200UL)]
  match @valuation.value_at(events, [], 300UL) {
    Err(OversoldAt((item, at, held, disposed))) => {
      assert_eq(item, "base-4")
      assert_eq(at, 200UL)
      assert_eq(held, 1U)
      assert_eq(disposed, 2U)
    }
    _ => fail("expected OversoldAt")
  }
}

test "zero quantity event is refused" {
  let events = [buy("base-4", 0U, 1000L, 100UL)]
  match @valuation.value_at(events, [], 200UL) {
    Err(ZeroQuantity(_)) => ()
    _ => fail("expected ZeroQuantity")
  }
}

test "the series samples valuation at each step and ends at until" {
  let events = [buy("base-4", 1U, 1000L, 0UL), buy("base-4", 1U, 2000L, 2UL * day_sec)]
  let quotes = [quote("base-4", 3000L, 0UL)]
  match @valuation.series(events, quotes, 0UL, 3UL * day_sec, day_sec) {
    Ok(points) => {
      assert_eq(points.length(), 4)
      assert_eq(points[0].at, 0UL)
      assert_eq(points[0].cost_basis_minor, 1000L)
      assert_eq(points[1].cost_basis_minor, 1000L)
      assert_eq(points[2].cost_basis_minor, 3000L)
      assert_eq(points[2].market_value_minor, 6000L)
      assert_eq(points[3].at, 3UL * day_sec)
    }
    Err(_) => fail("series failed")
  }

  match @valuation.series(events, [], 0UL, 10UL * day_sec, 3UL * day_sec) {
    Ok(points) => {
      assert_eq(points.length(), 5)
      assert_eq(points[0].at, 0UL)
      assert_eq(points[1].at, 3UL * day_sec)
      assert_eq(points[2].at, 6UL * day_sec)
      assert_eq(points[3].at, 9UL * day_sec)
      assert_eq(points[4].at, 10UL * day_sec)
    }
    Err(_) => fail("series failed")
  }

  match @valuation.series(events, [], 0UL, day_sec, 0UL) {
    Err(ZeroStep) => ()
    _ => fail("expected ZeroStep")
  }
}

test "empty event log has no answer" {
  match @valuation.value_at([], [], 100UL) {
    Err(Empty) => ()
    _ => fail("expected Empty")
  }
}
