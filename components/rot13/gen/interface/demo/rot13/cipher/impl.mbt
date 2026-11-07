// Implementation of demo:rot13@0.1.0 cipher interface in MoonBit

///|
pub fn rot13(text : String) -> String {
  let buf = StringBuilder()
  let a_lower = 'a'.to_int()
  let a_upper = 'A'.to_int()
  for c in text {
    if c >= 'a' && c <= 'z' {
      let shifted = (c.to_int() - a_lower + 13) % 26 + a_lower
      buf.write_char(shifted.unsafe_to_char())
    } else if c >= 'A' && c <= 'Z' {
      let shifted = (c.to_int() - a_upper + 13) % 26 + a_upper
      buf.write_char(shifted.unsafe_to_char())
    } else {
      buf.write_char(c)
    }
  }
  buf.to_string()
}
