//! Regex-free ASCII scanners with byte offsets: every pattern is ASCII, so slicing is safe.

#[allow(warnings)]
mod bindings;

use bindings::exports::pii::redact::redactor::{Finding, Guest, Kind, Options};

struct Component;

/// Most specific first, so a card/SSN digit run claims its bytes before phone can.
const PRIORITY: [Kind; 5] = [Kind::CreditCard, Kind::Ssn, Kind::Email, Kind::Ip, Kind::Phone];

fn wanted(opts: &Options) -> Vec<Kind> {
    if opts.kinds.is_empty() {
        PRIORITY.to_vec()
    } else {
        // honor the caller's set, but always scan in priority order.
        PRIORITY.iter().copied().filter(|k| opts.kinds.iter().any(|w| same_kind(*w, *k))).collect()
    }
}

fn same_kind(a: Kind, b: Kind) -> bool {
    matches!(
        (a, b),
        (Kind::Email, Kind::Email)
            | (Kind::CreditCard, Kind::CreditCard)
            | (Kind::Ssn, Kind::Ssn)
            | (Kind::Phone, Kind::Phone)
            | (Kind::Ip, Kind::Ip)
    )
}

fn scan(text: &str, opts: &Options) -> Vec<Finding> {
    let bytes = text.as_bytes();
    let mut out: Vec<Finding> = Vec::new();

    for kind in wanted(opts) {
        let candidates = match kind {
            Kind::Email => scan_email(bytes),
            Kind::CreditCard => scan_credit_card(bytes),
            Kind::Ssn => scan_ssn(bytes),
            Kind::Phone => scan_phone(bytes),
            Kind::Ip => scan_ip(bytes),
        };
        for (start, length) in candidates {
            if !overlaps(&out, start, length) {
                out.push(Finding { kind, start: start as u32, length: length as u32 });
            }
        }
    }

    out.sort_by_key(|f| f.start);
    out
}

fn overlaps(found: &[Finding], start: usize, length: usize) -> bool {
    let end = start + length;
    found.iter().any(|f| {
        let fs = f.start as usize;
        let fe = fs + f.length as usize;
        start < fe && fs < end
    })
}

fn is_digit(b: u8) -> bool {
    b.is_ascii_digit()
}
fn is_alpha(b: u8) -> bool {
    b.is_ascii_alphabetic()
}
/// `[A-Za-z0-9._%+-]` — the email local-part class.
fn is_local(b: u8) -> bool {
    is_alpha(b) || is_digit(b) || matches!(b, b'.' | b'_' | b'%' | b'+' | b'-')
}
/// `[A-Za-z0-9.-]` — the email domain class.
fn is_domain(b: u8) -> bool {
    is_alpha(b) || is_digit(b) || matches!(b, b'.' | b'-')
}

fn scan_email(b: &[u8]) -> Vec<(usize, usize)> {
    let n = b.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        if b[i] != b'@' {
            i += 1;
            continue;
        }
        let mut ls = i;
        while ls > 0 && is_local(b[ls - 1]) {
            ls -= 1;
        }
        let mut de = i + 1;
        while de < n && is_domain(b[de]) {
            de += 1;
        }
        let local_ok = ls < i;
        let domain = &b[i + 1..de];
        let valid_domain = domain_has_tld(domain);
        if local_ok && valid_domain {
            // trim a trailing '.' or '-' that isn't part of the address.
            let mut end = de;
            while end > i + 1 && matches!(b[end - 1], b'.' | b'-') {
                end -= 1;
            }
            out.push((ls, end - ls));
            i = end;
        } else {
            i += 1;
        }
    }
    out
}

fn domain_has_tld(domain: &[u8]) -> bool {
    let mut end = domain.len();
    while end > 0 && matches!(domain[end - 1], b'.' | b'-') {
        end -= 1;
    }
    let domain = &domain[..end];
    let dot = match domain.iter().rposition(|&c| c == b'.') {
        Some(p) => p,
        None => return false,
    };
    let tld = &domain[dot + 1..];
    tld.len() >= 2 && tld.iter().all(|&c| is_alpha(c)) && dot > 0
}

// 13..=19 digits, single space/hyphen separators, Luhn-checked.
fn scan_credit_card(b: &[u8]) -> Vec<(usize, usize)> {
    let n = b.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        if !is_digit(b[i]) {
            i += 1;
            continue;
        }
        // don't start mid-run.
        if i > 0 && (is_digit(b[i - 1]) || matches!(b[i - 1], b'-' | b' ')) {}
        let start = i;
        let mut digits: Vec<u8> = Vec::new();
        let mut j = i;
        while j < n {
            if is_digit(b[j]) {
                digits.push(b[j] - b'0');
                j += 1;
            } else if (b[j] == b' ' || b[j] == b'-')
                && j + 1 < n
                && is_digit(b[j + 1])
                && !digits.is_empty()
            {
                j += 1;
            } else {
                break;
            }
        }
        let end = j;
        let count = digits.len();
        if (13..=19).contains(&count) && luhn(&digits) {
            out.push((start, end - start));
            i = end;
        } else {
            // advance past this digit run to avoid re-scanning the same head.
            i = next_non_card(b, start);
        }
    }
    out
}

fn next_non_card(b: &[u8], start: usize) -> usize {
    let mut i = start;
    let n = b.len();
    while i < n && (is_digit(b[i]) || b[i] == b' ' || b[i] == b'-') {
        i += 1;
    }
    if i == start {
        start + 1
    } else {
        i
    }
}

fn luhn(digits: &[u8]) -> bool {
    let mut sum = 0u32;
    let mut dbl = false;
    for &d in digits.iter().rev() {
        let mut v = d as u32;
        if dbl {
            v *= 2;
            if v > 9 {
                v -= 9;
            }
        }
        sum += v;
        dbl = !dbl;
    }
    sum.is_multiple_of(10)
}

// Exactly NNN-NN-NNNN with hyphens.
fn scan_ssn(b: &[u8]) -> Vec<(usize, usize)> {
    let n = b.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 11 <= n {
        // not preceded by a digit (avoid matching inside a longer number).
        if i > 0 && is_digit(b[i - 1]) {
            i += 1;
            continue;
        }
        let w = &b[i..i + 11];
        let shape = is_digit(w[0])
            && is_digit(w[1])
            && is_digit(w[2])
            && w[3] == b'-'
            && is_digit(w[4])
            && is_digit(w[5])
            && w[6] == b'-'
            && is_digit(w[7])
            && is_digit(w[8])
            && is_digit(w[9])
            && is_digit(w[10]);
        let clean_tail = i + 11 == n || !is_digit(b[i + 11]);
        if shape && clean_tail {
            out.push((i, 11));
            i += 11;
        } else {
            i += 1;
        }
    }
    out
}

// Conservative: 10..=15 digits, accepted only with a leading '+' or the NANP shape.
fn scan_phone(b: &[u8]) -> Vec<(usize, usize)> {
    let n = b.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let plus = b[i] == b'+';
        let starts = plus || b[i] == b'(' || is_digit(b[i]);
        if !starts {
            i += 1;
            continue;
        }
        // don't start mid-number.
        if i > 0 && (is_digit(b[i - 1]) || matches!(b[i - 1], b'+' | b')')) {
            i += 1;
            continue;
        }
        let start = i;
        let mut digit_count = 0usize;
        let mut j = i;
        while j < n {
            match b[j] {
                c if is_digit(c) => {
                    digit_count += 1;
                    j += 1;
                }
                b' ' | b'-' | b'(' | b')' | b'+' => j += 1,
                _ => break,
            }
        }
        let mut end = j;
        while end > start && !is_digit(b[end - 1]) {
            end -= 1;
        }
        let span = &b[start..end];
        let nanp = looks_nanp(span);
        if (10..=15).contains(&digit_count) && (plus || nanp) {
            out.push((start, end - start));
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

fn looks_nanp(s: &[u8]) -> bool {
    matches_pat(s, b"NNN-NNN-NNNN") || matches_pat(s, b"(NNN) NNN-NNNN")
}

/// 'N' is any ASCII digit; every other byte matches literally.
fn matches_pat(s: &[u8], pat: &[u8]) -> bool {
    if s.len() != pat.len() {
        return false;
    }
    s.iter().zip(pat).all(|(&c, &p)| if p == b'N' { is_digit(c) } else { c == p })
}

fn scan_ip(b: &[u8]) -> Vec<(usize, usize)> {
    let n = b.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        if !is_digit(b[i]) {
            i += 1;
            continue;
        }
        if i > 0 && (is_digit(b[i - 1]) || b[i - 1] == b'.') {
            i += 1;
            continue;
        }
        if let Some(end) = parse_ipv4(b, i) {
            // not followed by a digit or dot (avoid matching a longer token).
            let clean = end == n || !(is_digit(b[end]) || b[end] == b'.');
            if clean {
                out.push((i, end - i));
                i = end;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn parse_ipv4(b: &[u8], i: usize) -> Option<usize> {
    let n = b.len();
    let mut pos = i;
    for octet in 0..4 {
        let oct_start = pos;
        let mut val: u32 = 0;
        let mut len = 0;
        while pos < n && is_digit(b[pos]) && len < 3 {
            val = val * 10 + (b[pos] - b'0') as u32;
            pos += 1;
            len += 1;
        }
        if len == 0 || val > 255 {
            return None;
        }
        // Reject zero-padded octets like "01"; a bare "0" is fine.
        if len > 1 && b[oct_start] == b'0' {
            return None;
        }
        if octet < 3 {
            if pos >= n || b[pos] != b'.' {
                return None;
            }
            pos += 1;
        }
    }
    Some(pos)
}

fn placeholder(kind: Kind) -> &'static str {
    match kind {
        Kind::Email => "[EMAIL]",
        Kind::CreditCard => "[CARD]",
        Kind::Ssn => "[SSN]",
        Kind::Phone => "[PHONE]",
        Kind::Ip => "[IP]",
    }
}

fn rewrite(text: &str, findings: &[Finding], sub: impl Fn(Kind, &str) -> String) -> String {
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    for f in findings {
        let start = f.start as usize;
        let end = start + f.length as usize;
        if start >= cursor {
            out.push_str(&text[cursor..start]);
            out.push_str(&sub(f.kind, &text[start..end]));
            cursor = end;
        }
    }
    out.push_str(&text[cursor..]);
    out
}

fn mask_span(kind: Kind, s: &str) -> String {
    match kind {
        Kind::Email => mask_email(s),
        Kind::CreditCard => mask_card(s),
        Kind::Ssn => mask_ssn(s),
        Kind::Phone => mask_phone(s),
        Kind::Ip => mask_ip(s),
    }
}

fn mask_email(s: &str) -> String {
    let (local, rest) = match s.split_once('@') {
        Some(v) => v,
        None => return s.to_string(),
    };
    let first_local = local.chars().next().unwrap_or('x');
    let (dom_head, tld) = match rest.rsplit_once('.') {
        Some((_, tld)) => (rest.chars().next().unwrap_or('x'), tld),
        None => (rest.chars().next().unwrap_or('x'), ""),
    };
    if tld.is_empty() {
        format!("{first_local}***@{dom_head}***")
    } else {
        format!("{first_local}***@{dom_head}***.{tld}")
    }
}

fn mask_card(s: &str) -> String {
    let digits: Vec<char> = s.chars().filter(|c| c.is_ascii_digit()).collect();
    let count = digits.len();
    if count < 4 {
        return "*".repeat(s.len());
    }
    let last4: String = digits[count - 4..].iter().collect();
    if count == 16 {
        format!("**** **** **** {last4}")
    } else {
        format!("{}{}", "*".repeat(count - 4), last4)
    }
}

fn mask_ssn(s: &str) -> String {
    let last4: String = s.chars().filter(|c| c.is_ascii_digit()).skip(5).collect();
    format!("***-**-{last4}")
}

fn mask_phone(s: &str) -> String {
    let total = s.chars().filter(|c| c.is_ascii_digit()).count();
    let mut seen = 0usize;
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_digit() {
            seen += 1;
            if seen > total.saturating_sub(2) {
                out.push(c);
            } else {
                out.push('*');
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn mask_ip(s: &str) -> String {
    match s.rsplit_once('.') {
        Some((_, last)) => format!("***.***.***.{last}"),
        None => s.to_string(),
    }
}

impl Guest for Component {
    fn detect(text: String, opts: Options) -> Vec<Finding> {
        scan(&text, &opts)
    }

    fn redact(text: String, opts: Options) -> String {
        let findings = scan(&text, &opts);
        rewrite(&text, &findings, |k, _| placeholder(k).to_string())
    }

    fn mask(text: String, opts: Options) -> String {
        let findings = scan(&text, &opts);
        rewrite(&text, &findings, mask_span)
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(text: &str, k: Kind) -> Vec<(usize, usize)> {
        match k {
            Kind::Email => scan_email(text.as_bytes()),
            Kind::CreditCard => scan_credit_card(text.as_bytes()),
            Kind::Ssn => scan_ssn(text.as_bytes()),
            Kind::Phone => scan_phone(text.as_bytes()),
            Kind::Ip => scan_ip(text.as_bytes()),
        }
    }

    /// Both directions pinned: a false negative leaks a card, a false positive destroys an order number.
    #[test]
    fn luhn_accepts_real_check_digits_and_rejects_near_misses() {
        assert!(luhn(&[4, 5, 3, 9, 1, 4, 8, 8, 0, 3, 4, 3, 6, 4, 6, 7]));
        assert!(luhn(&[7, 9, 9, 2, 7, 3, 9, 8, 7, 1, 3]));
        assert!(!luhn(&[4, 5, 3, 9, 1, 4, 8, 8, 0, 3, 4, 3, 6, 4, 6, 8]));
        assert!(!luhn(&[7, 9, 9, 2, 7, 3, 9, 8, 7, 3, 1]));
    }

    #[test]
    fn a_card_is_found_with_or_without_separators() {
        assert_eq!(spans("pay 4539148803436467 now", Kind::CreditCard).len(), 1);
        assert_eq!(spans("4539-1488-0343-6467", Kind::CreditCard).len(), 1);
        assert_eq!(spans("4539 1488 0343 6467", Kind::CreditCard).len(), 1);
        assert!(spans("4539148803436468", Kind::CreditCard).is_empty());
    }

    #[test]
    fn an_ssn_does_not_match_inside_a_longer_number() {
        assert_eq!(spans("ssn 123-45-6789 ok", Kind::Ssn), vec![(4, 11)]);
        assert!(spans("9123-45-6789", Kind::Ssn).is_empty(), "a leading digit disqualifies it");
        assert!(spans("123-45-67891", Kind::Ssn).is_empty(), "a trailing digit disqualifies it");
        assert!(spans("123456789", Kind::Ssn).is_empty(), "hyphens are required");
    }

    /// `[CARD]` and `[PHONE]` are not interchangeable to whatever reads the output.
    #[test]
    fn a_higher_priority_kind_claims_an_overlapping_span() {
        let opts = Options { kinds: vec![] };
        let found = scan("call 4539148803436467 today", &opts);
        assert_eq!(found.len(), 1, "one span, not one per scanner");
        assert!(matches!(found[0].kind, Kind::CreditCard), "the card wins over the phone shape");
    }

    /// A caller replaces spans in order; unsorted spans corrupt later offsets.
    #[test]
    fn findings_are_sorted_by_start() {
        let opts = Options { kinds: vec![] };
        let found = scan("a@b.com then 123-45-6789 then c@d.org", &opts);
        assert!(found.len() >= 2);
        let starts: Vec<usize> = found.iter().map(|f| f.start as usize).collect();
        let mut sorted = starts.clone();
        sorted.sort_unstable();
        assert_eq!(starts, sorted);
    }

    /// An address needs a plausible TLD, or every `foo@bar` in prose is PII.
    #[test]
    fn an_email_needs_a_tld() {
        assert_eq!(spans("mail me at a@b.com ok", Kind::Email).len(), 1);
        assert!(spans("a@localhost", Kind::Email).is_empty());
        assert!(spans("not an @ sign", Kind::Email).is_empty());
    }
}
