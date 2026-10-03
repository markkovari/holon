//! `artifact-probe` — an instrument for `artifact:cache` (see wit/probe.wit).
//!
//!   GET  /lookup?producer=&version=&inputs=&params=   hit | claimed | pending
//!   GET  /id?…same…                                   the derived id, no store touched
//!   POST /put?claim=            body is the artifact
//!   GET  /get?id=
//!   POST /abandon?claim=

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "../blob-store/wit",
            "../../host/wit/deps/comp-store",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../artifact-cache/wit",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "wit",
        ],
        world: "comp:artifactprobe/artifact-probe",
        generate_all,
    });
    /// Stable names for the p3 WASI modules (see `guestio`'s p3 section).
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}

use bindings::artifact::cache::store as cache;
use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Method, Request, Response};
use serde_json::json;

guestio::guest_p3_respond!();

struct Component;

fn param(query: &str, key: &str) -> String {
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.replace('+', " "))
        .unwrap_or_default()
}

/// `inputs` is comma-separated: enough for a probe, and it keeps the ordering
/// visible in the URL, which is a property the cache cares about.
fn key_from(query: &str) -> cache::ArtifactKey {
    let raw = param(query, "inputs");
    cache::ArtifactKey {
        producer: param(query, "producer"),
        version: param(query, "version"),
        inputs: if raw.is_empty() {
            Vec::new()
        } else {
            raw.split(',').map(str::to_string).collect()
        },
        params: param(query, "params"),
    }
}

fn err(e: cache::CacheError) -> String {
    let (kind, msg) = match e {
        cache::CacheError::Unavailable(m) => ("unavailable", m),
        cache::CacheError::Invalid(m) => ("invalid", m),
        cache::CacheError::NotYourClaim(m) => ("not-your-claim", m),
    };
    json!({ "error": kind, "detail": msg }).to_string()
}

/// A ceiling on a request body, not a policy: past this the read gives up and
/// the body reads as empty, rather than growing until the store's memory cap
/// traps the component and the connection simply closes.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_p3_read_body!(MAX_BODY_BYTES);

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let path = request.get_path_with_query().unwrap_or_else(|| "/".to_string());
        let (route, query) = match path.split_once('?') {
            Some((r, q)) => (r.to_string(), q.to_string()),
            None => (path.clone(), String::new()),
        };
        let method = request.get_method();

        let body = match (&method, route.as_str()) {
            (Method::Get, "/id") => {
                json!({ "id": cache::derive_id(&key_from(&query)) }).to_string()
            }
            (Method::Get, "/lookup") => match cache::lookup(&key_from(&query)) {
                Ok(cache::Outcome::Hit(a)) => json!({
                    "state": "hit",
                    "id": a.id,
                    "content": String::from_utf8_lossy(&a.bytes),
                    "producer": a.producer,
                })
                .to_string(),
                Ok(cache::Outcome::Claimed(token)) => {
                    json!({ "state": "claimed", "claim": token }).to_string()
                }
                Ok(cache::Outcome::Pending(ms)) => {
                    json!({ "state": "pending", "retry_ms": ms }).to_string()
                }
                Err(e) => err(e),
            },
            (Method::Post, "/put") => {
                let claim = param(&query, "claim");
                let bytes = read_body(request).await.unwrap_or_default();
                match cache::put(&claim, &bytes, "text/plain") {
                    Ok(id) => json!({ "stored": id }).to_string(),
                    Err(e) => err(e),
                }
            }
            (Method::Post, "/abandon") => match cache::abandon(&param(&query, "claim")) {
                Ok(()) => json!({ "abandoned": true }).to_string(),
                Err(e) => err(e),
            },
            (Method::Get, "/get") => match cache::get(&param(&query, "id")) {
                Ok(Some(a)) => json!({
                    "id": a.id, "content": String::from_utf8_lossy(&a.bytes),
                })
                .to_string(),
                Ok(None) => json!({ "found": false }).to_string(),
                Err(e) => err(e),
            },
            _ => json!({
                "service": "artifact-probe",
                "routes": ["/lookup", "/id", "/put", "/get", "/abandon"]
            })
            .to_string(),
        };

        respond(200, "application/json", body)
    }
}

bindings::export!(Component with_types_in bindings);
