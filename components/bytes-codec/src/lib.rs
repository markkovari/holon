//! Decoding one alphabet with the other's table silently produces different bytes; padding is optional in.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alphabet {
    /// What a JSON payload or an HTTP header carries.
    Standard,
    /// What a URL, a JWT and WebAuthn use.
    UrlSafe,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    NotInAlphabet { at: usize, found: char },
    TruncatedGroup { length: usize },
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

/// `UrlSafe` is unpadded because the specifications that use it forbid `=`.
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

/// `=` on URL-safe input is wrong per spec but unambiguous, and refusing it fails on real tokens.
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
            let v = (values[i] as u32) << 18
                | (values[i + 1] as u32) << 12
                | (values[i + 2] as u32) << 6;
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

pub fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

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

#[cfg(target_arch = "wasm32")]
#[allow(warnings)]
mod bindings;

#[cfg(target_arch = "wasm32")]
use bindings::exports::bytes::codec::codec as w;

#[cfg(target_arch = "wasm32")]
struct Component;

#[cfg(target_arch = "wasm32")]
fn alphabet_in(a: w::Alphabet) -> Alphabet {
    match a {
        w::Alphabet::Standard => Alphabet::Standard,
        w::Alphabet::UrlSafe => Alphabet::UrlSafe,
    }
}

#[cfg(target_arch = "wasm32")]
fn err_out(e: DecodeError) -> w::DecodeError {
    match e {
        DecodeError::NotInAlphabet { at, found } => {
            w::DecodeError::NotInAlphabet((at as u32, found.to_string()))
        }
        DecodeError::TruncatedGroup { length } => w::DecodeError::TruncatedGroup(length as u32),
        DecodeError::MisplacedPadding { at } => w::DecodeError::MisplacedPadding(at as u32),
    }
}

#[cfg(target_arch = "wasm32")]
impl w::Guest for Component {
    fn encode(bytes: Vec<u8>, alphabet: w::Alphabet) -> String {
        crate::encode(&bytes, alphabet_in(alphabet))
    }
    fn decode(text: String, alphabet: w::Alphabet) -> Result<Vec<u8>, w::DecodeError> {
        crate::decode(&text, alphabet_in(alphabet)).map_err(err_out)
    }
    fn to_hex(bytes: Vec<u8>) -> String {
        crate::to_hex(&bytes)
    }
    fn from_hex(text: String) -> Result<Vec<u8>, w::DecodeError> {
        crate::from_hex(&text).map_err(err_out)
    }
}

#[cfg(target_arch = "wasm32")]
bindings::export!(Component with_types_in bindings);
