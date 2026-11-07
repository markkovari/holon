pub fn call(args : Json) -> Result[Json, String] {
  guard args is { "n": Number(n, ..), .. } else {
    return Err("n is required")
  }
  let limit = n.to_int()
  if limit < 2 {
    return Ok({ "count": Json::number(0.0) })
  }
  let size = limit
  let is_prime = Array::make(size, true)
  is_prime[0] = false
  is_prime[1] = false
  let mut i = 2
  while i * i < size {
    if is_prime[i] {
      let mut j = i * i
      while j < size {
        is_prime[j] = false
        j = j + i
      }
    }
    i = i + 1
  }
  let mut count = 0
  for k in 0..<size {
    if is_prime[k] {
      count = count + 1
    }
  }
  Ok({ "count": Json::number(count.to_double()) })
}