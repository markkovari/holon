test "parses a 2-exponent currency into minor units" {
  match @arithmetic.parse("10.99", "USD") {
    Ok(a) => {
      assert_eq(a.units, 1099L)
      assert_eq(a.currency, "USD")
    }
    Err(_) => fail("expected Ok")
  }
}

test "formats minor units back to a decimal string" {
  let a = @arithmetic.parse("10.99", "USD").unwrap()
  assert_eq(@arithmetic.format(a).unwrap(), "10.99")
}

test "handles a 0-exponent currency (JPY)" {
  let jpy = @arithmetic.parse("1000", "JPY").unwrap()
  assert_eq(jpy.units, 1000L)
  assert_eq(@arithmetic.format(jpy).unwrap(), "1000")
}

test "adds two same-currency amounts" {
  let a = @arithmetic.parse("10.99", "USD").unwrap()
  let b = @arithmetic.parse("0.01", "USD").unwrap()
  let sum = @arithmetic.add(a, b).unwrap()
  assert_eq(sum.units, 1100L)
  assert_eq(@arithmetic.format(sum).unwrap(), "11.00")
}

test "subtracts two same-currency amounts" {
  let a = @arithmetic.parse("10.99", "USD").unwrap()
  let b = @arithmetic.parse("0.01", "USD").unwrap()
  let diff = @arithmetic.subtract(a, b).unwrap()
  assert_eq(diff.units, 1098L)
  assert_eq(@arithmetic.format(diff).unwrap(), "10.98")
}

test "scales an amount by an integer factor" {
  let a = @arithmetic.parse("10.99", "USD").unwrap()
  let scaled = @arithmetic.scale(a, 3L).unwrap()
  assert_eq(scaled.units, 3297L)
  assert_eq(@arithmetic.format(scaled).unwrap(), "32.97")
}

test "allocates a total across shares without losing pennies" {
  let total = @arithmetic.Amount::{ units: 1000L, currency: "USD" }
  let parts = @arithmetic.allocate(total, 3U).unwrap()
  assert_eq(parts.length(), 3)
  assert_eq(parts[0].units, 334L)
  assert_eq(parts[1].units, 333L)
  assert_eq(parts[2].units, 333L)
  let sum = parts[0].units + parts[1].units + parts[2].units
  assert_eq(sum, 1000L)
}

test "rejects adding mismatched currencies" {
  let a = @arithmetic.parse("10.99", "USD").unwrap()
  let b = @arithmetic.parse("1000", "JPY").unwrap()
  match @arithmetic.add(a, b) {
    Err(CurrencyMismatch) => ()
    _ => fail("expected CurrencyMismatch")
  }
}

test "rejects an unknown currency on parse" {
  match @arithmetic.parse("1.00", "ZZZ") {
    Err(UnknownCurrency("ZZZ")) => ()
    _ => fail("expected UnknownCurrency")
  }
}

test "orders amounts via compare" {
  let small = @arithmetic.parse("0.01", "USD").unwrap()
  let big = @arithmetic.parse("10.99", "USD").unwrap()
  assert_eq(@arithmetic.compare(small, big).unwrap(), -1)
  assert_eq(@arithmetic.compare(big, small).unwrap(), 1)
  assert_eq(
    @arithmetic.compare(big, @arithmetic.parse("10.99", "USD").unwrap()).unwrap(),
    0,
  )
}
