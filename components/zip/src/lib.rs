//! `zip` — write a .zip archive from a list of named byte blobs
//!
//! A dependency-free ZIP writer using the STORE method (no compression): for
//! each file a local header + its raw bytes, then a central directory and the
//! end-of-central-directory record. Every value is little-endian; each entry
//! carries a CRC-32 (IEEE, computed bit-by-bit — fine for the small text/CSV/JSON
//! blobs this bundles). Any unzip tool reads the result. No state, no host
//! imports, no external crates.

mod inflate;
mod read;

pub use read::{extract, ZipError};

/// One archive member. The plain-Rust twin of the WIT record, so the held-out
/// specification can judge this crate without a component runtime — the pattern
/// `components/bytes-codec` uses, and the reason this crate is now an `rlib` too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    pub name: String,
    pub data: Vec<u8>,
}

/// CRC-32 (IEEE 802.3, polynomial 0xEDB88320), the checksum ZIP entries carry.
pub(crate) fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

fn u16le(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn u32le(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

// A fixed valid DOS date (1980-01-01) + time (00:00) — day 0 would be invalid.
const DOS_TIME: u16 = 0;
const DOS_DATE: u16 = 0x0021;

/// Build a complete ZIP archive (STORE method) from `files`.
pub fn archive(files: &[File]) -> Vec<u8> {
    {
        let mut out = Vec::new();
        // (crc, size, local-header-offset, name) per entry, for the central dir.
        let mut central: Vec<(u32, u32, u32, Vec<u8>)> = Vec::new();

        for f in files {
            let name = f.name.as_bytes();
            let crc = crc32(&f.data);
            let size = f.data.len() as u32;
            let offset = out.len() as u32;

            // local file header
            u32le(&mut out, 0x0403_4b50);
            u16le(&mut out, 20); // version needed
            u16le(&mut out, 0); // flags
            u16le(&mut out, 0); // method = store
            u16le(&mut out, DOS_TIME);
            u16le(&mut out, DOS_DATE);
            u32le(&mut out, crc);
            u32le(&mut out, size); // compressed == uncompressed (store)
            u32le(&mut out, size);
            u16le(&mut out, name.len() as u16);
            u16le(&mut out, 0); // extra len
            out.extend_from_slice(name);
            out.extend_from_slice(&f.data);

            central.push((crc, size, offset, name.to_vec()));
        }

        // central directory
        let cd_offset = out.len() as u32;
        for (crc, size, offset, name) in &central {
            u32le(&mut out, 0x0201_4b50);
            u16le(&mut out, 20); // version made by
            u16le(&mut out, 20); // version needed
            u16le(&mut out, 0); // flags
            u16le(&mut out, 0); // method = store
            u16le(&mut out, DOS_TIME);
            u16le(&mut out, DOS_DATE);
            u32le(&mut out, *crc);
            u32le(&mut out, *size);
            u32le(&mut out, *size);
            u16le(&mut out, name.len() as u16);
            u16le(&mut out, 0); // extra len
            u16le(&mut out, 0); // comment len
            u16le(&mut out, 0); // disk number start
            u16le(&mut out, 0); // internal attrs
            u32le(&mut out, 0); // external attrs
            u32le(&mut out, *offset);
            out.extend_from_slice(name);
        }
        let cd_size = out.len() as u32 - cd_offset;

        // end of central directory
        u32le(&mut out, 0x0605_4b50);
        u16le(&mut out, 0); // this disk
        u16le(&mut out, 0); // disk with cd
        u16le(&mut out, central.len() as u16); // entries this disk
        u16le(&mut out, central.len() as u16); // total entries
        u32le(&mut out, cd_size);
        u32le(&mut out, cd_offset);
        u16le(&mut out, 0); // comment len

        out
    }
}

// ---- the component -----------------------------------------------------
//
// A mapping between the WIT types and the ones above, and nothing else — the logic
// is judged by `tests/` against the plain functions.

#[cfg(target_arch = "wasm32")]
#[allow(warnings)]
mod bindings;

#[cfg(target_arch = "wasm32")]
use bindings::exports::zip::archive::archiver as w;

#[cfg(target_arch = "wasm32")]
struct Component;

#[cfg(target_arch = "wasm32")]
impl w::Guest for Component {
    fn archive(files: Vec<w::File>) -> Vec<u8> {
        let files: Vec<File> =
            files.into_iter().map(|f| File { name: f.name, data: f.data }).collect();
        crate::archive(&files)
    }

    fn extract(bytes: Vec<u8>) -> Result<Vec<w::File>, w::ZipError> {
        crate::extract(&bytes)
            .map(|fs| fs.into_iter().map(|f| w::File { name: f.name, data: f.data }).collect())
            .map_err(|e| match e {
                ZipError::NotAZip => w::ZipError::NotAZip,
                ZipError::Truncated { at } => w::ZipError::Truncated(at),
                ZipError::UnsupportedMethod { method } => w::ZipError::UnsupportedMethod(method),
                ZipError::BadChecksum { name } => w::ZipError::BadChecksum(name),
                ZipError::BadDeflate { why } => w::ZipError::BadDeflate(why),
            })
    }
}

#[cfg(target_arch = "wasm32")]
bindings::export!(Component with_types_in bindings);
