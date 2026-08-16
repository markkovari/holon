//! `transit-domain` — a public-transport ticketing service (docs/apps/TRANSIT.md) as ONE
//! composed wasm HTTP component. Exports `wasi:http`; imports only WIT contracts:
//! the composed auth-guard (`auth:identity`), `records:store`, `qr:encode` (the
//! scannable ticket), `lock:mutex` (single-use under concurrency) and the wall
//! clock. No bespoke auth, storage, QR encoder, or locking.

#[allow(warnings)]
mod bindings;

use serde_json::{json, Map, Value};

use bindings::auth::identity::accounts;
use bindings::auth::identity::authorizer;
use bindings::auth::identity::rbac;
use bindings::auth::identity::session;
use bindings::auth::identity::types::{AuthError, Principal};
use bindings::qr::encode::encoder as qr;
use bindings::records::store::store as records;
use bindings::wasi::clocks::wall_clock;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

const TENANT: &str = "transit";
const FARES: &str = "fares";
const TICKETS: &str = "tickets";

/// The seeded fare catalog: (key, name, kind, minutes, price-cents).
/// kind: "single" = one validation; "duration"/"pass" = unlimited within window.
const CATALOG: &[(&str, &str, &str, i64, i64)] = &[
    ("single", "Single ride", "single", 0, 250),
    ("t60", "60-minute ticket", "duration", 60, 350),
    ("t90", "90-minute ticket", "duration", 90, 450),
    ("month", "Monthly pass", "pass", 30 * 24 * 60, 5500),
];

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

            (Method::Get, ["api", "fares"]) => list_fares(&request),
            (Method::Post, ["api", "tickets"]) => buy_ticket(&request),
            (Method::Get, ["api", "tickets"]) => my_tickets(&request),
            (Method::Get, ["api", "tickets", id]) => ticket_detail(&request, id),
            (Method::Get, ["api", "tickets", id, "qr.svg"]) => ticket_qr(&request, id),
            (Method::Post, ["api", "validate"]) => validate(&request),
            _ => Outcome::Err(404, "not_found".into()),
        };
        emit(response_out, outcome);
    }
}

enum Outcome {
    Json(u16, String),
    Err(u16, String),
    Auth(AuthError),
    // status, content-type, optional download filename, body.
    File(u16, String, Option<String>, Vec<u8>),
}

fn now() -> u64 {
    wall_clock::now().seconds
}

fn usage() -> Outcome {
    Outcome::Json(
        200,
        json!({
            "service": "transit",
            "about": "public-transport ticketing — single / duration / monthly fares as QR tickets, camera validation with a single-use lock",
            "auth": "POST /api/register|login|logout (role: rider|validator), GET /api/me",
            "rider": "GET /api/fares, POST /api/tickets {fare}, GET /api/tickets, GET /api/tickets/{id}/qr.svg",
            "validator": "POST /api/validate {code}"
        })
        .to_string(),
    )
}

// ---- auth (auth-guard: auth:identity) ---------------------------------------

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

fn is_validator(p: &Principal) -> bool {
    p.roles.iter().any(|r| r == "validator")
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
    // demo self-assign of the global role (an operator would grant it in prod).
    let wanted = body["role"].as_str().unwrap_or("rider");
    let role = if ["rider", "validator"].contains(&wanted) { wanted } else { "rider" };
    let _ = rbac::assign_role(&p.tenant, &p.subject, role);
    Outcome::Json(201, json!({ "subject": p.subject, "roles": [role] }).to_string())
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
        Ok(p) => Outcome::Json(
            200,
            json!({ "subject": p.subject, "roles": p.roles, "is_validator": is_validator(&p) }).to_string(),
        ),
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

// ---- fares ------------------------------------------------------------------

/// Seed the fare catalog on first read (idempotent).
fn ensure_fares() {
    let empty = records::list_records(FARES, 1, "").map(|p| p.entries.is_empty()).unwrap_or(true);
    if !empty {
        return;
    }
    for (key, name, kind, minutes, price) in CATALOG {
        let d = json!({ "key": key, "name": name, "kind": kind, "minutes": minutes, "price": price });
        let _ = records::create(FARES, &d.to_string(), &["key".to_string()]);
    }
}

fn fare(key: &str) -> Option<Value> {
    records::find_by(FARES, "key", &json!(key).to_string())
        .ok()
        .and_then(|v| v.into_iter().next())
        .and_then(|e| serde_json::from_str::<Value>(&e.data).ok())
}

fn list_fares(request: &IncomingRequest) -> Outcome {
    if let Err(o) = introspect(request) {
        return o;
    }
    ensure_fares();
    let mut items: Vec<Value> = records::list_records(FARES, 100, "")
        .map(|p| p.entries)
        .unwrap_or_default()
        .iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
        .collect();
    items.sort_by_key(|f| f["price"].as_i64().unwrap_or(0));
    Outcome::Json(200, json!({ "items": items }).to_string())
}

// ---- tickets ----------------------------------------------------------------

fn buy_ticket(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    ensure_fares();
    let key = b["fare"].as_str().unwrap_or("");
    let f = match fare(key) {
        Some(f) => f,
        None => return Outcome::Err(422, "unknown fare".into()),
    };
    let d = json!({
        "id": Value::Null, "rider": p.subject,
        "fare": key, "fare_name": f["name"], "kind": f["kind"], "minutes": f["minutes"], "price": f["price"],
        "purchased": now(), "activated": 0, "uses": 0
    });
    match records::create(TICKETS, &d.to_string(), &["rider".to_string()]) {
        Ok(rec) => Outcome::Json(201, hydrate(&rec.id, &rec.data)),
        Err(e) => store_err(e),
    }
}

fn get_ticket(id: &str) -> Option<Value> {
    records::get(TICKETS, id).ok().and_then(|e| serde_json::from_str::<Value>(&e.data).ok())
}

/// Compute (status, valid_until, remaining_min) from a ticket's stored fields.
/// status: "valid" (bought, unused) | "active" (duration/pass, in window) |
/// "used" (single, consumed) | "expired" (window lapsed).
fn status_of(t: &Value) -> (String, Option<u64>, Option<i64>) {
    let kind = t["kind"].as_str().unwrap_or("single");
    let activated = t["activated"].as_u64().unwrap_or(0);
    let minutes = t["minutes"].as_i64().unwrap_or(0);
    if activated == 0 {
        return ("valid".into(), None, None);
    }
    if kind == "single" {
        return ("used".into(), None, None);
    }
    let until = activated + (minutes as u64) * 60;
    let now = now();
    if now <= until {
        ("active".into(), Some(until), Some(((until - now) / 60) as i64))
    } else {
        ("expired".into(), Some(until), Some(0))
    }
}

/// Attach id + computed status fields to a ticket's stored JSON.
fn hydrate(id: &str, data: &str) -> String {
    let mut v: Value = serde_json::from_str(data).unwrap_or_else(|_| json!({}));
    v["id"] = json!(id);
    let (status, until, remaining) = status_of(&v);
    v["status"] = json!(status);
    v["valid_until"] = json!(until);
    v["remaining_min"] = json!(remaining);
    v.to_string()
}

fn my_tickets(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let mut items: Vec<Value> = records::find_by(TICKETS, "rider", &json!(p.subject).to_string())
        .unwrap_or_default()
        .iter()
        .filter_map(|e| serde_json::from_str::<Value>(&hydrate(&e.id, &e.data)).ok())
        .collect();
    // newest first.
    items.sort_by(|a, b| b["purchased"].as_u64().cmp(&a["purchased"].as_u64()));
    Outcome::Json(200, json!({ "items": items }).to_string())
}

fn ticket_detail(request: &IncomingRequest, id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let t = match get_ticket(id) {
        Some(t) => t,
        None => return Outcome::Err(404, "not_found".into()),
    };
    if !is_validator(&p) && t["rider"].as_str() != Some(&p.subject) {
        return Outcome::Err(403, "not your ticket".into());
    }
    Outcome::Json(200, hydrate(id, &t.to_string()))
}

fn ticket_qr(request: &IncomingRequest, id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let t = match get_ticket(id) {
        Some(t) => t,
        None => return Outcome::Err(404, "not_found".into()),
    };
    if !is_validator(&p) && t["rider"].as_str() != Some(&p.subject) {
        return Outcome::Err(403, "not your ticket".into());
    }
    // the QR payload is the ticket id itself — unguessable and record-backed.
    match qr::svg(id, qr::Ecc::Medium, 2) {
        Ok(svg) => Outcome::File(200, "image/svg+xml".into(), None, svg.into_bytes()),
        Err(_) => Outcome::Err(500, "qr encode failed".into()),
    }
}

// ---- validation (the single-use, concurrency-safe critical section) ---------

fn validate(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    if !is_validator(&p) {
        return Outcome::Err(403, "validators only".into());
    }
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let code = b["code"].as_str().unwrap_or("").trim().to_string();

    // Single-use under concurrency via record-revision CAS: read the ticket + its
    // revision, decide, then write back guarded by that revision. A losing writer
    // gets a revision-conflict, re-reads the now-activated ticket, and the second
    // decision sees "already used" — so exactly one concurrent scan accepts.
    for _ in 0..32 {
        let entry = match records::get(TICKETS, &code) {
            Ok(e) => e,
            // a fabricated code matches no record -> reject (still 200 for the scanner).
            Err(_) => return reject("unknown ticket", &Value::Null),
        };
        let t: Value = serde_json::from_str(&entry.data).unwrap_or_else(|_| json!({}));
        match decide(&t, &p.subject) {
            // no state change (already used / expired) -> answer immediately.
            Step::Done(outcome) => return outcome,
            // activation / use recorded -> commit guarded by the read revision.
            Step::Commit(next, outcome) => match records::update(TICKETS, &code, &next.to_string(), entry.revision) {
                Ok(_) => return outcome,
                Err(records::StoreError::RevisionConflict(_)) => continue, // lost the race; re-read
                Err(e) => return store_err(e),
            },
        }
    }
    Outcome::Err(503, "validation contended, please retry".into())
}

/// The result of applying the fare rules: either a final answer (no write) or a
/// new ticket state to commit plus the response to return once it commits.
enum Step {
    Done(Outcome),
    Commit(Value, Outcome),
}

/// The fare rules — pure w.r.t. the store: reads the ticket, returns the next
/// state (for accepts) + the ACCEPT/REJECT response. The CAS loop does the write.
fn decide(t: &Value, validator: &str) -> Step {
    let kind = t["kind"].as_str().unwrap_or("single").to_string();
    let minutes = t["minutes"].as_i64().unwrap_or(0);
    let activated = t["activated"].as_u64().unwrap_or(0);
    let now = now();

    let with_use = |t: &Value, activate: u64| -> Value {
        let mut n = t.clone();
        if activate > 0 {
            n["activated"] = json!(activate);
        }
        n["uses"] = json!(n["uses"].as_u64().unwrap_or(0) + 1);
        let ev = json!({ "at": now, "by": validator });
        match n["history"].as_array_mut() {
            Some(arr) => arr.push(ev),
            None => n["history"] = json!([ev]),
        }
        n
    };

    if kind == "single" {
        if activated == 0 {
            Step::Commit(with_use(t, now), accept("single ride — enjoy your trip", &kind, None, None))
        } else {
            Step::Done(reject("already used", t))
        }
    } else {
        // duration / pass: activate on first scan, then valid until the window ends.
        if activated == 0 {
            let until = now + (minutes as u64) * 60;
            Step::Commit(with_use(t, now), accept("ticket activated", &kind, Some(until), Some(minutes)))
        } else {
            let until = activated + (minutes as u64) * 60;
            if now <= until {
                Step::Commit(with_use(t, 0), accept("valid", &kind, Some(until), Some(((until - now) / 60) as i64)))
            } else {
                Step::Done(reject("expired", t))
            }
        }
    }
}

fn accept(reason: &str, kind: &str, until: Option<u64>, remaining: Option<i64>) -> Outcome {
    Outcome::Json(
        200,
        json!({ "result": "accept", "reason": reason, "kind": kind, "valid_until": until, "remaining_min": remaining, "at": now() }).to_string(),
    )
}

fn reject(reason: &str, t: &Value) -> Outcome {
    Outcome::Json(
        200,
        json!({ "result": "reject", "reason": reason, "kind": t["kind"], "fare_name": t["fare_name"], "at": now() }).to_string(),
    )
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

fn read_body(request: &IncomingRequest) -> Result<Vec<u8>, ()> {
    let b = request.consume().map_err(|_| ())?;
    let stream = b.stream().map_err(|_| ())?;
    let mut buf = Vec::new();
    loop {
        match stream.blocking_read(8192) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => buf.extend_from_slice(&chunk),
            Err(_) => break,
        }
    }
    Ok(buf)
}

fn emit(response_out: ResponseOutparam, result: Outcome) {
    if let Outcome::File(code, ctype, name, bytes) = result {
        let disp = name.map(|n| format!("attachment; filename=\"{}\"", n));
        return respond(response_out, code, &ctype, disp.as_deref(), &bytes);
    }
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
        Outcome::File(..) => unreachable!(),
    };
    respond(response_out, code, "application/json", None, body.as_bytes());
}

fn respond(response_out: ResponseOutparam, status: u16, ctype: &str, disposition: Option<&str>, body: &[u8]) {
    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[ctype.as_bytes().to_vec()]);
    if let Some(d) = disposition {
        let _ = headers.set(&"content-disposition".to_string(), &[d.as_bytes().to_vec()]);
    }
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
