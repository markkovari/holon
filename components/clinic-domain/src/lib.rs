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

mod access;
#[allow(warnings)]
mod bindings;
mod owners;
mod reports;
mod visits;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::id::generate::generator as ids;
use bindings::records::store::store as records;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};
use serde_json::{json, Value};

guestio::guest_write_all!();

struct Component;

/// What a handler answers with: a status, and a body that is JSON unless the
/// handler says otherwise.
pub struct Reply {
    pub status: u16,
    /// The JSON body. `Value::Null` means no body at all — see `no_content`.
    pub json: Value,
    /// A body the router must NOT serialise as JSON, with its content type.
    ///
    /// `text/csv` cannot be expressed as a `Value`: `Value::String` serialises
    /// to a JSON string *literal*, surrounding quotes and `\"` escapes included,
    /// so a CSV parser reads one quoted blob instead of six columns. Both parts
    /// of the run that needed this asked for it in the same words before the
    /// scaffold had it.
    pub raw: Option<(String, Vec<u8>)>,
}

impl Reply {
    pub fn json(status: u16, body: Value) -> Self {
        Reply { status, json: body, raw: None }
    }
    pub fn err(status: u16, code: &str) -> Self {
        Reply::json(status, json!({ "error": code }))
    }
    /// 204 carries no body, and a JSON `null` is not "no body".
    pub fn no_content() -> Self {
        Reply::json(204, Value::Null)
    }
    /// A body sent through byte-for-byte, under the content type you name.
    pub fn raw(status: u16, content_type: &str, bytes: Vec<u8>) -> Self {
        Reply { status, json: Value::Null, raw: Some((content_type.to_string(), bytes)) }
    }
}

/// The path segments of a request, its query string, and its bearer token.
///
/// `bearer` is here rather than read per-handler because the header plumbing is
/// scaffold: `IncomingRequest` is consumed to read the body, so a part that went
/// looking for the header afterwards would find nothing. Empty when absent.
pub struct Route {
    pub segments: Vec<String>,
    pub query: String,
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

use guestfmt::percent_decode as percent;

/// One owner and three pets, written straight to the store.
///
/// Three, because `access-and-search` is judged on RANKING and a corpus of one
/// ranks trivially. `pet_id` stays the first of them: `e2e-visits.sh` books
/// against that field and predates this.
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
    for (name, species) in [("Marbles", "cat"), ("Biscuit", "dog")] {
        let _ = records::create(
            "pets",
            &json!({ "owner_id": owner, "name": name, "species": species, "born": "2022-03-04" })
                .to_string(),
            &["owner_id".to_string()],
        );
    }
    let _ = ids::ulid();
    Reply::json(201, json!({ "owner_id": owner, "pet_id": pet }))
}

/// The token out of `Authorization: Bearer <token>`, or empty.
fn bearer(request: &IncomingRequest) -> String {
    let headers = request.headers();
    let Some(value) = headers.get("authorization").into_iter().next() else {
        return String::new();
    };
    String::from_utf8_lossy(&value).strip_prefix("Bearer ").unwrap_or_default().trim().to_string()
}

/// A ceiling on a request body, not a policy: past this the read gives up and
/// the body reads as empty, rather than growing until the store's memory cap
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
                // Same reasoning as the error arm below: an over-long body reads
                // as empty rather than as a plausible prefix of itself.
                if out.len() + chunk.len() > MAX_BODY_BYTES {
                    return String::new();
                }
                out.extend_from_slice(&chunk);
            }
            Err(bindings::wasi::io::streams::StreamError::Closed) => break,
            // No error channel here, so the choice is a truncated body or none.
            // None: a caller parsing an empty body fails cleanly, where half a
            // JSON document can parse into something plausible and wrong.
            Err(_) => return String::new(),
        }
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
            bearer: bearer(&request),
        };
        let method = request.method();
        let body = match method {
            Method::Post | Method::Put | Method::Patch => read_body(&request),
            _ => String::new(),
        };

        // The router: `/health` here, everything else to the half that owns it.
        let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
        let Reply { status, json: payload, raw } = match seg.as_slice() {
            ["health"] => Reply::json(200, json!({ "ok": true })),
            // A fixture, not a feature. The `visits` half cannot book anything
            // without a pet, and pets belong to the OTHER half — so a gate that
            // judges visits alone needs a way to get one that does not depend on
            // code the branch under test has not written. Scaffold, not writable,
            // and it says what it is.
            ["test", "seed"] => seed(),
            // Before the `api/pets` arm below, or `/api/pets/search` reads as a
            // pet whose id is "search" and the owners half answers 404.
            ["api", "staff", ..] | ["api", "pets", "search"] => {
                access::handle(&method, &route, &body)
            }
            ["api", "owners", ..] | ["api", "pets", ..] => owners::handle(&method, &route, &body),
            ["api", "visits", ..] => visits::handle(&method, &route, &body),
            ["api", "reports", ..] => reports::handle(&method, &route, &body),
            _ => Reply::err(404, "not_found"),
        };

        let headers = Fields::new();
        let content_type = match &raw {
            Some((ct, _)) => ct.as_str(),
            None => "application/json",
        };
        let _ = headers.set("content-type", &[content_type.as_bytes().to_vec()]);
        let resp = OutgoingResponse::new(headers);
        let _ = resp.set_status_code(status);
        let out = resp.body().expect("body");
        ResponseOutparam::set(response_out, Ok(resp));
        if let Ok(stream) = out.write() {
            match &raw {
                // Byte-for-byte. `to_string()` here is what turned a CSV
                // document into a JSON string literal.
                Some((_, bytes)) => {
                    let _ = write_all(&stream, bytes);
                }
                None if !payload.is_null() => {
                    let _ = write_all(&stream, payload.to_string().as_bytes());
                }
                None => {}
            }
            drop(stream);
        }
        let _ = OutgoingBody::finish(out, None);
    }
}

bindings::export!(Component with_types_in bindings);
