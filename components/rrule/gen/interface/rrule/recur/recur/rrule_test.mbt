test "daily recurrence" {
  let r : @recur.Rule = {
    frequency: @recur.DAILY,
    interval: 2U,
    by_weekday: [],
    count: 3U,
    until: "",
  }
  let res = @recur.expand("2024-01-01", r, "2024-01-01", "2024-01-10")
  match res {
    Ok(dates) => {
      assert_eq(dates.length(), 3)
      assert_eq(dates[0], "2024-01-01")
      assert_eq(dates[1], "2024-01-03")
      assert_eq(dates[2], "2024-01-05")
    }
    Err(_) => panic()
  }
}

test "weekly recurrence on specific days" {
  // 2024-01-01 was a Monday (weekday 0).
  // Repeat on Mon (0) and Wed (2).
  let r : @recur.Rule = {
    frequency: @recur.WEEKLY,
    interval: 1U,
    by_weekday: [b'\x00', b'\x02'],
    count: 4U,
    until: "",
  }
  let res = @recur.expand("2024-01-01", r, "2024-01-01", "2024-01-31")
  match res {
    Ok(dates) => {
      assert_eq(dates.length(), 4)
      assert_eq(dates[0], "2024-01-01") // Mon
      assert_eq(dates[1], "2024-01-03") // Wed
      assert_eq(dates[2], "2024-01-08") // Mon
      assert_eq(dates[3], "2024-01-10") // Wed
    }
    Err(_) => panic()
  }
}

test "invalid date returns bad date error" {
  let r : @recur.Rule = {
    frequency: @recur.DAILY,
    interval: 1U,
    by_weekday: [],
    count: 0U,
    until: "",
  }
  let res = @recur.expand("not-a-date", r, "2024-01-01", "2024-01-10")
  match res {
    Ok(_) => panic()
    Err(BadDate(s)) => assert_eq(s, "not-a-date")
    Err(Unsupported(_)) => panic()
  }
}
