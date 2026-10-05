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
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../feature-flags/wit",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../event-bus/wit",
            "../../wit/deps/wasi-random-0.2.0",
            "../id-generate/wit",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "wit",
        ],
        world: "rollout:app/flags-domain",
        generate_all,
    });
    /// Stable names for the p3 WASI modules (see `guestio`'s p3 section).
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::clocks0_3_0_rc_2026_03_15 as clocks;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}

use serde_json::{json, Value};

use bindings::event::bus::bus;
use bindings::featureflags::guard::evaluator as flags;
use bindings::id::generate::generator as ids;
use bindings::p3::clocks::{monotonic_clock, system_clock};

use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Fields, Method, Request, Response};

guestio::guest_p3_respond!();

struct Component;

/// event-bus topic every rule change is published on (also the SSE cursor).
const CHANGES: &str = "flags";
const POLL_MS: u64 = 500;
const MAX_TICKS: u32 = 800; // ~7 min connection cap; the browser reconnects.
const COHORT_MAX: u32 = 500;

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let method = request.get_method();
        let path = request.get_path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        match (&method, seg.as_slice()) {
            (Method::Get, ["api", "stream"]) => stream_events(&path),
            _ => {
                let outcome = match (&method, seg.as_slice()) {
                    (Method::Get, [""]) => usage_json(),
                    (Method::Get, ["api", "flags"]) => list_flags(&path),
                    (Method::Post, ["api", "flags", name]) => set_rule(request, name).await,
                    (Method::Delete, ["api", "flags", name]) => clear_rule(&path, name),
                    (Method::Get, ["api", "eval"]) => eval_one(&path),
                    (Method::Get, ["api", "cohort"]) => cohort(&path),
                    _ => Outcome::Err(404, "not_found".into()),
                };
                emit(outcome)
            }
        }
    }
}

enum Outcome {
    Json(u16, String),
    Err(u16, String),
}

fn now() -> u64 {
    system_clock::now().seconds as u64
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

async fn set_rule(request: Request, name: &str) -> Outcome {
    let body = match parse_body(request).await {
        Ok(v) => v,
        Err(o) => return o,
    };
    let tenant = body["tenant"].as_str().unwrap_or("").to_string();
    let rule = match parse_rule(&body["rule"]) {
        Some(r) => r,
        None => {
            return Outcome::Err(422, "rule must be \"on\", \"off\", or a number 0..=100".into())
        }
    };
    match flags::set_rule(name, &tenant, rule) {
        Ok(()) => {
            publish_change(name, &tenant, &rule_label(&rule));
            Outcome::Json(
                200,
                json!({"flag": name, "tenant": tenant, "rule": rule_label(&rule)}).to_string(),
            )
        }
        Err(e) => flag_err(e),
    }
}

fn clear_rule(path: &str, name: &str) -> Outcome {
    let tenant = query_str(path, "tenant").unwrap_or_default();
    match flags::clear_rule(name, &tenant) {
        Ok(()) => {
            publish_change(name, &tenant, &json!("cleared"));
            Outcome::Json(
                200,
                json!({"flag": name, "tenant": tenant, "rule": "cleared"}).to_string(),
            )
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
        Ok(on) => {
            Outcome::Json(200, json!({"flag": flag, "subject": subject, "enabled": on}).to_string())
        }
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
fn stream_events(path: &str) -> Result<Response, ErrorCode> {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"text/event-stream".to_vec()]);
    let _ = headers.set("cache-control", &[b"no-cache".to_vec()]);
    let _ = headers.set("access-control-allow-origin", &[b"*".to_vec()]);

    let mut cursor = query_i64(path, "after").unwrap_or_else(current_seq);

    // The response goes back now; this task keeps writing frames into its body
    // stream while the host streams them to the browser.
    let (mut stream, rx) = bindings::wit_stream::new();
    wit_bindgen::spawn_local(async move {
        // `write_all` hands back what the reader never took: non-empty means the
        // browser went away.
        if !stream.write_all(b": connected\n\n".to_vec()).await.is_empty() {
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
            if !stream.write_all(frame.into_bytes()).await.is_empty() {
                break;
            }
            monotonic_clock::wait_for(POLL_MS * 1_000_000).await;
        }
    });
    let (trailers_tx, trailers_rx) = bindings::wit_future::new(|| Ok(None));
    drop(trailers_tx);
    let (response, _sent) = Response::new(headers, Some(rx), trailers_rx);
    let _ = response.set_status_code(200);
    Ok(response)
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

async fn parse_body(request: Request) -> Result<Value, Outcome> {
    let body =
        read_body(request).await.map_err(|_| Outcome::Err(400, "could not read body".into()))?;
    if body.is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    serde_json::from_slice(&body).map_err(|e| Outcome::Err(400, format!("bad json: {e}")))
}

/// The most a request body may be, before the component stops reading it.
///
/// There was no ceiling anywhere: when this was measured, 148 of the tree's 150
/// components accumulated whatever arrived until the guest hit wasmtime's 64 MiB
/// per-store memory cap and TRAPPED, which reaches the caller as a closed
/// connection saying nothing about a size.
/// A component that answers JSON has no business reading sixteen megabytes, and
/// the ones that legitimately handle uploads police it themselves with a 413 and a
/// granted max-size — those are left alone.
///
/// Generous on purpose. This is a backstop against an unbounded read, not a
/// content policy; an API that needs a real limit should state its own and say 413.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_p3_read_body!(MAX_BODY_BYTES);

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

use guestfmt::percent_decode as decode;

fn emit(result: Outcome) -> Result<Response, ErrorCode> {
    let (status, body) = match result {
        Outcome::Json(code, body) => (code, body),
        Outcome::Err(code, msg) => (code, json!({ "error": msg }).to_string()),
    };
    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
    let _ = headers.set("access-control-allow-origin", &[b"*".to_vec()]);
    respond_with(status, headers, body.into_bytes())
}

bindings::export!(Component with_types_in bindings);
