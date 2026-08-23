//! throttle:app — a live throttle wall over composed contracts.
//!
//! Each `hit` runs the request through two limiters: `ratelimit:guard` (a
//! fixed-window attempt counter that LOCKS a key after the ceiling) and
//! `quota:meter` (a cumulative budget with a reset period). The verdict —
//! allow / throttled(locked) / quota-exceeded — is published on `event:bus`,
//! and `GET /api/stream` sets its HTTP response early then LOOPS, writing each
//! verdict as an SSE `data:` frame while the host streams to the browser (same
//! server-push trick as pulse). No durable state of our own: the counters live
//! in the two limiter contracts.

#[allow(warnings)]
mod bindings;

use serde_json::{json, Value};

use bindings::event::bus::bus;
use bindings::id::generate::generator as ids;
use bindings::quota::meter::meter as quota;
use bindings::ratelimit::guard::limiter;
use bindings::wasi::clocks::monotonic_clock;
use bindings::wasi::clocks::wall_clock;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

/// Write a whole body, however long it is.
///
/// `blocking-write-and-flush` accepts at most 4096 bytes and TRAPS above that
/// rather than returning an error: the component dies mid-response and the caller
/// sees `connection closed before message completed`, three layers from the cause.
/// This bit a real run — a 4573-byte contract — and cost four failed starts to
/// find, so it is written the same way everywhere now.
///
/// Not a flat 4096-byte loop: `check-write` is the stream saying how much it will
/// take right now, usually far more, so this writes in whatever bites it offers,
/// waits on the pollable when it offers none, and flushes ONCE at the end.
///
/// Returns false when the stream is gone. For an SSE loop that means the client
/// hung up, which is ordinary and not an error.
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

const VERDICTS: &str = "throttle";
const POLL_MS: u64 = 400;
const MAX_TICKS: u32 = 800;
const DEFAULT_KEY: &str = "demo";
// quota budget: `QUOTA_LIMIT` requests per `QUOTA_PERIOD` seconds.
const QUOTA_LIMIT: u64 = 20;
const QUOTA_PERIOD: u64 = 30;

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        match (&method, seg.as_slice()) {
            (Method::Get, ["api", "stream"]) => stream_events(response_out, &path),
            _ => {
                let outcome = match (&method, seg.as_slice()) {
                    (Method::Get, [""]) => usage_json(),
                    (Method::Post, ["api", "hit"]) => hit(&request),
                    (Method::Post, ["api", "burst"]) => burst(&request),
                    (Method::Post, ["api", "fail"]) => fail(&request),
                    (Method::Post, ["api", "reset"]) => reset(&request),
                    (Method::Get, ["api", "state"]) => state(&path),
                    _ => Outcome::Err(404, "not_found".into()),
                };
                emit(response_out, outcome);
            }
        }
    }
}

enum Outcome {
    Json(u16, String),
    Err(u16, String),
}

fn now() -> u64 {
    wall_clock::now().seconds
}

fn usage_json() -> Outcome {
    Outcome::Json(
        200,
        json!({
            "service": "throttle",
            "about": "a live throttle wall — fixed-window lockout + cumulative quota, streamed over SSE",
            "hit": "POST /api/hit {key?, subject?}",
            "burst": "POST /api/burst {key?, n?}",
            "fail": "POST /api/fail {key?}",
            "reset": "POST /api/reset {key?}",
            "state": "GET /api/state?key=&subject=",
            "stream": "GET /api/stream?key=   (text/event-stream)"
        })
        .to_string(),
    )
}

// ---- the decision gate -------------------------------------------------------

/// One request through the wall. Order: ratelimit lockout first (cheap, and the
/// thing you throttle before spending budget), then the cumulative quota.
fn decide(key: &str, subject: &str) -> (u16, Value) {
    // 1. windowed lockout: check() returns attempts-left, or err(locked(after)).
    match limiter::check(key) {
        Ok(left) => {
            // consuming an attempt: record it so the window counts this request.
            let _ = limiter::record_failure(key);
            // 2. cumulative budget.
            match quota::record_usage(subject, 1, QUOTA_LIMIT, QUOTA_PERIOD) {
                Ok(bal) => {
                    let v = json!({
                        "verdict": "allow", "key": key, "attempts_left": left.saturating_sub(1),
                        "quota_used": bal.used, "quota_remaining": bal.remaining, "resets_at": bal.resets_at,
                    });
                    publish("allow", key, &v);
                    (200, v)
                }
                Err(quota::QuotaError::Exceeded(after)) => {
                    let v = json!({"verdict": "quota", "key": key, "retry_after": after, "quota_remaining": 0});
                    publish("quota", key, &v);
                    (429, v)
                }
                Err(quota::QuotaError::BackendUnavailable(m)) => (503, json!({"error": m})),
            }
        }
        Err(limiter::LimitError::Locked(after)) => {
            let v = json!({"verdict": "locked", "key": key, "retry_after": after, "attempts_left": 0});
            publish("locked", key, &v);
            (429, v)
        }
        Err(limiter::LimitError::BackendUnavailable(m)) => (503, json!({"error": m})),
    }
}

fn hit(request: &IncomingRequest) -> Outcome {
    let body = parse_body(request).unwrap_or(Value::Null);
    let key = body["key"].as_str().unwrap_or(DEFAULT_KEY).to_string();
    let subject = body["subject"].as_str().unwrap_or(&key).to_string();
    let (code, v) = decide(&key, &subject);
    Outcome::Json(code, v.to_string())
}

fn burst(request: &IncomingRequest) -> Outcome {
    let body = parse_body(request).unwrap_or(Value::Null);
    let key = body["key"].as_str().unwrap_or(DEFAULT_KEY).to_string();
    let n = body["n"].as_u64().unwrap_or(10).min(200);
    let mut allowed = 0u64;
    let mut throttled = 0u64;
    for _ in 0..n {
        let (code, _) = decide(&key, &key);
        if code == 200 {
            allowed += 1;
        } else {
            throttled += 1;
        }
    }
    Outcome::Json(200, json!({"key": key, "fired": n, "allowed": allowed, "throttled": throttled}).to_string())
}

/// Record a failure directly (drives lockout without consuming quota).
fn fail(request: &IncomingRequest) -> Outcome {
    let body = parse_body(request).unwrap_or(Value::Null);
    let key = body["key"].as_str().unwrap_or(DEFAULT_KEY).to_string();
    match limiter::record_failure(&key) {
        Ok(()) => {
            let v = json!({"verdict": "strike", "key": key});
            publish("strike", &key, &v);
            Outcome::Json(200, v.to_string())
        }
        Err(limiter::LimitError::Locked(after)) => {
            let v = json!({"verdict": "locked", "key": key, "retry_after": after});
            publish("locked", &key, &v);
            Outcome::Json(429, v.to_string())
        }
        Err(limiter::LimitError::BackendUnavailable(m)) => Outcome::Err(503, m),
    }
}

fn reset(request: &IncomingRequest) -> Outcome {
    let body = parse_body(request).unwrap_or(Value::Null);
    let key = body["key"].as_str().unwrap_or(DEFAULT_KEY).to_string();
    let _ = limiter::reset(&key);
    let _ = quota::reset(&key);
    publish("reset", &key, &json!({"verdict": "reset", "key": key}));
    Outcome::Json(200, json!({"reset": key}).to_string())
}

fn state(path: &str) -> Outcome {
    let key = query_str(path, "key").unwrap_or_else(|| DEFAULT_KEY.into());
    let subject = query_str(path, "subject").unwrap_or_else(|| key.clone());
    let (locked, attempts_left, retry_after) = match limiter::check(&key) {
        Ok(left) => (false, left, 0),
        Err(limiter::LimitError::Locked(after)) => (true, 0, after),
        Err(limiter::LimitError::BackendUnavailable(m)) => return Outcome::Err(503, m),
    };
    let bal = quota::peek(&subject, QUOTA_LIMIT, QUOTA_PERIOD).ok();
    Outcome::Json(
        200,
        json!({
            "key": key, "locked": locked, "attempts_left": attempts_left, "retry_after": retry_after,
            "quota_used": bal.as_ref().map(|b| b.used).unwrap_or(0),
            "quota_remaining": bal.as_ref().map(|b| b.remaining).unwrap_or(QUOTA_LIMIT),
            "quota_limit": QUOTA_LIMIT, "resets_at": bal.as_ref().map(|b| b.resets_at).unwrap_or(0),
        })
        .to_string(),
    )
}

// ---- events + SSE ------------------------------------------------------------

fn publish(verdict: &str, key: &str, detail: &Value) {
    let mut frame = detail.clone();
    frame["xid"] = json!(ids::short_code(8));
    frame["verdict"] = json!(verdict);
    frame["key"] = json!(key);
    frame["at"] = json!(now());
    let _ = bus::publish(VERDICTS, frame.to_string().as_bytes());
}

fn stream_events(response_out: ResponseOutparam, path: &str) {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"text/event-stream".to_vec()]);
    let _ = headers.set("cache-control", &[b"no-cache".to_vec()]);
    let _ = headers.set("access-control-allow-origin", &[b"*".to_vec()]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(200);
    let body = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));

    let mut cursor = query_i64(path, "after").unwrap_or_else(current_seq);

    {
        let stream = body.write().expect("write stream");
        if !write_all(&stream, b": connected\n\n") {
            return;
        }
        for _ in 0..MAX_TICKS {
            let (rows, new_cursor) = verdicts_after(cursor);
            cursor = new_cursor;
            let frame = if rows.is_empty() {
                ": ping\n\n".to_string()
            } else {
                rows.iter().map(|r| format!("data: {r}\n\n")).collect::<String>()
            };
            if !write_all(&stream, frame.as_bytes()) {
                break;
            }
            monotonic_clock::subscribe_duration(POLL_MS * 1_000_000).block();
        }
    }
    let _ = OutgoingBody::finish(body, None);
}

fn verdicts_after(after: i64) -> (Vec<Value>, i64) {
    let events = bus::poll(VERDICTS, "snapshot", 4096).unwrap_or_default();
    let mut rows: Vec<(i64, Value)> = events
        .iter()
        .filter_map(|e| {
            let seq: i64 = e.id.parse().ok()?;
            let mut v: Value = serde_json::from_slice(&e.payload).ok()?;
            (seq > after).then(|| {
                v["seq"] = json!(seq);
                (seq, v)
            })
        })
        .collect();
    rows.sort_by_key(|(seq, _)| *seq);
    let cursor = rows.last().map(|(seq, _)| *seq).unwrap_or(after);
    (rows.into_iter().map(|(_, v)| v).collect(), cursor)
}

fn current_seq() -> i64 {
    bus::poll(VERDICTS, "snapshot", 4096)
        .unwrap_or_default()
        .iter()
        .filter_map(|e| e.id.parse::<i64>().ok())
        .max()
        .unwrap_or(-1)
}

// ---- http plumbing -----------------------------------------------------------

fn parse_body(request: &IncomingRequest) -> Result<Value, Outcome> {
    let body = read_body(request).map_err(|_| Outcome::Err(400, "could not read body".into()))?;
    if body.is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    serde_json::from_slice(&body).map_err(|e| Outcome::Err(400, format!("bad json: {e}")))
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

fn read_body(request: &IncomingRequest) -> Result<Vec<u8>, ()> {
    let body = request.consume().map_err(|_| ())?;
    let stream = body.stream().map_err(|_| ())?;
    let mut buf = Vec::new();
    loop {
        match stream.blocking_read(8192) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => {
                // A ceiling, not a policy: past this the read stops and the caller
                // is told, rather than growing until the store's memory cap traps
                // the component and the connection just closes.
                if buf.len() + chunk.len() > MAX_BODY_BYTES {
                    return Err(());
                }
                buf.extend_from_slice(&chunk);
            }
            // `Closed` is how wasi:io says end-of-body; `LastOperationFailed` is a
            // read that went wrong. Collapsing both into `break` returns a TRUNCATED
            // body as if it were complete — the same silent truncation that, on the
            // write side, took four runs to find.
            Err(bindings::wasi::io::streams::StreamError::Closed) => break,
            Err(_) => return Err(()),
        }
    }
    Ok(buf)
}

fn query_str(path: &str, key: &str) -> Option<String> {
    let query = path.split('?').nth(1)?;
    query.split('&').find_map(|pair| {
        let mut it = pair.splitn(2, '=');
        (it.next()? == key).then(|| decode(it.next().unwrap_or("")))
    })
}

fn query_i64(path: &str, key: &str) -> Option<i64> {
    query_str(path, key)?.parse().ok()
}

fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(b as char);
                    i += 3;
                    continue;
                }
                out.push('%');
                i += 1;
            }
            b'+' => {
                out.push(' ');
                i += 1;
            }
            c => {
                out.push(c as char);
                i += 1;
            }
        }
    }
    out
}

fn emit(response_out: ResponseOutparam, result: Outcome) {
    match result {
        Outcome::Json(code, body) => respond(response_out, code, body.as_bytes()),
        Outcome::Err(code, msg) => {
            respond(response_out, code, json!({ "error": msg }).to_string().as_bytes())
        }
    }
}

fn respond(response_out: ResponseOutparam, status: u16, body: &[u8]) {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
    let _ = headers.set("access-control-allow-origin", &[b"*".to_vec()]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(status);
    let out = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    if !body.is_empty() {
        let stream = out.write().expect("write stream");
        for chunk in body.chunks(4096) {
            let _ = write_all(&stream, chunk);
        }
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);
