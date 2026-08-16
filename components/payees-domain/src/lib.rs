//! `payees-domain` — a payee book (docs/apps/PAYEES.md) as ONE composed wasm HTTP
//! component. Exports `wasi:http`; imports only WIT contracts: the composed
//! auth-guard (`auth:identity`), `records:store`, and `iban:validate` (the
//! country-length + mod-97 check). No bespoke auth, storage, or IBAN math.

#[allow(warnings)]
mod bindings;

use serde_json::{json, Map, Value};

use bindings::auth::identity::accounts;
use bindings::auth::identity::authorizer;
use bindings::auth::identity::session;
use bindings::auth::identity::types::{AuthError, Principal};
use bindings::iban::validate::validator as iban;
use bindings::records::store::store as records;
use bindings::wasi::clocks::wall_clock;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

const TENANT: &str = "payees";
const PAYEES: &str = "payees";

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let outcome = match (&method, seg.as_slice()) {
            (Method::Get, [""]) => usage(),
            (Method::Post, ["api", "register"]) => register(&request),
            (Method::Post, ["api", "login"]) => login(&request),
            (Method::Post, ["api", "logout"]) => logout(&request),
            (Method::Get, ["api", "me"]) => me(&request),

            (Method::Post, ["api", "verify"]) => verify(&request),
            (Method::Post, ["api", "payees"]) => create_payee(&request),
            (Method::Get, ["api", "payees"]) => list_payees(&request),
            (Method::Delete, ["api", "payees", id]) => delete_payee(&request, id),
            _ => Outcome::Err(404, "not_found".into()),
        };
        emit(response_out, outcome);
    }
}

enum Outcome {
    Json(u16, String),
    Err(u16, String),
    Auth(AuthError),
}

fn now() -> u64 {
    wall_clock::now().seconds
}

fn usage() -> Outcome {
    Outcome::Json(
        200,
        json!({
            "service": "payees",
            "about": "a payee book with IBAN-validated bank details (country length + mod-97 checksum, via iban:validate)",
            "auth": "POST /api/register|login|logout, GET /api/me",
            "verify": "POST /api/verify {iban} -> {valid, country, formatted, ...} | {valid:false, error}",
            "payees": "POST|GET /api/payees {name, iban}, DELETE /api/payees/{id}"
        })
        .to_string(),
    )
}

// ---- iban ------------------------------------------------------------------

fn iban_msg(e: &iban::IbanError) -> String {
    match e {
        iban::IbanError::TooShort => "too short".into(),
        iban::IbanError::BadCountry(c) => format!("country code must be two letters (got \"{c}\")"),
        iban::IbanError::BadChar(c) => format!("invalid character: \"{c}\""),
        iban::IbanError::BadLength((got, exp)) => format!("wrong length for that country: got {got}, expected {exp}"),
        iban::IbanError::BadCheck => "checksum failed — check for a typo".into(),
    }
}

fn info_json(i: &iban::IbanInfo) -> Value {
    json!({ "country": i.country, "check_digits": i.check_digits, "bban": i.bban, "formatted": i.formatted, "length": i.length })
}

fn verify(request: &IncomingRequest) -> Outcome {
    if let Err(o) = introspect(request) {
        return o;
    }
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let raw = b["iban"].as_str().unwrap_or("");
    match iban::validate(raw) {
        Ok(i) => {
            let mut v = info_json(&i);
            v["valid"] = json!(true);
            Outcome::Json(200, v.to_string())
        }
        Err(e) => Outcome::Json(200, json!({ "valid": false, "error": iban_msg(&e) }).to_string()),
    }
}

// ---- auth -------------------------------------------------------------------

fn bearer(request: &IncomingRequest) -> Option<String> {
    let headers = request.headers();
    let vals = headers.get(&"authorization".to_string());
    let raw = vals.first()?;
    let s = String::from_utf8(raw.clone()).ok()?;
    s.strip_prefix("Bearer ").map(|t| t.to_string())
}

fn introspect(request: &IncomingRequest) -> Result<Principal, Outcome> {
    let token = bearer(request).ok_or(Outcome::Auth(AuthError::InvalidToken("missing bearer".into())))?;
    authorizer::introspect(&token).map_err(Outcome::Auth)
}

fn register(request: &IncomingRequest) -> Outcome {
    let body = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let email = body["email"].as_str().unwrap_or("").trim().to_string();
    let password = body["password"].as_str().unwrap_or("").to_string();
    let p = match accounts::register(&email, &password, TENANT) {
        Ok(p) => p,
        Err(e) => return Outcome::Auth(e),
    };
    seed_demo(&p.subject);
    Outcome::Json(201, json!({ "subject": p.subject }).to_string())
}

fn login(request: &IncomingRequest) -> Outcome {
    let body = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let email = body["email"].as_str().unwrap_or("").trim().to_string();
    let password = body["password"].as_str().unwrap_or("").to_string();
    match accounts::login(&email, &password, TENANT) {
        Ok(tp) => Outcome::Json(
            200,
            json!({ "access_token": tp.access_token, "refresh_token": tp.refresh_token, "expires_in": tp.expires_in, "session_id": tp.session_id }).to_string(),
        ),
        Err(e) => Outcome::Auth(e),
    }
}

fn me(request: &IncomingRequest) -> Outcome {
    match introspect(request) {
        Ok(p) => Outcome::Json(200, json!({ "subject": p.subject, "roles": p.roles }).to_string()),
        Err(o) => o,
    }
}

fn logout(request: &IncomingRequest) -> Outcome {
    let token = match bearer(request) {
        Some(t) => t,
        None => return Outcome::Auth(AuthError::InvalidToken("missing bearer".into())),
    };
    match session::revoke(&token) {
        Ok(()) => Outcome::Json(200, json!({ "ok": true }).to_string()),
        Err(e) => Outcome::Auth(e),
    }
}

// ---- payees -----------------------------------------------------------------

fn create_payee(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let name = b["name"].as_str().unwrap_or("").trim().to_string();
    if name.is_empty() {
        return Outcome::Err(422, "name required".into());
    }
    // validate the IBAN before storing anything.
    let info = match iban::validate(b["iban"].as_str().unwrap_or("")) {
        Ok(i) => i,
        Err(e) => return Outcome::Err(422, format!("invalid IBAN: {}", iban_msg(&e))),
    };
    let d = json!({
        "name": name, "iban": format!("{}{}{}", info.country, info.check_digits, info.bban),
        "formatted": info.formatted, "country": info.country, "owner": p.subject, "created": now()
    });
    match records::create(PAYEES, &d.to_string(), &["owner".to_string()]) {
        Ok(rec) => {
            let mut v: Value = serde_json::from_str(&rec.data).unwrap_or(d);
            v["id"] = json!(rec.id);
            Outcome::Json(201, v.to_string())
        }
        Err(e) => store_err(e),
    }
}

fn list_payees(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let mut items: Vec<Value> = records::find_by(PAYEES, "owner", &json!(p.subject).to_string())
        .unwrap_or_default()
        .iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok().map(|mut v| {
            v["id"] = json!(e.id);
            v
        }))
        .collect();
    items.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    Outcome::Json(200, json!({ "items": items }).to_string())
}

fn delete_payee(request: &IncomingRequest, id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let owner = records::get(PAYEES, id)
        .ok()
        .and_then(|e| serde_json::from_str::<Value>(&e.data).ok())
        .and_then(|v| v["owner"].as_str().map(String::from));
    match owner {
        Some(o) if o == p.subject => {
            let _ = records::delete(PAYEES, id);
            Outcome::Json(200, json!({ "ok": true }).to_string())
        }
        Some(_) => Outcome::Err(403, "not your payee".into()),
        None => Outcome::Err(404, "not_found".into()),
    }
}

fn seed_demo(subject: &str) {
    let demo = [
        ("Acme Supplies GmbH", "DE89370400440532013000"),
        ("La Boulangerie SARL", "FR1420041010050500013M02606"),
        ("Northwind Ltd", "GB82WEST12345698765432"),
    ];
    for (name, raw) in demo {
        if let Ok(i) = iban::validate(raw) {
            let d = json!({
                "name": name, "iban": format!("{}{}{}", i.country, i.check_digits, i.bban),
                "formatted": i.formatted, "country": i.country, "owner": subject, "created": now()
            });
            let _ = records::create(PAYEES, &d.to_string(), &["owner".to_string()]);
        }
    }
}

// ---- http plumbing ----------------------------------------------------------

fn store_err(e: records::StoreError) -> Outcome {
    match e {
        records::StoreError::NotFound => Outcome::Err(404, "not_found".into()),
        records::StoreError::InvalidJson(m) => Outcome::Err(422, m),
        records::StoreError::RevisionConflict(_) => Outcome::Err(409, "conflict".into()),
        records::StoreError::BackendUnavailable(m) => Outcome::Err(503, m),
    }
}

fn body(request: &IncomingRequest) -> Result<Value, Outcome> {
    let raw = read_body(request).map_err(|_| Outcome::Err(400, "could not read body".into()))?;
    if raw.is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_slice(&raw).map_err(|e| Outcome::Err(400, format!("bad json: {e}")))
}

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
    let b = request.consume().map_err(|_| ())?;
    let stream = b.stream().map_err(|_| ())?;
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

fn emit(response_out: ResponseOutparam, result: Outcome) {
    let (code, body) = match result {
        Outcome::Json(c, b) => (c, b),
        Outcome::Err(c, m) => (c, json!({ "error": m }).to_string()),
        Outcome::Auth(e) => {
            let msg = match &e {
                AuthError::InvalidToken(m) => m.clone(),
                AuthError::InvalidCredentials => "invalid credentials".into(),
                other => format!("{other:?}"),
            };
            (401, json!({ "error": msg }).to_string())
        }
    };
    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[b"application/json".to_vec()]);
    let _ = headers.set(&"access-control-allow-origin".to_string(), &[b"*".to_vec()]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(code);
    let out = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    let bytes = body.as_bytes();
    if !bytes.is_empty() {
        let stream = out.write().expect("write stream");
        for chunk in bytes.chunks(4096) {
            let _ = stream.blocking_write_and_flush(chunk);
        }
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);
