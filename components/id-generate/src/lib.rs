//! ULIDs are monotonic within a millisecond: a second id increments the random part instead of redrawing.

#[allow(warnings)]
mod bindings;

use bindings::exports::id::generate::generator::Guest;
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::random::random::get_random_bytes;

struct Component;

/// Excludes I, L, O, U to avoid ambiguity.
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// 128 = 26*5 - 2: the top symbol carries only 2 bits. Checked against `01ARYZ6S41TSV4RRFFQ69G5FAV`.
fn encode_ulid(value: u128) -> String {
    // From the LSB, so bit 0 lands in a symbol (top-down dropped it: id and id+1 collided).
    let mut out = [0u8; 26];
    for i in 0..26 {
        let shift = 5 * i as u32;
        let bits = if shift >= 128 { 0 } else { ((value >> shift) & 0x1f) as usize };
        out[25 - i] = CROCKFORD[bits];
    }
    // SAFETY: every byte written is an ASCII char from CROCKFORD.
    String::from_utf8(out.to_vec()).unwrap()
}

fn assemble(time_ms: u64, rand80: u128) -> u128 {
    let time = (time_ms as u128 & 0xFFFF_FFFF_FFFF) << 80;
    let rand = rand80 & ((1u128 << 80) - 1);
    time | rand
}

fn random80() -> u128 {
    let b = get_random_bytes(10);
    let mut v: u128 = 0;
    for &byte in &b {
        v = (v << 8) | byte as u128;
    }
    v & ((1u128 << 80) - 1)
}

// WASM components run single-threaded, so a plain `static mut` is sound.
static mut LAST_MS: u64 = 0;
static mut LAST_RAND: u128 = 0;

fn now_ms() -> u64 {
    let n = wall_clock::now();
    n.seconds * 1000 + (n.nanoseconds as u64) / 1_000_000
}

const HEX: &[u8; 16] = b"0123456789abcdef";

fn push_hex(out: &mut String, byte: u8) {
    out.push(HEX[(byte >> 4) as usize] as char);
    out.push(HEX[(byte & 0x0f) as usize] as char);
}

impl Guest for Component {
    fn ulid() -> String {
        let ms = now_ms();
        let rand80 = unsafe {
            if ms == LAST_MS {
                // Same millisecond: increment so the id sorts strictly after the previous one.
                LAST_RAND = LAST_RAND.wrapping_add(1) & ((1u128 << 80) - 1);
                LAST_RAND
            } else {
                let r = random80();
                LAST_MS = ms;
                LAST_RAND = r;
                r
            }
        };
        encode_ulid(assemble(ms, rand80))
    }

    fn ulid_at(unix_millis: u64) -> String {
        // Explicit timestamp: the caller controls ordering, so no monotonic carry.
        encode_ulid(assemble(unix_millis, random80()))
    }

    fn uuid_v4() -> String {
        let mut b = get_random_bytes(16);
        b[6] = (b[6] & 0x0f) | 0x40;
        b[8] = (b[8] & 0x3f) | 0x80;

        let mut out = String::with_capacity(36);
        for (i, &byte) in b.iter().enumerate() {
            if i == 4 || i == 6 || i == 8 || i == 10 {
                out.push('-');
            }
            push_hex(&mut out, byte);
        }
        out
    }

    fn nanoid(length: u8) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-";
        let len = (length as usize).clamp(1, 64);
        let bytes = get_random_bytes(len as u64);
        let mut out = String::with_capacity(len);
        for &byte in &bytes {
            // 64 divides 256 evenly, so `byte % 64` has no modulo bias.
            out.push(ALPHABET[(byte % 64) as usize] as char);
        }
        out
    }

    fn short_code(length: u8) -> String {
        const ALPHABET: &[u8; 31] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZ";
        let len = (length as usize).clamp(1, 64);
        let bytes = get_random_bytes(len as u64);
        let mut out = String::with_capacity(len);
        for &byte in &bytes {
            // `byte % 31` is slightly biased: fine for a short human code, and avoids rejection sampling.
            out.push(ALPHABET[(byte % 31) as usize] as char);
        }
        out
    }
}

bindings::export!(Component with_types_in bindings);
