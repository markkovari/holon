//! `sheet-ingest` — a `.csv` or `.xlsx` someone exported, turned into rows
//!
//! `tests/xlsx.rs` is the specification and is not writable from here.
//!
//! ## What it does not do, on purpose
//!
//! It does not parse CSV and it does not open ZIP archives. `csv:codec` already
//! does the first — quoted fields, embedded delimiters, ragged rows — and
//! `zip:archive` does the second. Both are imported. What is left is the part
//! neither knows about: the OOXML shape of a worksheet.
//!
//! ## The two things a naive xlsx reader gets wrong
//!
//! **A gap is an omission, not a blank.** `<c r="A1"/><c r="C1"/>` is three columns
//! with the middle one empty, and a reader that pushes cells in document order
//! silently shifts every value left of the gap into the wrong column. That is a
//! bulk insert writing the price into the quantity field, and nothing fails. The
//! cell reference carries the column letter for exactly this reason, so the letter
//! is what decides position here — never the order.
//!
//! **Text lives somewhere else.** A cell with `t="s"` holds an INDEX into
//! `xl/sharedStrings.xml`, not a string. Reading the number is how a spreadsheet of
//! names becomes a spreadsheet of small integers.

/// One row, already lined up with the header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub cells: Vec<String>,
}

/// A sheet in the shape a bulk insert wants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sheet {
    pub header: Vec<String>,
    pub rows: Vec<Row>,
    pub sheet_name: String,
}

/// Why a file could not be read as a sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportError {
    UnknownFormat(String),
    Empty,
    NoHeader,
    DuplicateColumn(String),
    TooManyCells { row: u32, expected: u32, found: u32 },
    Archive(String),
    NoSheet,
    Csv(String),
}

/// `A` -> 0, `Z` -> 25, `AA` -> 26. The letters of a cell reference like `BC12`.
///
/// This is what decides a cell's column. Document order does not, because a row
/// with a gap in it omits the cell rather than emitting an empty one.
pub fn column_of(reference: &str) -> Option<usize> {
    let letters: String =
        reference.chars().take_while(|c| c.is_ascii_alphabetic()).collect::<String>().to_uppercase();
    if letters.is_empty() {
        return None;
    }
    let mut n = 0usize;
    for c in letters.chars() {
        n = n * 26 + (c as usize - 'A' as usize + 1);
    }
    Some(n - 1)
}

/// The text between the next `<tag ...>` and its `</tag>`, from `at`.
fn tag_text(xml: &str, tag: &str, from: usize) -> Option<(String, usize)> {
    let open = xml[from..].find(&format!("<{tag}"))? + from;
    let gt = xml[open..].find('>')? + open;
    // `<t/>` — an empty element, which is how a blank shared string is stored.
    if xml.as_bytes().get(gt - 1) == Some(&b'/') {
        return Some((String::new(), gt + 1));
    }
    let close = xml[gt..].find(&format!("</{tag}>"))? + gt;
    Some((unescape(&xml[gt + 1..close]), close + tag.len() + 3))
}

/// The five XML entities. Not a general parser: OOXML writes these and nothing
/// else, and a sheet full of `&amp;` where an `&` belongs is a visible bug.
fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        // Last, or `&amp;lt;` becomes `<`.
        .replace("&amp;", "&")
}

/// `xl/sharedStrings.xml` — the strings a sheet refers to by index.
///
/// A `<si>` may hold one `<t>` or several inside `<r>` runs when the text was
/// formatted mid-cell; the runs concatenate, which is what the spreadsheet shows.
pub fn shared_strings(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while let Some(open) = xml[at..].find("<si>").map(|i| i + at) {
        let end = match xml[open..].find("</si>") {
            Some(i) => i + open,
            None => break,
        };
        let body = &xml[open + 4..end];
        let mut text = String::new();
        let mut inner = 0usize;
        while let Some((t, next)) = tag_text(body, "t", inner) {
            text.push_str(&t);
            inner = next;
            if inner >= body.len() {
                break;
            }
        }
        out.push(text);
        at = end + 5;
    }
    out
}

/// `xl/worksheets/sheetN.xml` into rows of cells, positioned by column letter.
pub fn worksheet_rows(xml: &str, shared: &[String]) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut at = 0usize;
    while let Some(open) = xml[at..].find("<row").map(|i| i + at) {
        let head_end = match xml[open..].find('>') {
            Some(i) => i + open,
            None => break,
        };
        // `<row r="3"/>` — an empty row, which some writers emit for styling.
        let (body, next) = if xml.as_bytes()[head_end - 1] == b'/' {
            ("", head_end + 1)
        } else {
            match xml[head_end..].find("</row>") {
                Some(i) => (&xml[head_end + 1..head_end + i], head_end + i + 6),
                None => break,
            }
        };
        at = next;

        let mut cells: Vec<String> = Vec::new();
        let mut c = 0usize;
        while let Some(copen) = body[c..].find("<c ").map(|i| i + c) {
            let chead = match body[copen..].find('>') {
                Some(i) => i + copen,
                None => break,
            };
            let attrs = &body[copen..chead];
            let column = attrs
                .find("r=\"")
                .and_then(|i| {
                    let rest = &attrs[i + 3..];
                    rest.find('"').and_then(|j| column_of(&rest[..j]))
                })
                .unwrap_or(cells.len());
            let kind = attrs.find("t=\"").map(|i| {
                let rest = &attrs[i + 3..];
                rest[..rest.find('"').unwrap_or(0)].to_string()
            });

            let (cbody, cnext) = if body.as_bytes()[chead - 1] == b'/' {
                // `<c r="B2"/>` — present but empty.
                ("", chead + 1)
            } else {
                match body[chead..].find("</c>") {
                    Some(i) => (&body[chead + 1..chead + i], chead + i + 4),
                    None => break,
                }
            };
            c = cnext;

            let value = match kind.as_deref() {
                // An index into the shared strings, not a number.
                Some("s") => tag_text(cbody, "v", 0)
                    .and_then(|(v, _)| v.trim().parse::<usize>().ok())
                    .and_then(|i| shared.get(i).cloned())
                    .unwrap_or_default(),
                // Text stored in the cell itself.
                Some("inlineStr") => tag_text(cbody, "t", 0).map(|(v, _)| v).unwrap_or_default(),
                // Everything else — numbers, dates as serial numbers, booleans —
                // comes back as the literal `<v>`. Formatting is a rendering
                // concern and this is not a renderer.
                _ => tag_text(cbody, "v", 0).map(|(v, _)| v).unwrap_or_default(),
            };

            // Positioned by LETTER. The gap between `A` and `C` is a real empty
            // column, and pushing in document order would shift every later value
            // one place left.
            if cells.len() <= column {
                cells.resize(column + 1, String::new());
            }
            cells[column] = value;
        }
        rows.push(cells);
    }
    rows
}

/// Rows into a `Sheet`: first row is the header, the rest are lined up with it.
pub fn shape(mut rows: Vec<Vec<String>>, sheet_name: &str) -> Result<Sheet, ImportError> {
    // Trailing blank rows are what a spreadsheet leaves behind when someone deletes
    // content without deleting the row.
    while rows.last().is_some_and(|r| r.iter().all(|c| c.trim().is_empty())) {
        rows.pop();
    }
    if rows.is_empty() {
        return Err(ImportError::Empty);
    }
    let header: Vec<String> = rows.remove(0).iter().map(|h| h.trim().to_string()).collect();
    if header.iter().all(|h| h.is_empty()) {
        return Err(ImportError::NoHeader);
    }
    // Trailing empty header columns: a spreadsheet's used range often runs wider
    // than the data. Named columns are the contract; unnamed ones are noise.
    let width = header.iter().rposition(|h| !h.is_empty()).map(|i| i + 1).unwrap_or(0);
    let header: Vec<String> = header.into_iter().take(width).collect();

    let mut seen = std::collections::BTreeSet::new();
    for h in &header {
        if !seen.insert(h.clone()) {
            return Err(ImportError::DuplicateColumn(h.clone()));
        }
    }

    let mut out = Vec::with_capacity(rows.len());
    for (i, mut cells) in rows.into_iter().enumerate() {
        // A row longer than the header, once its trailing blanks are gone, is a
        // cell nobody asked to drop.
        while cells.len() > width && cells.last().is_some_and(|c| c.trim().is_empty()) {
            cells.pop();
        }
        if cells.len() > width {
            return Err(ImportError::TooManyCells {
                row: i as u32 + 2,
                expected: width as u32,
                found: cells.len() as u32,
            });
        }
        cells.resize(width, String::new());
        out.push(Row { cells });
    }
    Ok(Sheet { header, rows: out, sheet_name: sheet_name.to_string() })
}

/// Which reader a filename asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Comma-delimited.
    Csv,
    /// Tab-delimited. Same parser, different dialect.
    Tsv,
    Xlsx,
}

/// By EXTENSION, not by sniffing. The bytes of a CSV are not reliably
/// distinguishable from any other text, and guessing gives a confident wrong answer
/// on the first semicolon-delimited European export.
pub fn format_of(name: &str) -> Result<Format, ImportError> {
    let lower = name.to_lowercase();
    if lower.ends_with(".csv") {
        Ok(Format::Csv)
    } else if lower.ends_with(".tsv") || lower.ends_with(".tab") {
        Ok(Format::Tsv)
    } else if lower.ends_with(".xlsx") {
        Ok(Format::Xlsx)
    } else {
        Err(ImportError::UnknownFormat(name.to_string()))
    }
}

/// The worksheet part of an `.xlsx`, given its already-extracted members.
///
/// `sheet1.xml` rather than "the one the workbook says is first": the workbook's
/// relationship graph is another two files to parse, and every writer in the world
/// numbers the first sheet 1. Named in the WIT as a limit rather than hidden.
pub fn xlsx_from_parts(parts: &[(String, Vec<u8>)]) -> Result<Sheet, ImportError> {
    let find = |want: &str| {
        parts.iter().find(|(n, _)| n == want).map(|(_, d)| String::from_utf8_lossy(d).to_string())
    };
    let shared = find("xl/sharedStrings.xml").map(|x| shared_strings(&x)).unwrap_or_default();
    let (name, sheet_xml) = parts
        .iter()
        .filter(|(n, _)| n.starts_with("xl/worksheets/") && n.ends_with(".xml"))
        .min_by_key(|(n, _)| n.clone())
        .map(|(n, d)| (n.clone(), String::from_utf8_lossy(d).to_string()))
        .ok_or(ImportError::NoSheet)?;
    let short =
        name.rsplit('/').next().unwrap_or(&name).trim_end_matches(".xml").to_string();
    shape(worksheet_rows(&sheet_xml, &shared), &short)
}

// ---- the component -----------------------------------------------------
//
// The two IMPORTS are the point of this component: the CSV is parsed by
// `csv:codec` and the container is opened by `zip:archive`, both of which already
// existed. What is written here is the OOXML shape, which neither knows about.

#[cfg(target_arch = "wasm32")]
#[allow(warnings)]
mod bindings;

#[cfg(target_arch = "wasm32")]
use bindings::exports::sheet::ingest::reader as w;

#[cfg(target_arch = "wasm32")]
struct Component;

#[cfg(target_arch = "wasm32")]
fn err_out(e: ImportError) -> w::ImportError {
    match e {
        ImportError::UnknownFormat(s) => w::ImportError::UnknownFormat(s),
        ImportError::Empty => w::ImportError::Empty,
        ImportError::NoHeader => w::ImportError::NoHeader,
        ImportError::DuplicateColumn(s) => w::ImportError::DuplicateColumn(s),
        ImportError::TooManyCells { row, expected, found } => {
            w::ImportError::TooManyCells((row, expected, found))
        }
        ImportError::Archive(s) => w::ImportError::Archive(s),
        ImportError::NoSheet => w::ImportError::NoSheet,
        ImportError::Csv(s) => w::ImportError::Csv(s),
    }
}

#[cfg(target_arch = "wasm32")]
impl w::Guest for Component {
    fn read(name: String, bytes: Vec<u8>) -> Result<w::Sheet, w::ImportError> {
        use bindings::csv::codec::codec as csv;
        use bindings::zip::archive::archiver as zip;

        let sheet = match format_of(&name).map_err(err_out)? {
            Format::Csv | Format::Tsv => {
                let delimiter =
                    if matches!(format_of(&name), Ok(Format::Tsv)) { "\t" } else { "," };
                let text = String::from_utf8_lossy(&bytes).to_string();
                // `has-header: false` on purpose: the header is the FIRST ROW here,
                // and `shape` takes it. Asking csv:codec to consume it would leave
                // this component unable to report a duplicate column name.
                let dialect = csv::Dialect {
                    delimiter: delimiter.to_string(),
                    has_header: false,
                    trim: true,
                };
                let rows = csv::parse(&text, &dialect)
                    .map_err(|e| err_out(ImportError::Csv(format!("{e:?}"))))?;
                shape(rows.into_iter().map(|r| r.fields).collect(), "").map_err(err_out)?
            }
            Format::Xlsx => {
                let files = zip::extract(&bytes)
                    .map_err(|e| err_out(ImportError::Archive(format!("{e:?}"))))?;
                let parts: Vec<(String, Vec<u8>)> =
                    files.into_iter().map(|f| (f.name, f.data)).collect();
                xlsx_from_parts(&parts).map_err(err_out)?
            }
        };

        Ok(w::Sheet {
            header: sheet.header,
            rows: sheet.rows.into_iter().map(|r| w::Row { cells: r.cells }).collect(),
            sheet_name: sheet.sheet_name,
        })
    }
}

#[cfg(target_arch = "wasm32")]
bindings::export!(Component with_types_in bindings);
