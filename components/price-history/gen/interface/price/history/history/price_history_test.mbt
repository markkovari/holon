let day_sec : UInt64 = 86400UL
let eur : String = "EUR"

fn q(unit : Int64, day : UInt64, kind : @history.QuoteKind, source : String) -> @history.Quote {
  { unit_minor: unit, currency: eur, kind, source, at: day * day_sec }
}

fn market(unit : Int64, day : UInt64) -> @history.Quote {
  q(unit, day, MARKET, "fixture")
}

test "a price is the latest quote at or before the instant" {
  let quotes = [market(4000L, 10UL), market(9000L, 30UL)]
  match @history.at(quotes, MARKET, 20UL * day_sec) {
    Ok(obs) => {
      assert_eq(obs.unit_minor, 4000L)
      assert_eq(obs.observed_at, 10UL * day_sec)
      assert_eq(obs.age_seconds, 10UL * day_sec)
      assert_eq(obs.carried, true)
    }
    Err(_) => fail("expected priced")
  }
}

test "a quote at the exact instant is neither carried nor stale" {
  match @history.at([market(4000L, 10UL)], MARKET, 10UL * day_sec) {
    Ok(obs) => {
      assert_eq(obs.age_seconds, 0UL)
      assert_eq(obs.carried, false)
    }
    Err(_) => fail("expected priced")
  }
}

test "before the first quote there is no price" {
  match @history.at([market(4000L, 10UL)], MARKET, 5UL * day_sec) {
    Err(NotYetPriced) => ()
    _ => fail("expected NotYetPriced")
  }
}

test "kinds do not mix" {
  let quotes = [market(4000L, 10UL), q(1200L, 20UL, LOW, "fixture")]
  match @history.at(quotes, MARKET, 25UL * day_sec) {
    Ok(obs) => assert_eq(obs.unit_minor, 4000L)
    Err(_) => fail("expected priced")
  }
  match @history.at(quotes, LOW, 25UL * day_sec) {
    Ok(low) => assert_eq(low.unit_minor, 1200L)
    Err(_) => fail("expected low priced")
  }
  match @history.at(quotes, LAST_SOLD, 25UL * day_sec) {
    Err(NotYetPriced) => ()
    _ => fail("expected NotYetPriced for LAST_SOLD")
  }
}

test "the newest observation wins and ties are broken deterministically" {
  let forward = [
    q(4000L, 10UL, MARKET, "alpha"),
    q(5000L, 11UL, MARKET, "beta"),
  ]
  let reversed = [
    q(5000L, 11UL, MARKET, "beta"),
    q(4000L, 10UL, MARKET, "alpha"),
  ]
  let a = match @history.at(forward, MARKET, 20UL * day_sec) {
    Ok(obs) => obs
    Err(_) => fail("expected a")
  }
  let b = match @history.at(reversed, MARKET, 20UL * day_sec) {
    Ok(obs) => obs
    Err(_) => fail("expected b")
  }
  assert_eq(a.unit_minor, b.unit_minor)
  assert_eq(a.unit_minor, 5000L)

  let tied = [
    q(7000L, 10UL, MARKET, "zulu"),
    q(4000L, 10UL, MARKET, "alpha"),
  ]
  match @history.at(tied, MARKET, 10UL * day_sec) {
    Ok(obs) => {
      assert_eq(obs.source, "alpha")
      assert_eq(obs.unit_minor, 4000L)
    }
    Err(_) => fail("expected tied priced")
  }
}

test "two currencies are refused" {
  let usd : @history.Quote = {
    unit_minor: 4000L,
    currency: "USD",
    kind: MARKET,
    source: "fixture",
    at: 20UL * day_sec,
  }
  match @history.at([market(4000L, 10UL), usd], MARKET, 30UL * day_sec) {
    Err(MixedCurrency((expected, found))) => {
      assert_eq(expected, "EUR")
      assert_eq(found, "USD")
    }
    _ => fail("expected MixedCurrency")
  }
}

test "a gap carries forward and never interpolates" {
  let quotes = [market(4000L, 0UL), market(9000L, 4UL)]
  match @history.series(quotes, MARKET, 0UL, 4UL * day_sec, day_sec) {
    Ok(points) => {
      assert_eq(points.length(), 5)
      assert_eq(points[0].unit_minor, 4000L)
      assert_eq(points[0].carried, false)
      assert_eq(points[1].unit_minor, 4000L)
      assert_eq(points[1].carried, true)
      assert_eq(points[2].unit_minor, 4000L)
      assert_eq(points[2].carried, true)
      assert_eq(points[3].unit_minor, 4000L)
      assert_eq(points[3].carried, true)
      assert_eq(points[4].unit_minor, 9000L)
      assert_eq(points[4].carried, false)
    }
    Err(_) => fail("expected series")
  }
}

test "samples before first quote are absent" {
  match @history.series([market(4000L, 3UL)], MARKET, 0UL, 5UL * day_sec, day_sec) {
    Ok(points) => {
      assert_eq(points.length(), 3)
      assert_eq(points[0].at, 3UL * day_sec)
      assert_eq(points[1].at, 4UL * day_sec)
      assert_eq(points[2].at, 5UL * day_sec)
    }
    Err(_) => fail("expected series")
  }
}

test "the series always ends at until and zero step is refused" {
  match @history.series([market(4000L, 0UL)], MARKET, 0UL, 10UL * day_sec, 3UL * day_sec) {
    Ok(points) => {
      assert_eq(points.length(), 5)
      assert_eq(points[0].at, 0UL)
      assert_eq(points[1].at, 3UL * day_sec)
      assert_eq(points[2].at, 6UL * day_sec)
      assert_eq(points[3].at, 9UL * day_sec)
      assert_eq(points[4].at, 10UL * day_sec)
    }
    Err(_) => fail("expected series")
  }
  match @history.series([market(4000L, 0UL)], MARKET, 0UL, day_sec, 0UL) {
    Err(ZeroStep) => ()
    _ => fail("expected ZeroStep")
  }
}
