const EARTH_RADIUS_M : Double = 6371008.8
const M_PER_DEG_LAT : Double = 111320.0
const PI : Double = 3.14159265358979323846264338327950288

fn to_radians(deg : Double) -> Double {
  deg * PI / 180.0
}

fn validate_point(p : Point) -> Result[Unit, GeoError] {
  if p.lat >= -90.0 && p.lat <= 90.0 && p.lon >= -180.0 && p.lon <= 180.0 {
    Ok(())
  } else {
    Err(BadCoordinate)
  }
}

pub fn distance_meters(a : Point, b : Point) -> Result[Double, GeoError] {
  match validate_point(a) {
    Ok(_) => ()
    Err(e) => return Err(e)
  }
  match validate_point(b) {
    Ok(_) => ()
    Err(e) => return Err(e)
  }

  let lat1 = to_radians(a.lat)
  let lat2 = to_radians(b.lat)
  let dlat = to_radians(b.lat - a.lat)
  let dlon = to_radians(b.lon - a.lon)

  let sin_dlat_2 = @math.sin(dlat / 2.0)
  let sin_dlon_2 = @math.sin(dlon / 2.0)
  let h = sin_dlat_2 * sin_dlat_2 + @math.cos(lat1) * @math.cos(lat2) * sin_dlon_2 * sin_dlon_2
  let d = 2.0 * EARTH_RADIUS_M * @math.asin(h.sqrt())
  Ok(d)
}

fn clamp(v : Double, min_val : Double, max_val : Double) -> Double {
  if v < min_val {
    min_val
  } else if v > max_val {
    max_val
  } else {
    v
  }
}

fn abs(v : Double) -> Double {
  if v < 0.0 { -v } else { v }
}

pub fn bounding_box(center : Point, radius_meters : Double) -> Result[Bbox, GeoError] {
  match validate_point(center) {
    Ok(_) => ()
    Err(e) => return Err(e)
  }

  let dlat_deg = radius_meters / M_PER_DEG_LAT
  let cos_lat = @math.cos(to_radians(center.lat))
  let (min_lon, max_lon) = if abs(cos_lat) < 1.0e-12 {
    (-180.0, 180.0)
  } else {
    let dlon_deg = radius_meters / abs(M_PER_DEG_LAT * cos_lat)
    (clamp(center.lon - dlon_deg, -180.0, 180.0), clamp(center.lon + dlon_deg, -180.0, 180.0))
  }

  Ok({
    min_lat: clamp(center.lat - dlat_deg, -90.0, 90.0),
    min_lon,
    max_lat: clamp(center.lat + dlat_deg, -90.0, 90.0),
    max_lon,
  })
}

pub fn contains(box : Bbox, p : Point) -> Bool {
  p.lat >= box.min_lat && p.lat <= box.max_lat && p.lon >= box.min_lon && p.lon <= box.max_lon
}

fn parse_ipv4_view(s : StringView) -> Result[FixedArray[Int], Unit] {
  let parts = s.split(".").collect()
  if parts.length() != 4 {
    return Err(())
  }
  let octets : FixedArray[Int] = [0, 0, 0, 0]
  for i = 0; i < 4; i = i + 1 {
    let p = parts[i]
    if p.is_empty() || (p.length() > 1 && p[0] == '0') {
      return Err(())
    }
    let mut n = 0
    for j = 0; j < p.length(); j = j + 1 {
      let c = p[j]
      if c < '0' || c > '9' {
        return Err(())
      }
      n = n * 10 + (c.to_int() - '0'.to_int())
    }
    if n > 255 {
      return Err(())
    }
    octets[i] = n
  }
  Ok(octets)
}

fn parse_ipv4(s : String) -> Result[FixedArray[Int], Unit] {
  parse_ipv4_view(s[:])
}

fn classify_v4(o : FixedArray[Int]) -> IpClass {
  // Loopback: 127.0.0.0/8
  if o[0] == 127 {
    return LOOPBACK
  }
  // Private:
  // 10.0.0.0/8
  if o[0] == 10 {
    return PRIVATE
  }
  // 172.16.0.0/12: 172.16.0.0 - 172.31.255.255
  if o[0] == 172 && o[1] >= 16 && o[1] <= 31 {
    return PRIVATE
  }
  // 192.168.0.0/16
  if o[0] == 192 && o[1] == 168 {
    return PRIVATE
  }
  // Special:
  // 0.0.0.0/8 (unspecified)
  if o[0] == 0 {
    return SPECIAL
  }
  // 255.255.255.255 (broadcast)
  if o[0] == 255 && o[1] == 255 && o[2] == 255 && o[3] == 255 {
    return SPECIAL
  }
  // 224.0.0.0/4 (multicast: 224 - 239)
  if o[0] >= 224 && o[0] <= 239 {
    return SPECIAL
  }
  // 169.254.0.0/16 (link-local)
  if o[0] == 169 && o[1] == 254 {
    return SPECIAL
  }

  PUBLIC
}

fn parse_hex_digit(u : UInt16) -> Result[Int, Unit] {
  let c = u.to_int()
  if c >= '0'.to_int() && c <= '9'.to_int() {
    Ok(c - '0'.to_int())
  } else if c >= 'a'.to_int() && c <= 'f'.to_int() {
    Ok(c - 'a'.to_int() + 10)
  } else if c >= 'A'.to_int() && c <= 'F'.to_int() {
    Ok(c - 'A'.to_int() + 10)
  } else {
    Err(())
  }
}

// Parses IPv6, returning 16 bytes
fn parse_ipv6(s : String) -> Result[FixedArray[Int], Unit] {
  if s.is_empty() {
    return Err(())
  }

  // Check for IPv4-mapped, e.g. ::ffff:127.0.0.1
  let mut last_colon = -1
  for i = s.length() - 1; i >= 0; i = i - 1 {
    if s[i] == ':' {
      last_colon = i
      break
    }
  }

  if last_colon >= 0 {
    let sub = s[last_colon + 1:]
    match parse_ipv4_view(sub) {
      Ok(v4) => {
        let prefix = s[0:last_colon].to_owned()
        if prefix == "::ffff" || prefix == "0:0:0:0:0:ffff" || prefix == "::FFFF" {
          let bytes : FixedArray[Int] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xFF, 0xFF, v4[0], v4[1], v4[2], v4[3]]
          return Ok(bytes)
        }
      }
      Err(_) => ()
    }
  }

  // Find double colon "::"
  let mut dc_idx = -1
  for i = 0; i < s.length() - 1; i = i + 1 {
    if s[i] == ':' && s[i + 1] == ':' {
      dc_idx = i
      break
    }
  }

  if dc_idx >= 0 {
    // Only one double colon allowed
    let rest = s[dc_idx + 2:]
    for i = 0; i < rest.length() - 1; i = i + 1 {
      if rest[i] == ':' && rest[i + 1] == ':' {
        return Err(())
      }
    }
    let left_str = s[0:dc_idx]
    let right_str = rest

    let left_parts = if left_str.is_empty() { [] } else { left_str.split(":").collect() }
    let right_parts = if right_str.is_empty() { [] } else { right_str.split(":").collect() }

    let total_parts = left_parts.length() + right_parts.length()
    if total_parts > 7 {
      return Err(())
    }

    let words : FixedArray[Int] = [0, 0, 0, 0, 0, 0, 0, 0]
    for i = 0; i < left_parts.length(); i = i + 1 {
      let p = left_parts[i]
      if p.is_empty() || p.length() > 4 { return Err(()) }
      let mut val = 0
      for j = 0; j < p.length(); j = j + 1 {
        let h = match parse_hex_digit(p[j]) { Ok(d) => d; Err(_) => return Err(()) }
        val = (val << 4) | h
      }
      words[i] = val
    }

    let fill_count = 8 - total_parts
    let right_start = left_parts.length() + fill_count
    for i = 0; i < right_parts.length(); i = i + 1 {
      let p = right_parts[i]
      if p.is_empty() || p.length() > 4 { return Err(()) }
      let mut val = 0
      for j = 0; j < p.length(); j = j + 1 {
        let h = match parse_hex_digit(p[j]) { Ok(d) => d; Err(_) => return Err(()) }
        val = (val << 4) | h
      }
      words[right_start + i] = val
    }

    let bytes : FixedArray[Int] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
    for i = 0; i < 8; i = i + 1 {
      bytes[i * 2] = (words[i] >> 8) & 0xFF
      bytes[i * 2 + 1] = words[i] & 0xFF
    }
    return Ok(bytes)
  }

  let parts = s.split(":").collect()
  if parts.length() != 8 {
    return Err(())
  }
  let bytes : FixedArray[Int] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
  for i = 0; i < 8; i = i + 1 {
    let p = parts[i]
    if p.is_empty() || p.length() > 4 { return Err(()) }
    let mut val = 0
    for j = 0; j < p.length(); j = j + 1 {
      let h = match parse_hex_digit(p[j]) { Ok(d) => d; Err(_) => return Err(()) }
      val = (val << 4) | h
    }
    bytes[i * 2] = (val >> 8) & 0xFF
    bytes[i * 2 + 1] = val & 0xFF
  }
  Ok(bytes)
}

fn classify_v6(b : FixedArray[Int]) -> IpClass {
  // Loopback: ::1
  let mut is_loopback = true
  for i = 0; i < 15; i = i + 1 {
    if b[i] != 0 {
      is_loopback = false
      break
    }
  }
  if is_loopback && b[15] == 1 {
    return LOOPBACK
  }

  // Unspecified: ::
  let mut is_unspecified = true
  for i = 0; i < 16; i = i + 1 {
    if b[i] != 0 {
      is_unspecified = false
      break
    }
  }
  if is_unspecified {
    return SPECIAL
  }

  // Unique local: fc00::/7 -> (b[0] & 0xfe) == 0xfc
  if (b[0] & 0xFE) == 0xFC {
    return PRIVATE
  }

  // Multicast: ff00::/8
  if b[0] == 0xFF {
    return SPECIAL
  }

  // Link-local: fe80::/10 -> b[0] == 0xfe && (b[1] & 0xc0) == 0x80
  if b[0] == 0xFE && (b[1] & 0xC0) == 0x80 {
    return SPECIAL
  }

  // IPv4-mapped: ::ffff:a.b.c.d
  let mut is_mapped = true
  for i = 0; i < 10; i = i + 1 {
    if b[i] != 0 { is_mapped = false; break }
  }
  if is_mapped && b[10] == 0xFF && b[11] == 0xFF {
    let v4 : FixedArray[Int] = [b[12], b[13], b[14], b[15]]
    return classify_v4(v4)
  }

  PUBLIC
}

pub fn classify_ip(ip : String) -> Result[IpClass, GeoError] {
  match parse_ipv4(ip) {
    Ok(v4) => Ok(classify_v4(v4))
    Err(_) => {
      match parse_ipv6(ip) {
        Ok(v6) => Ok(classify_v6(v6))
        Err(_) => Err(BadIp)
      }
    }
  }
}
