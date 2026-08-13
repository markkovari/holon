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
mod bindings;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::knowledge::graph::store as graph;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

fn param(query: &str, key: &str) -> String {
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| percent(v))
        .unwrap_or_default()
}

/// Ids are file paths and URLs, so `%2F` has to survive the trip.
fn percent(s: &str) -> String {
    let b = s.replace('+', " ");
    let b = b.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match (b[i], b.get(i + 1), b.get(i + 2)) {
            (b'%', Some(h), Some(l)) => match u8::from_str_radix(
                core::str::from_utf8(&[*h, *l]).unwrap_or("zz"),
                16,
            ) {
                Ok(v) => {
                    out.push(v);
                    i += 3;
                }
                Err(_) => {
                    out.push(b[i]);
                    i += 1;
                }
            },
            _ => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

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

fn read_body(request: IncomingRequest) -> String {
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
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let (route, query) = match path.split_once('?') {
            Some((r, q)) => (r.to_string(), q.to_string()),
            None => (path.clone(), String::new()),
        };
        let method = request.method();
        let (kind, id) = (param(&query, "kind"), param(&query, "id"));

        let body = match (&method, route.as_str()) {
            (Method::Post, "/upsert") => {
                let props = read_body(request);
                let n = graph::Node { kind, id, properties: props };
                match graph::upsert(&n) {
                    Ok(()) => "{\"ok\":true}".to_string(),
                    Err(e) => err(e),
                }
            }
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
                let props = read_body(request);
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

        let headers = Fields::new();
        let _ = headers.set(&"content-type".to_string(), &[b"application/json".to_vec()]);
        let resp = OutgoingResponse::new(headers);
        let _ = resp.set_status_code(200);
        let out = resp.body().expect("body");
        ResponseOutparam::set(response_out, Ok(resp));
        if let Ok(stream) = out.write() {
            let _ = stream.blocking_write_and_flush(body.as_bytes());
            drop(stream);
        }
        let _ = OutgoingBody::finish(out, None);
    }
}

bindings::export!(Component with_types_in bindings);
