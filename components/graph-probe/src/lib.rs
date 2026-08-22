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
use bindings::wasi::io::streams::OutputStream;

/// Write a whole response body, however long it is.
///
/// `blocking-write-and-flush` accepts at most 4096 bytes and TRAPS above that
/// rather than returning an error, so a probe that answers with something big
/// simply dies and its caller sees an empty body. Measured: a contract file grew
/// past 4096 and every generation of a real run reported `the boundary failed:
/// unreadable answer (EOF while parsing a value at line 1 column 0)` — an error
/// about JSON, three components away from the write that caused it.
///
/// `check-write` is the stream saying how much it will take right now, so this
/// writes in whatever bites it offers and flushes once, rather than picking a
/// constant and flushing every 4 KB.
fn write_all(stream: &OutputStream, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        let ready = match stream.check_write() {
            Ok(0) => {
                // Zero is "full, wait" — not a failure. The pollable resolves
                // when the stream has drained.
                stream.subscribe().block();
                continue;
            }
            Ok(n) => n as usize,
            Err(_) => return,
        };
        let take = ready.min(bytes.len());
        if stream.write(&bytes[..take]).is_err() {
            return;
        }
        bytes = &bytes[take..];
    }
    let _ = stream.blocking_flush();
}

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

/// A ceiling on a request body, not a policy: past this the read gives up and
/// the body reads as empty, rather than growing until the store's memory cap
/// traps the component and the connection simply closes.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

fn read_body(request: IncomingRequest) -> String {
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
            // The escape hatch, and until now the only interface of the graph no
            // test could reach: `contract-registry` does every one of its reads and
            // writes through `query`, and a bug that lived only here — a statement
            // over 4096 bytes trapping the component — took down a real run while
            // every typed verb stayed green.
            (Method::Post, "/query") => match graph::query(&read_body(request)) {
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
        let _ = headers.set("content-type", &[b"application/json".to_vec()]);
        let resp = OutgoingResponse::new(headers);
        let _ = resp.set_status_code(200);
        let out = resp.body().expect("body");
        ResponseOutparam::set(response_out, Ok(resp));
        if let Ok(stream) = out.write() {
            write_all(&stream, body.as_bytes());
            drop(stream);
        }
        let _ = OutgoingBody::finish(out, None);
    }
}

bindings::export!(Component with_types_in bindings);
