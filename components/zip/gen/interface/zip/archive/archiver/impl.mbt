// Pure compute ZIP archiver and extractor (STORE + DEFLATE).

fn crc32(data : FixedArray[Byte]) -> UInt {
  let mut crc : UInt = 0xFFFFFFFFU
  for b in data {
    crc = crc ^ (b.to_int().reinterpret_as_uint() & 0xFFU)
    for _ in 0..<8 {
      crc = if (crc & 1U) != 0U { (crc >> 1) ^ 0xEDB88320U } else { crc >> 1 }
    }
  }
  crc ^ 0xFFFFFFFFU
}

fn u16le(out : Array[Byte], v : UInt) -> Unit {
  out.push((v & 0xFFU).to_byte())
  out.push(((v >> 8) & 0xFFU).to_byte())
}

fn u32le(out : Array[Byte], v : UInt) -> Unit {
  out.push((v & 0xFFU).to_byte())
  out.push(((v >> 8) & 0xFFU).to_byte())
  out.push(((v >> 16) & 0xFFU).to_byte())
  out.push(((v >> 24) & 0xFFU).to_byte())
}

const DOS_TIME : UInt = 0U
const DOS_DATE : UInt = 0x0021U

priv struct CentralRecord {
  crc : UInt
  size : UInt
  offset : UInt
  name_bytes : FixedArray[Byte]
}

///|
pub fn archive(files : Array[File]) -> FixedArray[Byte] {
  let out : Array[Byte] = []
  let central : Array[CentralRecord] = []

  for f in files {
    let name_bytes = @utf8.encode(f.name)
    let crc = crc32(f.data)
    let size = f.data.length().reinterpret_as_uint()
    let offset = out.length().reinterpret_as_uint()

    u32le(out, 0x04034B50U)
    u16le(out, 20U) // version needed
    u16le(out, 0U)  // flags
    u16le(out, 0U)  // method = store
    u16le(out, DOS_TIME)
    u16le(out, DOS_DATE)
    u32le(out, crc)
    u32le(out, size)
    u32le(out, size)
    u16le(out, name_bytes.length().reinterpret_as_uint())
    u16le(out, 0U)  // extra len
    for b in name_bytes {
      out.push(b)
    }
    for b in f.data {
      out.push(b)
    }
    central.push({ crc, size, offset, name_bytes: name_bytes.to_fixedarray() })
  }

  let cd_offset = out.length().reinterpret_as_uint()
  for entry in central {
    u32le(out, 0x02014B50U)
    u16le(out, 20U) // version made by
    u16le(out, 20U) // version needed
    u16le(out, 0U)  // flags
    u16le(out, 0U)  // method = store
    u16le(out, DOS_TIME)
    u16le(out, DOS_DATE)
    u32le(out, entry.crc)
    u32le(out, entry.size)
    u32le(out, entry.size)
    u16le(out, entry.name_bytes.length().reinterpret_as_uint())
    u16le(out, 0U)  // extra len
    u16le(out, 0U)  // comment len
    u16le(out, 0U)  // disk number start
    u16le(out, 0U)  // internal attrs
    u32le(out, 0U)  // external attrs
    u32le(out, entry.offset)
    for b in entry.name_bytes {
      out.push(b)
    }
  }
  let cd_size = out.length().reinterpret_as_uint() - cd_offset

  u32le(out, 0x06054B50U)
  u16le(out, 0U) // this disk
  u16le(out, 0U) // disk with cd
  u16le(out, central.length().reinterpret_as_uint()) // entries this disk
  u16le(out, central.length().reinterpret_as_uint()) // total entries
  u32le(out, cd_size)
  u32le(out, cd_offset)
  u16le(out, 0U) // comment len

  FixedArray::from_array(out)
}

fn u16at(b : FixedArray[Byte], at : Int) -> Result[UInt, ZipError] {
  if at + 2 > b.length() || at < 0 {
    return Err(Truncated(at.reinterpret_as_uint()))
  }
  let b0 = b[at].to_int().reinterpret_as_uint() & 0xFFU
  let b1 = b[at + 1].to_int().reinterpret_as_uint() & 0xFFU
  Ok(b0 | (b1 << 8))
}

fn u32at(b : FixedArray[Byte], at : Int) -> Result[UInt, ZipError] {
  if at + 4 > b.length() || at < 0 {
    return Err(Truncated(at.reinterpret_as_uint()))
  }
  let b0 = b[at].to_int().reinterpret_as_uint() & 0xFFU
  let b1 = b[at + 1].to_int().reinterpret_as_uint() & 0xFFU
  let b2 = b[at + 2].to_int().reinterpret_as_uint() & 0xFFU
  let b3 = b[at + 3].to_int().reinterpret_as_uint() & 0xFFU
  Ok(b0 | (b1 << 8) | (b2 << 16) | (b3 << 24))
}

fn find_eocd(b : FixedArray[Byte]) -> Int? {
  let len = b.length()
  if len < 22 {
    return None
  }
  let mut earliest = len - (22 + 0xFFFF)
  if earliest < 0 {
    earliest = 0
  }
  let mut i = len - 22
  while i >= earliest {
    if b[i] == (0x50).to_byte() &&
       b[i + 1] == (0x4B).to_byte() &&
       b[i + 2] == (0x05).to_byte() &&
       b[i + 3] == (0x06).to_byte() {
      return Some(i)
    }
    i = i - 1
  }
  None
}

///|
pub fn extract(bytes : FixedArray[Byte]) -> Result[Array[File], ZipError] {
  let eocd = match find_eocd(bytes) {
    Some(i) => i
    None => return Err(NotAZip)
  }
  let count = match u16at(bytes, eocd + 10) {
    Ok(v) => v.to_int()
    Err(e) => return Err(e)
  }
  let mut at = match u32at(bytes, eocd + 16) {
    Ok(v) => v.to_int()
    Err(e) => return Err(e)
  }

  let out : Array[File] = []
  for _ in 0..<count {
    let sig = match u32at(bytes, at) {
      Ok(v) => v
      Err(e) => return Err(e)
    }
    if sig != 0x02014B50U {
      return Err(Truncated(at.reinterpret_as_uint()))
    }
    let comp_method = match u16at(bytes, at + 10) {
      Ok(v) => v
      Err(e) => return Err(e)
    }
    let crc = match u32at(bytes, at + 16) {
      Ok(v) => v
      Err(e) => return Err(e)
    }
    let compressed = match u32at(bytes, at + 20) {
      Ok(v) => v.to_int()
      Err(e) => return Err(e)
    }
    let _uncompressed = match u32at(bytes, at + 24) {
      Ok(v) => v.to_int()
      Err(e) => return Err(e)
    }
    let name_len = match u16at(bytes, at + 28) {
      Ok(v) => v.to_int()
      Err(e) => return Err(e)
    }
    let extra_len = match u16at(bytes, at + 30) {
      Ok(v) => v.to_int()
      Err(e) => return Err(e)
    }
    let comment_len = match u16at(bytes, at + 32) {
      Ok(v) => v.to_int()
      Err(e) => return Err(e)
    }
    let local_hdr = match u32at(bytes, at + 42) {
      Ok(v) => v.to_int()
      Err(e) => return Err(e)
    }

    if at + 46 + name_len > bytes.length() {
      return Err(Truncated((at + 46).reinterpret_as_uint()))
    }
    let name_slice = FixedArray::make(name_len, (0).to_byte())
    for i in 0..<name_len {
      name_slice[i] = bytes[at + 46 + i]
    }
    let name = @utf8.decode_lossy(Bytes::from_fixedarray(name_slice)[:])
    at = at + 46 + name_len + extra_len + comment_len

    if name.has_suffix("/") {
      continue
    }

    let local_sig = match u32at(bytes, local_hdr) {
      Ok(v) => v
      Err(e) => return Err(e)
    }
    if local_sig != 0x04034B50U {
      return Err(Truncated(local_hdr.reinterpret_as_uint()))
    }
    let l_name = match u16at(bytes, local_hdr + 26) {
      Ok(v) => v.to_int()
      Err(e) => return Err(e)
    }
    let l_extra = match u16at(bytes, local_hdr + 28) {
      Ok(v) => v.to_int()
      Err(e) => return Err(e)
    }
    let start = local_hdr + 30 + l_name + l_extra
    if start + compressed > bytes.length() {
      return Err(Truncated(start.reinterpret_as_uint()))
    }
    let raw = FixedArray::make(compressed, (0).to_byte())
    for i in 0..<compressed {
      raw[i] = bytes[start + i]
    }

    let data : FixedArray[Byte] = match comp_method {
      0U => raw
      8U => {
        let raw_bytes = Bytes::from_fixedarray(raw)
        try {
          let decompressed = @flate.inflate_exact(raw_bytes[:])
          decompressed.to_fixedarray()
        } catch {
          @flate.InflateError(_, msg) => return Err(BadDeflate(msg))
          _ => return Err(BadDeflate("corrupted deflate stream"))
        }
      }
      other => return Err(UnsupportedMethod(other))
    }

    if crc32(data) != crc {
      return Err(BadChecksum(name))
    }
    out.push({ name, data })
  }
  Ok(out)
}
