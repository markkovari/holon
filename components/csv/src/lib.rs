//! `csv` — reference implementation of `csv:stream/codec`.
//!
//! A correct, dependency-free RFC 4180 CSV codec: parse delimited text into
//! rows (or header-keyed records) and format rows back out with proper quoting.
//! Pure compute — text in, structure out. No state, no host imports.
//!
//! Dialect: the delimiter is the first `char` of `opts.delimiter` (default `,`
//! when empty); the quote character is always `"`. Per RFC 4180, a field is
//! quoted on output when it contains the delimiter, a `"`, `\n`, or `\r`, and an
//! embedded `"` is doubled. `\r\n` is the canonical RFC 4180 record terminator.
//!
//! Parsing notes:
//!   * A quoted field begins with `"`; inside, `""` is a literal quote and
//!     embedded delimiters / newlines are literal until the closing quote.
//!   * Outside quotes, a lone `\r` is ignored (so `\r\n` collapses to a single
//!     row break).
//!   * An unterminated quoted field at EOF is `malformed`.
//!   * `opts.trim` trims ASCII whitespace from UNQUOTED fields only.
//!   * A single trailing newline does not yield an extra empty row, but
//!     genuinely empty fields and empty interior rows are preserved.

#[allow(warnings)]
mod bindings;

use bindings::exports::csv::codec::codec::{CsvError, Dialect, Guest, RecordRow, Row};

struct Component;

/// First char of the configured delimiter, defaulting to ',' when empty.
fn delim(opts: &Dialect) -> char {
    opts.delimiter.chars().next().unwrap_or(',')
}

/// RFC 4180 state-machine parse into rows of raw field strings.
///
/// Returns `Malformed("unterminated quoted field")` if a quote is opened but
/// never closed before EOF. A single trailing record terminator is not turned
/// into a spurious trailing empty row.
fn parse_rows(text: &str, opts: &Dialect) -> Result<Vec<Vec<String>>, CsvError> {
    let delimiter = delim(opts);
    let trim = opts.trim;

    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();

    // `in_quotes`: currently inside a quoted field.
    // `quoted`: this field was (at any point) a quoted field — disables trim.
    let mut in_quotes = false;
    let mut quoted = false;
    // Have we seen any content for the current field/row yet? Used so a trailing
    // terminator after a complete row does not emit an extra empty row.
    let mut pending = false;

    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    // Escaped doubled quote -> literal '"'.
                    chars.next();
                    field.push('"');
                } else {
                    // Closing quote.
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }

        match c {
            '"' => {
                // Opening quote of a quoted field.
                in_quotes = true;
                quoted = true;
                pending = true;
            }
            '\r' => {
                // Lone CR outside quotes is ignored; the following LF (if any)
                // ends the record.
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

    // Flush the final field/row unless the input ended exactly on a record
    // terminator (no pending content), which would otherwise add an empty row.
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

/// Quote a field per RFC 4180 if it contains the delimiter, a quote, CR or LF.
fn format_field(value: &str, delimiter: char) -> String {
    let needs_quote = value
        .chars()
        .any(|c| c == delimiter || c == '"' || c == '\n' || c == '\r');
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
        // Row 0 is always the header (per WIT doc), regardless of `has-header`.
        let header = &rows[0];
        let mut out = Vec::with_capacity(rows.len().saturating_sub(1));
        for (data_index, data_row) in rows[1..].iter().enumerate() {
            if data_row.len() != header.len() {
                // `data_index` is the 0-based index among DATA rows.
                return Err(CsvError::RaggedRow(data_index as u32));
            }
            let pairs = header
                .iter()
                .cloned()
                .zip(data_row.iter().cloned())
                .collect();
            out.push(RecordRow { pairs });
        }
        Ok(out)
    }

    fn format(rows: Vec<Row>, opts: Dialect) -> String {
        let delimiter = delim(&opts);
        let mut out = String::new();
        for (i, row) in rows.iter().enumerate() {
            if i > 0 {
                // RFC 4180 record terminator.
                out.push_str("\r\n");
            }
            for (j, field) in row.fields.iter().enumerate() {
                if j > 0 {
                    out.push(delimiter);
                }
                out.push_str(&format_field(field, delimiter));
            }
        }
        // No trailing terminator after the last row.
        out
    }
}

bindings::export!(Component with_types_in bindings);
