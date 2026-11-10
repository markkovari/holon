// Implementation of paginate:cursor/cursors

fn signing_key() -> String {
  match @config.get("cursor-secret") {
    Ok(Some(v)) => if v.length() > 0 { v } else { "paginate-default-secret" }
    _ => "paginate-default-secret"
  }
}

fn parse_uint(s : String) -> UInt? {
  if s.length() == 0 {
    return None
  }
  let mut res = 0U
  for i = 0; i < s.length(); i = i + 1 {
    let c = s[i]
    if c >= '0' && c <= '9' {
      res = res * 10U + (c.to_int() - '0'.to_int()).reinterpret_as_uint()
    } else {
      return None
    }
  }
  Some(res)
}

fn max_page_size() -> UInt {
  match @config.get("max-page-size") {
    Ok(Some(v)) => match parse_uint(v) {
      Some(n) => n
      None => 100U
    }
    _ => 100U
  }
}

// ----------------- Base64URL -----------------

let url_table : Array[Char] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_".to_array()

fn str_to_bytes(s : String) -> FixedArray[Byte] {
  let len = s.length()
  let arr = FixedArray::make(len, b'\x00')
  for i = 0; i < len; i = i + 1 {
    arr[i] = ((s[i].to_int()) & 0xFF).to_byte()
  }
  arr
}

fn bytes_to_str(bytes : FixedArray[Byte]) -> String {
  let buf = StringBuilder::new()
  for i = 0; i < bytes.length(); i = i + 1 {
    buf.write_char((bytes[i].to_int() & 0xFF).unsafe_to_char())
  }
  buf.to_string()
}

fn b64url_encode(bytes : FixedArray[Byte]) -> String {
  let out = StringBuilder::new()
  let mut i = 0
  while i < bytes.length() {
    let remaining = bytes.length() - i
    let b0 = bytes[i].to_int() & 0xFF
    let b1 = if remaining > 1 { bytes[i + 1].to_int() & 0xFF } else { 0 }
    let b2 = if remaining > 2 { bytes[i + 2].to_int() & 0xFF } else { 0 }
    let n = (b0 << 16) | (b1 << 8) | b2
    out.write_char(url_table[(n >> 18) & 0x3F])
    out.write_char(url_table[(n >> 12) & 0x3F])
    if remaining > 2 {
      out.write_char(url_table[(n >> 6) & 0x3F])
      out.write_char(url_table[n & 0x3F])
    } else if remaining == 2 {
      out.write_char(url_table[(n >> 6) & 0x3F])
    }
    i = i + 3
  }
  out.to_string()
}

fn char_value(c : Char) -> Int {
  let n = c.to_int()
  if n >= 'A'.to_int() && n <= 'Z'.to_int() {
    n - 'A'.to_int()
  } else if n >= 'a'.to_int() && n <= 'z'.to_int() {
    n - 'a'.to_int() + 26
  } else if n >= '0'.to_int() && n <= '9'.to_int() {
    n - '0'.to_int() + 52
  } else if c == '-' {
    62
  } else if c == '_' {
    63
  } else {
    -1
  }
}

fn b64url_decode(text : String) -> Result[FixedArray[Byte], Unit] {
  let chars = text.to_array()
  if chars.length() == 0 {
    return Ok(FixedArray::make(0, b'\x00'))
  }
  let values = []
  for i = 0; i < chars.length(); i = i + 1 {
    let v = char_value(chars[i])
    if v < 0 {
      return Err(())
    }
    values.push(v)
  }
  if values.length() % 4 == 1 {
    return Err(())
  }
  let out = []
  let mut i = 0
  while i + 4 <= values.length() {
    let n = (values[i] << 18) | (values[i + 1] << 12) | (values[i + 2] << 6) | values[i + 3]
    out.push(((n >> 16) & 0xFF).to_byte())
    out.push(((n >> 8) & 0xFF).to_byte())
    out.push((n & 0xFF).to_byte())
    i = i + 4
  }
  let rest = values.length() - i
  if rest == 2 {
    let n = (values[i] << 18) | (values[i + 1] << 12)
    out.push(((n >> 16) & 0xFF).to_byte())
  } else if rest == 3 {
    let n = (values[i] << 18) | (values[i + 1] << 12) | (values[i + 2] << 6)
    out.push(((n >> 16) & 0xFF).to_byte())
    out.push(((n >> 8) & 0xFF).to_byte())
  }
  Ok(FixedArray::from_array(out))
}

// ----------------- SHA-256 & HMAC -----------------

fn rotr(x : UInt, n : Int) -> UInt {
  (x >> n) | (x << (32 - n))
}

fn ch(x : UInt, y : UInt, z : UInt) -> UInt {
  (x & y) ^ ((x ^ 0xFFFFFFFFU) & z)
}

fn maj(x : UInt, y : UInt, z : UInt) -> UInt {
  (x & y) ^ (x & z) ^ (y & z)
}

fn sigma0(x : UInt) -> UInt {
  rotr(x, 2) ^ rotr(x, 13) ^ rotr(x, 22)
}

fn sigma1(x : UInt) -> UInt {
  rotr(x, 6) ^ rotr(x, 11) ^ rotr(x, 25)
}

fn gamma0(x : UInt) -> UInt {
  rotr(x, 7) ^ rotr(x, 18) ^ (x >> 3)
}

fn gamma1(x : UInt) -> UInt {
  rotr(x, 17) ^ rotr(x, 19) ^ (x >> 10)
}

let k_table : FixedArray[UInt] = [
  0x428a2f98U, 0x71374491U, 0xb5c0fbcfU, 0xe9b5dba5U,
  0x3956c25bU, 0x59f111f1U, 0x923f82a4U, 0xab1c5ed5U,
  0xd807aa98U, 0x12835b01U, 0x243185beU, 0x550c7dc3U,
  0x72be5d74U, 0x80deb1feU, 0x9bdc06a7U, 0xc19bf174U,
  0xe49b69c1U, 0xefbe4786U, 0x0fc19dc6U, 0x240ca1ccU,
  0x2de92c6fU, 0x4a7484aaU, 0x5cb0a9dcU, 0x76f988daU,
  0x983e5152U, 0xa831c66dU, 0xb00327c8U, 0xbf597fc7U,
  0xc6e00bf3U, 0xd5a79147U, 0x06ca6351U, 0x14292967U,
  0x27b70a85U, 0x2e1b2138U, 0x4d2c6dfcU, 0x53380d13U,
  0x650a7354U, 0x766a0abbU, 0x81c2c92eU, 0x92722c85U,
  0xa2bfe8a1U, 0xa81a664bU, 0xc24b8b70U, 0xc76c51a3U,
  0xd192e819U, 0xd6990624U, 0xf40e3585U, 0x106aa070U,
  0x19a4c116U, 0x1e376c08U, 0x2748774cU, 0x34b0bcb5U,
  0x391c0cb3U, 0x4ed8aa4aU, 0x5b9cca4fU, 0x682e6ff3U,
  0x748f82eeU, 0x78a5636fU, 0x84c87814U, 0x8cc70208U,
  0x90befffaU, 0xa4506cebU, 0xbef9a3f7U, 0xc67178f2U,
]

fn sha256(data : FixedArray[Byte]) -> FixedArray[Byte] {
  let len = data.length()
  let bit_len = (len.to_int64() * 8L).to_uint64()
  let rem = (len + 1) % 64
  let pad_zeros = if rem <= 56 { 56 - rem } else { 120 - rem }
  let total_len = len + 1 + pad_zeros + 8
  let padded = FixedArray::make(total_len, b'\x00')
  for i = 0; i < len; i = i + 1 {
    padded[i] = data[i]
  }
  padded[len] = b'\x80'
  for i = 0; i < 8; i = i + 1 {
    let shift = (7 - i) * 8
    padded[total_len - 8 + i] = ((bit_len >> shift) & 0xFFUL).to_byte()
  }

  let mut h0 = 0x6a09e667U
  let mut h1 = 0xbb67ae85U
  let mut h2 = 0x3c6ef372U
  let mut h3 = 0xa54ff53aU
  let mut h4 = 0x510e527fU
  let mut h5 = 0x9b05688cU
  let mut h6 = 0x1f83d9abU
  let mut h7 = 0x5be0cd19U

  let w = FixedArray::make(64, 0U)

  for chunk = 0; chunk < total_len / 64; chunk = chunk + 1 {
    let offset = chunk * 64
    for i = 0; i < 16; i = i + 1 {
      let b0 = (padded[offset + i * 4].to_int() & 0xFF).reinterpret_as_uint()
      let b1 = (padded[offset + i * 4 + 1].to_int() & 0xFF).reinterpret_as_uint()
      let b2 = (padded[offset + i * 4 + 2].to_int() & 0xFF).reinterpret_as_uint()
      let b3 = (padded[offset + i * 4 + 3].to_int() & 0xFF).reinterpret_as_uint()
      w[i] = (b0 << 24) | (b1 << 16) | (b2 << 8) | b3
    }
    for i = 16; i < 64; i = i + 1 {
      w[i] = gamma1(w[i - 2]) + w[i - 7] + gamma0(w[i - 15]) + w[i - 16]
    }

    let mut a = h0
    let mut b = h1
    let mut c = h2
    let mut d = h3
    let mut e = h4
    let mut f = h5
    let mut g = h6
    let mut h = h7

    for i = 0; i < 64; i = i + 1 {
      let t1 = h + sigma1(e) + ch(e, f, g) + k_table[i] + w[i]
      let t2 = sigma0(a) + maj(a, b, c)
      h = g
      g = f
      f = e
      e = d + t1
      d = c
      c = b
      b = a
      a = t1 + t2
    }

    h0 = h0 + a
    h1 = h1 + b
    h2 = h2 + c
    h3 = h3 + d
    h4 = h4 + e
    h5 = h5 + f
    h6 = h6 + g
    h7 = h7 + h
  }

  let out = FixedArray::make(32, b'\x00')
  let state = [h0, h1, h2, h3, h4, h5, h6, h7]
  for i = 0; i < 8; i = i + 1 {
    let val = state[i]
    out[i * 4] = ((val >> 24) & 0xFFU).to_byte()
    out[i * 4 + 1] = ((val >> 16) & 0xFFU).to_byte()
    out[i * 4 + 2] = ((val >> 8) & 0xFFU).to_byte()
    out[i * 4 + 3] = (val & 0xFFU).to_byte()
  }
  out
}

fn concat_bytes(a : FixedArray[Byte], b : FixedArray[Byte]) -> FixedArray[Byte] {
  let len_a = a.length()
  let len_b = b.length()
  let out = FixedArray::make(len_a + len_b, b'\x00')
  for i = 0; i < len_a; i = i + 1 {
    out[i] = a[i]
  }
  for i = 0; i < len_b; i = i + 1 {
    out[len_a + i] = b[i]
  }
  out
}

fn hmac_sha256(key : FixedArray[Byte], data : FixedArray[Byte]) -> FixedArray[Byte] {
  let k_block = FixedArray::make(64, b'\x00')
  if key.length() > 64 {
    let hashed_key = sha256(key)
    for i = 0; i < hashed_key.length(); i = i + 1 {
      k_block[i] = hashed_key[i]
    }
  } else {
    for i = 0; i < key.length(); i = i + 1 {
      k_block[i] = key[i]
    }
  }

  let k_ipad = FixedArray::make(64, b'\x00')
  let k_opad = FixedArray::make(64, b'\x00')
  for i = 0; i < 64; i = i + 1 {
    let kb = k_block[i].to_int() & 0xFF
    k_ipad[i] = (kb ^ 0x36).to_byte()
    k_opad[i] = (kb ^ 0x5c).to_byte()
  }

  let inner = sha256(concat_bytes(k_ipad, data))
  sha256(concat_bytes(k_opad, inner))
}

fn to_hex_8(bytes : FixedArray[Byte]) -> String {
  let digits = "0123456789abcdef".to_array()
  let out = StringBuilder::new()
  for i = 0; i < 8; i = i + 1 {
    let b = bytes[i].to_int() & 0xFF
    out.write_char(digits[(b >> 4) & 0x0F])
    out.write_char(digits[b & 0x0F])
  }
  out.to_string()
}

pub fn checksum(key : String, payload : String) -> String {
  let mac = hmac_sha256(str_to_bytes(key), str_to_bytes(payload))
  to_hex_8(mac)
}

pub fn verify(key : String, payload : String, expected_hex : String) -> Bool {
  if expected_hex.length() != 16 {
    return false
  }
  for i = 0; i < 16; i = i + 1 {
    let c = expected_hex[i]
    let is_hex = (c >= '0' && c <= '9') || (c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F')
    if !is_hex {
      return false
    }
  }
  let tag = checksum(key, payload)
  let exp = expected_hex.to_lower()
  let mut diff = 0
  for i = 0; i < 16; i = i + 1 {
    diff = diff | (tag[i].to_int() ^ exp[i].to_int())
  }
  diff == 0
}

// ----------------- Cursor Serialization -----------------

fn split_colon(s : String) -> Array[String] {
  let parts = []
  let mut start = 0
  for i = 0; i < s.length(); i = i + 1 {
    if s[i] == ':' {
      parts.push(s[start:i].to_owned())
      start = i + 1
    }
  }
  parts.push(s[start:].to_owned())
  parts
}

fn payload_of(pos : Position) -> String {
  let dir = if pos.forward { "1" } else { "0" }
  let k = b64url_encode(str_to_bytes(pos.sort_key))
  let id = b64url_encode(str_to_bytes(pos.last_id))
  "f:" + dir + ":" + k + ":" + id
}

fn decode_field(b64 : String) -> String? {
  match b64url_decode(b64) {
    Ok(b) => Some(bytes_to_str(b))
    Err(_) => None
  }
}

fn position_of(payload : String) -> Position? {
  let parts = split_colon(payload)
  if parts.length() != 4 {
    return None
  }
  if parts[0] != "f" {
    return None
  }
  let forward = match parts[1] {
    "1" => true
    "0" => false
    _ => return None
  }
  let sort_key = match decode_field(parts[2]) {
    Some(k) => k
    None => return None
  }
  let last_id = match decode_field(parts[3]) {
    Some(id) => id
    None => return None
  }
  Some({ sort_key, last_id, forward })
}

// ----------------- Exported Functions -----------------

///|
pub fn encode(pos : Position) -> String {
  let key = signing_key()
  let payload = payload_of(pos)
  let sum = checksum(key, payload)
  let b64_payload = b64url_encode(str_to_bytes(payload))
  b64_payload + "." + sum
}

///|
pub fn decode(cursor : String) -> Result[Position, CursorError] {
  let mut dot_idx = -1
  for i = 0; i < cursor.length(); i = i + 1 {
    if cursor[i] == '.' {
      dot_idx = i
      break
    }
  }
  if dot_idx < 0 {
    return Err(InvalidCursor)
  }
  let b64_payload = cursor[0:dot_idx].to_owned()
  let sum = cursor[dot_idx + 1:].to_owned()
  let payload_bytes = match b64url_decode(b64_payload) {
    Ok(b) => b
    Err(_) => return Err(InvalidCursor)
  }
  let payload = bytes_to_str(payload_bytes)
  let key = signing_key()
  if !verify(key, payload, sum) {
    return Err(InvalidCursor)
  }
  match position_of(payload) {
    Some(pos) => Ok(pos)
    None => Err(InvalidCursor)
  }
}

///|
pub fn clamp_limit(requested : UInt) -> Result[UInt, CursorError] {
  if requested == 0U {
    return Err(BadLimit)
  }
  let max = max_page_size()
  if requested < max {
    Ok(requested)
  } else {
    Ok(max)
  }
}

///|
pub fn build_page(
  first : Position?,
  last : Position?,
  more_before : Bool,
  more_after : Bool,
) -> PageInfo {
  let next_cursor = if more_after {
    match last {
      Some(pos) => Some(encode({ sort_key: pos.sort_key, last_id: pos.last_id, forward: true }))
      None => None
    }
  } else {
    None
  }
  let prev_cursor = if more_before {
    match first {
      Some(pos) => Some(encode({ sort_key: pos.sort_key, last_id: pos.last_id, forward: false }))
      None => None
    }
  } else {
    None
  }
  { next_cursor, prev_cursor, has_next: more_after, has_prev: more_before }
}
