///|
/// Mutable bit stream used to build the data codewords.
priv struct BitBuffer {
  data_ : Array[Byte]
  mut bit_len_ : Int32
}

///|
fn BitBuffer::new() -> BitBuffer {
  { data_: [], bit_len_: 0, }
}

///|
/// Appends the low `n` bits of `value`, most significant bit first.
fn BitBuffer::push_bits(self : BitBuffer, value : Int32, n : Int32) -> Unit {
  for k in 0..<n {
    let bit = (value >> (n - 1 - k)) & 1
    if self.bit_len_ % 8 == 0 {
      self.data_.push((0 : Byte))
    }
    if bit == 1 {
      let shift = 7 - self.bit_len_ % 8
      let idx = self.bit_len_ / 8
      self.data_[idx] = self.data_[idx] | (1 << shift).to_byte()
    }
    self.bit_len_ += 1
  }
}

///|
/// Terminator, byte alignment, and alternating pad bytes 0xEC / 0x11.
fn BitBuffer::pad_to(
  self : BitBuffer,
  capacity_bytes : Int32,
) -> Unit raise QrError {
  let capacity_bits = capacity_bytes * 8
  if self.bit_len_ > capacity_bits {
    raise QrError::Internal("bit buffer already exceeds data capacity")
  }
  let remaining = capacity_bits - self.bit_len_
  self.push_bits(0, if remaining < 4 { remaining } else { 4 })
  while self.bit_len_ % 8 != 0 {
    self.push_bits(0, 1)
  }
  let mut use_ec = true
  while self.data_.length() < capacity_bytes {
    self.data_.push(if use_ec { (0xEC : Byte) } else { (0x11 : Byte) })
    self.bit_len_ += 8
    use_ec = !use_ec
  }
  if self.bit_len_ != capacity_bits {
    raise QrError::Internal("bit buffer padding did not reach capacity")
  }
}

///|
fn BitBuffer::to_bytes(self : BitBuffer) -> Array[Byte] {
  self.data_.copy()
}
