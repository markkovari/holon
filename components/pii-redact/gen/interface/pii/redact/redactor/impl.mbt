// Implementation of pii:redact@0.1.0 redactor interface in MoonBit

fn is_digit(c : Char) -> Bool {
  c >= '0' && c <= '9'
}

fn is_alpha(c : Char) -> Bool {
  (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z')
}

fn is_local(c : Char) -> Bool {
  is_alpha(c) || is_digit(c) || c == '.' || c == '_' || c == '%' || c == '+' || c == '-'
}

fn is_domain(c : Char) -> Bool {
  is_alpha(c) || is_digit(c) || c == '.' || c == '-'
}

fn domain_has_tld(chars : Array[Char], start : Int, end : Int) -> Bool {
  let mut cur_end = end
  while cur_end > start && (chars[cur_end - 1] == '.' || chars[cur_end - 1] == '-') {
    cur_end = cur_end - 1
  }
  let mut dot = -1
  let mut p = cur_end - 1
  while p >= start {
    if chars[p] == '.' {
      dot = p
      break
    }
    p = p - 1
  }
  if dot < 0 || dot == start {
    return false
  }
  let tld_len = cur_end - (dot + 1)
  if tld_len < 2 {
    return false
  }
  for i = dot + 1; i < cur_end; i = i + 1 {
    if !is_alpha(chars[i]) {
      return false
    }
  }
  true
}

fn scan_email(chars : Array[Char]) -> Array[(Int, Int)] {
  let n = chars.length()
  let out : Array[(Int, Int)] = []
  let mut i = 0
  while i < n {
    if chars[i] != '@' {
      i = i + 1
      continue
    }
    let mut ls = i
    while ls > 0 && is_local(chars[ls - 1]) {
      ls = ls - 1
    }
    let mut de = i + 1
    while de < n && is_domain(chars[de]) {
      de = de + 1
    }
    let local_ok = ls < i
    let valid_domain = domain_has_tld(chars, i + 1, de)
    if local_ok && valid_domain {
      let mut cur_end = de
      while cur_end > i + 1 && (chars[cur_end - 1] == '.' || chars[cur_end - 1] == '-') {
        cur_end = cur_end - 1
      }
      out.push((ls, cur_end - ls))
      i = cur_end
    } else {
      i = i + 1
    }
  }
  out
}

pub fn luhn(digits : Array[Int]) -> Bool {
  let mut sum = 0
  let mut dbl = false
  let mut p = digits.length() - 1
  while p >= 0 {
    let mut v = digits[p]
    if dbl {
      v = v * 2
      if v > 9 {
        v = v - 9
      }
    }
    sum = sum + v
    dbl = !dbl
    p = p - 1
  }
  sum % 10 == 0
}

fn next_non_card(chars : Array[Char], start : Int) -> Int {
  let mut i = start
  let n = chars.length()
  while i < n {
    let c = chars[i]
    if is_digit(c) || c == ' ' || c == '-' {
      i = i + 1
    } else {
      break
    }
  }
  if i == start {
    start + 1
  } else {
    i
  }
}

fn scan_credit_card(chars : Array[Char]) -> Array[(Int, Int)] {
  let n = chars.length()
  let out : Array[(Int, Int)] = []
  let zero = '0'.to_int()
  let mut i = 0
  while i < n {
    if !is_digit(chars[i]) {
      i = i + 1
      continue
    }
    let start = i
    let digits : Array[Int] = []
    let mut j = i
    while j < n {
      let c = chars[j]
      if is_digit(c) {
        digits.push(c.to_int() - zero)
        j = j + 1
      } else if (c == ' ' || c == '-') && j + 1 < n && is_digit(chars[j + 1]) && digits.length() > 0 {
        j = j + 1
      } else {
        break
      }
    }
    let cur_end = j
    let count = digits.length()
    if count >= 13 && count <= 19 && luhn(digits) {
      out.push((start, cur_end - start))
      i = cur_end
    } else {
      i = next_non_card(chars, start)
    }
  }
  out
}

fn scan_ssn(chars : Array[Char]) -> Array[(Int, Int)] {
  let n = chars.length()
  let out : Array[(Int, Int)] = []
  let mut i = 0
  while i + 11 <= n {
    if i > 0 && is_digit(chars[i - 1]) {
      i = i + 1
      continue
    }
    let shape = is_digit(chars[i]) &&
      is_digit(chars[i + 1]) &&
      is_digit(chars[i + 2]) &&
      chars[i + 3] == '-' &&
      is_digit(chars[i + 4]) &&
      is_digit(chars[i + 5]) &&
      chars[i + 6] == '-' &&
      is_digit(chars[i + 7]) &&
      is_digit(chars[i + 8]) &&
      is_digit(chars[i + 9]) &&
      is_digit(chars[i + 10])
    let clean_tail = i + 11 == n || !is_digit(chars[i + 11])
    if shape && clean_tail {
      out.push((i, 11))
      i = i + 11
    } else {
      i = i + 1
    }
  }
  out
}

fn matches_pat(chars : Array[Char], start : Int, len : Int, pat : Array[Char]) -> Bool {
  if len != pat.length() {
    return false
  }
  for k = 0; k < len; k = k + 1 {
    let p = pat[k]
    if p == 'N' {
      if !is_digit(chars[start + k]) {
        return false
      }
    } else if chars[start + k] != p {
      return false
    }
  }
  true
}

let nanp_pat1 : Array[Char] = ['N', 'N', 'N', '-', 'N', 'N', 'N', '-', 'N', 'N', 'N', 'N']
let nanp_pat2 : Array[Char] = ['(', 'N', 'N', 'N', ')', ' ', 'N', 'N', 'N', '-', 'N', 'N', 'N', 'N']

fn looks_nanp(chars : Array[Char], start : Int, len : Int) -> Bool {
  matches_pat(chars, start, len, nanp_pat1) || matches_pat(chars, start, len, nanp_pat2)
}

fn scan_phone(chars : Array[Char]) -> Array[(Int, Int)] {
  let n = chars.length()
  let out : Array[(Int, Int)] = []
  let mut i = 0
  while i < n {
    let c0 = chars[i]
    let plus = c0 == '+'
    let starts = plus || c0 == '(' || is_digit(c0)
    if !starts {
      i = i + 1
      continue
    }
    if i > 0 {
      let prev = chars[i - 1]
      if is_digit(prev) || prev == '+' || prev == ')' {
        i = i + 1
        continue
      }
    }
    let start = i
    let mut digit_count = 0
    let mut j = i
    while j < n {
      let c = chars[j]
      if is_digit(c) {
        digit_count = digit_count + 1
        j = j + 1
      } else if c == ' ' || c == '-' || c == '(' || c == ')' || c == '+' {
        j = j + 1
      } else {
        break
      }
    }
    let mut cur_end = j
    while cur_end > start && !is_digit(chars[cur_end - 1]) {
      cur_end = cur_end - 1
    }
    let span_len = cur_end - start
    let nanp = looks_nanp(chars, start, span_len)
    if digit_count >= 10 && digit_count <= 15 && (plus || nanp) {
      out.push((start, span_len))
      i = j
    } else {
      i = i + 1
    }
  }
  out
}

fn parse_ipv4(chars : Array[Char], i : Int) -> Int? {
  let n = chars.length()
  let mut pos = i
  let zero = '0'.to_int()
  for octet = 0; octet < 4; octet = octet + 1 {
    let oct_start = pos
    let mut val = 0
    let mut len = 0
    while pos < n && is_digit(chars[pos]) && len < 3 {
      val = val * 10 + (chars[pos].to_int() - zero)
      pos = pos + 1
      len = len + 1
    }
    if len == 0 || val > 255 {
      return None
    }
    if len > 1 && chars[oct_start] == '0' {
      return None
    }
    if octet < 3 {
      if pos >= n || chars[pos] != '.' {
        return None
      }
      pos = pos + 1
    }
  }
  Some(pos)
}

fn scan_ip(chars : Array[Char]) -> Array[(Int, Int)] {
  let n = chars.length()
  let out : Array[(Int, Int)] = []
  let mut i = 0
  while i < n {
    if !is_digit(chars[i]) {
      i = i + 1
      continue
    }
    if i > 0 {
      let prev = chars[i - 1]
      if is_digit(prev) || prev == '.' {
        i = i + 1
        continue
      }
    }
    match parse_ipv4(chars, i) {
      Some(cur_end) => {
        let clean = cur_end == n || (!is_digit(chars[cur_end]) && chars[cur_end] != '.')
        if clean {
          out.push((i, cur_end - i))
          i = cur_end
          continue
        }
      }
      None => ()
    }
    i = i + 1
  }
  out
}

let priority : Array[Kind] = [CREDIT_CARD, SSN, EMAIL, IP, PHONE]

fn wanted(opts : Options) -> Array[Kind] {
  if opts.kinds.is_empty() {
    priority
  } else {
    let out : Array[Kind] = []
    for k in priority {
      let mut found = false
      for w in opts.kinds {
        if Kind::ordinal(w) == Kind::ordinal(k) {
          found = true
          break
        }
      }
      if found {
        out.push(k)
      }
    }
    out
  }
}

priv struct InternalFinding {
  kind : Kind
  start_char : Int
  len_char : Int
  start_byte : Int
  len_byte : Int
}

fn overlaps_internal(found : Array[InternalFinding], start : Int, length : Int) -> Bool {
  let cur_end = start + length
  for f in found {
    let fs = f.start_char
    let fe = fs + f.len_char
    if start < fe && fs < cur_end {
      return true
    }
  }
  false
}

fn compute_utf8_offsets(chars : Array[Char]) -> Array[Int] {
  let n = chars.length()
  let offsets = Array::make(n + 1, 0)
  let mut byte_pos = 0
  for i = 0; i < n; i = i + 1 {
    offsets[i] = byte_pos
    let c = chars[i].to_int()
    if c < 0x80 {
      byte_pos = byte_pos + 1
    } else if c < 0x800 {
      byte_pos = byte_pos + 2
    } else if c < 0x10000 {
      byte_pos = byte_pos + 3
    } else {
      byte_pos = byte_pos + 4
    }
  }
  offsets[n] = byte_pos
  offsets
}

fn chars_of_string(s : String) -> Array[Char] {
  let out : Array[Char] = []
  for c in s {
    out.push(c)
  }
  out
}

fn scan_internal(chars : Array[Char], opts : Options) -> Array[InternalFinding] {
  let offsets = compute_utf8_offsets(chars)
  let out : Array[InternalFinding] = []
  for kind in wanted(opts) {
    let candidates = match kind {
      EMAIL => scan_email(chars)
      CREDIT_CARD => scan_credit_card(chars)
      SSN => scan_ssn(chars)
      PHONE => scan_phone(chars)
      IP => scan_ip(chars)
    }
    for cand in candidates {
      let (start, len) = cand
      if !overlaps_internal(out, start, len) {
        let sb = offsets[start]
        let lb = offsets[start + len] - sb
        out.push(InternalFinding::{
          kind,
          start_char: start,
          len_char: len,
          start_byte: sb,
          len_byte: lb,
        })
      }
    }
  }
  out.sort_by(fn(a, b) { a.start_char - b.start_char })
  out
}

fn placeholder(kind : Kind) -> String {
  match kind {
    EMAIL => "[EMAIL]"
    CREDIT_CARD => "[CARD]"
    SSN => "[SSN]"
    PHONE => "[PHONE]"
    IP => "[IP]"
  }
}

fn mask_email(s : String) -> String {
  let chars = chars_of_string(s)
  let mut at_idx = -1
  for i = 0; i < chars.length(); i = i + 1 {
    if chars[i] == '@' {
      at_idx = i
      break
    }
  }
  if at_idx < 0 {
    return s
  }
  let first_local = if at_idx > 0 { chars[0] } else { 'x' }
  let mut dot_idx = -1
  for i = chars.length() - 1; i > at_idx; i = i - 1 {
    if chars[i] == '.' {
      dot_idx = i
      break
    }
  }
  let dom_head = if at_idx + 1 < chars.length() { chars[at_idx + 1] } else { 'x' }
  let buf = StringBuilder()
  buf.write_char(first_local)
  buf.write_string("***@")
  buf.write_char(dom_head)
  buf.write_string("***")
  if dot_idx > at_idx {
    buf.write_char('.')
    for i = dot_idx + 1; i < chars.length(); i = i + 1 {
      buf.write_char(chars[i])
    }
  }
  buf.to_string()
}

fn mask_card(s : String) -> String {
  let chars = chars_of_string(s)
  let digits : Array[Char] = []
  for c in chars {
    if is_digit(c) {
      digits.push(c)
    }
  }
  let count = digits.length()
  if count < 4 {
    let buf = StringBuilder()
    for _i = 0; _i < chars.length(); _i = _i + 1 {
      buf.write_char('*')
    }
    return buf.to_string()
  }
  let last4_buf = StringBuilder()
  for i = count - 4; i < count; i = i + 1 {
    last4_buf.write_char(digits[i])
  }
  let last4 = last4_buf.to_string()
  if count == 16 {
    "**** **** **** \{last4}"
  } else {
    let buf = StringBuilder()
    for _i = 0; _i < count - 4; _i = _i + 1 {
      buf.write_char('*')
    }
    buf.write_string(last4)
    buf.to_string()
  }
}

fn mask_ssn(s : String) -> String {
  let chars = chars_of_string(s)
  let digits : Array[Char] = []
  for c in chars {
    if is_digit(c) {
      digits.push(c)
    }
  }
  let last4_buf = StringBuilder()
  for i = 5; i < digits.length(); i = i + 1 {
    last4_buf.write_char(digits[i])
  }
  let last4 = last4_buf.to_string()
  "***-**-\{last4}"
}

fn mask_phone(s : String) -> String {
  let chars = chars_of_string(s)
  let mut total = 0
  for c in chars {
    if is_digit(c) {
      total = total + 1
    }
  }
  let cutoff = if total > 2 { total - 2 } else { 0 }
  let mut seen = 0
  let buf = StringBuilder()
  for c in chars {
    if is_digit(c) {
      seen = seen + 1
      if seen > cutoff {
        buf.write_char(c)
      } else {
        buf.write_char('*')
      }
    } else {
      buf.write_char(c)
    }
  }
  buf.to_string()
}

fn mask_ip(s : String) -> String {
  let chars = chars_of_string(s)
  let mut last_dot = -1
  for i = chars.length() - 1; i >= 0; i = i - 1 {
    if chars[i] == '.' {
      last_dot = i
      break
    }
  }
  if last_dot < 0 {
    s
  } else {
    let buf = StringBuilder()
    buf.write_string("***.***.***.")
    for i = last_dot + 1; i < chars.length(); i = i + 1 {
      buf.write_char(chars[i])
    }
    buf.to_string()
  }
}

fn mask_span(kind : Kind, s : String) -> String {
  match kind {
    EMAIL => mask_email(s)
    CREDIT_CARD => mask_card(s)
    SSN => mask_ssn(s)
    PHONE => mask_phone(s)
    IP => mask_ip(s)
  }
}

fn chars_to_string(chars : Array[Char], start : Int, end : Int) -> String {
  let buf = StringBuilder()
  for i = start; i < end; i = i + 1 {
    buf.write_char(chars[i])
  }
  buf.to_string()
}

fn rewrite(chars : Array[Char], findings : Array[InternalFinding], sub : (Kind, String) -> String) -> String {
  let out = StringBuilder()
  let mut cursor = 0
  for f in findings {
    let start = f.start_char
    let cur_end = start + f.len_char
    if start >= cursor {
      let before = chars_to_string(chars, cursor, start)
      out.write_string(before)
      let span_str = chars_to_string(chars, start, cur_end)
      out.write_string(sub(f.kind, span_str))
      cursor = cur_end
    }
  }
  let remaining = chars_to_string(chars, cursor, chars.length())
  out.write_string(remaining)
  out.to_string()
}

pub fn detect(text : String, opts : Options) -> Array[Finding] {
  let chars = chars_of_string(text)
  let internals = scan_internal(chars, opts)
  let out : Array[Finding] = []
  for f in internals {
    out.push(Finding::{
      kind: f.kind,
      start: f.start_byte.reinterpret_as_uint(),
      length: f.len_byte.reinterpret_as_uint(),
    })
  }
  out
}

pub fn redact(text : String, opts : Options) -> String {
  let chars = chars_of_string(text)
  let findings = scan_internal(chars, opts)
  rewrite(chars, findings, fn(k, _) { placeholder(k) })
}

pub fn mask(text : String, opts : Options) -> String {
  let chars = chars_of_string(text)
  let findings = scan_internal(chars, opts)
  rewrite(chars, findings, mask_span)
}
