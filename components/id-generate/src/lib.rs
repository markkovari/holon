//! `id-generate` — reference implementation of `id:generate/generator`.
//!
//! Every app mints identifiers, and most do it badly: `Date.now() + counter`
//! (not portable, not sortable, collides across processes) or a bare random
//! string (random => it scatters across a key-value store). This component is
//! the correct primitives, hand-rolled, no external crates:
//!
//!   - **ULID** — 26-char Crockford base32, a 48-bit millisecond time prefix
//!     followed by 80 bits of entropy. Because the time is the high bits and
//!     base32 preserves byte order, ULIDs sort *lexicographically* in time
//!     order. That matters for KV stores: records keyed by ULID land in
//!     insertion-time order, so range scans and "most recent N" are cheap. We
//!     also keep them **monotonic within a single millisecond** — when two ids
//!     are minted in the same ms we increment the random component by 1 instead
//!     of redrawing, so even sub-millisecond bursts stay sorted.
//!   - **UUIDv4** — 122 random bits, the universal "just give me a unique id".
//!     Not sortable; use when ordering does not matter.
//!   - **nanoid** — url-safe random id (A-Za-z0-9_-), compact and collision-safe.
//!   - **short-code** — human-friendly random code from an unambiguous alphabet
//!     (no 0/O/1/I/L) for invite / booking codes a person reads aloud.
//!
//! Pure-ish: `wasi:clocks/wall-clock` supplies the ULID time prefix,
//! `wasi:random/random` supplies entropy. The only state is the last-ms /
//! last-random pair used for ULID monotonicity.

#[allow(warnings)]
mod bindings;

use bindings::exports::id::generate::generator::Guest;
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::random::random::get_random_bytes;

struct Component;

// ---- Crockford base32 ----------------------------------------------------

/// Crockford base32 alphabet (32 symbols, excludes I L O U to avoid ambiguity).
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Encode a 128-bit value as exactly 26 Crockford base32 characters, ULID
/// layout. 128 = 26*5 - 2, so the most-significant symbol carries only the top
/// 2 bits of the value (value 0..=3); the remaining 25 symbols are full 5-bit
/// groups, most-significant first. (Verified against the canonical ULID vector
/// `01ARYZ6S41TSV4RRFFQ69G5FAV` -> time 1469918176385.)
fn encode_ulid(value: u128) -> String {
    // 26 Crockford symbols cover 130 bits; the 128-bit value is right-aligned
    // in that space, so encode 5-bit groups from the LSB up. out[25] is bits
    // 0..=4, out[1] is bits 120..=124, out[0] is the top bits (126..=127, with
    // the 130-bit padding above them zero). Encoding from the LSB ensures EVERY
    // bit — including bit 0 — lands in a symbol (the previous top-down version
    // dropped bit 0, so id and id+1 collided).
    let mut out = [0u8; 26];
    for i in 0..26 {
        let shift = 5 * i as u32;
        let bits = if shift >= 128 { 0 } else { ((value >> shift) & 0x1f) as usize };
        out[25 - i] = CROCKFORD[bits];
    }
    // SAFETY: every byte written is an ASCII char from CROCKFORD.
    String::from_utf8(out.to_vec()).unwrap()
}

/// Assemble a ULID from a 48-bit millisecond timestamp and an 80-bit random
/// payload into the canonical 128-bit value: `[48-bit time | 80-bit random]`.
fn assemble(time_ms: u64, rand80: u128) -> u128 {
    let time = (time_ms as u128 & 0xFFFF_FFFF_FFFF) << 80; // top 48 bits
    let rand = rand80 & ((1u128 << 80) - 1); // low 80 bits
    time | rand
}

/// Draw 80 fresh random bits as a u128 (low 80 bits populated).
fn random80() -> u128 {
    let b = get_random_bytes(10); // 10 bytes = 80 bits
    let mut v: u128 = 0;
    for &byte in &b {
        v = (v << 8) | byte as u128;
    }
    v & ((1u128 << 80) - 1)
}

// ---- ULID monotonic state ------------------------------------------------

// Last-issued (millisecond, 80-bit random) pair for monotonicity within a ms.
// WASM components run single-threaded (no shared-nothing threads inside a
// component instance), so a plain `static mut` is sound here — there is no
// concurrent access to guard against.
static mut LAST_MS: u64 = 0;
static mut LAST_RAND: u128 = 0;

/// Current wall-clock time in unix milliseconds.
fn now_ms() -> u64 {
    let n = wall_clock::now();
    n.seconds * 1000 + (n.nanoseconds as u64) / 1_000_000
}

// ---- hex (UUID) ----------------------------------------------------------

const HEX: &[u8; 16] = b"0123456789abcdef";

fn push_hex(out: &mut String, byte: u8) {
    out.push(HEX[(byte >> 4) as usize] as char);
    out.push(HEX[(byte & 0x0f) as usize] as char);
}

impl Guest for Component {
    fn ulid() -> String {
        let ms = now_ms();
        // Single-threaded wasm: this static-mut access is not racy.
        let rand80 = unsafe {
            if ms == LAST_MS {
                // Same millisecond: increment the random component so the new
                // id sorts strictly after the previous one. (80-bit overflow
                // into the next ms is astronomically unlikely and harmless.)
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
        // Explicit timestamp: fresh entropy, no monotonic carry needed (the
        // caller controls ordering via the timestamp they pass).
        encode_ulid(assemble(unix_millis, random80()))
    }

    fn uuid_v4() -> String {
        let mut b = get_random_bytes(16);
        // Version 4 (random) in the high nibble of byte 6.
        b[6] = (b[6] & 0x0f) | 0x40;
        // RFC 4122 variant (10xx) in the high bits of byte 8.
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
        // url-safe alphabet, exactly 64 chars.
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-";
        let len = (length as usize).clamp(1, 64);
        let bytes = get_random_bytes(len as u64);
        let mut out = String::with_capacity(len);
        for &byte in &bytes {
            // 64 divides 256 evenly, so `byte % 64` is perfectly uniform — no
            // modulo bias.
            out.push(ALPHABET[(byte % 64) as usize] as char);
        }
        out
    }

    fn short_code(length: u8) -> String {
        // Unambiguous human alphabet: no 0 O 1 I L. 31 chars.
        const ALPHABET: &[u8; 31] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZ";
        let len = (length as usize).clamp(1, 64);
        let bytes = get_random_bytes(len as u64);
        let mut out = String::with_capacity(len);
        for &byte in &bytes {
            // 31 does not divide 256, so `byte % 31` is *very slightly* biased
            // toward the low symbols. Acceptable for a short human-facing code
            // (not a cryptographic identifier); avoids rejection-sampling cost.
            out.push(ALPHABET[(byte % 31) as usize] as char);
        }
        out
    }
}

bindings::export!(Component with_types_in bindings);
