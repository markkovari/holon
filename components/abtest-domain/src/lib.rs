//! abtest:app — an A/B/n experiment console over composed contracts.
//!
//! Assignment is `experiment:assign` (sticky, weighted, named variants).
//! Attribution is `metrics:collect`: two counters per arm —
//!   `exp:{name}:{tenant}:{arm}:exposed`  and  `…:converted`
//! whose ratio is the conversion rate. Every assignment / exposure / conversion
//! publishes on `event:bus` (`abtest`), and `GET /api/stream` sets its HTTP
//! response early then LOOPS, writing each event as an SSE `data:` frame while
//! the host streams to the browser — same server-push trick as pulse. The
//! console can then show two DIFFERENT subjects landing in different arms live,
//! and the per-arm conversion bars pulling apart as conversions arrive.

#[allow(warnings)]
mod bindings;

use serde_json::{json, Value};

use bindings::event::bus::bus;
use bindings::experiment::assign::assigner as exp;
use bindings::id::generate::generator as ids;
use bindings::metrics::collect::collector as metrics;
use bindings::wasi::clocks::monotonic_clock;
use bindings::wasi::clocks::wall_clock;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

guestio::guest_write_all!();

struct Component;

/// event-bus topic every assignment / outcome is published on (also SSE cursor).
const EVENTS: &str = "abtest";
const POLL_MS: u64 = 500;
const MAX_TICKS: u32 = 800;
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
                    (Method::Post, ["api", "experiments", name]) => set_experiment(&request, name),
                    (Method::Get, ["api", "experiments", name]) => describe(&path, name),
                    (Method::Get, ["api", "assign"]) => assign_one(&path),
                    (Method::Get, ["api", "cohort"]) => cohort(&path),
                    (Method::Post, ["api", "expose"]) => record(&request, "exposed"),
                    (Method::Post, ["api", "convert"]) => record(&request, "converted"),
                    (Method::Get, ["api", "results"]) => results(&path),
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
            "service": "abtest",
            "about": "A/B/n experiment console — sticky weighted assignment + conversion attribution, live over SSE",
            "define": "POST /api/experiments/{name} {tenant, variants:[{name,weight}]}",
            "describe": "GET /api/experiments/{name}?tenant=",
            "assign": "GET /api/assign?exp=&tenant=&subject=",
            "cohort": "GET /api/cohort?exp=&tenant=&n=100",
            "expose": "POST /api/expose {exp, tenant, subject}",
            "convert": "POST /api/convert {exp, tenant, subject}",
            "results": "GET /api/results?exp=&tenant=",
            "stream": "GET /api/stream?exp=&tenant=   (text/event-stream)"
        })
        .to_string(),
    )
}

// ---- metric key scheme -------------------------------------------------------

/// `exp:{name}:{tenant}:{arm}:{kind}` — kind is "exposed" | "converted". Tenant
/// "" is kept literal (empty segment) so the global experiment has its own keys.
fn metric_key(name: &str, tenant: &str, arm: &str, kind: &str) -> String {
    format!("exp:{name}:{tenant}:{arm}:{kind}")
}

// ---- experiment definition ---------------------------------------------------

fn set_experiment(request: &IncomingRequest, name: &str) -> Outcome {
    let body = match parse_body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let tenant = body["tenant"].as_str().unwrap_or("").to_string();
    let variants: Vec<exp::Arm> = match body["variants"].as_array() {
        Some(arr) => arr
            .iter()
            .filter_map(|v| {
                let n = v["name"].as_str()?.to_string();
                let w = v["weight"].as_u64().unwrap_or(0) as u32;
                Some(exp::Arm { name: n, weight: w })
            })
            .collect(),
        None => return Outcome::Err(422, "variants array required".into()),
    };
    if variants.is_empty() {
        return Outcome::Err(422, "at least one variant required".into());
    }
    match exp::set_experiment(name, &tenant, &variants) {
        Ok(()) => {
            publish("reweight", name, &tenant, "", "");
            let arms: Vec<Value> =
                variants.iter().map(|v| json!({"name": v.name, "weight": v.weight})).collect();
            Outcome::Json(
                200,
                json!({"experiment": name, "tenant": tenant, "variants": arms}).to_string(),
            )
        }
        Err(e) => assign_err(e),
    }
}

fn describe(path: &str, name: &str) -> Outcome {
    let tenant = query_str(path, "tenant").unwrap_or_default();
    match exp::describe(name, &tenant) {
        Ok(vs) => {
            let arms: Vec<Value> =
                vs.iter().map(|v| json!({"name": v.name, "weight": v.weight})).collect();
            Outcome::Json(200, json!({"experiment": name, "variants": arms}).to_string())
        }
        Err(e) => assign_err(e),
    }
}

// ---- assignment --------------------------------------------------------------

fn assign_one(path: &str) -> Outcome {
    let name = query_str(path, "exp").unwrap_or_default();
    let tenant = query_str(path, "tenant").unwrap_or_default();
    let subject = query_str(path, "subject").unwrap_or_default();
    if name.is_empty() || subject.is_empty() {
        return Outcome::Err(422, "exp and subject required".into());
    }
    match exp::assign(&name, &ctx(&tenant, &subject)) {
        Ok(arm) => {
            publish("assign", &name, &tenant, &subject, &arm);
            Outcome::Json(200, json!({"exp": name, "subject": subject, "arm": arm}).to_string())
        }
        Err(e) => assign_err(e),
    }
}

fn ctx(tenant: &str, subject: &str) -> exp::Context {
    exp::Context { tenant: tenant.to_string(), subject: subject.to_string() }
}

fn cohort(path: &str) -> Outcome {
    let name = query_str(path, "exp").unwrap_or_default();
    let tenant = query_str(path, "tenant").unwrap_or_default();
    let n = query_i64(path, "n").unwrap_or(100).clamp(1, COHORT_MAX as i64) as u32;
    if name.is_empty() {
        return Outcome::Err(422, "exp required".into());
    }
    match exp::cohort(&name, &tenant, n) {
        Ok(cells) => {
            let rows: Vec<Value> =
                cells.iter().map(|c| json!({"subject": c.subject, "arm": c.arm})).collect();
            Outcome::Json(200, json!({ "exp": name, "n": n, "cells": rows }).to_string())
        }
        Err(e) => assign_err(e),
    }
}

// ---- attribution -------------------------------------------------------------

/// Record an exposure ("exposed") or conversion ("converted") for a subject.
/// Assigns the subject first (sticky) so the count lands on the right arm.
fn record(request: &IncomingRequest, kind: &str) -> Outcome {
    let body = match parse_body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let name = body["exp"].as_str().unwrap_or("").to_string();
    let tenant = body["tenant"].as_str().unwrap_or("").to_string();
    let subject = body["subject"].as_str().unwrap_or("").to_string();
    if name.is_empty() || subject.is_empty() {
        return Outcome::Err(422, "exp and subject required".into());
    }
    let arm = match exp::assign(&name, &ctx(&tenant, &subject)) {
        Ok(a) => a,
        Err(e) => return assign_err(e),
    };
    match metrics::incr(&metric_key(&name, &tenant, &arm, kind), 1) {
        Ok(v) => {
            publish(kind, &name, &tenant, &subject, &arm);
            Outcome::Json(
                200,
                json!({"exp": name, "subject": subject, "arm": arm, "kind": kind, "count": v})
                    .to_string(),
            )
        }
        Err(e) => metrics_err(e),
    }
}

/// Per-variant exposed / converted / rate for an experiment.
fn results(path: &str) -> Outcome {
    let name = query_str(path, "exp").unwrap_or_default();
    let tenant = query_str(path, "tenant").unwrap_or_default();
    if name.is_empty() {
        return Outcome::Err(422, "exp required".into());
    }
    let variants = match exp::describe(&name, &tenant) {
        Ok(vs) => vs,
        Err(e) => return assign_err(e),
    };
    let mut arms = Vec::with_capacity(variants.len());
    for v in &variants {
        let exposed = metrics::get(&metric_key(&name, &tenant, &v.name, "exposed")).unwrap_or(0);
        let converted =
            metrics::get(&metric_key(&name, &tenant, &v.name, "converted")).unwrap_or(0);
        let rate = if exposed == 0 { 0.0 } else { converted as f64 / exposed as f64 };
        arms.push(json!({
            "name": v.name, "weight": v.weight,
            "exposed": exposed, "converted": converted, "rate": rate,
        }));
    }
    Outcome::Json(200, json!({ "exp": name, "arms": arms }).to_string())
}

// ---- events + SSE ------------------------------------------------------------

fn publish(kind: &str, exp: &str, tenant: &str, subject: &str, arm: &str) {
    let frame = json!({
        "xid": ids::short_code(8),
        "kind": kind,     // assign | exposed | converted | reweight
        "exp": exp, "tenant": tenant, "subject": subject, "arm": arm,
        "at": now(),
    });
    let _ = bus::publish(EVENTS, frame.to_string().as_bytes());
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
            let (rows, new_cursor) = events_after(cursor);
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

fn events_after(after: i64) -> (Vec<Value>, i64) {
    let events = bus::poll(EVENTS, "snapshot", 4096).unwrap_or_default();
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
    bus::poll(EVENTS, "snapshot", 4096)
        .unwrap_or_default()
        .iter()
        .filter_map(|e| e.id.parse::<i64>().ok())
        .max()
        .unwrap_or(-1)
}

// ---- http plumbing -----------------------------------------------------------

fn assign_err(e: exp::AssignError) -> Outcome {
    match e {
        exp::AssignError::NotFound => Outcome::Err(404, "experiment not found".into()),
        exp::AssignError::InvalidVariants(m) => Outcome::Err(422, m),
        exp::AssignError::BackendUnavailable(m) => Outcome::Err(503, m),
    }
}

fn metrics_err(e: metrics::MetricsError) -> Outcome {
    match e {
        metrics::MetricsError::BackendUnavailable(m) => Outcome::Err(503, m),
    }
}

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

guestio::guest_read_body!(MAX_BODY_BYTES);

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

use guestfmt::percent_decode as decode;

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
