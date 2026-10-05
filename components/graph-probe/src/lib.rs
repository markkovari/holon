//! `graph-probe` — an instrument for `knowledge:graph/store` (see wit/probe.wit).
//!
//!   POST /upsert?kind=&id=      body is the properties JSON
//!   GET  /get?kind=&id=
//!   POST /relate?kind=&id=&edge=&to-kind=&to-id=
//!   GET  /neighbours?kind=&id=&edge=&dir=out|in|both
//!
//! Every route answers JSON and reports an error as `{"error":"..."}` with a 200,
//! because the thing under test is what the graph said, and a status code would
//! flatten "the database refused this" into the same shape as "the host refused
//! the egress" — which are the two failures this exists to tell apart.

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../../host/wit/deps/comp-secrets",
            "../knowledge-graph/wit",
            "wit",
        ],
        world: "comp:graphprobe/graph-probe",
        generate_all,
    });
    /// Stable names for the p3 WASI modules (see `guestio`'s p3 section).
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}

use bindings::knowledge::graph::store as graph;
use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Method, Request, Response};

guestio::guest_p3_respond!();

struct Component;

fn param(query: &str, key: &str) -> String {
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| percent(v))
        .unwrap_or_default()
}

use guestfmt::percent_decode as percent;

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn err(e: graph::GraphError) -> String {
    let (kind, msg) = match e {
        graph::GraphError::Rejected(m) => ("rejected", m),
        graph::GraphError::Unavailable(m) => ("unavailable", m),
        graph::GraphError::NotConfigured(m) => ("not-configured", m),
    };
    format!("{{\"error\":\"{kind}\",\"detail\":\"{}\"}}", esc(&msg))
}

fn node_json(n: &graph::Node) -> String {
    format!(
        "{{\"kind\":\"{}\",\"id\":\"{}\",\"properties\":{}}}",
        esc(&n.kind),
        esc(&n.id),
        // Already JSON, by contract.
        if n.properties.is_empty() { "{}" } else { &n.properties }
    )
}

/// A ceiling on a request body, not a policy: past this the read gives up and
/// the body reads as empty, rather than growing until the store's memory cap
/// traps the component and the connection simply closes.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_p3_read_body_text!(MAX_BODY_BYTES);

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let path = request.get_path_with_query().unwrap_or_else(|| "/".to_string());
        let (route, query) = match path.split_once('?') {
            Some((r, q)) => (r.to_string(), q.to_string()),
            None => (path.clone(), String::new()),
        };
        let method = request.get_method();
        let (kind, id) = (param(&query, "kind"), param(&query, "id"));

        let body = match (&method, route.as_str()) {
            (Method::Post, "/upsert") => {
                let props = read_body(request).await;
                let n = graph::Node { kind, id, properties: props };
                match graph::upsert(&n) {
                    Ok(()) => "{\"ok\":true}".to_string(),
                    Err(e) => err(e),
                }
            }
            // The escape hatch, and until now the only interface of the graph no
            // test could reach: `contract-registry` does every one of its reads and
            // writes through `query`, and a bug that lived only here — a statement
            // over 4096 bytes trapping the component — took down a real run while
            // every typed verb stayed green.
            (Method::Post, "/query") => match graph::query(&read_body(request).await) {
                Ok(body) => body,
                Err(e) => err(e),
            },
            (Method::Get, "/get") => match graph::get(&kind, &id) {
                Ok(Some(n)) => node_json(&n),
                Ok(None) => "{\"found\":false}".to_string(),
                Err(e) => err(e),
            },
            (Method::Post, "/relate") => {
                let from = graph::Node { kind, id, properties: String::new() };
                let to = graph::Node {
                    kind: param(&query, "to-kind"),
                    id: param(&query, "to-id"),
                    properties: String::new(),
                };
                let props = read_body(request).await;
                match graph::relate(&from, &param(&query, "edge"), &to, &props) {
                    Ok(()) => "{\"ok\":true}".to_string(),
                    Err(e) => err(e),
                }
            }
            (Method::Get, "/neighbours") => {
                let dir = match param(&query, "dir").as_str() {
                    "in" => graph::Direction::Incoming,
                    "both" => graph::Direction::Both,
                    _ => graph::Direction::Outgoing,
                };
                match graph::neighbours(&kind, &id, &param(&query, "edge"), dir, 50) {
                    Ok(ns) => format!(
                        "{{\"nodes\":[{}]}}",
                        ns.iter().map(node_json).collect::<Vec<_>>().join(",")
                    ),
                    Err(e) => err(e),
                }
            }
            _ => "{\"service\":\"graph-probe\",\"routes\":[\"/upsert\",\"/get\",\"/relate\",\"/neighbours\"]}"
                .to_string(),
        };

        respond(200, "application/json", body)
    }
}

bindings::export!(Component with_types_in bindings);
