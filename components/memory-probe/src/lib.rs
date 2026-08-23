//! `memory-probe` — an instrument for `knowledge:memory` (see wit/probe.wit).
//!
//!   POST /observe?ns=&key=&goal=&env=&attempt=&score=      body is the lesson
//!   POST /promote?goal=&score=&env=&attempt=               body is the lesson
//!   GET  /recall?goal=&k=&budget=&pools=&min=
//!   POST /attribute?keys=a,b&run=&ok=
//!   POST /evaluated?goal=&run=&score=&passed=&artifact=
//!   POST /decay?days=&min-uses=
//!   GET  /already-done?goal=&min=
//!
//! Every route answers JSON, and reports an error as `{"error":"…"}` with a **200**
//! — the same choice `graph-probe` made and for the same reason: the thing under
//! test is what the component said, and a status code would flatten "the policy
//! refused this" into the same shape as "the host refused the link", which are
//! exactly the two failures this exists to tell apart.

#[allow(warnings)]
mod bindings;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::knowledge::memory::memory::{self as mem, Entry, Namespace, RecallOpts};
use bindings::knowledge::memory::promotion;
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

/// Goals are sentences, so a space and a `%2F` both have to survive the trip.
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

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

fn num(query: &str, key: &str, default: u32) -> u32 {
    match param(query, key).as_str() {
        "" => default,
        v => v.parse().unwrap_or(default),
    }
}

fn signed(query: &str, key: &str, default: i32) -> i32 {
    match param(query, key).as_str() {
        "" => default,
        v => v.parse().unwrap_or(default),
    }
}

fn float(query: &str, key: &str) -> f64 {
    param(query, key).parse().unwrap_or(0.0)
}

fn flag(query: &str, key: &str) -> bool {
    matches!(param(query, key).as_str(), "true" | "1" | "yes")
}

fn ns_of(name: &str) -> Namespace {
    match name {
        "patterns" => Namespace::Patterns,
        "solutions" => Namespace::Solutions,
        // Anything unspelled is the pool an agent writes by default, never the
        // trusted one — the same rule the component itself applies.
        _ => Namespace::Errors,
    }
}

fn ns_name(ns: Namespace) -> &'static str {
    match ns {
        Namespace::Patterns => "patterns",
        Namespace::Solutions => "solutions",
        Namespace::Errors => "errors",
    }
}

/// `pools=errors,patterns` → the list. Empty means "all", which is the
/// component's own default and must stay distinguishable from "one pool".
fn pools_of(spec: &str) -> Vec<Namespace> {
    spec.split(',').filter(|s| !s.is_empty()).map(ns_of).collect()
}

fn err(e: mem::MemoryError) -> String {
    let (kind, msg) = match e {
        mem::MemoryError::Rejected(m) => ("rejected", m),
        mem::MemoryError::Unavailable(m) => ("unavailable", m),
        mem::MemoryError::Refused(m) => ("refused", m),
    };
    format!("{{\"error\":\"{kind}\",\"detail\":\"{}\"}}", esc(&msg))
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

/// A repeated query parameter, as a list: `tags=a,b,c`.
fn csv_param(query: &str, key: &str) -> Vec<String> {
    param(query, key)
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn entry_from(query: &str, text: String) -> Entry {
    Entry {
        ns: ns_of(&param(query, "ns")),
        key: param(query, "key"),
        text,
        goal: param(query, "goal"),
        env: param(query, "env"),
        attempt: param(query, "attempt"),
        score: signed(query, "score", -1),
        // Comma-separated, so a test can write against the structural half of
        // retrieval the same way it writes against everything else here.
        tags: csv_param(query, "tags"),
    }
}

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let (route, query) = match path.split_once('?') {
            Some((r, q)) => (r.to_string(), q.to_string()),
            None => (path.clone(), String::new()),
        };
        let method = request.method();

        let body = match (&method, route.as_str()) {
            (Method::Post, "/observe") => {
                let e = entry_from(&query, read_body(request));
                match mem::observe(&e) {
                    Ok(h) => format!("{{\"handle\":\"{}\"}}", esc(&h)),
                    Err(e) => err(e),
                }
            }

            (Method::Post, "/promote") => {
                // The namespace is not a parameter: `promote` decides it. Passing
                // one would let this probe claim a promotion into `errors`, which
                // is exactly what the component refuses to allow.
                let e = entry_from(&query, read_body(request));
                match promotion::promote(&e, signed(&query, "score", 0)) {
                    Ok(h) => format!("{{\"handle\":\"{}\"}}", esc(&h)),
                    Err(e) => err(e),
                }
            }

            (Method::Get, "/recall") => {
                let opts = RecallOpts {
                    k: num(&query, "k", 5),
                    budget: num(&query, "budget", 0),
                    pools: pools_of(&param(&query, "pools")),
                    min_similarity: float(&query, "min"),
                    tags: csv_param(&query, "tags"),
                };
                match mem::recall(&param(&query, "goal"), &opts) {
                    Ok(hits) => format!(
                        "{{\"hits\":[{}]}}",
                        hits.iter()
                            .map(|h| format!(
                                "{{\"key\":\"{}\",\"ns\":\"{}\",\"text\":\"{}\",\"similarity\":{:.6},\"dense\":{}}}",
                                esc(&h.key),
                                ns_name(h.ns),
                                esc(&h.text),
                                h.similarity,
                                h.dense
                            ))
                            .collect::<Vec<_>>()
                            .join(",")
                    ),
                    Err(e) => err(e),
                }
            }

            (Method::Post, "/attribute") => {
                let keys: Vec<String> = param(&query, "keys")
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
                match mem::attribute(&keys, &param(&query, "run"), flag(&query, "ok")) {
                    Ok(()) => "{\"ok\":true}".to_string(),
                    Err(e) => err(e),
                }
            }

            (Method::Post, "/evaluated") => match mem::evaluated(
                &param(&query, "goal"),
                &param(&query, "run"),
                signed(&query, "score", 0),
                flag(&query, "passed"),
                &param(&query, "artifact"),
            ) {
                Ok(()) => "{\"ok\":true}".to_string(),
                Err(e) => err(e),
            },

            (Method::Post, "/decay") => {
                match mem::decay(num(&query, "days", 30), num(&query, "min-uses", 2) as u64) {
                    Ok(gone) => format!("{{\"forgotten\":{gone}}}"),
                    Err(e) => err(e),
                }
            }

            (Method::Get, "/already-done") => {
                match mem::already_done(&param(&query, "goal"), float(&query, "min")) {
                    Ok(Some(p)) => format!(
                        "{{\"found\":true,\"goal\":\"{}\",\"similarity\":{:.6},\"score\":{},\"run\":\"{}\",\"artifact\":\"{}\",\"evaluations\":{}}}",
                        esc(&p.goal),
                        p.similarity,
                        p.score,
                        esc(&p.run),
                        esc(&p.artifact),
                        p.evaluations
                    ),
                    // Absence is an answer, and it is the answer this test's
                    // readiness check waits for.
                    Ok(None) => "{\"found\":false}".to_string(),
                    Err(e) => err(e),
                }
            }

            _ => "{\"service\":\"memory-probe\",\"routes\":[\"/observe\",\"/promote\",\"/recall\",\"/attribute\",\"/evaluated\",\"/already-done\"]}"
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
