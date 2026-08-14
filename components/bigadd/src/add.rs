//! Add two non-negative decimal integer strings (no leading zeros in or out,
//! except "0"). Schoolbook, right to left, with carry.
pub fn add(a: &str, b: &str) -> String {
    let a: Vec<u8> = a.bytes().collect();
    let b: Vec<u8> = b.bytes().collect();
    let mut out: Vec<u8> = Vec::new();
    let mut carry = 0u8;
    let mut i = a.len();
    let mut j = b.len();
    while i > 0 || j > 0 || carry > 0 {
        let mut s = carry;
        if i > 0 { i -= 1; s += a[i] - b'0'; }
        if j > 0 { j -= 1; s += b[j] - b'0'; }
        carry = s / 10;
        out.push(b'0' + s % 10);
    }
    if out.is_empty() { return "0".to_string(); }
    while out.len() > 1 && *out.last().unwrap() == b'0' { out.pop(); }
    out.reverse();
    String::from_utf8(out).unwrap()
}