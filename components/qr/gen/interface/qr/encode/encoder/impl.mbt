fn to_ec(level : Ecc) -> @core.EcLevel {
  match level {
    LOW => @core.EcLevel::L
    MEDIUM => @core.EcLevel::M
    QUARTILE => @core.EcLevel::Q
    HIGH => @core.EcLevel::H
  }
}

fn get_qr(data : String, level : Ecc) -> @core.QrCode? {
  let ec = to_ec(level)
  try Some(@core.encode(data, ec_level=ec)) catch {
    _ => None
  }
}

pub fn svg(data : String, level : Ecc, quiet_zone : UInt) -> Result[String, QrError] {
  let qr = match get_qr(data, level) {
    Some(q) => q
    None => return Err(QrError::TooLong("Failed to encode QR code: input too long"))
  }
  let w = qr.size()
  let qz = quiet_zone.to_int()
  let dim = w + 2 * qz
  let path = StringBuilder::new()
  for y = 0; y < w; y = y + 1 {
    for x = 0; x < w; x = x + 1 {
      if qr.get(y, x) {
        path.write_string("M")
        path.write_string((x + qz).to_string())
        path.write_char(' ')
        path.write_string((y + qz).to_string())
        path.write_string("h1v1h-1z")
      }
    }
  }
  let res = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 " + dim.to_string() + " " + dim.to_string() + "\" shape-rendering=\"crispEdges\"><rect width=\"" + dim.to_string() + "\" height=\"" + dim.to_string() + "\" fill=\"#fff\"/><path fill=\"#000\" d=\"" + path.to_string() + "\"/></svg>"
  Ok(res)
}

pub fn unicode(data : String, level : Ecc) -> Result[String, QrError] {
  let qr = match get_qr(data, level) {
    Some(q) => q
    None => return Err(QrError::TooLong("Failed to encode QR code: input too long"))
  }
  let w = qr.size()
  let dark = fn(x : Int, y : Int) -> Bool {
    if x >= 0 && y >= 0 && x < w && y < w {
      qr.get(y, x)
    } else {
      false
    }
  }
  let out = StringBuilder::new()
  let mut y = -1
  while y < w + 1 {
    for x = -1; x < w + 1; x = x + 1 {
      let top = dark(x, y)
      let bot = dark(x, y + 1)
      if top && bot {
        out.write_char('█')
      } else if top && !bot {
        out.write_char('▀')
      } else if !top && bot {
        out.write_char('▄')
      } else {
        out.write_char(' ')
      }
    }
    out.write_char('\n')
    y = y + 2
  }
  Ok(out.to_string())
}

pub fn matrix(data : String, level : Ecc) -> Result[String, QrError] {
  let qr = match get_qr(data, level) {
    Some(q) => q
    None => return Err(QrError::TooLong("Failed to encode QR code: input too long"))
  }
  let w = qr.size()
  let out = StringBuilder::new()
  out.write_string("{\"size\":")
  out.write_string(w.to_string())
  out.write_string(",\"modules\":[")
  for y = 0; y < w; y = y + 1 {
    if y > 0 {
      out.write_char(',')
    }
    out.write_char('[')
    for x = 0; x < w; x = x + 1 {
      if x > 0 {
        out.write_char(',')
      }
      if qr.get(y, x) {
        out.write_string("true")
      } else {
        out.write_string("false")
      }
    }
    out.write_char(']')
  }
  out.write_string("]}")
  Ok(out.to_string())
}
