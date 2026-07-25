//! `zip` — reference implementation of `zip:archive/archiver`.
//!
//! A dependency-free ZIP writer using the STORE method (no compression): for
//! each file a local header + its raw bytes, then a central directory and the
//! end-of-central-directory record. Every value is little-endian; each entry
//! carries a CRC-32 (IEEE, computed bit-by-bit — fine for the small text/CSV/JSON
//! blobs this bundles). Any unzip tool reads the result. No state, no host
//! imports, no external crates.

#[allow(warnings)]
mod bindings;

use bindings::exports::zip::archive::archiver::{File, Guest};

struct Component;

/// CRC-32 (IEEE 802.3, polynomial 0xEDB88320), the checksum ZIP entries carry.
fn crc32(data: &[u8]) -> u32 {
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

impl Guest for Component {
    fn archive(files: Vec<File>) -> Vec<u8> {
        let mut out = Vec::new();
        // (crc, size, local-header-offset, name) per entry, for the central dir.
        let mut central: Vec<(u32, u32, u32, Vec<u8>)> = Vec::new();

        for f in &files {
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

bindings::export!(Component with_types_in bindings);
