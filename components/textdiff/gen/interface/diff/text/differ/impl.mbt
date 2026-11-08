// Implementation of diff:text@0.1.0 differ interface in MoonBit

fn lines(s : String) -> Array[String] {
  if s.is_empty() {
    return []
  }
  let out : Array[String] = []
  let buf = StringBuilder()
  for c in s {
    if c == '\n' {
      out.push(buf.to_string())
      buf.reset()
    } else {
      buf.write_char(c)
    }
  }
  out.push(buf.to_string())
  out
}

fn patch_lines(s : String) -> Array[String] {
  if s.is_empty() {
    return []
  }
  let s_trimmed = if s.has_suffix("\n") {
    s[0:s.length() - 1].to_owned()
  } else {
    s
  }
  lines(s_trimmed)
}

fn edit_script(a : Array[String], b : Array[String]) -> Array[Op] {
  let n = a.length()
  let m = b.length()
  let dp : Array[Array[Int]] = Array::make(n + 1, [])
  for i = 0; i <= n; i = i + 1 {
    dp[i] = Array::make(m + 1, 0)
  }
  let mut i = n - 1
  while i >= 0 {
    let mut j = m - 1
    while j >= 0 {
      if a[i] == b[j] {
        dp[i][j] = dp[i + 1][j + 1] + 1
      } else {
        let right = dp[i][j + 1]
        let down = dp[i + 1][j]
        dp[i][j] = if down > right { down } else { right }
      }
      j = j - 1
    }
    i = i - 1
  }
  let ops : Array[Op] = []
  let mut cur_i = 0
  let mut cur_j = 0
  while cur_i < n && cur_j < m {
    if a[cur_i] == b[cur_j] {
      ops.push(Equal(a[cur_i]))
      cur_i = cur_i + 1
      cur_j = cur_j + 1
    } else if dp[cur_i + 1][cur_j] >= dp[cur_i][cur_j + 1] {
      ops.push(Delete(a[cur_i]))
      cur_i = cur_i + 1
    } else {
      ops.push(Insert(b[cur_j]))
      cur_j = cur_j + 1
    }
  }
  while cur_i < n {
    ops.push(Delete(a[cur_i]))
    cur_i = cur_i + 1
  }
  while cur_j < m {
    ops.push(Insert(b[cur_j]))
    cur_j = cur_j + 1
  }
  ops
}

priv struct Row {
  op : Op
  ai : Int
  bi : Int
}

fn rows_of(ops : Array[Op]) -> Array[Row] {
  let mut ai = 0
  let mut bi = 0
  let out : Array[Row] = []
  for op in ops {
    let r = Row::{ op, ai, bi }
    match op {
      Equal(_) => {
        ai = ai + 1
        bi = bi + 1
      }
      Delete(_) => ai = ai + 1
      Insert(_) => bi = bi + 1
    }
    out.push(r)
  }
  out
}

fn is_change(op : Op) -> Bool {
  match op {
    Equal(_) => false
    _ => true
  }
}

fn build_unified(a : String, b : String, from_label : String, to_label : String, context : Int) -> String {
  let al = lines(a)
  let bl = lines(b)
  let rows = rows_of(edit_script(al, bl))
  let mut any_change = false
  for r in rows {
    if is_change(r.op) {
      any_change = true
      break
    }
  }
  if !any_change {
    return ""
  }
  let n = rows.length()
  let keep : Array[Bool] = Array::make(n, false)
  for idx = 0; idx < n; idx = idx + 1 {
    if is_change(rows[idx].op) {
      let lo = if idx > context { idx - context } else { 0 }
      let hi = if idx + context < n { idx + context } else { n - 1 }
      for k = lo; k <= hi; k = k + 1 {
        keep[k] = true
      }
    }
  }
  let out = StringBuilder()
  out.write_string("--- \{from_label}\n+++ \{to_label}\n")
  let mut s = 0
  while s < n {
    if !keep[s] {
      s = s + 1
      continue
    }
    let mut e = s
    while e + 1 < n && keep[e + 1] {
      e = e + 1
    }
    let mut a_count = 0
    let mut b_count = 0
    let mut a_start = -1
    let mut b_start = -1
    for i = s; i <= e; i = i + 1 {
      let r = rows[i]
      match r.op {
        Insert(_) => ()
        _ => {
          a_count = a_count + 1
          if a_start < 0 {
            a_start = r.ai + 1
          }
        }
      }
      match r.op {
        Delete(_) => ()
        _ => {
          b_count = b_count + 1
          if b_start < 0 {
            b_start = r.bi + 1
          }
        }
      }
    }
    if a_start < 0 {
      a_start = rows[s].ai + 1
    }
    if b_start < 0 {
      b_start = rows[s].bi + 1
    }
    out.write_string("@@ -\{a_start},\{a_count} +\{b_start},\{b_count} @@\n")
    for i = s; i <= e; i = i + 1 {
      match rows[i].op {
        Equal(t) => out.write_string(" \{t}\n")
        Delete(t) => out.write_string("-\{t}\n")
        Insert(t) => out.write_string("+\{t}\n")
      }
    }
    s = e + 1
  }
  out.to_string()
}

fn hunk_a_start(header : String) -> Int? {
  let mut minus = -1
  for i = 0; i < header.length(); i = i + 1 {
    if header[i] == '-' {
      minus = i
      break
    }
  }
  if minus < 0 {
    return None
  }
  let mut val = 0
  let mut has_digit = false
  for i = minus + 1; i < header.length(); i = i + 1 {
    let c = header[i]
    if c >= '0' && c <= '9' {
      has_digit = true
      val = val * 10 + (c.to_int() - '0'.to_int())
    } else {
      break
    }
  }
  if has_digit {
    Some(val)
  } else {
    None
  }
}

fn apply(a : String, patch : String) -> Result[String, DiffError] {
  let al = lines(a)
  let p_lines = patch_lines(patch)
  let out : Array[String] = []
  let mut ai = 0
  let mut li = 0
  let pl_len = p_lines.length()
  while li < pl_len {
    let line = p_lines[li]
    li = li + 1
    if line.has_prefix("---") || line.has_prefix("+++") {
      continue
    }
    if !line.has_prefix("@@") {
      continue
    }
    let header = line[2:line.length()].to_owned()
    let start = match hunk_a_start(header) {
      Some(s) => s
      None => return Err(MalformedPatch("bad hunk header: \{line}"))
    }
    let target = if start > 0 { start - 1 } else { 0 }
    if target > al.length() {
      return Err(ContextMismatch("hunk starts at line \{start}, past end of source (\{al.length()})"))
    }
    while ai < target {
      out.push(al[ai])
      ai = ai + 1
    }
    while li < pl_len {
      let body = p_lines[li]
      if body.has_prefix("@@") {
        break
      }
      li = li + 1
      if body.is_empty() || body.has_prefix(" ") {
        let t = if body.length() > 1 { body[1:body.length()].to_owned() } else { "" }
        if ai >= al.length() || al[ai] != t {
          return Err(ContextMismatch("context line does not match source at line \{ai + 1}"))
        }
        out.push(al[ai])
        ai = ai + 1
      } else if body.has_prefix("-") {
        let t = body[1:body.length()].to_owned()
        if ai >= al.length() || al[ai] != t {
          return Err(ContextMismatch("deleted line does not match source at line \{ai + 1}"))
        }
        ai = ai + 1
      } else if body.has_prefix("+") {
        let t = body[1:body.length()].to_owned()
        out.push(t)
      } else if body.has_prefix("\\") {
        // ignore "\ No newline at end of file"
      } else {
        return Err(MalformedPatch("unrecognized patch line: \{body}"))
      }
    }
  }
  while ai < al.length() {
    out.push(al[ai])
    ai = ai + 1
  }
  let res = StringBuilder()
  for i = 0; i < out.length(); i = i + 1 {
    if i > 0 {
      res.write_char('\n')
    }
    res.write_string(out[i])
  }
  Ok(res.to_string())
}

pub fn diff_lines(a : String, b : String) -> Array[Op] {
  edit_script(lines(a), lines(b))
}

pub fn unified(a : String, b : String, from_label : String, to_label : String, context : UInt) -> String {
  build_unified(a, b, from_label, to_label, context.reinterpret_as_int())
}

pub fn apply_unified(a : String, patch : String) -> Result[String, DiffError] {
  apply(a, patch)
}
