//! `sheet-ingest` — a `.csv` or `.xlsx` someone exported, turned into rows
//! never document order (or price lands in quantity with nothing failing).

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub cells: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sheet {
    pub header: Vec<String>,
    pub rows: Vec<Row>,
    pub sheet_name: String,
}

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

/// `A` -> 0, `Z` -> 25, `AA` -> 26.
pub fn column_of(reference: &str) -> Option<usize> {
    let letters: String = reference
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_uppercase();
    if letters.is_empty() {
        return None;
    }
    let mut n = 0usize;
    for c in letters.chars() {
        n = n * 26 + (c as usize - 'A' as usize + 1);
    }
    Some(n - 1)
}

fn tag_text(xml: &str, tag: &str, from: usize) -> Option<(String, usize)> {
    let open = xml[from..].find(&format!("<{tag}"))? + from;
    let gt = xml[open..].find('>')? + open;
    // `<t/>`: how a blank shared string is stored.
    if xml.as_bytes().get(gt - 1) == Some(&b'/') {
        return Some((String::new(), gt + 1));
    }
    let close = xml[gt..].find(&format!("</{tag}>"))? + gt;
    Some((unescape(&xml[gt + 1..close]), close + tag.len() + 3))
}

/// OOXML writes only these five entities, so this is not a general parser.
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

/// Rich-text `<r>` runs inside one `<si>` concatenate, which is what the spreadsheet shows.
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

pub fn worksheet_rows(xml: &str, shared: &[String]) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut at = 0usize;
    while let Some(open) = xml[at..].find("<row").map(|i| i + at) {
        let head_end = match xml[open..].find('>') {
            Some(i) => i + open,
            None => break,
        };
        // Some writers emit an empty `<row r="3"/>` for styling.
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
                Some("inlineStr") => tag_text(cbody, "t", 0).map(|(v, _)| v).unwrap_or_default(),
                // Numbers, serial dates and booleans come back as the literal `<v>`; formatting is rendering.
                _ => tag_text(cbody, "v", 0).map(|(v, _)| v).unwrap_or_default(),
            };

            // Positioned by LETTER: the gap between `A` and `C` is a real empty column.
            if cells.len() <= column {
                cells.resize(column + 1, String::new());
            }
            cells[column] = value;
        }
        rows.push(cells);
    }
    rows
}

pub fn shape(mut rows: Vec<Vec<String>>, sheet_name: &str) -> Result<Sheet, ImportError> {
    // Left behind when someone deletes content without deleting the row.
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
    // A used range often runs wider than the data; unnamed columns are noise.
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
        // A row longer than the header, once its trailing blanks are gone, is a cell nobody asked to drop.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Csv,
    Tsv,
    Xlsx,
}

/// By extension, not sniffing: guessing is confidently wrong on the first semicolon-delimited
/// European export.
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

/// `sheet1.xml`, not the workbook's first sheet: that needs two more files parsed, and writers
/// number the first sheet 1 (a limit named in the WIT).
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
    let short = name.rsplit('/').next().unwrap_or(&name).trim_end_matches(".xml").to_string();
    shape(worksheet_rows(&sheet_xml, &shared), &short)
}

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
                // `has-header: false`: `shape` takes the first row itself, so it can report a duplicate
                // column name.
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
