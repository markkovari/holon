//! `stash-domain` — a personal note stash (docs/apps/STASH.md) as ONE composed wasm HTTP
//! component. Exports `wasi:http`; imports only WIT contracts: the composed
//! auth-guard (`auth:identity`), `records:store`, `zip:archive` (the export) and
//! `csv:codec` (the index inside it). No bespoke auth, storage, or zip library.

#[allow(warnings)]
mod bindings;

use serde_json::{json, Map, Value};

use bindings::auth::identity::accounts;
use bindings::auth::identity::authorizer;
use bindings::auth::identity::session;
use bindings::auth::identity::types::{AuthError, Principal};
use bindings::csv::codec::codec as csv;
use bindings::records::store::store as records;
use bindings::wasi::clocks::wall_clock;
use bindings::zip::archive::archiver as zip;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

const TENANT: &str = "stash";
const NOTES: &str = "notes";

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

            (Method::Post, ["api", "notes"]) => create_note(&request),
            (Method::Get, ["api", "notes"]) => list_notes(&request),
            (Method::Patch, ["api", "notes", id]) => edit_note(&request, id),
            (Method::Delete, ["api", "notes", id]) => delete_note(&request, id),
            (Method::Get, ["api", "export.zip"]) => export_zip(&request),
            _ => Outcome::Err(404, "not_found".into()),
        };
        emit(response_out, outcome);
    }
}

enum Outcome {
    Json(u16, String),
    Err(u16, String),
    Auth(AuthError),
    File(u16, String, Option<String>, Vec<u8>),
}

fn now() -> u64 {
    wall_clock::now().seconds
}

fn usage() -> Outcome {
    Outcome::Json(
        200,
        json!({
            "service": "stash",
            "about": "a personal note stash you can export as a .zip (Markdown + CSV index + manifest, via zip:archive)",
            "auth": "POST /api/register|login|logout, GET /api/me",
            "notes": "POST|GET /api/notes {title, body}, PATCH|DELETE /api/notes/{id}",
            "export": "GET /api/export.zip"
        })
        .to_string(),
    )
}

// ---- auth -------------------------------------------------------------------

guestio::guest_bearer!();

fn introspect(request: &IncomingRequest) -> Result<Principal, Outcome> {
    let token =
        bearer(request).ok_or(Outcome::Auth(AuthError::InvalidToken("missing bearer".into())))?;
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

// ---- notes ------------------------------------------------------------------

fn owned_notes(subject: &str) -> Vec<(String, Value)> {
    let mut v: Vec<(String, Value)> = records::find_by(NOTES, "owner", &json!(subject).to_string())
        .unwrap_or_default()
        .iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok().map(|v| (e.id.clone(), v)))
        .collect();
    v.sort_by_key(|(_, n)| n["created"].as_u64().unwrap_or(0));
    v
}

fn note(id: &str) -> Option<Value> {
    records::get(NOTES, id).ok().and_then(|e| serde_json::from_str::<Value>(&e.data).ok()).map(
        |mut v| {
            v["id"] = json!(id);
            v
        },
    )
}

fn create_note(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let title = b["title"].as_str().unwrap_or("").trim().to_string();
    if title.is_empty() {
        return Outcome::Err(422, "title required".into());
    }
    let d = json!({ "title": title, "body": b["body"].as_str().unwrap_or(""), "owner": p.subject, "created": now() });
    match records::create(NOTES, &d.to_string(), &["owner".to_string()]) {
        Ok(rec) => {
            let mut v: Value = serde_json::from_str(&rec.data).unwrap_or(d);
            v["id"] = json!(rec.id);
            Outcome::Json(201, v.to_string())
        }
        Err(e) => store_err(e),
    }
}

fn list_notes(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let items: Vec<Value> = owned_notes(&p.subject)
        .into_iter()
        .map(|(id, mut v)| {
            v["id"] = json!(id);
            v
        })
        .collect();
    Outcome::Json(200, json!({ "items": items }).to_string())
}

fn edit_note(request: &IncomingRequest, id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let mut n = match note(id) {
        Some(n) => n,
        None => return Outcome::Err(404, "not_found".into()),
    };
    if n["owner"].as_str() != Some(&p.subject) {
        return Outcome::Err(403, "not your note".into());
    }
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    if let Some(t) = b["title"].as_str() {
        n["title"] = json!(t.trim());
    }
    if let Some(body) = b["body"].as_str() {
        n["body"] = json!(body);
    }
    n.as_object_mut().map(|m| m.remove("id"));
    match records::update(NOTES, id, &n.to_string(), 0) {
        Ok(_) => {
            n["id"] = json!(id);
            Outcome::Json(200, n.to_string())
        }
        Err(e) => store_err(e),
    }
}

fn delete_note(request: &IncomingRequest, id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    match note(id) {
        Some(n) if n["owner"].as_str() == Some(&p.subject) => {
            let _ = records::delete(NOTES, id);
            Outcome::Json(200, json!({ "ok": true }).to_string())
        }
        Some(_) => Outcome::Err(403, "not your note".into()),
        None => Outcome::Err(404, "not_found".into()),
    }
}

// ---- the ZIP export (zip:archive + csv:codec) -------------------------------

/// Sanitize a title into a filename-safe slug.
fn slug(title: &str) -> String {
    let mut s: String = title
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        "note".into()
    } else {
        s
    }
}

fn export_zip(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let notes = owned_notes(&p.subject);

    let mut files: Vec<zip::File> = Vec::new();
    // one Markdown file per note (id suffix keeps names unique).
    let mut index_rows: Vec<csv::Row> =
        vec![csv::Row { fields: vec!["id".into(), "title".into(), "created".into()] }];
    let mut manifest_notes: Vec<Value> = Vec::new();
    for (id, n) in &notes {
        let title = n["title"].as_str().unwrap_or("untitled");
        let bodytext = n["body"].as_str().unwrap_or("");
        let created = n["created"].as_u64().unwrap_or(0);
        let short = &id[id.len().saturating_sub(6)..];
        files.push(zip::File {
            name: format!("notes/{}-{}.md", slug(title), short),
            data: format!("# {}\n\n{}\n", title, bodytext).into_bytes(),
        });
        index_rows
            .push(csv::Row { fields: vec![id.clone(), title.to_string(), created.to_string()] });
        manifest_notes.push(json!({ "id": id, "title": title }));
    }

    // index.csv via csv:codec.
    let dialect = csv::Dialect { delimiter: ",".into(), has_header: true, trim: false };
    let index_csv = csv::format(&index_rows, &dialect);
    files.push(zip::File { name: "index.csv".into(), data: index_csv.into_bytes() });

    // manifest.json.
    let manifest =
        json!({ "app": "stash", "exported": now(), "count": notes.len(), "notes": manifest_notes });
    files.push(zip::File { name: "manifest.json".into(), data: manifest.to_string().into_bytes() });

    // assemble the archive.
    let bytes = zip::archive(&files);
    Outcome::File(200, "application/zip".into(), Some("stash-export.zip".into()), bytes)
}

// ---- demo seed --------------------------------------------------------------

fn seed_demo(subject: &str) {
    let demo = [
        ("Welcome to stash", "Keep short notes here.\n\nHit **Export .zip** to download everything as a real ZIP — one `.md` per note, plus `index.csv` and `manifest.json`."),
        ("Shopping list", "- coffee\n- oat milk\n- a WIT component or two"),
        ("Idea", "An app that bundles your data into a zip using a *composed* zip:archive component — no zip library in the app."),
    ];
    for (title, bodytext) in demo {
        let d = json!({ "title": title, "body": bodytext, "owner": subject, "created": now() });
        let _ = records::create(NOTES, &d.to_string(), &["owner".to_string()]);
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

guestio::guest_read_body!(MAX_BODY_BYTES);

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

fn respond(
    response_out: ResponseOutparam,
    status: u16,
    ctype: &str,
    disposition: Option<&str>,
    body: &[u8],
) {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[ctype.as_bytes().to_vec()]);
    if let Some(d) = disposition {
        let _ = headers.set("content-disposition", &[d.as_bytes().to_vec()]);
    }
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
