//! The decoder: PNG bytes in, digits out. Pure compute, no host imports.
//!
//! ## Why a scanline decoder and not a library
//!
//! A linear barcode is one-dimensional. The bars carry no information vertically
//! — the height exists so that a scanner sweeping at an angle still crosses the
//! whole symbol — so a decoder needs one horizontal line of pixels, not an image
//! pipeline. That is what makes this fit in `wasm32-wasip2` with nothing behind
//! it: no host, no dataset, no model. Bytes in, digits out, the same answer
//! every time.
//!
//! ## What stops it returning a plausible constant
//!
//! Three of this repository's contract-only capabilities shipped one
//! (`"mocked_clipboard_text_123"`), and `docs/CURRENT.md` records that as worse
//! than returning nothing, because no caller could tell it from something that
//! works. Two things here make that failure impossible to ship quietly:
//!
//!   * **The check digit.** Every EAN/UPC and Code 128 result is verified
//!     against its own checksum before it is returned. A misread is far more
//!     likely to fail that than to pass it, so a decoder that guesses answers
//!     `err`, not a number.
//!   * **`not-a-barcode.png`.** A blank image is a fixture, and the gate asserts
//!     it decodes to nothing. A constant-returner fails on the first test.

use crate::tables::{C128, C128_STOP, G, L, PARITY, R};

/// What was read, and under which symbology.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub text: String,
    pub symbology: String,
}

/// A run of one colour, in pixels.
#[derive(Clone, Copy)]
struct Run {
    black: bool,
    len: u16,
}

/// How far a measured run may sit from its ideal width before the match is
/// refused, as a fraction of one module. 0.7 is ZXing's figure and the reason it
/// is not tighter is print gain: ink spreads, so bars read wide and spaces read
/// narrow by a consistent amount that this has to absorb without absorbing a
/// genuinely different pattern.
const MAX_VARIANCE: f32 = 0.7;

/// Scanlines to try per orientation. The bars carry nothing vertically, so any
/// row that crosses the whole symbol decodes — but a row through a thumb, a
/// crease or the human-readable digits underneath decodes nothing, which is why
/// this is not 1.
const SCANLINES: usize = 24;

/// Decode the first barcode in a PNG image.
pub fn decode_png(bytes: &[u8]) -> Result<Symbol, String> {
    let (gray, w, h) = to_gray(bytes)?;
    decode_gray(&gray, w, h).ok_or_else(|| "no barcode found".to_string())
}

/// PNG bytes to an 8-bit grayscale buffer.
fn to_gray(bytes: &[u8]) -> Result<(Vec<u8>, usize, usize), String> {
    let decoder = png::Decoder::new(bytes);
    let mut reader = decoder.read_info().map_err(|e| format!("not a readable PNG: {e}"))?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|e| format!("truncated PNG: {e}"))?;
    let (w, h) = (info.width as usize, info.height as usize);
    let px = info.color_type.samples();
    // 16-bit channels arrive big-endian; taking the high byte is the whole of
    // the conversion, and a barcode does not need the low one.
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
    // Rows first, then columns — which is 90 degrees, and 180 comes free because
    // each line is also read backwards. That covers a label held sideways or
    // upside down; a label held at 30 degrees is NOT covered, and the ceiling is
    // named in the crate docs rather than hidden here.
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

/// One line of pixels, both ways round.
fn decode_line(line: &[u8]) -> Option<Symbol> {
    let mut runs = to_runs(line)?;
    if let Some(found) = decode_runs(&runs) {
        return Some(found);
    }
    runs.reverse();
    decode_runs(&runs)
}

/// Pixels to runs of black and white.
///
/// The threshold is the midpoint of this line's own range, not a global or fixed
/// one: a photographed label is brighter at one end than the other, and a single
/// threshold for the whole image turns the dim end into one long black run.
fn to_runs(line: &[u8]) -> Option<Vec<Run>> {
    let (min, max) = line.iter().fold((255u8, 0u8), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    // A line with no contrast is blank paper, and thresholding it produces one
    // run of noise that the pattern matcher then works hard to reject.
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

/// Every symbology, at every position a symbol could start.
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

/// How far `widths` sits from `pattern`, in modules, or `None` if any single run
/// is further out than `MAX_VARIANCE`.
///
/// The module width is derived from THIS symbol's own total rather than the
/// image's, which is what lets one line decode a label that is slightly wider at
/// one end than the other.
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

/// The best-matching digit in one of the three EAN alphabets.
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

/// A guard: `n` runs of one module each, starting with a bar.
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
        // The middle guard is space-bar-space-bar-space, so it starts on a
        // space, and `guard` wants a bar — check its widths and its colour
        // separately rather than teach `guard` a second shape.
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
            // The leading digit is the parity pattern of the left six.
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
        // A UPC-A and an EAN-13 with a leading zero are the SAME BARS. Nothing
        // in the image distinguishes them, so the symbology is reported and the
        // text is not shortened: a caller keying a catalogue on the twelve
        // digits printed on an American box strips the zero itself, and one
        // that wants the GTIN-13 already has it.
        //
        // Returning twelve here was tried and is wrong — it silently truncated
        // `0166131860910`, a perfectly ordinary EAN-13 that begins with 0, in a
        // spot check of eighteen codes the decoder had never seen.
        let symbology = match (half, text.starts_with('0')) {
            (6, true) => "upc-a",
            (6, false) => "ean-13",
            _ => "ean-8",
        };
        return Some(Symbol { text, symbology: symbology.into() });
    }
    None
}

/// The modulo-10 check digit every EAN and UPC carries as its last digit.
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

/// Code 128 — the symbology on shelf and price labels, and the only one here
/// that carries letters.
///
/// Read whole and then decoded, rather than decoded as it is read. The check
/// symbol sits immediately before the stop and is indistinguishable from data
/// until the stop arrives, so a streaming decoder has to append it and take it
/// back — and taking it back means reconstructing its VALUE from the text it
/// already produced, across mode shifts, which is a second decoder to get wrong.
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

/// The Code 128 symbol value these six runs are closest to.
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
