// Implementation of demo:roman@0.1.0 numerals interface in MoonBit

let values : Array[(UInt, String)] = [
  (1000U, "M"),
  (900U, "CM"),
  (500U, "D"),
  (400U, "CD"),
  (100U, "C"),
  (90U, "XC"),
  (50U, "L"),
  (40U, "XL"),
  (10U, "X"),
  (9U, "IX"),
  (5U, "V"),
  (4U, "IV"),
  (1U, "I"),
]

///|
pub fn to_roman(n : UInt) -> String {
  let mut cur = n
  let buf = StringBuilder()
  for item in values {
    let (v, sym) = item
    while cur >= v {
      buf.write_string(sym)
      cur = cur - v
    }
  }
  buf.to_string()
}

fn char_val(c : Char) -> UInt {
  match c {
    'I' => 1U
    'V' => 5U
    'X' => 10U
    'L' => 50U
    'C' => 100U
    'D' => 500U
    'M' => 1000U
    _ => 0U
  }
}

///|
pub fn from_roman(s : String) -> UInt {
  let mut total = 0U
  let mut prev = 0U
  let len = s.length()
  for i = len - 1; i >= 0; i = i - 1 {
    let c = s[i].to_int().unsafe_to_char()
    let v = char_val(c)
    if v < prev {
      total = total - v
    } else {
      total = total + v
      prev = v
    }
  }
  total
}
