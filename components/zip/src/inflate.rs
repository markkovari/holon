//! DEFLATE (RFC 1951), decode only.
//!
//! Here because a `.xlsx` is a ZIP of XML and every spreadsheet writes it with
//! DEFLATE, so a reader that only handles STORE refuses every real file.
//!
//! Written out rather than taken as a dependency for the same reason the writer
//! above is: this crate builds for `wasm32-wasip2` and stays dependency-free, and
//! the decoder is about two hundred lines of well-specified table lookups. The
//! ENCODER is still out of scope — writing STORE is honest and small, and nothing
//! here needs to make an archive smaller.

/// Bits, least-significant first, which is the order DEFLATE packs them in.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    bit: u32,
    acc: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Bits { data, pos: 0, bit: 0, acc: 0 }
    }

    fn need(&mut self, n: u32) -> Result<(), String> {
        while self.bit < n {
            let byte = *self.data.get(self.pos).ok_or("ran out of input")? as u32;
            self.acc |= byte << self.bit;
            self.bit += 8;
            self.pos += 1;
        }
        Ok(())
    }

    fn take(&mut self, n: u32) -> Result<u32, String> {
        if n == 0 {
            return Ok(0);
        }
        self.need(n)?;
        let out = self.acc & ((1u32 << n) - 1);
        self.acc >>= n;
        self.bit -= n;
        Ok(out)
    }

    /// Drop to the next byte boundary — a stored block starts there.
    fn align(&mut self) {
        let drop = self.bit % 8;
        self.acc >>= drop;
        self.bit -= drop;
    }
}

/// A canonical Huffman table: for each code length, the first code and the symbols
/// that use it. Decoding walks bit by bit, which is slower than a lookup table and
/// is not the bottleneck for a spreadsheet.
struct Huffman {
    /// counts[len] = how many codes have this length
    counts: [u16; 16],
    /// symbols, ordered by (length, symbol)
    symbols: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Result<Self, String> {
        let mut counts = [0u16; 16];
        for &l in lengths {
            if l as usize >= 16 {
                return Err("a code length above 15".into());
            }
            counts[l as usize] += 1;
        }
        // Length 0 means "unused", not "a zero-bit code".
        counts[0] = 0;

        let mut offsets = [0u16; 16];
        for i in 1..15 {
            offsets[i + 1] = offsets[i] + counts[i];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (sym, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbols[offsets[l as usize] as usize] = sym as u16;
                offsets[l as usize] += 1;
            }
        }
        Ok(Huffman { counts, symbols })
    }

    fn decode(&self, bits: &mut Bits) -> Result<u16, String> {
        let mut code = 0i32;
        let mut first = 0i32;
        let mut index = 0i32;
        for len in 1..16 {
            code |= bits.take(1)? as i32;
            let count = self.counts[len] as i32;
            if code - first < count {
                return Ok(self.symbols[(index + (code - first)) as usize]);
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err("a code longer than 15 bits".into())
    }
}

const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u8; 29] =
    [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

fn fixed_tables() -> (Huffman, Huffman) {
    let mut lit = [0u8; 288];
    for (i, l) in lit.iter_mut().enumerate() {
        *l = match i {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    let dist = [5u8; 30];
    (Huffman::new(&lit).expect("fixed literal table"), Huffman::new(&dist).expect("fixed distances"))
}

/// The order RFC 1951 stores the code-length code lengths in. Not sorted, on
/// purpose: the lengths most likely to be zero are last, so a truncated list is
/// usually enough.
const CLEN_ORDER: [usize; 19] =
    [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

fn dynamic_tables(bits: &mut Bits) -> Result<(Huffman, Huffman), String> {
    let hlit = bits.take(5)? as usize + 257;
    let hdist = bits.take(5)? as usize + 1;
    let hclen = bits.take(4)? as usize + 4;

    let mut clen = [0u8; 19];
    for &slot in CLEN_ORDER.iter().take(hclen) {
        clen[slot] = bits.take(3)? as u8;
    }
    let clen_table = Huffman::new(&clen)?;

    let mut lengths = vec![0u8; hlit + hdist];
    let mut i = 0;
    while i < lengths.len() {
        let sym = clen_table.decode(bits)?;
        match sym {
            0..=15 => {
                lengths[i] = sym as u8;
                i += 1;
            }
            // Repeat the PREVIOUS length 3-6 times.
            16 => {
                if i == 0 {
                    return Err("a repeat with nothing to repeat".into());
                }
                let prev = lengths[i - 1];
                let n = 3 + bits.take(2)? as usize;
                for _ in 0..n {
                    if i >= lengths.len() {
                        return Err("a repeat past the end of the table".into());
                    }
                    lengths[i] = prev;
                    i += 1;
                }
            }
            // Repeat ZERO 3-10, then 11-138.
            17 | 18 => {
                let n = if sym == 17 { 3 + bits.take(3)? as usize } else { 11 + bits.take(7)? as usize };
                i = (i + n).min(lengths.len());
            }
            _ => return Err("a code-length symbol above 18".into()),
        }
    }
    let (lit, dist) = lengths.split_at(hlit);
    Ok((Huffman::new(lit)?, Huffman::new(dist)?))
}

/// Decompress a raw DEFLATE stream. `expected` sizes the output buffer; it is a
/// hint, not a limit, because a wrong one in the archive must not truncate a file.
pub fn inflate(data: &[u8], expected: usize) -> Result<Vec<u8>, String> {
    let mut bits = Bits::new(data);
    let mut out: Vec<u8> = Vec::with_capacity(expected);

    loop {
        let last = bits.take(1)?;
        match bits.take(2)? {
            // Stored: byte-aligned, a length and its complement, then raw bytes.
            0 => {
                bits.align();
                let len = bits.take(16)? as usize;
                let nlen = bits.take(16)? as usize;
                if len != !nlen & 0xFFFF {
                    return Err("a stored block whose length and complement disagree".into());
                }
                for _ in 0..len {
                    out.push(bits.take(8)? as u8);
                }
            }
            code @ (1 | 2) => {
                let (lit, dist) =
                    if code == 1 { fixed_tables() } else { dynamic_tables(&mut bits)? };
                loop {
                    let sym = lit.decode(&mut bits)? as usize;
                    match sym {
                        0..=255 => out.push(sym as u8),
                        256 => break,
                        257..=285 => {
                            let i = sym - 257;
                            let length =
                                LENGTH_BASE[i] as usize + bits.take(LENGTH_EXTRA[i] as u32)? as usize;
                            let d = dist.decode(&mut bits)? as usize;
                            if d >= 30 {
                                return Err("a distance symbol above 29".into());
                            }
                            let distance =
                                DIST_BASE[d] as usize + bits.take(DIST_EXTRA[d] as u32)? as usize;
                            if distance > out.len() {
                                return Err("a back-reference before the start of the output".into());
                            }
                            // Byte at a time: the ranges may overlap, which is how
                            // DEFLATE expresses a run.
                            let start = out.len() - distance;
                            for k in 0..length {
                                let b = out[start + k];
                                out.push(b);
                            }
                        }
                        _ => return Err("a literal/length symbol above 285".into()),
                    }
                }
            }
            _ => return Err("a block with reserved type 3".into()),
        }
        if last == 1 {
            return Ok(out);
        }
    }
}
