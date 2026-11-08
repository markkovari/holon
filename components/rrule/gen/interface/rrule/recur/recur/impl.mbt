fn days_from_civil(y_in : Int64, m : Int64, d : Int64) -> Int64 {
  let y = if m <= 2L { y_in - 1L } else { y_in }
  let era = (if y >= 0L { y } else { y - 399L }) / 400L
  let yoe = y - era * 400L
  let mp = if m > 2L { m - 3L } else { m + 9L }
  let doy = (153L * mp + 2L) / 5L + d - 1L
  let doe = yoe * 365L + yoe / 4L - yoe / 100L + doy
  era * 146097L + doe - 719468L
}

fn civil_from_days(days : Int64) -> (Int64, Int, Int) {
  let z = days + 719468L
  let era = (if z >= 0L { z } else { z - 146096L }) / 146097L
  let doe = z - era * 146097L
  let yoe = (doe - doe / 1460L + doe / 36524L - doe / 146096L) / 365L
  let y = yoe + era * 400L
  let doy = doe - (365L * yoe + yoe / 4L - yoe / 100L)
  let mp = (5L * doy + 2L) / 153L
  let d = (doy - (153L * mp + 2L) / 5L + 1L).to_int()
  let m = (if mp < 10L { mp + 3L } else { mp - 9L }).to_int()
  let final_y = y + (if m <= 2 { 1L } else { 0L })
  (final_y, m, d)
}

fn weekday(days : Int64) -> Int64 {
  (((days % 7L) + 3L) % 7L + 7L) % 7L
}

fn parse_int(s : StringView) -> Result[Int64, Unit] {
  if s.is_empty() {
    return Err(())
  }
  let mut neg = false
  let mut start = 0
  if s[0] == '-' {
    neg = true
    start = 1
  } else if s[0] == '+' {
    start = 1
  }
  if start >= s.length() {
    return Err(())
  }
  let mut acc = 0L
  for i = start; i < s.length(); i = i + 1 {
    let c = s[i]
    if c < '0' || c > '9' {
      return Err(())
    }
    let digit = (c.to_int() - '0'.to_int()).to_int64()
    acc = acc * 10L + digit
  }
  Ok(if neg { -acc } else { acc })
}

fn parse_date(s : String) -> Result[Int64, RecurError] {
  let parts = s.split("-").collect()
  if parts.length() != 3 {
    return Err(BadDate(s))
  }
  let y = match parse_int(parts[0]) {
    Ok(v) => v
    Err(_) => return Err(BadDate(s))
  }
  let m = match parse_int(parts[1]) {
    Ok(v) => v
    Err(_) => return Err(BadDate(s))
  }
  let d = match parse_int(parts[2]) {
    Ok(v) => v
    Err(_) => return Err(BadDate(s))
  }

  if m < 1L || m > 12L || d < 1L || d > 31L || y < -9999L || y > 9999L {
    return Err(BadDate(s))
  }
  Ok(days_from_civil(y, m, d))
}

fn pad4(n : Int64) -> String {
  let s = n.to_string()
  if s.length() == 1 {
    "000" + s
  } else if s.length() == 2 {
    "00" + s
  } else if s.length() == 3 {
    "0" + s
  } else {
    s
  }
}

fn pad2(n : Int) -> String {
  if n < 10 {
    "0" + n.to_string()
  } else {
    n.to_string()
  }
}

fn format_date(days : Int64) -> String {
  let (y, m, d) = civil_from_days(days)
  pad4(y) + "-" + pad2(m) + "-" + pad2(d)
}

const CAP : Int = 366

pub fn expand(
  dtstart : String,
  r : Rule,
  window_from : String,
  window_to : String
) -> Result[Array[String], RecurError] {
  let ds = match parse_date(dtstart) {
    Ok(d) => d
    Err(e) => return Err(e)
  }
  let wf = match parse_date(window_from) {
    Ok(d) => d
    Err(e) => return Err(e)
  }
  let wt = match parse_date(window_to) {
    Ok(d) => d
    Err(e) => return Err(e)
  }

  let until = if r.until.is_empty() {
    None
  } else {
    match parse_date(r.until) {
      Ok(d) => Some(d)
      Err(e) => return Err(e)
    }
  }

  let step = if r.interval == 0U { 1L } else { r.interval.to_int64() }
  let count = r.count.reinterpret_as_int()

  let out : Array[String] = []
  let mut n = 0

  let stop = fn(occ : Int64, n_cur : Int) -> Bool {
    (count > 0 && n_cur >= count) ||
    (match until { Some(u) => occ > u; None => false }) ||
    occ > wt ||
    n_cur > CAP
  }

  match r.frequency {
    DAILY => {
      let mut day = ds
      while !stop(day, n) {
        if day >= wf {
          out.push(format_date(day))
        }
        n += 1
        day += step
      }
    }
    WEEKLY => {
      let wds : Array[Int64] = []
      if r.by_weekday.length() == 0 {
        wds.push(weekday(ds))
      } else {
        for i = 0; i < r.by_weekday.length(); i = i + 1 {
          let w = r.by_weekday[i].to_int().to_int64()
          if w >= 0L && w < 7L {
            if !wds.contains(w) {
              wds.push(w)
            }
          }
        }
        wds.sort()
      }
      if wds.is_empty() {
        wds.push(weekday(ds))
      }

      let week0 = ds - weekday(ds)
      let mut k = 0L
      let mut done = false
      while !done {
        let base = week0 + k * 7L * step
        if base > wt || k.to_int() > CAP {
          break
        }
        for i = 0; i < wds.length(); i = i + 1 {
          let wd = wds[i]
          let occ = base + wd
          if occ < ds {
            continue
          }
          if stop(occ, n) {
            done = true
            break
          }
          if occ >= wf {
            out.push(format_date(occ))
          }
          n += 1
        }
        k += 1L
      }
    }
  }

  Ok(out)
}
