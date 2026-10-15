//! `pdf` — write a PDF document — text laid onto pages, with no native library and no headless browser
//!
//! A dependency-free PDF 1.4 writer: flows text blocks down US-Letter pages (612×792 pt, 54 pt margins), starting a new
//! page when the next line won't fit, and emits a complete file (object table, built-in Type1 Helvetica / Helvetica-Bold,
//! a content stream per page, `xref`, `trailer`). No state, no host imports, no external crates.
//!
//! Object numbering: 1 = Catalog, 2 = Pages, 3 = Helvetica, 4 = Helvetica-Bold, then two consecutive objects per page
//! (page dict, content stream) from 5. Byte offsets are recorded as the file is serialized so the `xref` table is exact:
//! a wrong offset is the classic way to produce a file readers reject.
//!
//! ponytail: Latin-1 text only (code points > 255 become `?`); embed a font subset if Unicode reports matter.

#[allow(warnings)]
mod bindings;

use bindings::exports::pdf::codec::codec::{Block, Document, Guest};

struct Component;

const PAGE_W: i32 = 612;
const PAGE_H: i32 = 792;
const MARGIN: i32 = 54;
const TOP: i32 = PAGE_H - MARGIN; // first baseline ceiling
const DEFAULT_SIZE: u32 = 11;

/// A single placed line: baseline position, font, size, escaped bytes.
struct Line {
    x: i32,
    y: i32,
    size: u32,
    bold: bool,
    text: Vec<u8>,
}

/// Map a code point to its WinAnsi (CP1252) byte, or `None` if the built-in fonts have no glyph. Latin-1 is identity; common "smart" punctuation lives in 0x80–0x9F.
fn winansi(c: char) -> Option<u8> {
    Some(match c {
        '—' => 0x97, // em dash
        '–' => 0x96, // en dash
        '‘' => 0x91,
        '’' => 0x92,
        '“' => 0x93,
        '”' => 0x94,
        '•' => 0x95,
        '…' => 0x85,
        '€' => 0x80,
        '™' => 0x99,
        c if (c as u32) <= 0xff => c as u8,
        _ => return None,
    })
}

/// Escape one string into PDF-literal bytes for WinAnsi text: `(`, `)`, `\` are backslash-escaped; unrepresentable code points become `?`.
fn escape(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '(' | ')' | '\\' => {
                out.push(b'\\');
                out.push(c as u8);
            }
            c => out.push(winansi(c).unwrap_or(b'?')),
        }
    }
    out
}

/// Lay blocks out into pages of placed lines. A block taller than the space left starts a new page.
fn layout(doc: &Document) -> Vec<Vec<Line>> {
    let mut pages: Vec<Vec<Line>> = vec![Vec::new()];
    let mut y = TOP;

    let mut blocks: Vec<Block> = Vec::new();
    if !doc.title.is_empty() {
        blocks.push(Block { text: doc.title.clone(), size: 20, bold: true, gap_before: 0 });
        blocks.push(Block { text: String::new(), size: 6, bold: false, gap_before: 0 });
    }
    blocks.extend(doc.blocks.iter().cloned());

    for b in &blocks {
        let size = if b.size == 0 { DEFAULT_SIZE } else { b.size };
        let leading = (size as i32) * 7 / 5 + 2; // ~1.4× the size
        y -= b.gap_before as i32;
        if y - leading < MARGIN {
            pages.push(Vec::new());
            y = TOP;
        }
        y -= leading;
        pages.last_mut().unwrap().push(Line {
            x: MARGIN,
            y,
            size,
            bold: b.bold,
            text: escape(&b.text),
        });
    }
    pages
}

/// The content stream bytes for one page's lines.
fn content(lines: &[Line]) -> Vec<u8> {
    let mut s = Vec::new();
    for ln in lines {
        if ln.text.is_empty() {
            continue;
        }
        let font = if ln.bold { "F2" } else { "F1" };
        s.extend_from_slice(
            format!("BT /{} {} Tf {} {} Td (", font, ln.size, ln.x, ln.y).as_bytes(),
        );
        s.extend_from_slice(&ln.text);
        s.extend_from_slice(b") Tj ET\n");
    }
    s
}

impl Guest for Component {
    fn render(doc: Document) -> Vec<u8> {
        let pages = layout(&doc);
        let n_pages = pages.len();

        // Object bodies indexed by (object number - 1). Fonts are 3 and 4; page i occupies objects 5+2i (page dict) and 6+2i (content stream).
        let page_obj = |i: usize| 5 + 2 * i;
        let content_obj = |i: usize| 6 + 2 * i;

        let kids: Vec<String> = (0..n_pages).map(|i| format!("{} 0 R", page_obj(i))).collect();

        let mut objects: Vec<Vec<u8>> = Vec::new();
        objects.push(b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());
        objects.push(
            format!(
                "<< /Type /Pages /Kids [ {} ] /Count {} /MediaBox [0 0 {} {}] >>",
                kids.join(" "),
                n_pages,
                PAGE_W,
                PAGE_H
            )
            .into_bytes(),
        );
        objects.push(
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                .to_vec(),
        );
        objects.push(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>".to_vec());

        objects.resize(4 + 2 * n_pages, Vec::new());
        for (i, lines) in pages.iter().enumerate() {
            let cbytes = content(lines);
            let page = format!(
                "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 3 0 R /F2 4 0 R >> >> /Contents {} 0 R >>",
                content_obj(i)
            );
            let mut stream = format!("<< /Length {} >>\nstream\n", cbytes.len()).into_bytes();
            stream.extend_from_slice(&cbytes);
            stream.extend_from_slice(b"\nendstream");
            objects[page_obj(i) - 1] = page.into_bytes();
            objects[content_obj(i) - 1] = stream;
        }

        let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::with_capacity(objects.len());
        for (i, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref_at = out.len();
        let size = objects.len() + 1;
        out.extend_from_slice(format!("xref\n0 {}\n", size).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for off in &offsets {
            out.extend_from_slice(format!("{:010} 00000 n \n", off).as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF", size, xref_at)
                .as_bytes(),
        );
        out
    }
}

bindings::export!(Component with_types_in bindings);
