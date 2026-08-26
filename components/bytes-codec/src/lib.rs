//! `bytes-codec` — turn bytes into text that survives a URL, a JSON string or a header, and back again
//!
//! `tests/codec.rs` is the specification and is not writable from here.
//!
//! Five components in this repository had written base64 by hand, in five different
//! shapes: two encode-only, three decode-only, two URL-safe, three standard, and one
//! that reached for a crate instead. None of them could be reused by any of the
//! others, and the one in an authentication path is the one it matters most to get
//! right.
//!
//! Pure compute: bytes in, text out, and back.
//!
//! ## Two alphabets, and they are not interchangeable
//!
//! Standard base64's last two characters are `+` and `/`. Both are meaningful inside
//! a URL and neither survives a path segment or a query string unescaped, which is
//! why WebAuthn, JWTs and every id-in-a-link use the URL-SAFE alphabet: `-` and `_`.
//! Decoding one with the other's table does not fail — it produces different bytes.
//! That is the failure worth designing against, because nothing reports it.
//!
//! ## Padding is optional on the way in and chosen on the way out
//!
//! `=` exists so a decoder knows how many bytes the last group holds, and the length
//! already says that. WebAuthn forbids it, JSON payloads usually carry it, and a
//! decoder that refuses unpadded input rejects half the real world. So: decode
//! accepts either, and encode is told which to produce.
//!
//! ## Hex, because the alternative is `format!("{:02x}")` in a loop
//!
//! Lowercase out, either case in — a digest printed by one tool and read by another
//! is the whole use, and the two tools rarely agree on case.

/// Which alphabet, and what to do about padding when encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alphabet {
    /// `+` and `/`, with `=` padding. What a JSON payload or an HTTP header carries.
    Standard,
    /// `-` and `_`, no padding. What a URL, a JWT and WebAuthn use.
    UrlSafe,
}

/// Why some text could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// A character that is in neither alphabet, with its position — which is the
    /// only useful thing to say about a payload you cannot print in an error.
    NotInAlphabet { at: usize, found: char },
    /// A length that cannot be a whole number of bytes. `abcde` is five characters,
    /// which is one group of four and one orphan.
    TruncatedGroup { length: usize },
    /// Padding somewhere other than the end, or too much of it.
    MisplacedPadding { at: usize },
}

const STD_TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const URL_TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn b64_val(c: char, alphabet: Alphabet) -> Option<u8> {
    match c {
        'A'..='Z' => Some(c as u8 - b'A'),
        'a'..='z' => Some(c as u8 - b'a' + 26),
        '0'..='9' => Some(c as u8 - b'0' + 52),
        '+' if alphabet == Alphabet::Standard => Some(62),
        '/' if alphabet == Alphabet::Standard => Some(63),
        '-' if alphabet == Alphabet::UrlSafe => Some(62),
        '_' if alphabet == Alphabet::UrlSafe => Some(63),
        _ => None,
    }
}

/// Encode bytes as base64 in `alphabet`.
///
/// `Standard` pads to a multiple of four; `UrlSafe` does not, because the
/// specifications that use it forbid it.
pub fn encode(bytes: &[u8], alphabet: Alphabet) -> String {
    let table = match alphabet {
        Alphabet::Standard => STD_TABLE,
        Alphabet::UrlSafe => URL_TABLE,
    };
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        let n = (b0 as u32) << 16 | (b1 as u32) << 8 | b2 as u32;
        out.push(table[(n >> 18 & 0x3F) as usize] as char);
        out.push(table[(n >> 12 & 0x3F) as usize] as char);
        match chunk.len() {
            3 => {
                out.push(table[(n >> 6 & 0x3F) as usize] as char);
                out.push(table[(n & 0x3F) as usize] as char);
            }
            2 => {
                out.push(table[(n >> 6 & 0x3F) as usize] as char);
                if alphabet == Alphabet::Standard {
                    out.push('=');
                }
            }
            1 => {
                if alphabet == Alphabet::Standard {
                    out.push('=');
                    out.push('=');
                }
            }
            _ => unreachable!(),
        }
    }
    out
}

/// Decode base64 in `alphabet`.
///
/// Padding is accepted whether or not the alphabet would have produced it: `=` on
/// the end of URL-safe input is wrong per the specification and unambiguous in
/// practice, and refusing it fails on real tokens.
pub fn decode(text: &str, alphabet: Alphabet) -> Result<Vec<u8>, DecodeError> {
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let chars: Vec<char> = text.chars().collect();
    let pad_start = chars.iter().position(|&c| c == '=').unwrap_or(chars.len());
    if chars[pad_start..].iter().any(|&c| c != '=') {
        return Err(DecodeError::MisplacedPadding { at: pad_start });
    }
    if chars.len() - pad_start > 2 {
        return Err(DecodeError::MisplacedPadding { at: pad_start });
    }

    let mut values = Vec::with_capacity(pad_start);
    for (i, &c) in chars[..pad_start].iter().enumerate() {
        match b64_val(c, alphabet) {
            Some(v) => values.push(v),
            None => return Err(DecodeError::NotInAlphabet { at: i, found: c }),
        }
    }

    let n = values.len();
    if n % 4 == 1 {
        return Err(DecodeError::TruncatedGroup { length: chars.len() });
    }

    let mut out = Vec::with_capacity(n / 4 * 3 + 2);
    let mut i = 0;
    while i + 4 <= n {
        let v = (values[i] as u32) << 18
            | (values[i + 1] as u32) << 12
            | (values[i + 2] as u32) << 6
            | values[i + 3] as u32;
        out.push((v >> 16) as u8);
        out.push((v >> 8) as u8);
        out.push(v as u8);
        i += 4;
    }
    match n - i {
        0 => {}
        2 => {
            let v = (values[i] as u32) << 18 | (values[i + 1] as u32) << 12;
            out.push((v >> 16) as u8);
        }
        3 => {
            let v = (values[i] as u32) << 18 | (values[i + 1] as u32) << 12 | (values[i + 2] as u32) << 6;
            out.push((v >> 16) as u8);
            out.push((v >> 8) as u8);
        }
        _ => unreachable!(),
    }
    Ok(out)
}

fn hex_val(c: char) -> Option<u8> {
    match c {
        '0'..='9' => Some(c as u8 - b'0'),
        'a'..='f' => Some(c as u8 - b'a' + 10),
        'A'..='F' => Some(c as u8 - b'A' + 10),
        _ => None,
    }
}

/// Lowercase hex.
pub fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Hex in either case.
pub fn from_hex(text: &str) -> Result<Vec<u8>, DecodeError> {
    let chars: Vec<char> = text.chars().collect();
    let mut values = Vec::with_capacity(chars.len());
    for (i, &c) in chars.iter().enumerate() {
        match hex_val(c) {
            Some(v) => values.push(v),
            None => return Err(DecodeError::NotInAlphabet { at: i, found: c }),
        }
    }
    if values.len() % 2 != 0 {
        return Err(DecodeError::TruncatedGroup { length: values.len() });
    }
    let mut out = Vec::with_capacity(values.len() / 2);
    for pair in values.chunks(2) {
        out.push((pair[0] << 4) | pair[1]);
    }
    Ok(out)
}
