fn period_or_default(period : UInt) -> UInt64 {
  if period == 0U {
    30UL
  } else {
    period.to_uint64()
  }
}

pub fn provision(issuer : String, account : String) -> Result[Provisioned, OtpError] {
  let raw = @random.get_random_bytes(20UL)
  let secret = @core.base32_encode(raw)
  let issuer_enc = @core.percent_encode(issuer)
  let account_enc = @core.percent_encode(account)
  let uri = "otpauth://totp/" + issuer_enc + ":" + account_enc + "?secret=" + secret + "&issuer=" + issuer_enc + "&algorithm=SHA1&digits=6&period=30"
  Ok({ secret, uri })
}

pub fn totp_at(secret : String, timestamp : UInt64, period : UInt, digits : Byte) -> Result[String, OtpError] {
  let bytes = match @core.base32_decode(secret) {
    Some(b) => b
    None => return Err(OtpError::BadSecret)
  }
  let counter = timestamp / period_or_default(period)
  match @core.hotp(bytes, counter, digits.to_int()) {
    Some(c) => Ok(c)
    None => Err(OtpError::BadDigits)
  }
}

pub fn totp_now(secret : String) -> Result[String, OtpError] {
  let now = @wall_clock.now().seconds
  totp_at(secret, now, 30U, (6).to_byte())
}

pub fn verify(secret : String, code : String, period : UInt, digits : Byte, skew : UInt) -> Result[Bool, OtpError] {
  let bytes = match @core.base32_decode(secret) {
    Some(b) => b
    None => return Err(OtpError::BadSecret)
  }
  let d = digits.to_int()
  if d < 6 || d > 8 {
    return Err(OtpError::BadDigits)
  }

  let step = period_or_default(period)
  let now = @wall_clock.now().seconds
  let counter0 = now / step
  let skew64 = skew.to_uint64()
  let lo = if counter0 >= skew64 { counter0 - skew64 } else { 0UL }
  let hi = counter0 + skew64

  let mut matched = false
  let mut c = lo
  while c <= hi {
    match @core.hotp(bytes, c, d) {
      Some(candidate) => {
        if @core.constant_time_eq(candidate, code) {
          matched = true
        }
      }
      None => ()
    }
    c = c + 1UL
  }
  Ok(matched)
}

pub fn hotp_at(secret : String, counter : UInt64, digits : Byte) -> Result[String, OtpError] {
  let bytes = match @core.base32_decode(secret) {
    Some(b) => b
    None => return Err(OtpError::BadSecret)
  }
  match @core.hotp(bytes, counter, digits.to_int()) {
    Some(c) => Ok(c)
    None => Err(OtpError::BadDigits)
  }
}

pub fn recovery_codes(count : UInt) -> Array[String] {
  let n = count.to_int()
  let codes = Array::new()
  for i = 0; i < n; i = i + 1 {
    let raw = @random.get_random_bytes(5UL)
    let b32 = @core.base32_encode(raw).to_lower()
    let part1 = b32[0:4].to_owned()
    let part2 = b32[4:8].to_owned()
    codes.push(part1 + "-" + part2)
  }
  codes
}
