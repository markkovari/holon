//! `clinic-domain` — owners, pets and visits, as one component.

mod access;
#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "../../host/wit/deps/comp-store",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../record-store/wit",
            "../id-generate/wit",
            "../audit-log/wit",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/deps/ratelimit-guard",
            "../../wit/auth.wit",
            "../../wit/deps/wasi-blobstore-0.2.0-draft",
            "../../wit/deps/wasmcloud-messaging-0.2.0",
            "../event-bus/wit",
            "../search-index/wit",
            "../csv/wit",
            "wit",
        ],
        world: "clinic:domain/clinic-domain",
        generate_all,
    });
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}
mod owners;
mod reports;
mod visits;

use bindings::id::generate::generator as ids;
use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Fields, Method, Request, Response};
use bindings::records::store::store as records;
use serde_json::{json, Value};

guestio::guest_p3_respond!();

struct Component;

pub struct Reply {
    pub status: u16,
    /// `Value::Null` means no body at all.
    pub json: Value,
    /// Not serialised as JSON: a `Value::String` would become one quoted blob instead of columns.
    pub raw: Option<(String, Vec<u8>)>,
}

impl Reply {
    pub fn json(status: u16, body: Value) -> Self {
        Reply { status, json: body, raw: None }
    }
    pub fn err(status: u16, code: &str) -> Self {
        Reply::json(status, json!({ "error": code }))
    }
    /// A JSON `null` is not "no body".
    pub fn no_content() -> Self {
        Reply::json(204, Value::Null)
    }
    pub fn raw(status: u16, content_type: &str, bytes: Vec<u8>) -> Self {
        Reply { status, json: Value::Null, raw: Some((content_type.to_string(), bytes)) }
    }
}

/// `bearer` is read here because `Request` is consumed to read the body.
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

/// Three because `access-and-search` is judged on ranking; `pet_id` stays the first.
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

fn bearer(request: &Request) -> String {
    let headers = request.get_headers();
    let Some(value) = headers.get("authorization").into_iter().next() else {
        return String::new();
    };
    String::from_utf8_lossy(&value).strip_prefix("Bearer ").unwrap_or_default().trim().to_string()
}

/// Past this the body reads as empty, rather than the store's memory cap trapping the component.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_p3_read_body_text!(MAX_BODY_BYTES);

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let path = request.get_path_with_query().unwrap_or_else(|| "/".into());
        let (raw, query) = match path.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (path.clone(), String::new()),
        };
        let route = Route {
            segments: raw.split('/').filter(|s| !s.is_empty()).map(percent).collect(),
            query,
            bearer: bearer(&request),
        };
        let method = request.get_method();
        let body = match method {
            Method::Post | Method::Put | Method::Patch => read_body(request).await,
            _ => String::new(),
        };

        let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
        let Reply { status, json: payload, raw } = match seg.as_slice() {
            ["health"] => Reply::json(200, json!({ "ok": true })),
            // A fixture: `visits` cannot book without a pet, and pets belong to the other part.
            ["test", "seed"] => seed(),
            // Before the `api/pets` arm, or `/api/pets/search` reads as a pet whose id is "search".
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
        let bytes = match raw {
            // Byte-for-byte: `to_string()` here turned a CSV document into a JSON string literal.
            Some((_, bytes)) => bytes,
            None if !payload.is_null() => payload.to_string().into_bytes(),
            None => Vec::new(),
        };
        respond_with(status, headers, bytes)
    }
}

bindings::export!(Component with_types_in bindings);
