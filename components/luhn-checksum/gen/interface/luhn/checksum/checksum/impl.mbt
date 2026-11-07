fn folded_sum(digits : String, double_rightmost : Bool) -> Int? {
  let len = digits.length()
  if len == 0 {
    return None
  }
  let mut sum = 0
  let mut i = 0
  while i < len {
    let idx = len - 1 - i
    let code = digits.at(idx).to_int()
    if code < '0'.to_int() || code > '9'.to_int() {
      return None
    }
    let d = code - '0'.to_int()
    let doubles = (i % 2 == 0) == double_rightmost
    if doubles {
      let twice = d * 2
      if twice > 9 {
        sum = sum + (twice - 9)
      } else {
        sum = sum + twice
      }
    } else {
      sum = sum + d
    }
    i = i + 1
  }
  Some(sum)
}

pub fn is_valid(digits : String) -> Bool {
  match folded_sum(digits, false) {
    Some(sum) => sum % 10 == 0
    None => false
  }
}

pub fn checksum_digit(digits : String) -> Byte? {
  match folded_sum(digits, true) {
    Some(sum) => {
      let rem = sum % 10
      let check = (10 - rem) % 10
      Some(check.to_byte())
    }
    None => None
  }
}

pub fn append_checksum(digits : String) -> String? {
  match checksum_digit(digits) {
    Some(digit) => {
      let digit_char = ('0'.to_int() + digit.to_int()).unsafe_to_char()
      let buf = StringBuilder()
      buf.write_string(digits)
      buf.write_char(digit_char)
      Some(buf.to_string())
    }
    None => None
  }
}
