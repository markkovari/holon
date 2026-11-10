// Core OTP logic: SHA-1, HMAC-SHA1, Base32, HOTP, TOTP

let b32_alphabet : String = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"

pub fn base32_encode(data : FixedArray[Byte]) -> String {
  let buf = StringBuilder::new()
  let mut buffer : UInt = 0U
  let mut bits : Int = 0
  for i = 0; i < data.length(); i = i + 1 {
    let b = data[i].to_int().to_uint() & 0xFFU
    buffer = (buffer << 8) | b
    bits = bits + 8
    while bits >= 5 {
      bits = bits - 5
      let idx = ((buffer >> bits) & 0x1FU).to_int()
      buf.write_char(b32_alphabet[idx].to_int().unsafe_to_char())
    }
  }
  if bits > 0 {
    let idx = ((buffer << (5 - bits)) & 0x1FU).to_int()
    buf.write_char(b32_alphabet[idx].to_int().unsafe_to_char())
  }
  buf.to_string()
}

fn b32_val(c : Int) -> Int? {
  if c >= 65 && c <= 90 { // 'A'..='Z'
    Some(c - 65)
  } else if c >= 97 && c <= 122 { // 'a'..='z'
    Some(c - 97)
  } else if c >= 50 && c <= 55 { // '2'..='7'
    Some(c - 50 + 26)
  } else {
    None
  }
}

pub fn base32_decode(s : String) -> FixedArray[Byte]? {
  let out = Array::new()
  let mut buffer : UInt = 0U
  let mut bits : Int = 0
  for i = 0; i < s.length(); i = i + 1 {
    let c = s[i].to_int()
    if c == 32 || c == 9 || c == 10 || c == 13 {
      continue
    }
    match b32_val(c) {
      Some(v) => {
        buffer = (buffer << 5) | v.to_uint()
        bits = bits + 5
        if bits >= 8 {
          bits = bits - 8
          let byte_val = ((buffer >> bits) & 0xFFU).to_int().to_byte()
          out.push(byte_val)
        }
      }
      None => return None
    }
  }
  let res = FixedArray::make(out.length(), b'\x00')
  for i = 0; i < out.length(); i = i + 1 {
    res[i] = out[i]
  }
  Some(res)
}

fn rotl32(x : UInt, n : Int) -> UInt {
  (x << n) | (x >> (32 - n))
}

pub fn sha1(data : FixedArray[Byte]) -> FixedArray[Byte] {
  let len_bytes = data.length()
  let bit_len = len_bytes.to_uint64() * 8UL

  let mut k = len_bytes + 1
  while (k % 64) != 56 {
    k = k + 1
  }
  let total_len = k + 8
  let padded = FixedArray::make(total_len, b'\x00')
  for i = 0; i < len_bytes; i = i + 1 {
    padded[i] = data[i]
  }
  padded[len_bytes] = b'\x80'

  for i = 0; i < 8; i = i + 1 {
    let shift = (7 - i) * 8
    padded[k + i] = ((bit_len >> shift) & 0xFFUL).to_int().to_byte()
  }

  let mut h0 = 0x67452301U
  let mut h1 = 0xEFCDAB89U
  let mut h2 = 0x98BADCFEU
  let mut h3 = 0x10325476U
  let mut h4 = 0xC3D2E1F0U

  let w = FixedArray::make(80, 0U)
  let num_blocks = total_len / 64

  for block = 0; block < num_blocks; block = block + 1 {
    let offset = block * 64
    for i = 0; i < 16; i = i + 1 {
      let b0 = padded[offset + i * 4].to_int().to_uint() & 0xFFU
      let b1 = padded[offset + i * 4 + 1].to_int().to_uint() & 0xFFU
      let b2 = padded[offset + i * 4 + 2].to_int().to_uint() & 0xFFU
      let b3 = padded[offset + i * 4 + 3].to_int().to_uint() & 0xFFU
      w[i] = (b0 << 24) | (b1 << 16) | (b2 << 8) | b3
    }
    for i = 16; i < 80; i = i + 1 {
      w[i] = rotl32(w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16], 1)
    }

    let mut a = h0
    let mut b = h1
    let mut c = h2
    let mut d = h3
    let mut e = h4

    for i = 0; i < 80; i = i + 1 {
      let f : UInt = if i < 20 {
        (b & c) | ((b ^ 0xFFFFFFFFU) & d)
      } else if i < 40 {
        b ^ c ^ d
      } else if i < 60 {
        (b & c) | (b & d) | (c & d)
      } else {
        b ^ c ^ d
      }
      let k_val : UInt = if i < 20 {
        0x5A827999U
      } else if i < 40 {
        0x6ED9EBA1U
      } else if i < 60 {
        0x8F1BBCDCU
      } else {
        0xCA62C1D6U
      }

      let temp = rotl32(a, 5) + f + e + k_val + w[i]
      e = d
      d = c
      c = rotl32(b, 30)
      b = a
      a = temp
    }

    h0 = h0 + a
    h1 = h1 + b
    h2 = h2 + c
    h3 = h3 + d
    h4 = h4 + e
  }

  let digest = FixedArray::make(20, b'\x00')
  let h_arr = [h0, h1, h2, h3, h4]
  for i = 0; i < 5; i = i + 1 {
    let val = h_arr[i]
    digest[i * 4] = ((val >> 24) & 0xFFU).to_int().to_byte()
    digest[i * 4 + 1] = ((val >> 16) & 0xFFU).to_int().to_byte()
    digest[i * 4 + 2] = ((val >> 8) & 0xFFU).to_int().to_byte()
    digest[i * 4 + 3] = (val & 0xFFU).to_int().to_byte()
  }
  digest
}

pub fn hmac_sha1(key : FixedArray[Byte], message : FixedArray[Byte]) -> FixedArray[Byte] {
  let k = FixedArray::make(64, b'\x00')
  if key.length() > 64 {
    let hashed_key = sha1(key)
    for i = 0; i < 20; i = i + 1 {
      k[i] = hashed_key[i]
    }
  } else {
    for i = 0; i < key.length(); i = i + 1 {
      k[i] = key[i]
    }
  }

  let inner_msg = FixedArray::make(64 + message.length(), b'\x00')
  for i = 0; i < 64; i = i + 1 {
    inner_msg[i] = (k[i].to_int() ^ 0x36).to_byte()
  }
  for i = 0; i < message.length(); i = i + 1 {
    inner_msg[64 + i] = message[i]
  }
  let inner_hash = sha1(inner_msg)

  let outer_msg = FixedArray::make(64 + 20, b'\x00')
  for i = 0; i < 64; i = i + 1 {
    outer_msg[i] = (k[i].to_int() ^ 0x5C).to_byte()
  }
  for i = 0; i < 20; i = i + 1 {
    outer_msg[64 + i] = inner_hash[i]
  }
  sha1(outer_msg)
}

pub fn hotp(secret_bytes : FixedArray[Byte], counter : UInt64, digits : Int) -> String? {
  if digits < 6 || digits > 8 {
    return None
  }
  let msg = FixedArray::make(8, b'\x00')
  for i = 0; i < 8; i = i + 1 {
    let shift = (7 - i) * 8
    msg[i] = ((counter >> shift) & 0xFFUL).to_int().to_byte()
  }

  let mac = hmac_sha1(secret_bytes, msg)
  let offset = (mac[19].to_int() & 0x0F)
  let b0 = (mac[offset].to_int() & 0x7F).to_uint()
  let b1 = (mac[offset + 1].to_int() & 0xFF).to_uint()
  let b2 = (mac[offset + 2].to_int() & 0xFF).to_uint()
  let b3 = (mac[offset + 3].to_int() & 0xFF).to_uint()
  let bin_code = (b0 << 24) | (b1 << 16) | (b2 << 8) | b3

  let mut modulo = 1U
  for i = 0; i < digits; i = i + 1 {
    modulo = modulo * 10U
  }
  let code = bin_code % modulo
  let mut s = code.to_string()
  while s.length() < digits {
    s = "0" + s
  }
  Some(s)
}

pub fn constant_time_eq(a : String, b : String) -> Bool {
  if a.length() != b.length() {
    return false
  }
  let mut diff = 0
  for i = 0; i < a.length(); i = i + 1 {
    diff = diff | (a[i].to_int() ^ b[i].to_int())
  }
  diff == 0
}

let hex_upper : String = "0123456789ABCDEF"

pub fn percent_encode(s : String) -> String {
  let buf = StringBuilder::new()
  for i = 0; i < s.length(); i = i + 1 {
    let c = s[i].to_int()
    if (c >= 65 && c <= 90) || (c >= 97 && c <= 122) || (c >= 48 && c <= 57) || c == 45 || c == 95 || c == 46 || c == 126 {
      buf.write_char(c.unsafe_to_char())
    } else {
      buf.write_char('%')
      let hi = (c >> 4) & 0x0F
      let lo = c & 0x0F
      buf.write_char(hex_upper[hi].to_int().unsafe_to_char())
      buf.write_char(hex_upper[lo].to_int().unsafe_to_char())
    }
  }
  buf.to_string()
}
