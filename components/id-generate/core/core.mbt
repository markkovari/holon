pub let crockford : String = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"

pub fn encode_ulid(time_ms : UInt64, rand80_hi : UInt64, rand80_lo : UInt64) -> String {
  let hi = ((time_ms & 0xFFFF_FFFF_FFFFUL) << 16) | (rand80_hi & 0xFFFFUL)
  let lo = rand80_lo
  let buf = StringBuilder::new(size_hint=26)
  let chars = FixedArray::make(26, '0')
  for i = 0; i < 26; i = i + 1 {
    let shift = 5 * i
    let bits : Int = if shift + 5 <= 64 {
      ((lo >> shift) & 0x1FUL).to_int()
    } else if shift < 64 {
      let lo_part = (lo >> 60) & 0x0FUL
      let hi_part = (hi & 1UL) << 4
      (lo_part | hi_part).to_int()
    } else {
      let hi_shift = shift - 64
      if hi_shift <= 59 {
        ((hi >> hi_shift) & 0x1FUL).to_int()
      } else {
        ((hi >> hi_shift) & 0x07UL).to_int()
      }
    }
    chars[25 - i] = crockford[bits].to_int().unsafe_to_char()
  }
  for i = 0; i < 26; i = i + 1 {
    buf.write_char(chars[i])
  }
  buf.to_string()
}

pub(all) struct UlidState {
  mut last_ms : UInt64
  mut last_rand_hi : UInt64
  mut last_rand_lo : UInt64
}

let state : UlidState = { last_ms: 0UL, last_rand_hi: 0UL, last_rand_lo: 0UL }

pub fn next_ulid(now_ms : UInt64, r80_hi : UInt64, r80_lo : UInt64) -> String {
  if now_ms == state.last_ms {
    // Increment 80-bit counter
    if state.last_rand_lo == 0xFFFF_FFFF_FFFF_FFFFUL {
      state.last_rand_lo = 0UL
      state.last_rand_hi = (state.last_rand_hi + 1UL) & 0xFFFFUL
    } else {
      state.last_rand_lo = state.last_rand_lo + 1UL
    }
  } else {
    state.last_ms = now_ms
    state.last_rand_hi = r80_hi & 0xFFFFUL
    state.last_rand_lo = r80_lo
  }
  encode_ulid(now_ms, state.last_rand_hi, state.last_rand_lo)
}

let hex_chars : String = "0123456789abcdef"

pub fn format_uuid_v4(bytes : FixedArray[Byte]) -> String {
  let b = FixedArray::make(16, b'\x00')
  for i = 0; i < 16; i = i + 1 {
    b[i] = bytes[i]
  }
  b[6] = ((b[6].to_int() & 0x0f) | 0x40).to_byte()
  b[8] = ((b[8].to_int() & 0x3f) | 0x80).to_byte()

  let buf = StringBuilder::new(size_hint=36)
  for i = 0; i < 16; i = i + 1 {
    if i == 4 || i == 6 || i == 8 || i == 10 {
      buf.write_char('-')
    }
    let byte_val = b[i].to_int() & 0xFF
    let hi_idx = (byte_val >> 4) & 0x0F
    let lo_idx = byte_val & 0x0F
    buf.write_char(hex_chars[hi_idx].to_int().unsafe_to_char())
    buf.write_char(hex_chars[lo_idx].to_int().unsafe_to_char())
  }
  buf.to_string()
}

let nanoid_alphabet : String = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-"

pub fn format_nanoid(bytes : FixedArray[Byte], length : Byte) -> String {
  let len = length.to_int().clamp(min=1, max=64)
  let buf = StringBuilder::new(size_hint=len)
  for i = 0; i < len; i = i + 1 {
    let b = bytes[i].to_int() & 0xFF
    let idx = b % 64
    buf.write_char(nanoid_alphabet[idx].to_int().unsafe_to_char())
  }
  buf.to_string()
}

let short_code_alphabet : String = "23456789ABCDEFGHJKMNPQRSTUVWXYZ"

pub fn format_short_code(bytes : FixedArray[Byte], length : Byte) -> String {
  let len = length.to_int().clamp(min=1, max=64)
  let buf = StringBuilder::new(size_hint=len)
  for i = 0; i < len; i = i + 1 {
    let b = bytes[i].to_int() & 0xFF
    let idx = b % 31
    buf.write_char(short_code_alphabet[idx].to_int().unsafe_to_char())
  }
  buf.to_string()
}
