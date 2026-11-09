test "civil_roundtrip_epoch" {
  let (y0, m0, d0) = @parser.civil_from_days(0L)
  assert_eq(y0, 1970L)
  assert_eq(m0, 1U)
  assert_eq(d0, 1U)
  assert_eq(@parser.weekday(0L), 4U)

  let (y1, m1, d1) = @parser.civil_from_days(18628L)
  assert_eq(y1, 2021L)
  assert_eq(m1, 1U)
  assert_eq(d1, 1U)
  assert_eq(@parser.weekday(18628L), 5U)
}

test "parse_normalizes_macros_and_names" {
  match @parser.parse("@hourly") {
    Ok(s) => assert_eq(s, "0 * * * *")
    Err(_) => fail("failed to parse @hourly")
  }
  match @parser.parse("@daily") {
    Ok(s) => assert_eq(s, "0 0 * * *")
    Err(_) => fail("failed to parse @daily")
  }
  match @parser.parse("0 0 * jan mon") {
    Ok(s) => assert_eq(s, "0 0 * 1 1")
    Err(_) => fail("failed to parse 0 0 * jan mon")
  }
  match @parser.parse("*/15 * * * *") {
    Ok(s) => assert_eq(s, "0,15,30,45 * * * *")
    Err(_) => fail("failed to parse */15 * * * *")
  }
}

test "every_six_hours" {
  // 2021-01-01 00:00:00 UTC = 1609459200UL
  let t0 = 1609459200UL
  match @parser.next("0 */6 * * *", t0, 4U) {
    Ok(got) => {
      assert_eq(got.length(), 4)
      assert_eq(got[0], t0 + 6UL * 3600UL)
      assert_eq(got[1], t0 + 12UL * 3600UL)
      assert_eq(got[2], t0 + 18UL * 3600UL)
      assert_eq(got[3], t0 + 24UL * 3600UL)
    }
    Err(_) => fail("failed next for every 6 hours")
  }
}

test "matches_and_dom_dow_or_rule" {
  // 2021-01-04 09:30 UTC was a Monday = 1609752600UL
  let mon_0930 = 1609752600UL
  match @parser.matches("30 9 * * mon", mon_0930) {
    Ok(b) => assert_eq(b, true)
    Err(_) => fail("failed matches")
  }
  match @parser.matches("30 9 * * mon", mon_0930 + 86400UL) {
    Ok(b) => assert_eq(b, false)
    Err(_) => fail("failed matches")
  }
  // Check day-of-month and day-of-week OR logic:
  // Friday 2021-01-01 00:00:00 matches both 1st of month and Friday
  match @parser.matches("0 0 1 * fri", 1609459200UL) {
    Ok(b) => assert_eq(b, true)
    Err(_) => fail("failed matches 1st and fri")
  }
}

test "leap_day_yearly" {
  match @parser.next("0 0 29 2 *", 1609459200UL, 1U) {
    Ok(got) => {
      assert_eq(got.length(), 1)
      assert_eq(got[0], 1709164800UL) // 2024-02-29 00:00:00 UTC
    }
    Err(_) => fail("failed leap_day_yearly")
  }
}

test "errors" {
  assert_eq(@parser.parse("* * *") is Err(_), true)
  assert_eq(@parser.parse("60 * * * *") is Err(_), true)
  assert_eq(@parser.parse("* * * * xyz") is Err(_), true)
  assert_eq(@parser.parse("*/0 * * * *") is Err(_), true)
}
