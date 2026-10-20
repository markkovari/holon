//! Order keys: digit strings not ending in `0`, compared as bytes, so there is always a key between two.
//! Same-spot concurrent inserts get the same key; the symbol key breaks the tie, so it is not a conflict.

use crate::model::OpId;

/// The implicit key of a symbol created at `op` with no placement.
pub fn derived(op: OpId) -> String {
    format!("{op:020}5")
}

pub fn is_key(k: &str) -> bool {
    !k.is_empty() && k.bytes().all(|b| b.is_ascii_digit()) && !k.ends_with('0')
}

/// `None` is unbounded; both must be well-formed and `lo < hi`.
pub fn between(lo: Option<&str>, hi: Option<&str>) -> String {
    let lo = lo.unwrap_or("");
    debug_assert!(lo.is_empty() || is_key(lo), "{lo:?}");
    debug_assert!(hi.is_none_or(is_key), "{hi:?}");
    debug_assert!(hi.is_none_or(|h| lo < h), "{lo:?} !< {hi:?}");
    let out = mid(lo.as_bytes(), hi.map(str::as_bytes));
    String::from_utf8(out).expect("digits")
}

/// Fractional indexing in base 10; never ends in `0`, so there is always room below it.
fn mid(a: &[u8], b: Option<&[u8]>) -> Vec<u8> {
    let digit = |c: u8| c - b'0';
    if let Some(b) = b {
        let mut n = 0;
        while n < b.len() && a.get(n).copied().unwrap_or(b'0') == b[n] {
            n += 1;
        }
        if n > 0 {
            let mut out = b[..n].to_vec();
            out.extend(mid(a.get(n..).unwrap_or(&[]), Some(&b[n..])));
            return out;
        }
    }
    let da = a.first().map(|&c| digit(c)).unwrap_or(0);
    let db = b.map(|b| digit(b[0])).unwrap_or(10);
    if db - da > 1 {
        return vec![b'0' + (da + db) / 2];
    }
    if let Some(b) = b {
        if b.len() > 1 {
            // b's first digit alone is above a and below b.
            return vec![b[0]];
        }
    }
    let mut out = vec![b'0' + da];
    out.extend(mid(a.get(1..).unwrap_or(&[]), None));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(lo: Option<&str>, hi: Option<&str>) -> String {
        let m = between(lo, hi);
        assert!(is_key(&m), "{m:?}");
        if let Some(lo) = lo {
            assert!(lo < m.as_str(), "{lo} < {m}");
        }
        if let Some(hi) = hi {
            assert!(m.as_str() < hi, "{m} < {hi}");
        }
        m
    }

    #[test]
    fn derived_keys_sort_by_op() {
        assert!(derived(9) < derived(10));
        assert!(derived(1) < derived(u64::MAX));
        assert!(is_key(&derived(0)) && is_key(&derived(u64::MAX)));
    }

    #[test]
    fn between_is_strict() {
        check(None, None);
        check(None, Some("5"));
        check(Some("5"), None);
        check(Some("1"), Some("2"));
        check(Some("19"), Some("2"));
        check(Some("1"), Some("25"));
        check(Some("1"), Some("11"));
        check(Some("0001"), Some("0002"));
        check(Some(&derived(3)), Some(&derived(4)));
        check(None, Some(&derived(1)));
        check(Some("99999"), None);
    }

    #[test]
    fn repeated_inserts_always_find_room() {
        let (lo, hi) = (derived(1), derived(2));
        let mut last = lo.clone();
        for _ in 0..200 {
            last = check(Some(&last), Some(&hi));
        }
        let mut first = hi.clone();
        for _ in 0..200 {
            first = check(Some(&lo), Some(&first));
        }
        let mut keys = vec![derived(1), derived(2), derived(3)];
        let mut x: u64 = 0x9e3779b97f4a7c15;
        for _ in 0..2000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let i = (x as usize) % (keys.len() + 1);
            let lo = if i == 0 { None } else { Some(keys[i - 1].as_str()) };
            let hi = keys.get(i).map(String::as_str);
            let k = check(lo, hi);
            keys.insert(i, k);
        }
        let mut sorted = keys.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted, keys);
    }
}
