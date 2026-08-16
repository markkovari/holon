//! flags:app — a live feature-rollout console over composed contracts.
//!
//! Rules live entirely in `featureflags:guard` (runtime rules in its kv store,
//! config-defined flags from wasi:config). The domain stores nothing durable of
//! its own — it evaluates, mutates rules, and publishes each change on
//! `event:bus` (`flags`). `GET /api/cohort` evaluates a flag across N synthetic
//! subjects (`subject-0 … subject-{n-1}`) — the on/off grid the console renders;
//! because the contract buckets percentage rollouts on a stable hash, the same
//! subjects stay on across evaluations (STICKY cohorts, the visible payoff).
//! `GET /api/stream` sets its HTTP response early then LOOPS, writing each rule
//! change as an SSE `data:` frame while the host streams to the browser — the
//! same server-push trick as pulse, carrying live config propagation.

#[allow(warnings)]
mod bindings;

use serde_json::{json, Value};

use bindings::event::bus::bus;
use bindings::featureflags::guard::evaluator as flags;
use bindings::id::generate::generator as ids;
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

/// event-bus topic every rule change is published on (also the SSE cursor).
const CHANGES: &str = "flags";
const POLL_MS: u64 = 500;
const MAX_TICKS: u32 = 800; // ~7 min connection cap; the browser reconnects.
const COHORT_MAX: u32 = 500;

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
                    (Method::Get, ["api", "flags"]) => list_flags(&path),
                    (Method::Post, ["api", "flags", name]) => set_rule(&request, name),
                    (Method::Delete, ["api", "flags", name]) => clear_rule(&path, name),
                    (Method::Get, ["api", "eval"]) => eval_one(&path),
                    (Method::Get, ["api", "cohort"]) => cohort(&path),
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
            "service": "flags",
            "about": "live feature-rollout console — set a rule, watch it propagate to every open window over SSE; percentage cohorts are sticky",
            "list": "GET /api/flags?tenant=",
            "set": "POST /api/flags/{name} {tenant, rule}   rule: \"on\"|\"off\"|N (percent)",
            "clear": "DELETE /api/flags/{name}?tenant=",
            "eval": "GET /api/eval?flag=&tenant=&subject=",
            "cohort": "GET /api/cohort?flag=&tenant=&n=100",
            "stream": "GET /api/stream?tenant=   (text/event-stream)"
        })
        .to_string(),
    )
}

// ---- rule read/write ---------------------------------------------------------

fn ctx(tenant: &str, subject: &str) -> flags::Context {
    flags::Context { tenant: tenant.to_string(), subject: subject.to_string() }
}

fn rule_label(r: &flags::Rule) -> Value {
    match r {
        flags::Rule::Enabled => json!("on"),
        flags::Rule::Disabled => json!("off"),
        flags::Rule::Percentage(p) => json!(*p),
    }
}

fn source_label(s: &flags::Source) -> &'static str {
    match s {
        flags::Source::Config => "config",
        flags::Source::GlobalOverride => "global-override",
        flags::Source::TenantOverride => "tenant-override",
    }
}

fn list_flags(path: &str) -> Outcome {
    let tenant = query_str(path, "tenant").unwrap_or_default();
    match flags::list_flags(&tenant) {
        Ok(states) => {
            let rows: Vec<Value> = states
                .iter()
                .map(|s| json!({"name": s.name, "rule": rule_label(&s.rule), "source": source_label(&s.source)}))
                .collect();
            Outcome::Json(200, json!({ "flags": rows }).to_string())
        }
        Err(e) => flag_err(e),
    }
}

/// Parse a rule from the request body's `rule` field: `"on"`, `"off"`, or a
/// number 0..=100 (percentage rollout).
fn parse_rule(v: &Value) -> Option<flags::Rule> {
    if let Some(s) = v.as_str() {
        return match s.trim().to_ascii_lowercase().as_str() {
            "on" | "true" | "enabled" => Some(flags::Rule::Enabled),
            "off" | "false" | "disabled" => Some(flags::Rule::Disabled),
            n => n.trim_end_matches('%').parse::<u8>().ok().map(flags::Rule::Percentage),
        };
    }
    v.as_u64().map(|n| flags::Rule::Percentage(n.min(100) as u8))
}

fn set_rule(request: &IncomingRequest, name: &str) -> Outcome {
    let body = match parse_body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let tenant = body["tenant"].as_str().unwrap_or("").to_string();
    let rule = match parse_rule(&body["rule"]) {
        Some(r) => r,
        None => return Outcome::Err(422, "rule must be \"on\", \"off\", or a number 0..=100".into()),
    };
    match flags::set_rule(name, &tenant, rule) {
        Ok(()) => {
            publish_change(name, &tenant, &rule_label(&rule));
            Outcome::Json(200, json!({"flag": name, "tenant": tenant, "rule": rule_label(&rule)}).to_string())
        }
        Err(e) => flag_err(e),
    }
}

fn clear_rule(path: &str, name: &str) -> Outcome {
    let tenant = query_str(path, "tenant").unwrap_or_default();
    match flags::clear_rule(name, &tenant) {
        Ok(()) => {
            publish_change(name, &tenant, &json!("cleared"));
            Outcome::Json(200, json!({"flag": name, "tenant": tenant, "rule": "cleared"}).to_string())
        }
        Err(e) => flag_err(e),
    }
}

fn publish_change(flag: &str, tenant: &str, rule: &Value) {
    let frame = json!({
        "xid": ids::short_code(8),
        "flag": flag,
        "tenant": tenant,
        "rule": rule,
        "at": now(),
    });
    let _ = bus::publish(CHANGES, frame.to_string().as_bytes());
}

// ---- evaluation --------------------------------------------------------------

fn eval_one(path: &str) -> Outcome {
    let flag = query_str(path, "flag").unwrap_or_default();
    let tenant = query_str(path, "tenant").unwrap_or_default();
    let subject = query_str(path, "subject").unwrap_or_default();
    if flag.is_empty() {
        return Outcome::Err(422, "flag required".into());
    }
    match flags::is_enabled(&flag, &ctx(&tenant, &subject)) {
        Ok(on) => Outcome::Json(200, json!({"flag": flag, "subject": subject, "enabled": on}).to_string()),
        Err(e) => flag_err(e),
    }
}

/// Evaluate a flag across N synthetic subjects (`subject-0 …`) — the console
/// grid. The on/off pattern is a property of the contract's stable hash, so it
/// stays sticky as the percentage moves.
fn cohort(path: &str) -> Outcome {
    let flag = query_str(path, "flag").unwrap_or_default();
    let tenant = query_str(path, "tenant").unwrap_or_default();
    let n = query_i64(path, "n").unwrap_or(100).clamp(1, COHORT_MAX as i64) as u32;
    if flag.is_empty() {
        return Outcome::Err(422, "flag required".into());
    }
    let mut on = 0u32;
    let mut cells = Vec::with_capacity(n as usize);
    for i in 0..n {
        let subject = format!("subject-{i}");
        let enabled = flags::is_enabled(&flag, &ctx(&tenant, &subject)).unwrap_or(false);
        if enabled {
            on += 1;
        }
        cells.push(json!({"subject": subject, "enabled": enabled}));
    }
    Outcome::Json(200, json!({ "flag": flag, "n": n, "on": on, "cells": cells }).to_string())
}

// ---- the SSE stream ----------------------------------------------------------

/// Hold the connection open and push each rule change as an SSE `data:` frame.
/// A browser re-fetches `/api/cohort` on each frame to repaint the grid live.
fn stream_events(response_out: ResponseOutparam, path: &str) {
    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[b"text/event-stream".to_vec()]);
    let _ = headers.set(&"cache-control".to_string(), &[b"no-cache".to_vec()]);
    let _ = headers.set(&"access-control-allow-origin".to_string(), &[b"*".to_vec()]);
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
            let (rows, new_cursor) = changes_after(cursor);
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

/// Change events on the bus with id (seq) > after, oldest-first, plus cursor.
fn changes_after(after: i64) -> (Vec<Value>, i64) {
    let events = bus::poll(CHANGES, "snapshot", 4096).unwrap_or_default();
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
    bus::poll(CHANGES, "snapshot", 4096)
        .unwrap_or_default()
        .iter()
        .filter_map(|e| e.id.parse::<i64>().ok())
        .max()
        .unwrap_or(-1)
}

// ---- http plumbing -----------------------------------------------------------

fn flag_err(e: flags::FlagError) -> Outcome {
    match e {
        flags::FlagError::BackendUnavailable(m) => Outcome::Err(503, m),
    }
}

fn parse_body(request: &IncomingRequest) -> Result<Value, Outcome> {
    let body = read_body(request).map_err(|_| Outcome::Err(400, "could not read body".into()))?;
    if body.is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    serde_json::from_slice(&body).map_err(|e| Outcome::Err(400, format!("bad json: {e}")))
}

fn read_body(request: &IncomingRequest) -> Result<Vec<u8>, ()> {
    let body = request.consume().map_err(|_| ())?;
    let stream = body.stream().map_err(|_| ())?;
    let mut buf = Vec::new();
    loop {
        match stream.blocking_read(8192) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => buf.extend_from_slice(&chunk),
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

/// Read query param `key` as a string.
fn query_str(path: &str, key: &str) -> Option<String> {
    let query = path.split('?').nth(1)?;
    query.split('&').find_map(|pair| {
        let mut it = pair.splitn(2, '=');
        (it.next()? == key).then(|| decode(it.next().unwrap_or("")))
    })
}

/// Read query param `key` as an i64.
fn query_i64(path: &str, key: &str) -> Option<i64> {
    query_str(path, key)?.parse().ok()
}

/// Minimal percent-decode (enough for tenant/subject/flag names in the query).
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
    let _ = headers.set(&"content-type".to_string(), &[b"application/json".to_vec()]);
    let _ = headers.set(&"access-control-allow-origin".to_string(), &[b"*".to_vec()]);
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
