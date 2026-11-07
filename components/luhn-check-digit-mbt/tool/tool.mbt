pub fn call(args : Json) -> Result[Json, String] {
  guard args is { "number": String(number), .. } else {
    return Err("number is required")
  }
  let bytes = @text.utf8(number)
  let n = bytes.length()
  let mut sum = 0
  for i in 0..<n {
    let pos_from_right = n - i
    let d = (bytes.at(i).to_int() - 48)
    let full_pos = pos_from_right + 1
    let val = if full_pos % 2 == 0 {
      let doubled = d * 2
      if doubled >= 10 { doubled - 9 } else { doubled }
    } else {
      d
    }
    sum = sum + val
  }
  let check = (10 - sum % 10) % 10
  Ok({ "check_digit": Json::number(check.to_double()), "sum": Json::number(sum.to_double()) })
}