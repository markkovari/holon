//! Reading an archive back: find the central directory, then inflate each entry.
//!
//! A READER that only handled STORE would refuse every real spreadsheet, because `.xlsx` files
//! are written with DEFLATE.
//!
//! The central directory is authoritative, not the local headers: a local header may carry zeroes
//! for sizes and CRC and defer them to a data descriptor (common in streamed files).

use crate::{crc32, inflate::inflate, File};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZipError {
    NotAZip,
    Truncated { at: u32 },
    UnsupportedMethod { method: u32 },
    BadChecksum { name: String },
    BadDeflate { why: String },
}

fn u16at(b: &[u8], at: usize) -> Result<u16, ZipError> {
    b.get(at..at + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
        .ok_or(ZipError::Truncated { at: at as u32 })
}

fn u32at(b: &[u8], at: usize) -> Result<u32, ZipError> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or(ZipError::Truncated { at: at as u32 })
}

/// The end-of-central-directory record, found by scanning BACKWARDS: it is last and may be
/// followed by a comment of up to 64 KB, and a forward scan would also match inside compressed
/// data.
fn find_eocd(b: &[u8]) -> Option<usize> {
    const EOCD: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];
    if b.len() < 22 {
        return None;
    }
    let earliest = b.len().saturating_sub(22 + 0xFFFF);
    (earliest..=b.len() - 22).rev().find(|&i| b[i..i + 4] == EOCD)
}

pub fn extract(bytes: &[u8]) -> Result<Vec<File>, ZipError> {
    let eocd = find_eocd(bytes).ok_or(ZipError::NotAZip)?;
    let count = u16at(bytes, eocd + 10)? as usize;
    let mut at = u32at(bytes, eocd + 16)? as usize;

    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        if u32at(bytes, at)? != 0x0201_4b50 {
            return Err(ZipError::Truncated { at: at as u32 });
        }
        let method = u16at(bytes, at + 10)? as u32;
        let crc = u32at(bytes, at + 16)?;
        let compressed = u32at(bytes, at + 20)? as usize;
        let uncompressed = u32at(bytes, at + 24)? as usize;
        let name_len = u16at(bytes, at + 28)? as usize;
        let extra_len = u16at(bytes, at + 30)? as usize;
        let comment_len = u16at(bytes, at + 32)? as usize;
        let local = u32at(bytes, at + 42)? as usize;
        let name_bytes = bytes
            .get(at + 46..at + 46 + name_len)
            .ok_or(ZipError::Truncated { at: (at + 46) as u32 })?;
        // Lossy: a ZIP name is bytes, and an archive with a mangled name in it is
        // still worth reading. The caller is matching on `xl/worksheets/sheet1.xml`.
        let name = String::from_utf8_lossy(name_bytes).to_string();
        at += 46 + name_len + extra_len + comment_len;

        if name.ends_with('/') {
            continue;
        }

        if u32at(bytes, local)? != 0x0403_4b50 {
            return Err(ZipError::Truncated { at: local as u32 });
        }
        let l_name = u16at(bytes, local + 26)? as usize;
        let l_extra = u16at(bytes, local + 28)? as usize;
        let start = local + 30 + l_name + l_extra;
        let raw =
            bytes.get(start..start + compressed).ok_or(ZipError::Truncated { at: start as u32 })?;

        let data = match method {
            0 => raw.to_vec(),
            8 => inflate(raw, uncompressed).map_err(|why| ZipError::BadDeflate { why })?,
            other => return Err(ZipError::UnsupportedMethod { method: other }),
        };

        if crc32(&data) != crc {
            return Err(ZipError::BadChecksum { name });
        }
        out.push(File { name, data });
    }
    Ok(out)
}
