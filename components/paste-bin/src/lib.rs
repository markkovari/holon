//! bin:app — a paste / gist bin over a chain of mostly pure-compute contracts.
//!
//! The pipeline is a fold over stateless transforms, with exactly one stateful
//! step:
//!
//!   validate::validate   (body required, length-bounded)   — pure
//!   pii::mask            (emails / cards / ssn masked)      — pure, at INGEST
//!   records::create      (store the already-redacted body)  — the ONE stateful step
//!   slug::slugify/uniquify (URL-safe slug from the title)   — pure
//!
//! and on read:
//!
//!   md::to_html          (sanitized Markdown -> HTML)        — pure
//!   md::to_text          (plain-text preview)                — pure
//!
//! The headline is that redaction happens BEFORE storage — the raw PII never
//! lands in the record store — and that four of the five contracts are pure
//! functions with no host state of their own.

#[allow(warnings)]
mod bindings;

use serde_json::{json, Value};

use bindings::md::render::renderer as md;
use bindings::pii::redact::redactor as pii;
use bindings::records::store::store as records;
use bindings::slug::generate::generator as slug;
use bindings::validate::schema::validator as validate;
use bindings::wasi::clocks::wall_clock;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

const PASTES: &str = "pastes";
const MAX_BODY: u32 = 100_000;

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let outcome = match (&method, seg.as_slice()) {
            (Method::Get, [""]) => usage_json(),
            (Method::Post, ["api", "paste"]) => create_paste(&request),
            (Method::Get, ["api", "paste", id]) => get_paste(id),
            (Method::Get, ["api", "pastes"]) => list_pastes(),
            (Method::Get, ["api", "raw", id]) => raw_paste(id),
            _ => Outcome::err(404, "not_found"),
        };
        emit(response_out, outcome);
    }
}

enum Outcome {
    Json(u16, String),
    Text(u16, String),
}
impl Outcome {
    fn err(code: u16, msg: &str) -> Outcome {
        Outcome::Json(code, json!({ "error": msg }).to_string())
    }
}

fn now() -> u64 {
    wall_clock::now().seconds
}

/// The PII kinds masked at ingest.
fn pii_opts() -> pii::Options {
    pii::Options {
        kinds: vec![pii::Kind::Email, pii::Kind::CreditCard, pii::Kind::Ssn, pii::Kind::Phone],
    }
}

fn usage_json() -> Outcome {
    Outcome::Json(
        200,
        json!({
            "service": "bin",
            "about": "a paste/gist bin — validate -> redact PII -> store -> slug; a pure-compute pipeline with one stateful step",
            "create": "POST /api/paste {title?, body, syntax?}",
            "get": "GET /api/paste/{id}  (metadata + rendered HTML + preview)",
            "list": "GET /api/pastes",
            "raw": "GET /api/raw/{id}  (text/plain, redacted body)"
        })
        .to_string(),
    )
}

// ---- create: validate -> redact -> store -> slug -----------------------------

fn create_paste(request: &IncomingRequest) -> Outcome {
    let body = match parse_body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let title = body["title"].as_str().unwrap_or("").trim().to_string();
    let raw = body["body"].as_str().unwrap_or("").to_string();
    let syntax = body["syntax"].as_str().unwrap_or("markdown").trim().to_string();

    // 1. validate — pure. body required, length-bounded.
    let rules = vec![validate::Rule {
        field: "body".into(),
        kind: validate::Kind::Text,
        required: true,
        min_len: 1,
        max_len: MAX_BODY,
        min_value: None,
        max_value: None,
        one_of: vec![],
    }];
    let errs = validate::validate(&json!({"body": raw}).to_string(), &rules);
    if !errs.is_empty() {
        let fe: Vec<Value> = errs
            .iter()
            .map(|e| json!({"field": e.field, "code": e.code, "message": e.message}))
            .collect();
        return Outcome::Json(422, json!({"error": "validation_failed", "fields": fe}).to_string());
    }

    // 2. redact — pure, at INGEST. Count findings first (for the response), then
    // mask, so the raw PII never reaches the store.
    let opts = pii_opts();
    let findings = pii::detect(&raw, &opts).len();
    let redacted = pii::mask(&raw, &opts);

    // 3. store — the ONE stateful step. Slug is derived after we know the id.
    let title = if title.is_empty() { "untitled".to_string() } else { title };
    let doc = json!({"title": title, "body": redacted, "syntax": syntax, "redacted": findings, "at": now()});
    let entry = match records::create(PASTES, &doc.to_string(), &["slug".to_string()]) {
        Ok(e) => e,
        Err(e) => return store_err(e),
    };

    // 4. slug — pure. URL-safe + de-duplicated against existing slugs.
    let taken = existing_slugs();
    let base = slug::slugify(&title);
    let unique = slug::uniquify(&base, &taken);
    let mut stored: Value = serde_json::from_str(&entry.data).unwrap_or(doc);
    stored["id"] = json!(entry.id);
    stored["slug"] = json!(unique);
    let _ = records::update(PASTES, &entry.id, &stored.to_string(), entry.revision);

    Outcome::Json(201, json!({"id": entry.id, "slug": unique, "redacted": findings}).to_string())
}

fn existing_slugs() -> Vec<String> {
    records::list_records(PASTES, 1000, "")
        .map(|p| {
            p.entries
                .iter()
                .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
                .filter_map(|v| v["slug"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

// ---- read: render ------------------------------------------------------------

fn get_paste(id: &str) -> Outcome {
    let entry = match records::get(PASTES, id) {
        Ok(e) => e,
        Err(records::StoreError::NotFound) => return Outcome::err(404, "not_found"),
        Err(e) => return store_err(e),
    };
    let doc: Value = serde_json::from_str(&entry.data).unwrap_or_else(|_| json!({}));
    let stored_body = doc["body"].as_str().unwrap_or("");
    // render markdown -> sanitized HTML + a plain-text preview — pure compute.
    let html = md::to_html(stored_body);
    let preview: String = md::to_text(stored_body).chars().take(160).collect();
    Outcome::Json(
        200,
        json!({
            "id": id,
            "slug": doc["slug"],
            "title": doc["title"],
            "syntax": doc["syntax"],
            "redacted": doc["redacted"],
            "at": doc["at"],
            "html": html,
            "preview": preview,
        })
        .to_string(),
    )
}

fn raw_paste(id: &str) -> Outcome {
    match records::get(PASTES, id) {
        Ok(entry) => {
            let doc: Value = serde_json::from_str(&entry.data).unwrap_or_else(|_| json!({}));
            Outcome::Text(200, doc["body"].as_str().unwrap_or("").to_string())
        }
        Err(records::StoreError::NotFound) => Outcome::err(404, "not_found"),
        Err(e) => store_err(e),
    }
}

fn list_pastes() -> Outcome {
    match records::list_records(PASTES, 50, "") {
        Ok(page) => {
            let rows: Vec<Value> = page
                .entries
                .iter()
                .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
                .map(|v| json!({"id": v["id"], "slug": v["slug"], "title": v["title"], "syntax": v["syntax"], "redacted": v["redacted"], "at": v["at"]}))
                .collect();
            Outcome::Json(200, json!({"pastes": rows}).to_string())
        }
        Err(e) => store_err(e),
    }
}

// ---- error mapping -----------------------------------------------------------

fn store_err(e: records::StoreError) -> Outcome {
    match e {
        records::StoreError::NotFound => Outcome::err(404, "not_found"),
        records::StoreError::InvalidJson(m) => Outcome::Json(422, json!({"error": m}).to_string()),
        records::StoreError::RevisionConflict(_) => Outcome::err(409, "conflict"),
        records::StoreError::BackendUnavailable(m) => {
            Outcome::Json(503, json!({"error": m}).to_string())
        }
    }
}

// ---- http plumbing -----------------------------------------------------------

fn parse_body(request: &IncomingRequest) -> Result<Value, Outcome> {
    let body = read_body(request).map_err(|_| Outcome::err(400, "could not read body"))?;
    if body.is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    serde_json::from_slice(&body)
        .map_err(|e| Outcome::Json(400, json!({"error": format!("bad json: {e}")}).to_string()))
}

/// A ceiling on a request body, not a policy: past this the read stops and the
/// caller is told, rather than growing until the store's memory cap traps the
/// component and the connection simply closes.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_read_body!(MAX_BODY_BYTES);

fn emit(response_out: ResponseOutparam, result: Outcome) {
    match result {
        Outcome::Json(code, body) => {
            respond(response_out, code, "application/json", body.as_bytes())
        }
        Outcome::Text(code, body) => {
            respond(response_out, code, "text/plain; charset=utf-8", body.as_bytes())
        }
    }
}

fn respond(response_out: ResponseOutparam, status: u16, content_type: &str, body: &[u8]) {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[content_type.as_bytes().to_vec()]);
    let _ = headers.set("access-control-allow-origin", &[b"*".to_vec()]);
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
