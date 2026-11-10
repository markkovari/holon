///|
/// GF(2^8) log table for primitive polynomial 0x11D, primitive element 2.
fn init_gf_tables() -> (Array[Int32], Array[Int32]) {
  let exp = Array::make(512, 0)
  let log = Array::make(256, 0)
  let mut x = 1
  for i in 0..<255 {
    exp[i] = x
    log[x] = i
    x = x << 1
    if x >= 256 {
      x = x ^ 0x11D
    }
  }
  for i in 255..<512 {
    exp[i] = exp[i - 255]
  }
  (exp, log)
}

///|
let gf_tables : (Array[Int32], Array[Int32]) = init_gf_tables()

///|
let gf_exp : Array[Int32] = gf_tables.0

///|
let gf_log : Array[Int32] = gf_tables.1

///|
fn gf_mul_int(a : Int32, b : Int32) -> Int32 {
  if a == 0 || b == 0 {
    0
  } else {
    gf_exp[gf_log[a] + gf_log[b]]
  }
}

///|
/// Multiplication in GF(2^8).
///
/// GF(2^8) 上的乘法。
pub fn gf_mul(a : Byte, b : Byte) -> Byte {
  gf_mul_int(a.to_int(), b.to_int()).to_byte()
}

///|
/// Generator polynomial coefficients (excluding the leading 1) for `nsym` parity bytes.
fn rs_divisor(nsym : Int32) -> Array[Int32] {
  let result = Array::make(nsym, 0)
  if nsym > 0 {
    result[nsym - 1] = 1
    let mut root = 1
    for _ in 0..<nsym {
      for j in 0..<nsym {
        result[j] = gf_mul_int(result[j], root)
        if j + 1 < nsym {
          result[j] = result[j] ^ result[j + 1]
        }
      }
      root = gf_mul_int(root, 2)
    }
  }
  result
}

///|
/// Reed-Solomon parity bytes for `msg` with `nsym` ECC codewords.
///
/// 对 `msg` 计算 `nsym` 个 ECC 校验字节的 Reed-Solomon 编码。
pub fn rs_encode(msg : ArrayView[Byte], nsym : Int32) -> Array[Byte] {
  if nsym <= 0 {
    return []
  }
  let divisor = rs_divisor(nsym)
  let result = Array::make(nsym, 0)
  for b in msg {
    let factor = b.to_int() ^ result[0]
    for i in 0..<(nsym - 1) {
      result[i] = result[i + 1]
    }
    if nsym > 0 {
      result[nsym - 1] = 0
    }
    for i in 0..<nsym {
      result[i] = result[i] ^ gf_mul_int(divisor[i], factor)
    }
  }
  result.map(fn(x) { x.to_byte() })
}
