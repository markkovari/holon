//! A minimal CBOR (RFC 8949) reader — only what WebAuthn actually sends.
//!
//! CTAP2 requires *canonical* CBOR: definite lengths, shortest-form integers.
//! So this handles unsigned/negative integers, byte and text strings, arrays and
//! maps, and refuses everything else (indefinite lengths, tags, floats) instead
//! of guessing. Decoding also reports how many bytes an item consumed, which is
//! how `authData` finds where the COSE public key ends and extensions begin.
//!
//! ponytail: hand-rolled instead of pulling in a CBOR crate — this is ~100 lines
//! for a closed subset, and the repo's "no new dependency" property is worth more
//! than the generality. Swap in `ciborium` if a full decoder is ever needed.

#[derive(Debug, Clone, PartialEq)]
pub enum Cbor {
    Uint(u64),
    /// Negative integer; stored as the value itself (e.g. -7).
    Nint(i64),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<Cbor>),
    /// Keys are kept as `Cbor` because COSE keys are ints and attestation
    /// objects are text — a `BTreeMap` would need one key type.
    Map(Vec<(Cbor, Cbor)>),
}

impl Cbor {
    /// Look up a text key in a map.
    pub fn get(&self, key: &str) -> Option<&Cbor> {
        match self {
            Cbor::Map(pairs) => pairs.iter().find(|(k, _)| matches!(k, Cbor::Text(t) if t == key)).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Look up an integer key in a map (COSE labels: 1 = kty, 3 = alg, -1 = crv…).
    pub fn get_int(&self, key: i64) -> Option<&Cbor> {
        let want = |k: &Cbor| match k {
            Cbor::Uint(u) => *u as i64 == key,
            Cbor::Nint(n) => *n == key,
            _ => false,
        };
        match self {
            Cbor::Map(pairs) => pairs.iter().find(|(k, _)| want(k)).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Cbor::Bytes(b) => Some(b),
            _ => None,
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Cbor::Text(t) => Some(t),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Cbor::Uint(u) => i64::try_from(*u).ok(),
            Cbor::Nint(n) => Some(*n),
            _ => None,
        }
    }
}

/// Decode one CBOR item; returns it and the number of bytes consumed.
pub fn decode(buf: &[u8]) -> Result<(Cbor, usize), String> {
    let (major, arg, mut used) = head(buf)?;
    match major {
        0 => Ok((Cbor::Uint(arg), used)),
        // -1 - arg, per RFC 8949 §3.1.
        1 => Ok((Cbor::Nint(-1 - i64::try_from(arg).map_err(|_| "negative int too large")?), used)),
        2 | 3 => {
            let n = arg as usize;
            let end = used.checked_add(n).filter(|e| *e <= buf.len()).ok_or("string past end of buffer")?;
            let raw = buf[used..end].to_vec();
            let item = if major == 2 {
                Cbor::Bytes(raw)
            } else {
                Cbor::Text(String::from_utf8(raw).map_err(|_| "text string is not utf-8")?)
            };
            Ok((item, end))
        }
        4 => {
            let mut items = Vec::with_capacity((arg as usize).min(64));
            for _ in 0..arg {
                let (item, n) = decode(&buf[used..])?;
                used += n;
                items.push(item);
            }
            Ok((Cbor::Array(items), used))
        }
        5 => {
            let mut pairs = Vec::with_capacity((arg as usize).min(64));
            for _ in 0..arg {
                let (k, n) = decode(&buf[used..])?;
                used += n;
                let (v, n) = decode(&buf[used..])?;
                used += n;
                pairs.push((k, v));
            }
            Ok((Cbor::Map(pairs), used))
        }
        _ => Err(format!("unsupported cbor major type {major}")),
    }
}

/// Read the initial byte plus any following length/value bytes.
fn head(buf: &[u8]) -> Result<(u8, u64, usize), String> {
    let b0 = *buf.first().ok_or("empty cbor buffer")?;
    let major = b0 >> 5;
    let info = b0 & 0x1f;
    let (arg, used) = match info {
        0..=23 => (info as u64, 1),
        24 | 25 | 26 | 27 => {
            let n = 1usize << (info - 24); // 1, 2, 4, 8 bytes
            if buf.len() < 1 + n {
                return Err("truncated cbor length".into());
            }
            let mut v = 0u64;
            for b in &buf[1..1 + n] {
                v = (v << 8) | *b as u64;
            }
            (v, 1 + n)
        }
        // 28..=30 are reserved; 31 is an indefinite length, which canonical
        // CTAP2 CBOR never uses.
        _ => return Err("indefinite-length or reserved cbor".into()),
    };
    Ok((major, arg, used))
}

/// Encode a canonical CBOR map with integer keys — used only to re-serialise a
/// COSE key for storage comparisons in tests. Kept tiny on purpose.
#[cfg(test)]
pub fn encode_int_map(pairs: &std::collections::BTreeMap<i64, Cbor>) -> Vec<u8> {
    let mut out = vec![0xa0 | pairs.len() as u8];
    for (k, v) in pairs {
        out.extend(encode_head(if *k < 0 { 1 } else { 0 }, if *k < 0 { (-1 - k) as u64 } else { *k as u64 }));
        match v {
            Cbor::Uint(u) => out.extend(encode_head(0, *u)),
            Cbor::Nint(n) => out.extend(encode_head(1, (-1 - n) as u64)),
            Cbor::Bytes(b) => {
                out.extend(encode_head(2, b.len() as u64));
                out.extend(b);
            }
            Cbor::Text(t) => {
                out.extend(encode_head(3, t.len() as u64));
                out.extend(t.as_bytes());
            }
            _ => unreachable!("test encoder handles scalars only"),
        }
    }
    out
}

#[cfg(test)]
fn encode_head(major: u8, arg: u64) -> Vec<u8> {
    let m = major << 5;
    match arg {
        0..=23 => vec![m | arg as u8],
        24..=0xff => vec![m | 24, arg as u8],
        0x100..=0xffff => vec![m | 25, (arg >> 8) as u8, arg as u8],
        _ => {
            let mut v = vec![m | 26];
            v.extend((arg as u32).to_be_bytes());
            v
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn scalars_and_negative_labels() {
        assert_eq!(decode(&[0x00]).unwrap(), (Cbor::Uint(0), 1));
        assert_eq!(decode(&[0x17]).unwrap(), (Cbor::Uint(23), 1));
        assert_eq!(decode(&[0x18, 0x64]).unwrap(), (Cbor::Uint(100), 2));
        // COSE alg ES256 = -7, and RS256 = -257 (needs a 2-byte argument).
        assert_eq!(decode(&[0x26]).unwrap(), (Cbor::Nint(-7), 1));
        assert_eq!(decode(&[0x39, 0x01, 0x00]).unwrap(), (Cbor::Nint(-257), 3));
    }

    #[test]
    fn strings_report_consumed_length() {
        let (v, n) = decode(b"\x43abc").unwrap();
        assert_eq!((v, n), (Cbor::Bytes(b"abc".to_vec()), 4));
        let (v, n) = decode(b"\x63fmt").unwrap();
        assert_eq!((v, n), (Cbor::Text("fmt".into()), 4));
    }

    #[test]
    fn cose_es256_key_round_trip() {
        // The shape every platform authenticator sends: {1: 2, 3: -7, -1: 1, -2: x, -3: y}
        let mut m = BTreeMap::new();
        m.insert(1, Cbor::Uint(2));
        m.insert(3, Cbor::Nint(-7));
        m.insert(-1, Cbor::Uint(1));
        m.insert(-2, Cbor::Bytes(vec![0xaa; 32]));
        m.insert(-3, Cbor::Bytes(vec![0xbb; 32]));
        let bytes = encode_int_map(&m);
        let (key, used) = decode(&bytes).unwrap();
        assert_eq!(used, bytes.len(), "consumed exactly the key — how authData finds its end");
        assert_eq!(key.get_int(1).unwrap().as_i64(), Some(2));
        assert_eq!(key.get_int(3).unwrap().as_i64(), Some(-7));
        assert_eq!(key.get_int(-2).unwrap().as_bytes().unwrap().len(), 32);
        assert!(key.get_int(-4).is_none());
    }

    #[test]
    fn attestation_object_shape() {
        // {"fmt": "none", "attStmt": {}, "authData": h'0102'}
        let bytes = b"\xa3\x63fmt\x64none\x67attStmt\xa0\x68authData\x42\x01\x02";
        let (v, used) = decode(bytes).unwrap();
        assert_eq!(used, bytes.len());
        assert_eq!(v.get("fmt").unwrap().as_text(), Some("none"));
        assert_eq!(v.get("authData").unwrap().as_bytes(), Some(&[1u8, 2][..]));
    }

    #[test]
    fn refuses_what_it_cannot_do() {
        assert!(decode(&[]).is_err(), "empty");
        assert!(decode(&[0x43, 0x01]).is_err(), "byte string past the end");
        assert!(decode(&[0x5f]).is_err(), "indefinite length");
        assert!(decode(&[0xfb, 0, 0, 0, 0, 0, 0, 0, 0]).is_err(), "float");
        assert!(decode(&[0x19, 0x01]).is_err(), "truncated 2-byte length");
    }
}
