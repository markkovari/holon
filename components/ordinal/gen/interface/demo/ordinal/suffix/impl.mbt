// Implementation of demo:ordinal@0.1.0 suffix interface in MoonBit

///|
pub fn ordinal(n : UInt) -> String {
  let rem100 = n % 100U
  let suffix = if rem100 >= 11U && rem100 <= 13U {
    "th"
  } else {
    match n % 10U {
      1U => "st"
      2U => "nd"
      3U => "rd"
      _ => "th"
    }
  }
  "\{n}\{suffix}"
}
