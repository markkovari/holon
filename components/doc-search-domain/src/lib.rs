//! `doc-search-domain` — retrieval-augmented answers over an indexed library, behind
//! a TOTP step-up, a per-subject budget and a cache.
//!
//! ## What is scaffold and what is the goal
//!
//! This file is the ROUTER and no part may write it: it dispatches to `library`,
//! `answer` and `stepup`, answers `/health`, mints a test token, seeds a library, and
//! can mark a session stepped-up directly. Three parts need it and none owns it.
//!
//! `src/library.rs`, `src/answer.rs` and `src/stepup.rs` are the goal.
//! `CONTRACT.md` is what they must agree on.
//!
//! ## Why these three
//!
//! The chain is about COST, not order. `library` decides what can be found, `stepup`
//! decides who may spend, and `answer` is the only part that spends anything — so a
//! second identical question must cost nothing, and a refusal must not be remembered
//! as an answer. Neither is visible in one request, and neither is checkable inside
//! one part.

mod answer;
#[allow(warnings)]
mod bindings;
mod library;
mod stepup;

use bindings::auth::identity::session as auth_session;
use bindings::auth::identity::types as auth_types;
use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::records::store::store as records;
use bindings::search::index::index as search;
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};
use serde_json::{json, Value};

/// Write a whole body, however long it is.
///
/// `blocking-write-and-flush` accepts at most 4096 bytes and TRAPS above that
/// rather than returning an error: the component dies mid-response and the caller
/// sees `connection closed before message completed`, three layers from the cause.
fn write_all(stream: &bindings::wasi::io::streams::OutputStream, mut bytes: &[u8]) -> bool {
    while !bytes.is_empty() {
        let ready = match stream.check_write() {
            Ok(0) => {
                stream.subscribe().block();
                continue;
            }
            Ok(n) => n as usize,
            Err(_) => return false,
        };
        let take = ready.min(bytes.len());
        if stream.write(&bytes[..take]).is_err() {
            return false;
        }
        bytes = &bytes[take..];
    }
    stream.blocking_flush().is_ok()
}

struct Component;

/// What a handler answers with: a status and a JSON body.
pub struct Reply {
    pub status: u16,
    /// `Value::Null` means no body at all — see `no_content`.
    pub json: Value,
}

impl Reply {
    pub fn json(status: u16, body: Value) -> Self {
        Reply { status, json: body }
    }
    pub fn err(status: u16, code: &str) -> Self {
        Reply::json(status, json!({ "error": code }))
    }
    /// 204 carries no body, and a JSON `null` is not "no body".
    pub fn no_content() -> Self {
        Reply::json(204, Value::Null)
    }
}

/// One request, as a part sees it.
///
/// The bearer is handed over as a STRING and not as a principal: resolving it is
/// `auth:identity/authorizer`'s job and doing it here would take the part's whole
/// reason for importing that capability away.
pub struct Route {
    pub segments: Vec<String>,
    pub query: String,
    /// The `Authorization: Bearer …` value, empty when the header is absent.
    pub bearer: String,
}

impl Route {
    pub fn param(&self, key: &str) -> String {
        self.query
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .find(|(k, _)| *k == key)
            .map(|(_, v)| percent(v))
            .unwrap_or_default()
    }
}

fn percent(s: &str) -> String {
    let b = s.replace('+', " ");
    let b = b.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match (b[i], b.get(i + 1), b.get(i + 2)) {
            (b'%', Some(h), Some(l)) => {
                match u8::from_str_radix(core::str::from_utf8(&[*h, *l]).unwrap_or("zz"), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b[i]);
                        i += 1;
                    }
                }
            }
            _ => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A `wasi:config` value as a number, with a default.
///
/// Scaffold: reading config is plumbing every part would otherwise write out, and the
/// contract names the four keys. What a part does with the number is the goal; parsing
/// it is not.
pub fn cfg_u64(key: &str, default: u64) -> u64 {
    bindings::wasi::config::store::get(key)
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Unix seconds, for anything that has to be stamped.
pub fn now_secs() -> u64 {
    wall_clock::now().seconds
}

/// RFC3339 UTC seconds — what the contract stores in `reported_at`/`assisted_at`.
///
/// Written out by hand because this component has no date library and does not need
/// one: the epoch-to-civil conversion is twenty lines and a dependency is a decision.
pub fn rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let tod = secs % 86_400;
    // Howard Hinnant's civil_from_days, the shift-to-March algorithm.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// A token for a test caller, so no gate has to log in through a part it is not
/// judging.
///
/// Scaffold, and it is `session::issue` rather than a hand-built JWT for the same
/// reason the parts are made to call `authorize`: a fixture that mints its own
/// tokens is a fixture that can drift from what the verifier accepts, and then
/// every part fails for the router's reason.
fn mint(body: &str) -> Reply {
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let subject = req.get("subject").and_then(Value::as_str).unwrap_or("ada").to_string();
    let scopes: Vec<String> = match req.get("scopes").and_then(Value::as_array) {
        Some(list) => list.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        None => vec!["docs:write".into(), "docs:read".into()],
    };
    let principal = auth_types::Principal {
        subject,
        tenant: "docsearch".into(),
        roles: vec![],
        scopes,
        expires_at: now_secs() + 3600,
    };
    match auth_session::issue(&principal) {
        Ok(pair) => Reply::json(201, json!({ "token": pair.access_token })),
        Err(_) => Reply::err(503, "token_unavailable"),
    }
}

/// Three documents, stored AND indexed, so `answer` and `stepup` can be judged before
/// `library` exists.
///
/// Scaffold, and it says what it is: the documents are deliberately specific, because
/// `answer`'s gate has to tell a real model answer from a canned sentence, and the only
/// way to do that without comparing strings is to ask about something that appears
/// nowhere else.
fn seed() -> Reply {
    let mut ids = Vec::new();
    for (title, text, tag) in [
        (
            "Deploying to the lattice",
            "A unit is placed by the reconciler, which polls inventory every three \
             seconds. A parked app with min zero is activated by the first request that \
             reaches it, so the first response after a deploy is slower than the rest.",
            "ops",
        ),
        (
            "The pooling allocator",
            "comp-host runs wasmtime with the pooling allocator, which reserves linear \
             memory up front. Raising the per-instance ceiling past sixty-four mebibytes \
             costs reserved address space per instance, not per request.",
            "runtime",
        ),
        (
            "Egress is default-deny",
            "A component reaches nothing outbound until an authority is granted by name. \
             Loopback and RFC1918 need a second opt-in, because on a lattice node those \
             are the bus and every other node.",
            "security",
        ),
    ] {
        let doc = json!({ "title": title, "text": text, "tag": tag });
        let entry = match records::create("docs", &doc.to_string(), &["tag".to_string()]) {
            Ok(e) => e,
            Err(_) => return Reply::err(500, "seed_failed"),
        };
        // Indexed the way the contract says a document is indexed, because a fixture
        // that indexed it differently would make `answer`'s retrieval untestable.
        if search::index_doc(&entry.id, &format!("{title}\n{text}"), &[tag.to_string()]).is_err() {
            return Reply::err(500, "seed_index_failed");
        }
        ids.push(entry.id);
    }
    Reply::json(201, json!({ "doc_ids": ids }))
}

/// A verified step-up, written straight to the store.
///
/// `answer` is judged with `stepup` stubbed, and its first check is that the session is
/// stepped up — without this it could only ever be judged on its 403.
fn mark_stepped_up(body: &str) -> Reply {
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let subject = req.get("subject").and_then(Value::as_str).unwrap_or("ada");
    let doc = json!({ "subject": subject, "verified_at": now_secs(), "secret": "fixture" });
    match records::create("stepups", &doc.to_string(), &["subject".to_string()]) {
        Ok(e) => Reply::json(201, json!({ "id": e.id, "subject": subject })),
        Err(_) => Reply::err(500, "stepup_failed"),
    }
}

/// A ceiling on a body read into memory, not a policy: past this the read gives up
/// and the body reads as empty, rather than growing until the store's memory cap
/// traps the component and the connection simply closes.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

fn read_body(request: &IncomingRequest) -> String {
    let Ok(body) = request.consume() else { return String::new() };
    let Ok(stream) = body.stream() else { return String::new() };
    let mut out = Vec::new();
    loop {
        match stream.blocking_read(64 * 1024) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => {
                if out.len() + chunk.len() > MAX_BODY_BYTES {
                    return String::new();
                }
                out.extend_from_slice(&chunk);
            }
            Err(bindings::wasi::io::streams::StreamError::Closed) => break,
            // No error channel here, so the choice is a truncated body or none.
            // None: a caller parsing an empty body fails cleanly, where half a JSON
            // document can parse into something plausible and wrong.
            Err(_) => return String::new(),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// One header, as a string. Absent, repeated or non-UTF8 all read as empty.
fn header(request: &IncomingRequest, name: &str) -> String {
    let fields = request.headers();
    let values = fields.get(name);
    values.first().map(|v| String::from_utf8_lossy(v).into_owned()).unwrap_or_default()
}

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let path = request.path_with_query().unwrap_or_else(|| "/".into());
        let (raw_path, query) = match path.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (path.clone(), String::new()),
        };
        let bearer = header(&request, "authorization")
            .strip_prefix("Bearer ")
            .unwrap_or_default()
            .to_string();
        let route = Route {
            segments: raw_path.split('/').filter(|s| !s.is_empty()).map(percent).collect(),
            query,
            bearer,
        };
        let method = request.method();
        let body = match method {
            Method::Post | Method::Put | Method::Patch => read_body(&request),
            _ => String::new(),
        };

        // The router: `/health`, the token and the fixture here, everything else to
        // the part that owns it.
        let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
        let Reply { status, json: payload } = match seg.as_slice() {
            ["health"] => Reply::json(200, json!({ "ok": true })),
            ["test", "token"] => mint(&body),
            ["test", "seed"] => seed(),
            ["test", "stepup"] => mark_stepped_up(&body),
            // The stored document, straight out of the store. Scaffold, and it says
            // what it is: a part must be judgeable on what it WROTE without
            // depending on the part that owns the route for reading it back.
            ["test", "doc", id] => match records::get("docs", id) {
                Ok(e) => Reply::json(200, serde_json::from_str(&e.data).unwrap_or(json!({}))),
                Err(_) => Reply::err(404, "not_found"),
            },
            ["api", "answer"] => answer::handle(&method, &route, &body),
            ["api", "mfa", ..] => stepup::handle(&method, &route, &body),
            // `search` before the catch-all: it is the library's, and a match on
            // ["api", ..] alone would swallow it.
            // `["api", "docs"]` is not listed: `["api", "docs", ..]` already
            // matches the zero-extra-segment case, and naming both made the
            // second arm unreachable.
            ["api", "search"] | ["api", "docs", ..] => library::handle(&method, &route, &body),
            _ => Reply::err(404, "not_found"),
        };

        let headers = Fields::new();
        let _ = headers.set("content-type", &[b"application/json".to_vec()]);
        let resp = OutgoingResponse::new(headers);
        let _ = resp.set_status_code(status);
        let out = resp.body().expect("body");
        ResponseOutparam::set(response_out, Ok(resp));
        if let Ok(stream) = out.write() {
            if !payload.is_null() {
                let _ = write_all(&stream, payload.to_string().as_bytes());
            }
            drop(stream);
        }
        let _ = OutgoingBody::finish(out, None);
    }
}

bindings::export!(Component with_types_in bindings);
