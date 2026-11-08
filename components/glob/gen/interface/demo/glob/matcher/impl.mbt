// Implementation of demo:glob@0.1.0 matcher interface in MoonBit

fn m(p : Array[Char], pi : Int, t : Array[Char], ti : Int) -> Bool {
  if pi == p.length() {
    return ti == t.length()
  }
  match p[pi] {
    '*' => {
      let mut k = ti
      while true {
        if m(p, pi + 1, t, k) {
          return true
        }
        if k == t.length() {
          return false
        }
        k = k + 1
      }
      false
    }
    '?' => ti < t.length() && m(p, pi + 1, t, ti + 1)
    '[' => {
      if ti >= t.length() {
        return false
      }
      let mut i = pi + 1
      let mut neg = false
      if i < p.length() && (p[i] == '!' || p[i] == '^') {
        neg = true
        i = i + 1
      }
      let mut hit = false
      let mut first = true
      while i < p.length() && (p[i] != ']' || first) {
        first = false
        if i + 2 < p.length() && p[i + 1] == '-' && p[i + 2] != ']' {
          if t[ti] >= p[i] && t[ti] <= p[i + 2] {
            hit = true
          }
          i = i + 3
        } else {
          if t[ti] == p[i] {
            hit = true
          }
          i = i + 1
        }
      }
      if i >= p.length() {
        return false
      }
      if hit != neg {
        m(p, i + 1, t, ti + 1)
      } else {
        false
      }
    }
    c => ti < t.length() && t[ti] == c && m(p, pi + 1, t, ti + 1)
  }
}

///|
pub fn matches(pattern : String, text : String) -> Bool {
  let p : Array[Char] = []
  for c in pattern {
    p.push(c)
  }
  let t : Array[Char] = []
  for c in text {
    t.push(c)
  }
  m(p, 0, t, 0)
}
