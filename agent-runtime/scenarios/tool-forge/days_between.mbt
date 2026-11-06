///|
fn days_from_civil(y : Int, m : Int, d : Int) -> Int {
  let y = if m <= 2 { y - 1 } else { y }
  let era = (if y >= 0 { y } else { y - 399 }) / 400
  let yoe = y - era * 400
  let mp = (m + 9) % 12
  let doy = (153 * mp + 2) / 5 + d - 1
  let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy
  era * 146097 + doe - 719468
}

///|
fn digits(s : String, from : Int, to : Int) -> Int? {
  let mut n = 0
  for i in from..<to {
    let c = s.at(i).to_int() - 48
    if c < 0 || c > 9 {
      return None
    }
    n = n * 10 + c
  }
  Some(n)
}

///|
fn date(s : String) -> Int? {
  if s.length() != 10 || s.at(4).to_int() != 45 || s.at(7).to_int() != 45 {
    return None
  }
  guard digits(s, 0, 4) is Some(y) else { return None }
  guard digits(s, 5, 7) is Some(m) else { return None }
  guard digits(s, 8, 10) is Some(d) else { return None }
  if m < 1 || m > 12 || d < 1 || d > 31 {
    return None
  }
  Some(days_from_civil(y, m, d))
}

///|
fn weekday(z : Int) -> String {
  let names = [
    "Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday",
  ]
  names[(z % 7 + 11) % 7]
}

///|
pub fn call(args : Json) -> Result[Json, String] {
  guard args is { "from": String(from), "to": String(to), .. } else {
    return Err("from and to are required, as YYYY-MM-DD")
  }
  guard date(from) is Some(a) else { return Err("from must be YYYY-MM-DD") }
  guard date(to) is Some(b) else { return Err("to must be YYYY-MM-DD") }
  Ok({
    "days": Json::number((b - a).to_double()),
    "from_weekday": Json::string(weekday(a)),
    "to_weekday": Json::string(weekday(b)),
  })
}
