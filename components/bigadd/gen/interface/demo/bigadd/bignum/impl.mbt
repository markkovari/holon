// Implementation of demo:bigadd@0.1.0 bignum interface in MoonBit

///|
pub fn add(a : String, b : String) -> String {
  let mut i = a.length()
  let mut j = b.length()
  let mut carry = 0
  let out : Array[Char] = []
  let zero_code = '0'.to_int()

  while i > 0 || j > 0 || carry > 0 {
    let mut sum = carry
    if i > 0 {
      i = i - 1
      let digit = a[i].to_int() - zero_code
      sum = sum + digit
    }
    if j > 0 {
      j = j - 1
      let digit = b[j].to_int() - zero_code
      sum = sum + digit
    }
    carry = sum / 10
    let digit_char = (zero_code + (sum % 10)).unsafe_to_char()
    out.push(digit_char)
  }

  if out.is_empty() {
    return "0"
  }

  while out.length() > 1 && out[out.length() - 1] == '0' {
    let _ = out.pop()
  }

  let buf = StringBuilder()
  for k = out.length() - 1; k >= 0; k = k - 1 {
    buf.write_char(out[k])
  }
  buf.to_string()
}
