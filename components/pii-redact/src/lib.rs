//! `pii-redact` — reference implementation of `pii:redact`.
//!
//! Detect and mask personally-identifiable information in free text before it
//! lands in a log line, an audit trail, an LLM prompt, or an analytics sink.
//! Five hand-rolled, regex-free scanners cover the common high-risk patterns:
//! email, credit-card (Luhn-checked), US SSN, phone (NANP / international-ish),
//! and IPv4. Each match is reported as a `finding` with **byte** offsets
//! (`start` + `length`), so callers can splice spans out of the original
//! `String` without re-decoding. All scanning operates over the UTF-8 bytes of
//! the input — every pattern we match is pure ASCII, so a byte index always
//! falls on a char boundary and slicing is safe.
//!
//! Three entry points share one scanner:
//!   * `detect` — report spans only (no modification).
//!   * `redact` — replace each span with a typed placeholder (`[EMAIL]`, …).
//!   * `mask`   — partially mask, keeping a little context (`j***@e***.com`,
//!                card last-4, etc.).
//!
//! Overlap handling: scanners run in a fixed priority order — credit-card and
//! SSN (the most specific) first, then email and IPv4, then phone last. A
//! match is dropped if any of its bytes are already covered by an
//! earlier-priority finding, so a card or SSN always wins over a phone match
//! that would otherwise swallow the same digits. The returned list is
//! non-overlapping and sorted by `start`.
//!
//! Pairs with audit:log and any logging path. Pure compute: text in, redacted
//! text + findings out. No state, no host imports.

#[allow(warnings)]
mod bindings;

use bindings::exports::pii::redact::redactor::{Finding, Guest, Kind, Options};

struct Component;

// ---- scanning -----------------------------------------------------------

/// Priority order for overlap resolution: most specific first. Phone is last
/// so a card/SSN digit run claims its bytes before phone can.
const PRIORITY: [Kind; 5] = [
    Kind::CreditCard,
    Kind::Ssn,
    Kind::Email,
    Kind::Ip,
    Kind::Phone,
];

fn wanted(opts: &Options) -> Vec<Kind> {
    if opts.kinds.is_empty() {
        PRIORITY.to_vec()
    } else {
        // honor the caller's set, but always scan in priority order.
        PRIORITY
            .iter()
            .copied()
            .filter(|k| opts.kinds.iter().any(|w| same_kind(*w, *k)))
            .collect()
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

/// Run every wanted scanner, dropping any candidate that overlaps a span
/// already claimed by a higher-priority kind. Returns findings sorted by start.
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
                out.push(Finding {
                    kind,
                    start: start as u32,
                    length: length as u32,
                });
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

// ---- byte-class helpers -------------------------------------------------

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

// ---- email --------------------------------------------------------------
// local@domain.tld : run of local chars, '@', run of domain chars containing
// at least one '.', last label 2+ letters.
fn scan_email(b: &[u8]) -> Vec<(usize, usize)> {
    let n = b.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        if b[i] != b'@' {
            i += 1;
            continue;
        }
        // walk left over the local part.
        let mut ls = i;
        while ls > 0 && is_local(b[ls - 1]) {
            ls -= 1;
        }
        // walk right over the domain.
        let mut de = i + 1;
        while de < n && is_domain(b[de]) {
            de += 1;
        }
        let local_ok = ls < i;
        // require a dot in the domain with a 2+ letter final label.
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

/// Domain must contain a '.' and end in a 2+ letter label.
fn domain_has_tld(domain: &[u8]) -> bool {
    // strip trailing dots/dashes for the tld check.
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

// ---- credit card --------------------------------------------------------
// A run of 13..=19 digits, optionally separated by single spaces or hyphens.
// Strip separators; must be all digits and pass Luhn.
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
        if i > 0 && (is_digit(b[i - 1]) || matches!(b[i - 1], b'-' | b' ')) {
            // previous was a separator/digit -> this is a continuation; skip
            // ahead to a clean boundary below.
        }
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
                // single separator between digit groups.
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

/// Advance past the current digit/separator run.
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
    sum % 10 == 0
}

// ---- ssn ----------------------------------------------------------------
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
        // not followed by a digit.
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

// ---- phone --------------------------------------------------------------
// Conservative: a run of digits/space/hyphen/parens/'+' totaling 10..=15
// digits, accepted only if it leads with '+' OR matches the NANP shape
// (NNN-NNN-NNNN or (NNN) NNN-NNNN).
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
        // trim trailing non-digits.
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

/// `NNN-NNN-NNNN` or `(NNN) NNN-NNNN`.
fn looks_nanp(s: &[u8]) -> bool {
    matches_pat(s, b"NNN-NNN-NNNN") || matches_pat(s, b"(NNN) NNN-NNNN")
}

/// Match `s` against a pattern where 'N' means any ASCII digit and every other
/// byte must match literally.
fn matches_pat(s: &[u8], pat: &[u8]) -> bool {
    if s.len() != pat.len() {
        return false;
    }
    s.iter().zip(pat).all(|(&c, &p)| {
        if p == b'N' {
            is_digit(c)
        } else {
            c == p
        }
    })
}

// ---- ipv4 ---------------------------------------------------------------
// Dotted quad, each octet 0..=255.
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

/// Parse four dot-separated octets starting at `i`; return the end index.
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
        // reject leading-zero padded octets like "01" (keep it strict-ish but
        // allow a bare "0").
        if len > 1 && b[oct_start] == b'0' {
            return None;
        }
        if octet < 3 {
            if pos >= n || b[pos] != b'.' {
                return None;
            }
            pos += 1; // consume '.'
        }
    }
    Some(pos)
}

// ---- redact / mask ------------------------------------------------------

fn placeholder(kind: Kind) -> &'static str {
    match kind {
        Kind::Email => "[EMAIL]",
        Kind::CreditCard => "[CARD]",
        Kind::Ssn => "[SSN]",
        Kind::Phone => "[PHONE]",
        Kind::Ip => "[IP]",
    }
}

/// Walk findings in order, copying gaps and substituting each span.
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

/// `john@example.com` -> `j***@e***.com` (keep first local char, first domain
/// char, and the TLD).
fn mask_email(s: &str) -> String {
    let (local, rest) = match s.split_once('@') {
        Some(v) => v,
        None => return s.to_string(),
    };
    let first_local = local.chars().next().unwrap_or('x');
    // tld = text after the last '.'
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

/// Keep last 4 digits; mask the rest. 16-digit numbers come back grouped as
/// `**** **** **** 1234`, otherwise `***...1234`.
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

/// `***-**-1234` (keep last 4).
fn mask_ssn(s: &str) -> String {
    let last4: String = s.chars().filter(|c| c.is_ascii_digit()).skip(5).collect();
    format!("***-**-{last4}")
}

/// Keep the last two digits; mask other digits with '*'; preserve separators.
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

/// Mask the first three octets, keep the last: `***.***.***.123`.
fn mask_ip(s: &str) -> String {
    match s.rsplit_once('.') {
        Some((_, last)) => format!("***.***.***.{last}"),
        None => s.to_string(),
    }
}

// ---- guest --------------------------------------------------------------

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
        rewrite(&text, &findings, |k, span| mask_span(k, span))
    }
}

bindings::export!(Component with_types_in bindings);
