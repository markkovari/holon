//! RFC 4180: `trim` touches unquoted fields only; one trailing newline adds no empty row.

#[allow(warnings)]
mod bindings;

use bindings::exports::csv::codec::codec::{CsvError, Dialect, Guest, RecordRow, Row};

struct Component;

fn delim(opts: &Dialect) -> char {
    opts.delimiter.chars().next().unwrap_or(',')
}

fn parse_rows(text: &str, opts: &Dialect) -> Result<Vec<Vec<String>>, CsvError> {
    let delimiter = delim(opts);
    let trim = opts.trim;

    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();

    // `quoted`: this field was ever quoted, which disables trim.
    let mut in_quotes = false;
    let mut quoted = false;
    // So a trailing terminator after a complete row adds no empty row.
    let mut pending = false;

    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }

        match c {
            '"' => {
                in_quotes = true;
                quoted = true;
                pending = true;
            }
            '\r' => {
                // Ignored outside quotes; the following LF ends the record.
            }
            '\n' => {
                push_field(&mut row, &mut field, quoted, trim);
                quoted = false;
                rows.push(std::mem::take(&mut row));
                pending = false;
            }
            ch if ch == delimiter => {
                push_field(&mut row, &mut field, quoted, trim);
                quoted = false;
                pending = true;
            }
            ch => {
                field.push(ch);
                pending = true;
            }
        }
    }

    if in_quotes {
        return Err(CsvError::Malformed("unterminated quoted field".to_string()));
    }

    if pending {
        push_field(&mut row, &mut field, quoted, trim);
        rows.push(row);
    }

    Ok(rows)
}

fn push_field(row: &mut Vec<String>, field: &mut String, quoted: bool, trim: bool) {
    let mut value = std::mem::take(field);
    if trim && !quoted {
        value = value.trim_matches(|c: char| c.is_ascii_whitespace()).to_string();
    }
    row.push(value);
}

fn format_field(value: &str, delimiter: char) -> String {
    let needs_quote = value.chars().any(|c| c == delimiter || c == '"' || c == '\n' || c == '\r');
    if needs_quote {
        let mut out = String::with_capacity(value.len() + 2);
        out.push('"');
        for c in value.chars() {
            if c == '"' {
                out.push('"');
            }
            out.push(c);
        }
        out.push('"');
        out
    } else {
        value.to_string()
    }
}

impl Guest for Component {
    fn parse(text: String, opts: Dialect) -> Result<Vec<Row>, CsvError> {
        let rows = parse_rows(&text, &opts)?;
        Ok(rows.into_iter().map(|fields| Row { fields }).collect())
    }

    fn parse_records(text: String, opts: Dialect) -> Result<Vec<RecordRow>, CsvError> {
        let rows = parse_rows(&text, &opts)?;
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        // Row 0 is always the header (per the WIT), regardless of `has-header`.
        let header = &rows[0];
        let mut out = Vec::with_capacity(rows.len().saturating_sub(1));
        for (data_index, data_row) in rows[1..].iter().enumerate() {
            if data_row.len() != header.len() {
                return Err(CsvError::RaggedRow(data_index as u32));
            }
            let pairs = header.iter().cloned().zip(data_row.iter().cloned()).collect();
            out.push(RecordRow { pairs });
        }
        Ok(out)
    }

    fn format(rows: Vec<Row>, opts: Dialect) -> String {
        let delimiter = delim(&opts);
        let mut out = String::new();
        for (i, row) in rows.iter().enumerate() {
            if i > 0 {
                out.push_str("\r\n");
            }
            for (j, field) in row.fields.iter().enumerate() {
                if j > 0 {
                    out.push(delimiter);
                }
                out.push_str(&format_field(field, delimiter));
            }
        }
        out
    }
}

bindings::export!(Component with_types_in bindings);
