//! `clinic-domain` — owners, pets and visits, as one component.
//!
//! ## What is scaffold and what is the goal
//!
//! This file is the ROUTER and it is not writable by either part: it dispatches to
//! `owners` and `visits`, and it answers `/health` so the harness can tell "the
//! component is not up" from "the component is wrong". Both halves need it and
//! neither owns it, which is exactly the shape a shared file has to have when two
//! agents are working at once — the alternative is a merge conflict on every run.
//!
//! `src/owners.rs` and `src/visits.rs` are the goal. `CONTRACT.md` is what they
//! must agree on.

#[allow(warnings)]
mod bindings;
mod owners;
mod visits;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::id::generate::generator as ids;
use bindings::records::store::store as records;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};
use serde_json::{json, Value};

struct Component;

/// What a handler answers with: a status and a JSON body.
pub struct Reply(pub u16, pub Value);

impl Reply {
    pub fn json(status: u16, body: Value) -> Self {
        Reply(status, body)
    }
    pub fn err(status: u16, code: &str) -> Self {
        Reply(status, json!({ "error": code }))
    }
    /// 204 carries no body, and a JSON `null` is not "no body".
    pub fn no_content() -> Self {
        Reply(204, Value::Null)
    }
}

/// The path segments of a request, and its query string.
pub struct Route {
    pub segments: Vec<String>,
    pub query: String,
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

/// One owner and one pet, written straight to the store.
fn seed() -> Reply {
    let owner = match records::create(
        "owners",
        &json!({ "name": "Seed Owner", "email": "seed@example.test" }).to_string(),
        &[],
    ) {
        Ok(e) => e.id,
        Err(_) => return Reply::err(500, "seed_failed"),
    };
    let pet = match records::create(
        "pets",
        &json!({ "owner_id": owner, "name": "Seed Pet", "species": "cat", "born": "2021-01-01" })
            .to_string(),
        &["owner_id".to_string()],
    ) {
        Ok(e) => e.id,
        Err(_) => return Reply::err(500, "seed_failed"),
    };
    let _ = ids::ulid();
    Reply::json(201, json!({ "owner_id": owner, "pet_id": pet }))
}

fn read_body(request: &IncomingRequest) -> String {
    let Ok(body) = request.consume() else { return String::new() };
    let Ok(stream) = body.stream() else { return String::new() };
    let mut out = Vec::new();
    while let Ok(chunk) = stream.blocking_read(64 * 1024) {
        if chunk.is_empty() {
            break;
        }
        out.extend_from_slice(&chunk);
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let path = request.path_with_query().unwrap_or_else(|| "/".into());
        let (raw, query) = match path.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (path.clone(), String::new()),
        };
        let route = Route {
            segments: raw.split('/').filter(|s| !s.is_empty()).map(percent).collect(),
            query,
        };
        let method = request.method();
        let body = match method {
            Method::Post | Method::Put | Method::Patch => read_body(&request),
            _ => String::new(),
        };

        // The router: `/health` here, everything else to the half that owns it.
        let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
        let Reply(status, payload) = match seg.as_slice() {
            ["health"] => Reply::json(200, json!({ "ok": true })),
            // A fixture, not a feature. The `visits` half cannot book anything
            // without a pet, and pets belong to the OTHER half — so a gate that
            // judges visits alone needs a way to get one that does not depend on
            // code the branch under test has not written. Scaffold, not writable,
            // and it says what it is.
            ["test", "seed"] => seed(),
            ["api", "owners", ..] | ["api", "pets", ..] => owners::handle(&method, &route, &body),
            ["api", "visits", ..] => visits::handle(&method, &route, &body),
            _ => Reply::err(404, "not_found"),
        };

        let headers = Fields::new();
        let _ = headers.set(&"content-type".to_string(), &[b"application/json".to_vec()]);
        let resp = OutgoingResponse::new(headers);
        let _ = resp.set_status_code(status);
        let out = resp.body().expect("body");
        ResponseOutparam::set(response_out, Ok(resp));
        if let Ok(stream) = out.write() {
            if !payload.is_null() {
                let _ = stream.blocking_write_and_flush(payload.to_string().as_bytes());
            }
            drop(stream);
        }
        let _ = OutgoingBody::finish(out, None);
    }
}

bindings::export!(Component with_types_in bindings);
