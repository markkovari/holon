// MIME email parser (RFC 5322 / RFC 2045-2047).

priv struct Header {
  key : String
  value : String
}

fn char_at(s : String, i : Int) -> Char {
  s[i].to_int().unsafe_to_char()
}

fn is_ws(c : Char) -> Bool {
  c == ' ' || c == '\t' || c == '\n' || c == '\r'
}

fn sub_str(s : String, start : Int, end : Int) -> String {
  let len = s.length()
  let actual_start = if start < 0 { 0 } else { start }
  let actual_end = if end > len { len } else { end }
  if actual_start >= actual_end {
    return ""
  }
  let buf = StringBuilder()
  for i = actual_start; i < actual_end; i = i + 1 {
    buf.write_char(char_at(s, i))
  }
  buf.to_string()
}

fn trim_str(s : String) -> String {
  let len = s.length()
  let mut start = 0
  while start < len && is_ws(char_at(s, start)) {
    start = start + 1
  }
  let mut end = len
  while end > start && is_ws(char_at(s, end - 1)) {
    end = end - 1
  }
  sub_str(s, start, end)
}

fn find_char(s : String, ch : Char, from : Int) -> Int? {
  let len = s.length()
  for i = from; i < len; i = i + 1 {
    if char_at(s, i) == ch {
      return Some(i)
    }
  }
  None
}

fn find_str(s : String, needle : String, from : Int) -> Int? {
  let s_len = s.length()
  let n_len = needle.length()
  if n_len == 0 {
    return Some(from)
  }
  if from + n_len > s_len {
    return None
  }
  for i = from; i <= s_len - n_len; i = i + 1 {
    let mut matched = true
    for j = 0; j < n_len; j = j + 1 {
      if s[i + j] != needle[j] {
        matched = false
        break
      }
    }
    if matched {
      return Some(i)
    }
  }
  None
}

fn str_starts_with(s : String, prefix : String) -> Bool {
  let p_len = prefix.length()
  if s.length() < p_len {
    return false
  }
  for i = 0; i < p_len; i = i + 1 {
    if s[i] != prefix[i] {
      return false
    }
  }
  true
}

fn str_ends_with(s : String, suffix : String) -> Bool {
  let s_len = s.length()
  let suf_len = suffix.length()
  if s_len < suf_len {
    return false
  }
  let offset = s_len - suf_len
  for i = 0; i < suf_len; i = i + 1 {
    if s[offset + i] != suffix[i] {
      return false
    }
  }
  true
}

fn str_contains(s : String, needle : String) -> Bool {
  find_str(s, needle, 0) is Some(_)
}

fn to_lower_ascii(s : String) -> String {
  let buf = StringBuilder()
  for c in s {
    let code = c.to_int()
    if code >= 'A'.to_int() && code <= 'Z'.to_int() {
      buf.write_char((code + 32).unsafe_to_char())
    } else {
      buf.write_char(c)
    }
  }
  buf.to_string()
}

fn b64_char_val(c : Char) -> Int {
  let n = c.to_int()
  if n >= 'A'.to_int() && n <= 'Z'.to_int() {
    n - 'A'.to_int()
  } else if n >= 'a'.to_int() && n <= 'z'.to_int() {
    n - 'a'.to_int() + 26
  } else if n >= '0'.to_int() && n <= '9'.to_int() {
    n - '0'.to_int() + 52
  } else if c == '+' {
    62
  } else if c == '/' {
    63
  } else {
    -1
  }
}

fn decode_b64(text : String) -> String {
  let values : Array[Int] = []
  for c in text {
    if is_ws(c) || c == '=' {
      continue
    }
    let v = b64_char_val(c)
    if v >= 0 {
      values.push(v)
    }
  }
  let bytes : Array[Byte] = []
  let mut i = 0
  while i + 4 <= values.length() {
    let n = (values[i] << 18) | (values[i + 1] << 12) | (values[i + 2] << 6) | values[i + 3]
    bytes.push(((n >> 16) & 0xFF).to_byte())
    bytes.push(((n >> 8) & 0xFF).to_byte())
    bytes.push((n & 0xFF).to_byte())
    i = i + 4
  }
  let rest = values.length() - i
  if rest == 2 {
    let n = (values[i] << 18) | (values[i + 1] << 12)
    bytes.push(((n >> 16) & 0xFF).to_byte())
  } else if rest == 3 {
    let n = (values[i] << 18) | (values[i + 1] << 12) | (values[i + 2] << 6)
    bytes.push(((n >> 16) & 0xFF).to_byte())
    bytes.push(((n >> 8) & 0xFF).to_byte())
  }
  @utf8.decode_lossy(Bytes::from_array(bytes)[:])
}

fn hex_val(c : Char) -> Int? {
  let code = c.to_int()
  if code >= '0'.to_int() && code <= '9'.to_int() {
    Some(code - '0'.to_int())
  } else if code >= 'a'.to_int() && code <= 'f'.to_int() {
    Some(code - 'a'.to_int() + 10)
  } else if code >= 'A'.to_int() && code <= 'F'.to_int() {
    Some(code - 'A'.to_int() + 10)
  } else {
    None
  }
}

fn decode_quoted_printable(s : String, is_q_word : Bool) -> String {
  let bytes : Array[Byte] = []
  let len = s.length()
  let mut i = 0
  while i < len {
    let c = char_at(s, i)
    if is_q_word && c == '_' {
      bytes.push((32).to_byte())
      i = i + 1
      continue
    }
    if c == '=' {
      if i + 2 < len && char_at(s, i + 1) == '\r' && char_at(s, i + 2) == '\n' {
        i = i + 3
        continue
      }
      if i + 1 < len && char_at(s, i + 1) == '\n' {
        i = i + 2
        continue
      }
      if i + 2 < len {
        match (hex_val(char_at(s, i + 1)), hex_val(char_at(s, i + 2))) {
          (Some(h1), Some(h2)) => {
            bytes.push(((h1 << 4) | h2).to_byte())
            i = i + 3
            continue
          }
          _ => ()
        }
      }
    }
    let code = c.to_int()
    if code <= 0x7F {
      bytes.push(code.to_byte())
    } else {
      let encoded = @utf8.encode(c.to_string())
      for b in encoded {
        bytes.push(b)
      }
    }
    i = i + 1
  }
  @utf8.decode_lossy(Bytes::from_array(bytes)[:])
}

fn decode_mime_word(word : String) -> String {
  if !str_starts_with(word, "=?") || !str_ends_with(word, "?=") {
    return word
  }
  let inner = sub_str(word, 2, word.length() - 2)
  let q1 = match find_char(inner, '?', 0) {
    Some(idx) => idx
    None => return word
  }
  let q2 = match find_char(inner, '?', q1 + 1) {
    Some(idx) => idx
    None => return word
  }
  let enc = to_lower_ascii(sub_str(inner, q1 + 1, q2))
  let encoded_text = sub_str(inner, q2 + 1, inner.length())
  if enc == "b" {
    decode_b64(encoded_text)
  } else if enc == "q" {
    decode_quoted_printable(encoded_text, true)
  } else {
    word
  }
}

fn decode_header_value(v : String) -> String {
  if !str_contains(v, "=?") {
    return v
  }
  let out = StringBuilder()
  let mut pos = 0
  let len = v.length()
  while pos < len {
    match find_str(v, "=?", pos) {
      None => {
        out.write_string(sub_str(v, pos, len))
        break
      }
      Some(start_idx) => {
        out.write_string(sub_str(v, pos, start_idx))
        match find_str(v, "?=", start_idx + 2) {
          None => {
            out.write_string(sub_str(v, start_idx, len))
            break
          }
          Some(end_idx) => {
            let word = sub_str(v, start_idx, end_idx + 2)
            out.write_string(decode_mime_word(word))
            pos = end_idx + 2
          }
        }
      }
    }
  }
  out.to_string()
}

fn parse_headers_and_body(text : String) -> (Array[Header], String) {
  let (header_str, body_str) = match find_str(text, "\r\n\r\n", 0) {
    Some(idx) => (sub_str(text, 0, idx), sub_str(text, idx + 4, text.length()))
    None => match find_str(text, "\n\n", 0) {
      Some(idx) => (sub_str(text, 0, idx), sub_str(text, idx + 2, text.length()))
      None => (text, "")
    }
  }

  let raw_lines : Array[String] = []
  let mut cur_pos = 0
  let h_len = header_str.length()
  while cur_pos < h_len {
    let next_nl = match find_char(header_str, '\n', cur_pos) {
      Some(idx) => idx
      None => h_len
    }
    let mut line = sub_str(header_str, cur_pos, next_nl)
    if str_ends_with(line, "\r") {
      line = sub_str(line, 0, line.length() - 1)
    }
    raw_lines.push(line)
    cur_pos = next_nl + 1
  }

  let unfolded : Array[String] = []
  for line in raw_lines {
    if line.length() == 0 {
      continue
    }
    if str_starts_with(line, " ") || str_starts_with(line, "\t") {
      if unfolded.length() > 0 {
        let last = unfolded[unfolded.length() - 1]
        unfolded[unfolded.length() - 1] = last + " " + trim_str(line)
      }
    } else {
      unfolded.push(line)
    }
  }

  let headers : Array[Header] = []
  for l in unfolded {
    match find_char(l, ':', 0) {
      Some(colon) => {
        let k = to_lower_ascii(trim_str(sub_str(l, 0, colon)))
        let v = decode_header_value(trim_str(sub_str(l, colon + 1, l.length())))
        headers.push({ key: k, value: v })
      }
      None => ()
    }
  }

  (headers, body_str)
}

fn get_header(headers : Array[Header], name : String) -> String? {
  let target = to_lower_ascii(name)
  for h in headers {
    if h.key == target {
      return Some(h.value)
    }
  }
  None
}

fn parse_param(header_val : String, param_name : String) -> String? {
  let lower = to_lower_ascii(header_val)
  let target = to_lower_ascii(param_name) + "="
  let idx = match find_str(lower, target, 0) {
    Some(i) => i
    None => return None
  }
  let mut start = idx + target.length()
  let len = header_val.length()
  if start < len && char_at(header_val, start) == '"' {
    start = start + 1
    let mut end = start
    while end < len && char_at(header_val, end) != '"' {
      end = end + 1
    }
    Some(sub_str(header_val, start, end))
  } else {
    let mut end = start
    while end < len && char_at(header_val, end) != ';' && !is_ws(char_at(header_val, end)) {
      end = end + 1
    }
    Some(sub_str(header_val, start, end))
  }
}

fn decode_body(body : String, encoding : String) -> String {
  let enc = to_lower_ascii(trim_str(encoding))
  if enc == "base64" {
    decode_b64(body)
  } else if enc == "quoted-printable" {
    decode_quoted_printable(body, false)
  } else {
    body
  }
}

priv struct Extracted {
  mut text : String
  mut html : String?
}

fn split_by_delim(s : String, delim : String) -> Array[String] {
  let res : Array[String] = []
  let mut pos = 0
  let s_len = s.length()
  let d_len = delim.length()
  while pos < s_len {
    match find_str(s, delim, pos) {
      Some(idx) => {
        res.push(sub_str(s, pos, idx))
        pos = idx + d_len
      }
      None => {
        res.push(sub_str(s, pos, s_len))
        break
      }
    }
  }
  res
}

fn extract_parts(headers : Array[Header], body : String, out : Extracted) -> Unit {
  let content_type = match get_header(headers, "content-type") {
    Some(ct) => ct
    None => "text/plain"
  }
  let transfer_encoding = match get_header(headers, "content-transfer-encoding") {
    Some(te) => te
    None => ""
  }
  let lower_ct = to_lower_ascii(content_type)

  if str_contains(lower_ct, "multipart/") {
    match parse_param(content_type, "boundary") {
      Some(boundary) => {
        let delim = "--" + boundary
        let parts = split_by_delim(body, delim)
        for p in parts {
          let trimmed = trim_str(p)
          if trimmed.length() == 0 || str_starts_with(trimmed, "--") {
            continue
          }
          let (sub_headers, sub_body) = parse_headers_and_body(trimmed)
          extract_parts(sub_headers, sub_body, out)
        }
      }
      None => ()
    }
  } else if str_contains(lower_ct, "text/plain") {
    if out.text.length() == 0 {
      out.text = decode_body(body, transfer_encoding)
    }
  } else if str_contains(lower_ct, "text/html") {
    if out.html is None {
      out.html = Some(decode_body(body, transfer_encoding))
    }
  } else if !str_contains(lower_ct, "/") {
    // Default to plain text
    if out.text.length() == 0 {
      out.text = decode_body(body, transfer_encoding)
    }
  }
}

///|
pub fn parse(raw : FixedArray[Byte]) -> Result[Email, ParseError] {
  let text = @utf8.decode_lossy(Bytes::from_array(raw)[:])
  let (headers, body) = parse_headers_and_body(text)

  let sender = match get_header(headers, "from") {
    Some(s) => s
    None => return Err(Malformed("Missing From header"))
  }

  let subject = match get_header(headers, "subject") {
    Some(s) => s
    None => ""
  }
  let in_reply_to = get_header(headers, "in-reply-to")

  let extracted : Extracted = { text: "", html: None }
  extract_parts(headers, body, extracted)

  Ok({
    sender,
    subject,
    text: extracted.text,
    html: extracted.html,
    in_reply_to,
  })
}
