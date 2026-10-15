//! The decoder: PNG bytes in, digits out. Pure compute, no host imports.
//!
//! Why a scanline decoder and not a library: the bars carry no information vertically,
//! so one horizontal line of pixels suffices, not an image pipeline. That is what lets
//! this fit in `wasm32-wasip2` with nothing behind it.
//!
//! What stops it returning a plausible constant (three contract-only capabilities
//! shipped one; `docs/CURRENT.md` calls that worse than nothing):
//!   * **The check digit.** Every EAN/UPC and Code 128 result is verified against its
//!     checksum, so a decoder that guesses answers `err`, not a number.
//!   * **`not-a-barcode.png`.** A blank-image fixture the gate asserts decodes to nothing.

use crate::tables::{C128, C128_STOP, G, L, PARITY, R};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub text: String,
    pub symbology: String,
}

#[derive(Clone, Copy)]
struct Run {
    black: bool,
    len: u16,
}

/// How far a measured run may sit from its ideal width (as a fraction of a module)
/// before the match is refused. 0.7 is ZXing's figure; not tighter because print gain
/// makes bars read wide and spaces narrow by a consistent amount.
const MAX_VARIANCE: f32 = 0.7;

/// Scanlines to try per orientation: a row through a thumb, a crease or the
/// human-readable digits decodes nothing, so not 1.
const SCANLINES: usize = 24;

pub fn decode_png(bytes: &[u8]) -> Result<Symbol, String> {
    let (gray, w, h) = to_gray(bytes)?;
    decode_gray(&gray, w, h).ok_or_else(|| "no barcode found".to_string())
}

fn to_gray(bytes: &[u8]) -> Result<(Vec<u8>, usize, usize), String> {
    let decoder = png::Decoder::new(bytes);
    let mut reader = decoder.read_info().map_err(|e| format!("not a readable PNG: {e}"))?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|e| format!("truncated PNG: {e}"))?;
    let (w, h) = (info.width as usize, info.height as usize);
    let px = info.color_type.samples();
    // 16-bit channels are big-endian; the high byte is all a barcode needs.
    let step = if info.bit_depth == png::BitDepth::Sixteen { 2 } else { 1 };
    let mut gray = Vec::with_capacity(w * h);
    for i in 0..w * h {
        let at = i * px * step;
        let v = match px {
            // Rec. 601 luma. A red-on-white label is a real thing and reads as
            // mid-grey under a plain average, which then binarises wrong.
            3 | 4 => {
                let r = buf[at] as u32;
                let g = buf[at + step] as u32;
                let b = buf[at + 2 * step] as u32;
                ((r * 299 + g * 587 + b * 114) / 1000) as u8
            }
            _ => buf[at],
        };
        gray.push(v);
    }
    Ok((gray, w, h))
}

/// Try every orientation this can handle, and return the first symbol whose
/// checksum holds.
pub fn decode_gray(gray: &[u8], w: usize, h: usize) -> Option<Symbol> {
    if w < 24 || h == 0 || gray.len() < w * h {
        return None;
    }
    // Rows then columns (90 degrees); 180 is free since each line is also read
    // backwards. A label held at 30 degrees is NOT covered (see the crate docs).
    for by_column in [false, true] {
        let (across, along) = if by_column { (h, w) } else { (w, h) };
        for i in 0..SCANLINES {
            // Spread over the middle 90%: the extreme rows are quiet zone or
            // page edge, and the human-readable digits sit at the very bottom.
            let at = along * (i * 2 + 1) / (SCANLINES * 2);
            let line: Vec<u8> = if by_column {
                (0..across).map(|y| gray[y * w + at]).collect()
            } else {
                gray[at * w..at * w + across].to_vec()
            };
            if let Some(found) = decode_line(&line) {
                return Some(found);
            }
        }
    }
    None
}

fn decode_line(line: &[u8]) -> Option<Symbol> {
    let mut runs = to_runs(line)?;
    if let Some(found) = decode_runs(&runs) {
        return Some(found);
    }
    runs.reverse();
    decode_runs(&runs)
}

/// Pixels to runs of black and white. The threshold is the midpoint of this line's own
/// range: a photographed label is brighter at one end, and one global threshold turns
/// the dim end into one long black run.
fn to_runs(line: &[u8]) -> Option<Vec<Run>> {
    let (min, max) = line.iter().fold((255u8, 0u8), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    // No contrast is blank paper; thresholding it yields noise the matcher works hard to reject.
    if max - min < 40 {
        return None;
    }
    let threshold = (min as u16 + max as u16) / 2;
    let mut runs: Vec<Run> = Vec::new();
    for &v in line {
        let black = (v as u16) < threshold;
        match runs.last_mut() {
            Some(r) if r.black == black && r.len < u16::MAX => r.len += 1,
            _ => runs.push(Run { black, len: 1 }),
        }
    }
    Some(runs)
}

fn decode_runs(runs: &[Run]) -> Option<Symbol> {
    for start in 0..runs.len() {
        if !runs[start].black {
            continue;
        }
        if let Some(found) = decode_ean(runs, start).or_else(|| decode_code128(runs, start)) {
            return Some(found);
        }
    }
    None
}

/// How far `widths` sits from `pattern`, in modules, or `None` if any run is further out
/// than `MAX_VARIANCE`. The module width comes from THIS symbol's own total, so one line
/// can decode a label slightly wider at one end.
fn variance(widths: &[u16], pattern: &[u16]) -> Option<f32> {
    let total: u32 = widths.iter().map(|&w| w as u32).sum();
    let modules: u32 = pattern.iter().map(|&w| w as u32).sum();
    if total < modules {
        return None;
    }
    let unit = total as f32 / modules as f32;
    let mut score = 0.0;
    for (&w, &p) in widths.iter().zip(pattern) {
        let diff = (w as f32 - p as f32 * unit).abs();
        if diff > MAX_VARIANCE * unit {
            return None;
        }
        score += diff / unit;
    }
    Some(score)
}

fn digit(widths: &[u16], table: &[[u16; 4]; 10]) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for (d, pattern) in table.iter().enumerate() {
        if let Some(score) = variance(widths, pattern) {
            if best.is_none_or(|(_, b)| score < b) {
                best = Some((d, score));
            }
        }
    }
    best.map(|(d, _)| d)
}

fn widths_at(runs: &[Run], at: usize, n: usize) -> Option<Vec<u16>> {
    runs.get(at..at + n).map(|r| r.iter().map(|r| r.len).collect())
}

fn guard(runs: &[Run], at: usize, n: usize) -> bool {
    let Some(w) = widths_at(runs, at, n) else { return false };
    let ones = vec![1u16; n];
    runs[at].black && variance(&w, &ones).is_some()
}

/// EAN-13, UPC-A and EAN-8 — one layout with two lengths.
fn decode_ean(runs: &[Run], start: usize) -> Option<Symbol> {
    // 13 digits: guard(3) + 6 digits(24) + middle(5) + 6 digits(24) + guard(3).
    // 8 digits:  guard(3) + 4 digits(16) + middle(5) + 4 digits(16) + guard(3).
    for half in [6usize, 4] {
        if !guard(runs, start, 3) {
            return None;
        }
        let left_at = start + 3;
        let middle_at = left_at + half * 4;
        let right_at = middle_at + 5;
        let end_at = right_at + half * 4;
        // The middle guard starts on a space and `guard` wants a bar, so check its
        // widths and colour separately.
        let Some(mid) = widths_at(runs, middle_at, 5) else { continue };
        if runs[middle_at].black || variance(&mid, &[1, 1, 1, 1, 1]).is_none() {
            continue;
        }
        if !guard(runs, end_at, 3) {
            continue;
        }
        let mut digits = Vec::with_capacity(half * 2);
        let mut parity = 0u8;
        let mut ok = true;
        for i in 0..half {
            let Some(w) = widths_at(runs, left_at + i * 4, 4) else {
                ok = false;
                break;
            };
            // L and G are told apart by which table matches, and for EAN-13 that
            // IS the leading digit — it is drawn nowhere else.
            match (digit(&w, &L), digit(&w, &G)) {
                (Some(d), None) => digits.push(d),
                (None, Some(d)) => {
                    parity |= 1 << (half - 1 - i);
                    digits.push(d);
                }
                // Both or neither: ambiguous, and guessing here is how a
                // decoder invents a product code.
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            continue;
        }
        for i in 0..half {
            let Some(w) = widths_at(runs, right_at + i * 4, 4) else {
                ok = false;
                break;
            };
            match digit(&w, &R) {
                Some(d) => digits.push(d),
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            continue;
        }
        let text = if half == 6 {
            let lead = PARITY.iter().position(|&p| p == parity)?;
            let mut s = lead.to_string();
            s.extend(digits.iter().map(|d| char::from_digit(*d as u32, 10).unwrap()));
            s
        } else {
            // EAN-8 has no parity digit: all four left digits are L, and a G
            // among them is a misread, not a leading digit.
            if parity != 0 {
                continue;
            }
            digits.iter().map(|d| char::from_digit(*d as u32, 10).unwrap()).collect()
        };
        if !checksum_ok(&text) {
            continue;
        }
        // A UPC-A and an EAN-13 with a leading zero are the SAME BARS, so the symbology
        // is reported and the text is not shortened. Returning twelve digits silently
        // truncated `0166131860910`, an ordinary EAN-13 that begins with 0.
        let symbology = match (half, text.starts_with('0')) {
            (6, true) => "upc-a",
            (6, false) => "ean-13",
            _ => "ean-8",
        };
        return Some(Symbol { text, symbology: symbology.into() });
    }
    None
}

fn checksum_ok(text: &str) -> bool {
    let d: Vec<u32> = text.chars().filter_map(|c| c.to_digit(10)).collect();
    if d.len() != text.len() || d.len() < 8 {
        return false;
    }
    let (body, check) = d.split_at(d.len() - 1);
    // Weights run 3,1,3,1… backwards from the check digit, which is the same
    // rule for EAN-8 and EAN-13 and is why this needs no length branch.
    let sum: u32 =
        body.iter().rev().enumerate().map(|(i, v)| v * if i % 2 == 0 { 3 } else { 1 }).sum();
    (10 - sum % 10) % 10 == check[0]
}

/// Code 128 — the symbology on shelf and price labels, and the only one here that
/// carries letters. Read whole, then decoded: the check symbol is indistinguishable
/// from data until the stop arrives, and taking it back from a streaming decode means
/// reconstructing its VALUE across mode shifts — a second decoder to get wrong.
fn decode_code128(runs: &[Run], start: usize) -> Option<Symbol> {
    let start_value = best_c128(&widths_at(runs, start, 6)?)?;
    // 103, 104, 105 are Start A, B and C. Any other value means this is the
    // middle of a code rather than its beginning.
    let mut mode = match start_value {
        103 => 'A',
        104 => 'B',
        105 => 'C',
        _ => return None,
    };

    let mut values: Vec<u8> = Vec::new();
    let mut at = start + 6;
    loop {
        let widths = widths_at(runs, at, 6)?;
        // Checked BEFORE the symbol table: some values are close enough to the
        // stop that a wrong answer is available if it is read as data.
        if variance(&widths, &C128_STOP).is_some() {
            break;
        }
        values.push(best_c128(&widths)?);
        at += 6;
        // Code 128 tops out at 80-odd data symbols in practice; well past that
        // is a scanline walking through noise, not a label.
        if values.len() > 128 {
            return None;
        }
    }

    // start + sum(position * value), against the check symbol the code carries.
    let (data, check) = values.split_at(values.len().checked_sub(1)?);
    let sum = data
        .iter()
        .enumerate()
        .fold(start_value as u32, |acc, (i, &v)| acc + (i as u32 + 1) * v as u32);
    if sum % 103 != check[0] as u32 {
        return None;
    }

    let mut text = String::new();
    for &value in data {
        match mode {
            'C' => match value {
                0..=99 => text.push_str(&format!("{value:02}")),
                100 => mode = 'A',
                101 => mode = 'B',
                _ => return None,
            },
            _ => match value {
                // 0..94 is printable ASCII offset by 32 in mode B. Mode A maps
                // 64..94 to control characters, which a shelf label does not
                // carry and this does not decode.
                0..=94 if mode == 'B' || value >= 32 => text.push(char::from(value + 32)),
                99 => mode = 'C',
                100 if mode == 'A' => mode = 'B',
                101 if mode == 'B' => mode = 'A',
                // FNC1..FNC4 and the shifts: real, and not what a price label
                // uses. Refusing beats inventing.
                _ => return None,
            },
        }
    }
    if text.is_empty() {
        return None;
    }
    Some(Symbol { text, symbology: "code-128".into() })
}

fn best_c128(widths: &[u16]) -> Option<u8> {
    let mut best: Option<(u8, f32)> = None;
    for (v, pattern) in C128.iter().enumerate() {
        if let Some(score) = variance(widths, pattern) {
            if best.is_none_or(|(_, b)| score < b) {
                best = Some((v as u8, score));
            }
        }
    }
    best.map(|(v, _)| v)
}
