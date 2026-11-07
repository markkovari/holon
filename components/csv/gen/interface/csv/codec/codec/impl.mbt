// Implementation of csv:codec@0.1.0 codec interface in MoonBit

fn delim(opts : Dialect) -> Char {
  if opts.delimiter.is_empty() {
    ','
  } else {
    opts.delimiter[0].to_int().unsafe_to_char()
  }
}

fn trim_ascii_whitespace(s : String) -> String {
  let chars : Array[Char] = []
  for c in s {
    chars.push(c)
  }
  let mut start = 0
  while start < chars.length() &&
        (
          chars[start] == ' ' ||
          chars[start] == '\t' ||
          chars[start] == '\r' ||
          chars[start] == '\n'
        ) {
    start = start + 1
  }
  let mut end = chars.length()
  while end > start &&
        (
          chars[end - 1] == ' ' ||
          chars[end - 1] == '\t' ||
          chars[end - 1] == '\r' ||
          chars[end - 1] == '\n'
        ) {
    end = end - 1
  }
  let buf = StringBuilder()
  for i = start; i < end; i = i + 1 {
    buf.write_char(chars[i])
  }
  buf.to_string()
}

fn push_field(
  row : Array[String],
  field : StringBuilder,
  quoted : Bool,
  trim : Bool
) -> StringBuilder {
  let mut val = field.to_string()
  if trim && !quoted {
    val = trim_ascii_whitespace(val)
  }
  row.push(val)
  StringBuilder()
}

fn parse_rows(
  text : String,
  opts : Dialect
) -> Result[Array[Array[String]], CsvError] {
  let delimiter = delim(opts)
  let trim = opts.trim

  let rows : Array[Array[String]] = []
  let mut row : Array[String] = []
  let mut field = StringBuilder()

  let mut in_quotes = false
  let mut quoted = false
  let mut pending = false

  let chars : Array[Char] = []
  for c in text {
    chars.push(c)
  }
  let mut i = 0

  while i < chars.length() {
    let c = chars[i]
    if in_quotes {
      if c == '"' {
        if i + 1 < chars.length() && chars[i + 1] == '"' {
          i = i + 1
          field.write_char('"')
        } else {
          in_quotes = false
        }
      } else {
        field.write_char(c)
      }
      i = i + 1
      continue
    }

    match c {
      '"' => {
        in_quotes = true
        quoted = true
        pending = true
      }
      '\r' => ()
      '\n' => {
        field = push_field(row, field, quoted, trim)
        quoted = false
        rows.push(row)
        row = []
        pending = false
      }
      ch =>
        if ch == delimiter {
          field = push_field(row, field, quoted, trim)
          quoted = false
          pending = true
        } else {
          field.write_char(ch)
          pending = true
        }
    }
    i = i + 1
  }

  if in_quotes {
    return Err(CsvError::Malformed("unterminated quoted field"))
  }

  if pending {
    let _ = push_field(row, field, quoted, trim)
    rows.push(row)
  }

  Ok(rows)
}

///|
pub fn parse(text : String, opts : Dialect) -> Result[Array[Row], CsvError] {
  let raw_rows = match parse_rows(text, opts) {
    Ok(r) => r
    Err(e) => return Err(e)
  }
  let out : Array[Row] = []
  for r in raw_rows {
    out.push(Row::{ fields: r })
  }
  Ok(out)
}

///|
pub fn parse_records(
  text : String,
  opts : Dialect
) -> Result[Array[RecordRow], CsvError] {
  let raw_rows = match parse_rows(text, opts) {
    Ok(r) => r
    Err(e) => return Err(e)
  }
  if raw_rows.is_empty() {
    return Ok([])
  }
  let header = raw_rows[0]
  let out : Array[RecordRow] = []
  for data_idx = 1; data_idx < raw_rows.length(); data_idx = data_idx + 1 {
    let data_row = raw_rows[data_idx]
    if data_row.length() != header.length() {
      return Err(CsvError::RaggedRow((data_idx - 1).reinterpret_as_uint()))
    }
    let pairs : Array[(String, String)] = []
    for j = 0; j < header.length(); j = j + 1 {
      pairs.push((header[j], data_row[j]))
    }
    out.push(RecordRow::{ pairs })
  }
  Ok(out)
}

fn format_field(value : String, delimiter : Char) -> String {
  let mut needs_quote = false
  for c in value {
    if c == delimiter || c == '"' || c == '\n' || c == '\r' {
      needs_quote = true
      break
    }
  }
  if needs_quote {
    let buf = StringBuilder()
    buf.write_char('"')
    for c in value {
      if c == '"' {
        buf.write_char('"')
      }
      buf.write_char(c)
    }
    buf.write_char('"')
    buf.to_string()
  } else {
    value
  }
}

///|
pub fn format(rows : Array[Row], opts : Dialect) -> String {
  let delimiter = delim(opts)
  let buf = StringBuilder()
  for i = 0; i < rows.length(); i = i + 1 {
    if i > 0 {
      buf.write_string("\r\n")
    }
    let row = rows[i]
    for j = 0; j < row.fields.length(); j = j + 1 {
      if j > 0 {
        buf.write_char(delimiter)
      }
      buf.write_string(format_field(row.fields[j], delimiter))
    }
  }
  buf.to_string()
}
