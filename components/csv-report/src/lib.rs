//! report:app — batch CSV import -> typed validate -> store -> paged report ->
//! CSV export, over composed contracts.
//!
//! The ingest round-trip: `POST /api/import` takes a raw CSV body, `csv::parse`
//! turns it into rows, and each data row is validated against a fixed field-rule
//! set through `validate::validate`. Rows with zero field-errors persist to
//! `records::store`; rejected rows come back in the response with their
//! per-field errors, so a bad upload is diagnosable, not just refused.
//!
//! `GET /api/rows` reads the clean set back through `paginate::cursor` — the
//! requested limit is clamped by the contract and the store's continuation is
//! wrapped in an opaque cursor, so the client loads more without ever seeing an
//! offset. `GET /api/export` re-serializes the clean rows with `csv::format` —
//! the same codec that parsed them, proving the round-trip.

#[allow(warnings)]
mod bindings;

use serde_json::{json, Value};

use bindings::csv::codec::codec as csv;
use bindings::paginate::cursor::cursors as paginate;
use bindings::records::store::store as records;
use bindings::validate::schema::validator as validate;
use bindings::wasi::clocks::wall_clock;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

const ROWS: &str = "rows";
/// The CSV columns, in order — also the export header + the validate targets.
const COLUMNS: &[&str] = &["name", "email", "age", "role"];

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let outcome = match (&method, seg.as_slice()) {
            (Method::Get, [""]) => usage_json(),
            (Method::Get, ["api", "schema"]) => schema_json(),
            (Method::Post, ["api", "import"]) => import(&request),
            (Method::Get, ["api", "rows"]) => rows(&path),
            (Method::Get, ["api", "export"]) => export(),
            (Method::Get, ["api", "stats"]) => stats(),
            _ => Outcome::err(404, "not_found"),
        };
        emit(response_out, outcome);
    }
}

enum Outcome {
    Json(u16, String),
    /// Raw text with a content-type (the CSV export path).
    Text(u16, String, String),
}
impl Outcome {
    fn err(code: u16, msg: &str) -> Outcome {
        Outcome::Json(code, json!({ "error": msg }).to_string())
    }
}

fn now() -> u64 {
    wall_clock::now().seconds
}

/// The field-rule set the importer validates every row against. Fixed for the
/// demo; a real deploy would load it per-collection from config.
fn rules() -> Vec<validate::Rule> {
    let text = |field: &str, required: bool, max: u32| validate::Rule {
        field: field.into(),
        kind: validate::Kind::Text,
        required,
        min_len: 0,
        max_len: max,
        min_value: None,
        max_value: None,
        one_of: vec![],
    };
    vec![
        validate::Rule { max_len: 80, ..text("name", true, 80) },
        validate::Rule { kind: validate::Kind::Email, ..text("email", true, 200) },
        validate::Rule {
            kind: validate::Kind::Integer,
            min_value: Some(0.0),
            max_value: Some(130.0),
            ..text("age", false, 0)
        },
        validate::Rule { one_of: vec!["admin".into(), "user".into(), "guest".into()], ..text("role", true, 0) },
    ]
}

fn kind_name(k: validate::Kind) -> &'static str {
    match k {
        validate::Kind::Text => "text",
        validate::Kind::Integer => "integer",
        validate::Kind::Number => "number",
        validate::Kind::Boolean => "boolean",
        validate::Kind::Email => "email",
        validate::Kind::Alphanumeric => "alphanumeric",
        validate::Kind::Uuid => "uuid",
    }
}

/// Coerce a raw CSV cell to the JSON type its field-rule declares. If the cell
/// can't be coerced it's left as a string so `validate` surfaces the type error
/// rather than us swallowing it here.
fn coerce(field: &str, raw: &str, rules: &[validate::Rule]) -> Value {
    let kind = rules.iter().find(|r| r.field == field).map(|r| r.kind);
    match kind {
        Some(validate::Kind::Integer) => raw.parse::<i64>().map(|n| json!(n)).unwrap_or_else(|_| json!(raw)),
        Some(validate::Kind::Number) => raw.parse::<f64>().map(|n| json!(n)).unwrap_or_else(|_| json!(raw)),
        Some(validate::Kind::Boolean) => match raw.to_lowercase().as_str() {
            "true" | "1" | "yes" => json!(true),
            "false" | "0" | "no" => json!(false),
            _ => json!(raw),
        },
        _ => json!(raw),
    }
}

fn usage_json() -> Outcome {
    Outcome::Json(
        200,
        json!({
            "service": "report",
            "about": "batch CSV import -> typed validate -> store -> paged report -> CSV export",
            "schema": "GET /api/schema",
            "import": "POST /api/import  (raw CSV body, header row required)",
            "rows": "GET /api/rows?limit=&after=  (opaque cursor)",
            "export": "GET /api/export  -> text/csv",
            "stats": "GET /api/stats"
        })
        .to_string(),
    )
}

fn schema_json() -> Outcome {
    let rs: Vec<Value> = rules()
        .iter()
        .map(|r| {
            json!({
                "field": r.field,
                "kind": kind_name(r.kind),
                "required": r.required,
                "min_value": r.min_value,
                "max_value": r.max_value,
                "one_of": r.one_of,
            })
        })
        .collect();
    Outcome::Json(200, json!({"columns": COLUMNS, "rules": rs}).to_string())
}

// ---- import: parse -> validate -> store --------------------------------------

fn import(request: &IncomingRequest) -> Outcome {
    let text = match read_body(request) {
        Ok(b) => String::from_utf8_lossy(&b).to_string(),
        Err(_) => return Outcome::err(400, "could not read body"),
    };
    if text.trim().is_empty() {
        return Outcome::err(422, "empty CSV");
    }
    let dialect = csv::Dialect { delimiter: ",".into(), has_header: true, trim: true };
    let parsed = match csv::parse_records(&text, &dialect) {
        Ok(rows) => rows,
        Err(csv::CsvError::Malformed(m)) => return Outcome::err(422, &format!("malformed CSV: {m}")),
        Err(csv::CsvError::RaggedRow(n)) => return Outcome::err(422, &format!("ragged row at line {n}")),
    };

    let rule_set = rules();
    let mut imported = 0u64;
    let mut rejected: Vec<Value> = Vec::new();

    for (i, rec) in parsed.iter().enumerate() {
        // build a JSON object from the record's name->value pairs, coercing each
        // cell to the JSON type its rule expects. CSV is all strings on the
        // wire; typed validation (integer ranges, booleans) only means anything
        // once the cell is the JSON type it claims to be. A cell that won't
        // coerce stays a string, so `validate` reports the type error itself.
        let mut obj = serde_json::Map::new();
        for (k, v) in &rec.pairs {
            obj.insert(k.clone(), coerce(k, v, &rule_set));
        }
        let doc = Value::Object(obj);
        let errs = validate::validate(&doc.to_string(), &rule_set);
        if errs.is_empty() {
            let mut stored = doc.clone();
            stored["at"] = json!(now());
            match records::create(ROWS, &stored.to_string(), &["email".to_string()]) {
                Ok(_) => imported += 1,
                Err(e) => rejected.push(json!({"line": i + 2, "row": doc, "errors": [{"field": "_store", "message": store_msg(e)}]})),
            }
        } else {
            let fe: Vec<Value> = errs
                .iter()
                .map(|e| json!({"field": e.field, "code": e.code, "message": e.message}))
                .collect();
            rejected.push(json!({"line": i + 2, "row": doc, "errors": fe}));
        }
    }

    Outcome::Json(
        200,
        json!({"imported": imported, "rejected": rejected.len(), "rejects": rejected}).to_string(),
    )
}

// ---- paged report ------------------------------------------------------------

fn rows(path: &str) -> Outcome {
    let requested: u32 = query_str(path, "limit").and_then(|s| s.parse().ok()).unwrap_or(20);
    let limit = match paginate::clamp_limit(requested) {
        Ok(l) => l,
        Err(_) => return Outcome::err(400, "bad limit"),
    };
    // the client cursor is our opaque wrapper around the store's continuation.
    let after = match query_str(path, "after") {
        Some(c) if !c.is_empty() => match paginate::decode(&c) {
            Ok(pos) => pos.last_id,
            Err(_) => return Outcome::err(400, "invalid cursor"),
        },
        _ => String::new(),
    };

    let page = match records::list_records(ROWS, limit, &after) {
        Ok(p) => p,
        Err(e) => return store_err(e),
    };
    let items: Vec<Value> = page
        .entries
        .iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
        .collect();

    // wrap the store's continuation in an opaque cursor for the next page.
    let next = if page.next.is_empty() {
        Value::Null
    } else {
        let pos = paginate::Position { sort_key: String::new(), last_id: page.next.clone(), forward: true };
        json!(paginate::encode(&pos))
    };
    Outcome::Json(200, json!({"rows": items, "next": next}).to_string())
}

// ---- CSV export --------------------------------------------------------------

fn export() -> Outcome {
    let mut out_rows: Vec<csv::Row> = vec![csv::Row { fields: COLUMNS.iter().map(|s| s.to_string()).collect() }];
    let mut after = String::new();
    loop {
        let page = match records::list_records(ROWS, 100, &after) {
            Ok(p) => p,
            Err(e) => return store_err(e),
        };
        for e in &page.entries {
            if let Ok(v) = serde_json::from_str::<Value>(&e.data) {
                let fields = COLUMNS
                    .iter()
                    .map(|c| match &v[*c] {
                        Value::String(s) => s.clone(),
                        Value::Null => String::new(),
                        other => other.to_string(),
                    })
                    .collect();
                out_rows.push(csv::Row { fields });
            }
        }
        if page.next.is_empty() {
            break;
        }
        after = page.next;
    }
    let dialect = csv::Dialect { delimiter: ",".into(), has_header: true, trim: false };
    let text = csv::format(&out_rows, &dialect);
    Outcome::Text(200, "text/csv".into(), text)
}

fn stats() -> Outcome {
    let count = records::count(ROWS).unwrap_or(0);
    Outcome::Json(200, json!({"rows": count}).to_string())
}

// ---- error mapping -----------------------------------------------------------

fn store_msg(e: records::StoreError) -> String {
    match e {
        records::StoreError::NotFound => "not_found".into(),
        records::StoreError::InvalidJson(m) => m,
        records::StoreError::RevisionConflict(_) => "conflict".into(),
        records::StoreError::BackendUnavailable(m) => m,
    }
}

fn store_err(e: records::StoreError) -> Outcome {
    match e {
        records::StoreError::NotFound => Outcome::err(404, "not_found"),
        records::StoreError::InvalidJson(m) => Outcome::Json(422, json!({"error": m}).to_string()),
        records::StoreError::RevisionConflict(_) => Outcome::err(409, "conflict"),
        records::StoreError::BackendUnavailable(m) => Outcome::Json(503, json!({"error": m}).to_string()),
    }
}

// ---- http plumbing -----------------------------------------------------------

/// The most a request body may be, before the component stops reading it.
///
/// There was no ceiling anywhere: 148 of 150 components accumulated whatever
/// arrived until the guest hit wasmtime's 64 MiB per-store memory cap and TRAPPED,
/// which reaches the caller as a closed connection saying nothing about a size.
/// A component that answers JSON has no business reading sixteen megabytes, and
/// the ones that legitimately handle uploads police it themselves with a 413 and a
/// granted max-size — those are left alone.
///
/// Generous on purpose. This is a backstop against an unbounded read, not a
/// content policy; an API that needs a real limit should state its own and say 413.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

fn read_body(request: &IncomingRequest) -> Result<Vec<u8>, ()> {
    let body = request.consume().map_err(|_| ())?;
    let stream = body.stream().map_err(|_| ())?;
    let mut buf = Vec::new();
    loop {
        match stream.blocking_read(8192) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => {
                // A ceiling, not a policy: past this the read stops and the caller
                // is told, rather than growing until the store's memory cap traps
                // the component and the connection just closes.
                if buf.len() + chunk.len() > MAX_BODY_BYTES {
                    return Err(());
                }
                buf.extend_from_slice(&chunk);
            }
            // `Closed` is how wasi:io says end-of-body; `LastOperationFailed` is a
            // read that went wrong. Collapsing both into `break` returns a TRUNCATED
            // body as if it were complete — the same silent truncation that, on the
            // write side, took four runs to find.
            Err(bindings::wasi::io::streams::StreamError::Closed) => break,
            Err(_) => return Err(()),
        }
    }
    Ok(buf)
}

fn query_str(path: &str, key: &str) -> Option<String> {
    let query = path.split('?').nth(1)?;
    query.split('&').find_map(|pair| {
        let mut it = pair.splitn(2, '=');
        (it.next()? == key).then(|| decode(it.next().unwrap_or("")))
    })
}

fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(b as char);
                    i += 3;
                    continue;
                }
                out.push('%');
                i += 1;
            }
            b'+' => {
                out.push(' ');
                i += 1;
            }
            c => {
                out.push(c as char);
                i += 1;
            }
        }
    }
    out
}

fn emit(response_out: ResponseOutparam, result: Outcome) {
    match result {
        Outcome::Json(code, body) => respond(response_out, code, "application/json", body.as_bytes()),
        Outcome::Text(code, ct, body) => respond(response_out, code, &ct, body.as_bytes()),
    }
}

fn respond(response_out: ResponseOutparam, status: u16, content_type: &str, body: &[u8]) {
    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[content_type.as_bytes().to_vec()]);
    let _ = headers.set(&"access-control-allow-origin".to_string(), &[b"*".to_vec()]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(status);
    let out = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    if !body.is_empty() {
        let stream = out.write().expect("write stream");
        for chunk in body.chunks(4096) {
            let _ = stream.blocking_write_and_flush(chunk);
        }
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);
