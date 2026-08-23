//! Glob match: `*` = zero or more chars, `?` = exactly one, `[abc]`/`[a-z]` =
//! a character class, everything else literal.
pub fn matches(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    m(&p, 0, &t, 0)
}

fn m(p: &[char], pi: usize, t: &[char], ti: usize) -> bool {
    if pi == p.len() {
        return ti == t.len();
    }
    match p[pi] {
        '*' => {
            let mut k = ti;
            loop {
                if m(p, pi + 1, t, k) {
                    return true;
                }
                if k == t.len() {
                    return false;
                }
                k += 1;
            }
        }
        '?' => ti < t.len() && m(p, pi + 1, t, ti + 1),
        '[' => {
            if ti >= t.len() {
                return false;
            }
            let mut i = pi + 1;
            let mut neg = false;
            if i < p.len() && (p[i] == '!' || p[i] == '^') {
                neg = true;
                i += 1;
            }
            let mut hit = false;
            let mut first = true;
            while i < p.len() && (p[i] != ']' || first) {
                first = false;
                if i + 2 < p.len() && p[i + 1] == '-' && p[i + 2] != ']' {
                    if t[ti] >= p[i] && t[ti] <= p[i + 2] {
                        hit = true;
                    }
                    i += 3;
                } else {
                    if t[ti] == p[i] {
                        hit = true;
                    }
                    i += 1;
                }
            }
            if i >= p.len() {
                return false;
            }
            if hit != neg {
                m(p, i + 1, t, ti + 1)
            } else {
                false
            }
        }
        c => ti < t.len() && t[ti] == c && m(p, pi + 1, t, ti + 1),
    }
}
