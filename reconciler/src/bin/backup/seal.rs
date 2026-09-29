//! Sealing a part: chunked ChaCha20-Poly1305, so a backup in someone else's
//! bucket is bytes nobody there can read or quietly alter.
//!
//! ```text
//! file  = MAGIC(8) || prefix(8) || chunk*
//! chunk = len(u32 BE) || ciphertext(len)          -- plaintext <= CHUNK bytes
//! nonce = prefix || index(u32 BE)
//! aad   = name || 0x00 || index(u32 BE) || last(u8)
//! ```
//!
//! The AAD is what makes it a file and not a bag of chunks: `name` (the part's
//! path inside its backup, backup id included) stops a part being swapped for
//! another, `index` stops reordering, and `last` stops truncation — a reader
//! that reaches EOF without having opened a chunk marked last refuses, as it
//! does bytes after one. A fresh random `prefix` per file keeps nonces unique
//! under one key across every file it ever seals.

use std::io::{self, Read, Write};

use anyhow::{bail, Context, Result};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key as AeadKey, Nonce};
use sha2::{Digest, Sha256};

const MAGIC: &[u8; 8] = b"HOLONBK1";
const CHUNK: usize = 1 << 20;
const TAG: usize = 16;

pub struct Key([u8; 32]);

impl Key {
    /// A key file holds 64 hex characters — `comp-backup keygen` writes one.
    pub fn load(path: &str) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
        let bytes = hex::decode(text.trim()).with_context(|| format!("{path} is not hex"))?;
        let key: [u8; 32] = bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("{path} must hold 32 bytes (64 hex characters)"))?;
        Ok(Self(key))
    }

    pub fn generate() -> Result<String> {
        let mut k = [0u8; 32];
        getrandom::getrandom(&mut k).context("reading the OS random source")?;
        Ok(hex::encode(k))
    }

    /// Recorded in the manifest so a restore with the wrong key says so before
    /// it downloads anything. The first 8 bytes of a hash, not the key.
    pub fn fingerprint(&self) -> String {
        hex::encode(&Sha256::digest(self.0)[..8])
    }

    fn cipher(&self) -> ChaCha20Poly1305 {
        ChaCha20Poly1305::new(&AeadKey::from(self.0))
    }
}

fn nonce(prefix: &[u8; 8], index: u32) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[..8].copy_from_slice(prefix);
    n[8..].copy_from_slice(&index.to_be_bytes());
    n
}

fn aad(name: &str, index: u32, last: bool) -> Vec<u8> {
    let mut a = Vec::with_capacity(name.len() + 6);
    a.extend_from_slice(name.as_bytes());
    a.push(0);
    a.extend_from_slice(&index.to_be_bytes());
    a.push(last as u8);
    a
}

/// Seal all of `input` into `output`.
pub fn seal(key: &Key, name: &str, mut input: impl Read, mut output: impl Write) -> Result<()> {
    let cipher = key.cipher();
    let mut prefix = [0u8; 8];
    getrandom::getrandom(&mut prefix).context("reading the OS random source")?;
    output.write_all(MAGIC)?;
    output.write_all(&prefix)?;
    // One chunk of lookahead: whether a chunk is the last is only known once
    // the next read comes back empty.
    let mut cur = read_chunk(&mut input)?;
    let mut index: u32 = 0;
    loop {
        let next = if cur.len() == CHUNK { read_chunk(&mut input)? } else { Vec::new() };
        let last = next.is_empty();
        let ct = cipher
            .encrypt(
                &Nonce::from(nonce(&prefix, index)),
                Payload { msg: &cur, aad: &aad(name, index, last) },
            )
            .map_err(|_| anyhow::anyhow!("sealing chunk {index} of {name}"))?;
        output.write_all(&(ct.len() as u32).to_be_bytes())?;
        output.write_all(&ct)?;
        if last {
            return Ok(());
        }
        index = index.checked_add(1).context("a part too large to seal")?;
        cur = next;
    }
}

/// Open all of `input` into `output`, refusing anything altered, reordered,
/// truncated, extended, or sealed under another name or key.
pub fn open(key: &Key, name: &str, mut input: impl Read, mut output: impl Write) -> Result<()> {
    let cipher = key.cipher();
    let mut head = [0u8; 16];
    input.read_exact(&mut head).context("a sealed part shorter than its header")?;
    if &head[..8] != MAGIC {
        bail!("{name} is not a sealed part");
    }
    let prefix: [u8; 8] = head[8..].try_into().unwrap();
    let mut index: u32 = 0;
    loop {
        let mut len = [0u8; 4];
        match input.read_exact(&mut len) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                bail!("{name} is truncated: it ends before its last chunk")
            }
            Err(e) => return Err(e.into()),
        }
        let len = u32::from_be_bytes(len) as usize;
        if len > CHUNK + TAG {
            bail!("{name}: chunk {index} claims {len} bytes");
        }
        let mut ct = vec![0u8; len];
        input
            .read_exact(&mut ct)
            .with_context(|| format!("{name} is truncated in chunk {index}"))?;
        let n = &Nonce::from(nonce(&prefix, index));
        // Try "not last" first: every chunk but one is.
        let (pt, last) = match cipher
            .decrypt(n, Payload { msg: &ct, aad: &aad(name, index, false) })
        {
            Ok(pt) => (pt, false),
            Err(_) => match cipher.decrypt(n, Payload { msg: &ct, aad: &aad(name, index, true) }) {
                Ok(pt) => (pt, true),
                Err(_) => bail!(
                    "{name}: chunk {index} does not open — wrong key, altered bytes, or a part \
                     from somewhere else"
                ),
            },
        };
        output.write_all(&pt)?;
        if last {
            let mut extra = [0u8; 1];
            if input.read(&mut extra)? != 0 {
                bail!("{name} has bytes after its last chunk");
            }
            return Ok(());
        }
        index = index.checked_add(1).context("chunk index overflow")?;
    }
}

fn read_chunk(r: &mut impl Read) -> Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(CHUNK);
    r.take(CHUNK as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> Key {
        Key([7u8; 32])
    }

    fn round(len: usize) {
        let data: Vec<u8> = (0..len).map(|i| (i * 31 % 251) as u8).collect();
        let mut sealed = Vec::new();
        seal(&key(), "b/p", &data[..], &mut sealed).unwrap();
        if len >= 64 {
            assert!(
                !sealed.windows(64).any(|w| w == &data[..64]),
                "plaintext visible in the sealed file"
            );
        }
        let mut back = Vec::new();
        open(&key(), "b/p", &sealed[..], &mut back).unwrap();
        assert_eq!(back, data, "len {len}");
    }

    #[test]
    fn sizes_around_the_chunk_boundary_round_trip() {
        for len in [0, 1, CHUNK - 1, CHUNK, CHUNK + 1, 2 * CHUNK, 2 * CHUNK + 5] {
            round(len);
        }
    }

    fn sealed(len: usize) -> Vec<u8> {
        let mut out = Vec::new();
        seal(&key(), "b/p", &vec![1u8; len][..], &mut out).unwrap();
        out
    }

    #[test]
    fn tampering_of_every_kind_is_refused() {
        let s = sealed(2 * CHUNK + 10);
        let sink = || Vec::new();
        // A flipped bit.
        let mut t = s.clone();
        t[40] ^= 1;
        assert!(open(&key(), "b/p", &t[..], sink()).is_err());
        // Another name: the part moved to a different place in the backup.
        assert!(open(&key(), "b/other", &s[..], sink()).is_err());
        // Another key.
        assert!(open(&Key([8u8; 32]), "b/p", &s[..], sink()).is_err());
        // Truncated at a chunk boundary: every surviving chunk opens, and the
        // reader still refuses, because none of them was the last.
        let first = 16 + 4 + CHUNK + TAG;
        assert!(open(&key(), "b/p", &s[..first], sink()).is_err());
        // Bytes appended after the last chunk.
        let mut t = s.clone();
        t.push(0);
        assert!(open(&key(), "b/p", &t[..], sink()).is_err());
    }

    #[test]
    fn two_seals_of_one_file_differ() {
        assert_ne!(sealed(100), sealed(100), "the nonce prefix must be fresh per file");
    }
}
