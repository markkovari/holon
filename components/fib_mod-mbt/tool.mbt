fn fib_pair(n : Int, m : Int) -> (Int, Int) {
  if n == 0 {
    return (0, 1)
  }
  let (a, b) = fib_pair(n / 2, m)
  let c = (a * ((2 * b - a + m) % m)) % m
  let d = (a * a + b * b) % m
  if n % 2 == 0 {
    (c, d)
  } else {
    (d, (c + d) % m)
  }
}

pub fn call(args : Json) -> Result[Json, String] {
  guard args is { "n": Number(n, ..), "mod": Number(mod, ..), .. } else {
    return Err("n and mod are required")
  }
  let n = n.to_int()
  let m = mod.to_int()
  let (f, _) = fib_pair(n, m)
  Ok({ "result": Json::number(f.to_double()) })
}
