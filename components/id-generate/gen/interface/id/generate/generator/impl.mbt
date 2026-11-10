pub fn ulid() -> String {
  let now = @wall_clock.now()
  let ms = now.seconds * 1000UL + (now.nanoseconds.to_uint64() / 1000000UL)
  let bytes = @random.get_random_bytes(10UL)
  let hi : UInt64 = ((bytes[0].to_int().to_uint64() & 0xFFUL) << 8) | (bytes[1].to_int().to_uint64() & 0xFFUL)
  let mut lo : UInt64 = 0UL
  for i = 2; i < 10; i = i + 1 {
    lo = (lo << 8) | (bytes[i].to_int().to_uint64() & 0xFFUL)
  }
  @core.next_ulid(ms, hi, lo)
}

pub fn ulid_at(unix_millis : UInt64) -> String {
  let bytes = @random.get_random_bytes(10UL)
  let hi : UInt64 = ((bytes[0].to_int().to_uint64() & 0xFFUL) << 8) | (bytes[1].to_int().to_uint64() & 0xFFUL)
  let mut lo : UInt64 = 0UL
  for i = 2; i < 10; i = i + 1 {
    lo = (lo << 8) | (bytes[i].to_int().to_uint64() & 0xFFUL)
  }
  @core.encode_ulid(unix_millis, hi, lo)
}

pub fn uuid_v4() -> String {
  let bytes = @random.get_random_bytes(16UL)
  @core.format_uuid_v4(bytes)
}

pub fn nanoid(length : Byte) -> String {
  let len = length.to_int().clamp(min=1, max=64)
  let bytes = @random.get_random_bytes(len.to_uint64())
  @core.format_nanoid(bytes, length)
}

pub fn short_code(length : Byte) -> String {
  let len = length.to_int().clamp(min=1, max=64)
  let bytes = @random.get_random_bytes(len.to_uint64())
  @core.format_short_code(bytes, length)
}
