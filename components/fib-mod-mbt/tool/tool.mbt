pub fn call(args : Json) -> Result[Json, String] {
  guard args is { "n": Number(n, ..), "mod": Number(mod, ..), .. } else {
    return Err("n and mod are required")
  }
  let n = n.to_int();
  let mod = mod.to_int();
  fn fib_pair(n: Int, mod: Int) -> (Int, Int) {
    if n == 0 {
      return (0, 1);
    }
    let (a, b) = fib_pair(n >> 1, mod);
    let two_b = (b * 2) % mod;
    let c = (a * ((two_b - a + mod) % mod)) % mod;
    let d = (a * a + b * b) % mod;
    if (n & 1) == 0 {
      return (c, d);
    } else {
      return (d, (c + d) % mod);
    }
  }
  let (f_n, _) = fib_pair(n, mod);
  Ok({ "result": Json::number(f_n.to_double()) })
}
