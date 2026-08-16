//! jobs:app — a durable background-job queue over composed contracts.
//!
//! The queue substrate is `outbox:dispatch` (durable enqueue-with-delay, claim
//! under a crash-safe lease, fail-with-backoff, dead-letter, replay). Each drain
//! tick claims a batch and runs every job through the `durable:workflow`
//! execution seam — the in-process backend by default, the golem-workflow
//! provider when you want crash-resumable durable execution. On success a job is
//! acked (and, if recurring, rescheduled at the next `cron:expr` fire time); on
//! failure the outbox reschedules with backoff and eventually dead-letters.
//! `idempotency:guard` makes enqueue exactly-once, and a mirrored `records:store`
//! row per job powers the live SSE board. The queue owns durability of *when* and
//! *whether-retried*; the workflow backend owns durability of *during*.

#[allow(warnings)]
mod bindings;

use serde_json::{json, Value};

use bindings::cron::expr::parser as cron;
use bindings::durable::workflow::orchestrator as workflow;
use bindings::idempotency::guard::store as idem;
use bindings::outbox::dispatch::queue as outbox;
use bindings::records::store::store as records;
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

// The board SPA, embedded so the component serves it on any host (the native
// host also has it via --static-dir; on the v2 operator there is no static-dir,
// so GET / must serve it here). Single source — the example's index.html.
const BOARD: &str = include_str!("../../../examples/jobs/public/index.html");

const JOBS: &str = "jobs";
const BATCH: u32 = 10;
const LEASE: u64 = 30;
const IDEM_TTL: u64 = 3600;
const POLL_MS: u64 = 800;
const MAX_TICKS: u32 = 700;

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        match (&method, seg.as_slice()) {
            (Method::Get, ["api", "events"]) => stream_events(response_out, &path),
            _ => {
                let outcome = match (&method, seg.as_slice()) {
                    (Method::Get, [""]) => Outcome::Html(200, BOARD.to_string()),
                    (Method::Get, ["api"]) => usage_json(),
                    (Method::Post, ["api", "jobs"]) => enqueue(&request),
                    (Method::Get, ["api", "jobs"]) => Outcome::Json(200, board_json().to_string()),
                    (Method::Post, ["api", "tick"]) => Outcome::Json(200, drain_once().to_string()),
                    (Method::Post, ["api", "jobs", oid, "replay"]) => replay(oid),
                    _ => Outcome::Err(404, "not_found".into()),
                };
                emit(response_out, outcome);
            }
        }
    }
}

enum Outcome {
    Json(u16, String),
    Html(u16, String),
    Err(u16, String),
}

fn now() -> u64 {
    wall_clock::now().seconds
}

fn usage_json() -> Outcome {
    Outcome::Json(
        200,
        json!({
            "service": "jobs",
            "about": "durable background-job queue — schedule, retry with backoff, dead-letter, replay; jobs run through a swappable durable:workflow backend",
            "enqueue": "POST /api/jobs {type, payload?, delay?, cron?, key?}",
            "board": "GET /api/jobs",
            "tick": "POST /api/tick   (drain one batch)",
            "replay": "POST /api/jobs/{id}/replay",
            "stream": "GET /api/events   (text/event-stream, self-ticking board)"
        })
        .to_string(),
    )
}

// ---- job records (the dashboard mirror, keyed by outbox id) ------------------

fn touch(d: &mut Value) {
    d["updated"] = json!(now());
}

/// The job record for an outbox id, as (record-id, data), if present.
fn job_by_oid(oid: &str) -> Option<(String, Value)> {
    records::find_by(JOBS, "oid", &json!(oid).to_string())
        .ok()?
        .into_iter()
        .next()
        .and_then(|e| serde_json::from_str::<Value>(&e.data).ok().map(|d| (e.id, d)))
}

fn save(rid: &str, d: &Value) {
    let _ = records::update(JOBS, rid, &d.to_string(), 0);
}

/// The client-facing view of a job (the outbox id is its stable handle).
fn job_view(d: &Value) -> Value {
    json!({
        "id": d["oid"],
        "type": d["type"],
        "payload": d["payload"],
        "state": d["state"],
        "attempts": d["attempts"],
        "cron": d["cron"],
        "result": d["result"],
        "error": d["error"],
        "created": d["created"],
        "updated": d["updated"],
    })
}

fn board_json() -> Value {
    let entries = records::list_records(JOBS, 500, "").map(|p| p.entries).unwrap_or_default();
    let mut jobs: Vec<Value> = entries
        .iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
        .collect();
    // newest first
    jobs.sort_by(|a, b| b["created"].as_u64().unwrap_or(0).cmp(&a["created"].as_u64().unwrap_or(0)));
    let mut counts = json!({"queued":0,"running":0,"done":0,"dead":0});
    for j in &jobs {
        let s = j["state"].as_str().unwrap_or("");
        if let Some(n) = counts.get_mut(s).and_then(|v| v.as_u64()) {
            counts[s] = json!(n + 1);
        }
    }
    json!({ "jobs": jobs.iter().map(job_view).collect::<Vec<_>>(), "counts": counts })
}

// ---- enqueue ----------------------------------------------------------------

fn enqueue(request: &IncomingRequest) -> Outcome {
    let body = match parse_body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let job_type = body["type"].as_str().unwrap_or("").trim().to_string();
    if job_type.is_empty() {
        return Outcome::Err(422, "type required".into());
    }
    // payload: accept a JSON object/value or a string; store as a JSON string.
    let payload = match body.get("payload") {
        Some(Value::String(s)) => s.clone(),
        Some(v) if !v.is_null() => v.to_string(),
        _ => "{}".to_string(),
    };
    let cron_expr = body["cron"].as_str().unwrap_or("").trim().to_string();
    let key = body["key"].as_str().map(|s| s.to_string());

    // exactly-once: a repeated key replays the first response instead of
    // enqueuing again.
    if let Some(k) = &key {
        match idem::begin(k, IDEM_TTL) {
            Ok(Some(cached)) => {
                return Outcome::Json(cached.status, String::from_utf8_lossy(&cached.body).into_owned())
            }
            Ok(None) => {}
            Err(idem::IdemError::InProgress) => return Outcome::Err(409, "duplicate in progress".into()),
            Err(idem::IdemError::BackendUnavailable(m)) => return Outcome::Err(503, m),
        }
    }

    // recurring or delayed → compute the first delay.
    let delay = if !cron_expr.is_empty() {
        if cron::parse(&cron_expr).is_err() {
            if let Some(k) = &key {
                let _ = idem::forget(k);
            }
            return Outcome::Err(422, "invalid cron expression".into());
        }
        cron_delay(&cron_expr)
    } else {
        body["delay"].as_u64().unwrap_or(0)
    };

    let oid = match outbox::enqueue(&job_type, payload.as_bytes(), delay) {
        Ok(id) => id,
        Err(e) => {
            if let Some(k) = &key {
                let _ = idem::forget(k);
            }
            return outbox_err(e);
        }
    };
    let d = json!({
        "oid": oid, "type": job_type, "payload": payload, "state": "queued",
        "attempts": 0, "cron": cron_expr, "result": "", "error": "",
        "created": now(), "updated": now(),
    });
    if let Err(e) = records::create(JOBS, &d.to_string(), &["oid".to_string(), "state".to_string()]) {
        return store_err(e);
    }
    let resp = json!({ "job": job_view(&d) }).to_string();
    if let Some(k) = &key {
        let _ = idem::complete(k, 201, resp.as_bytes());
    }
    Outcome::Json(201, resp)
}

/// Seconds until the next cron fire (0 if none within horizon).
fn cron_delay(expr: &str) -> u64 {
    cron::next(expr, now(), 1)
        .ok()
        .and_then(|v| v.into_iter().next())
        .map(|t| t.saturating_sub(now()))
        .unwrap_or(0)
}

// ---- drain: claim -> run -> ack/fail ----------------------------------------

fn drain_once() -> Value {
    let claimed = outbox::claim(BATCH, LEASE).unwrap_or_default();
    let n = claimed.len();
    let (mut done, mut requeued, mut dead) = (0u32, 0u32, 0u32);

    for e in claimed {
        let attempt = e.attempts + 1;
        let rec = job_by_oid(&e.id);
        // mark running
        if let Some((rid, mut d)) = rec.clone() {
            d["state"] = json!("running");
            d["attempts"] = json!(attempt);
            touch(&mut d);
            save(&rid, &d);
        }
        // payload + the current attempt number (the in-process backend reads it).
        let stored = rec.as_ref().map(|(_, d)| d["payload"].as_str().unwrap_or("{}").to_string());
        let mut pl: Value = stored
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_else(|| json!({}));
        if let Some(o) = pl.as_object_mut() {
            o.insert("attempt".to_string(), json!(attempt));
        }
        let req = workflow::RunRequest { workflow_id: e.topic.clone(), payload: pl.to_string() };

        match workflow::trigger(&req) {
            Ok(out) => {
                let _ = outbox::ack(&e.id);
                done += 1;
                if let Some((rid, mut d)) = rec {
                    d["state"] = json!("done");
                    d["attempts"] = json!(attempt);
                    d["result"] = json!(out);
                    d["error"] = json!("");
                    touch(&mut d);
                    save(&rid, &d);
                    let cx = d["cron"].as_str().unwrap_or("").to_string();
                    if !cx.is_empty() {
                        reschedule(&cx, &e.topic, d["payload"].as_str().unwrap_or("{}"));
                    }
                }
            }
            Err(err) => {
                let msg = run_err_msg(&err);
                let st = outbox::fail(&e.id).unwrap_or(outbox::State::Dead);
                let terminal = matches!(st, outbox::State::Dead);
                if terminal {
                    dead += 1;
                } else {
                    requeued += 1;
                }
                if let Some((rid, mut d)) = rec {
                    d["state"] = json!(if terminal { "dead" } else { "queued" });
                    d["attempts"] = json!(attempt);
                    d["error"] = json!(msg);
                    touch(&mut d);
                    save(&rid, &d);
                }
            }
        }
    }
    json!({ "claimed": n, "done": done, "requeued": requeued, "dead": dead })
}

/// Recurring job: enqueue the next occurrence + mirror a fresh record.
fn reschedule(cron_expr: &str, topic: &str, payload: &str) {
    let delay = cron_delay(cron_expr);
    if let Ok(oid) = outbox::enqueue(topic, payload.as_bytes(), delay) {
        let d = json!({
            "oid": oid, "type": topic, "payload": payload, "state": "queued",
            "attempts": 0, "cron": cron_expr, "result": "", "error": "",
            "created": now(), "updated": now(),
        });
        let _ = records::create(JOBS, &d.to_string(), &["oid".to_string(), "state".to_string()]);
    }
}

// ---- replay -----------------------------------------------------------------

fn replay(oid: &str) -> Outcome {
    match outbox::replay(oid) {
        Ok(()) => {}
        Err(outbox::OutboxError::NotFound) => return Outcome::Err(404, "not a dead job".into()),
        Err(e) => return outbox_err(e),
    }
    if let Some((rid, mut d)) = job_by_oid(oid) {
        d["state"] = json!("queued");
        d["error"] = json!("");
        touch(&mut d);
        save(&rid, &d);
        return Outcome::Json(200, json!({ "job": job_view(&d) }).to_string());
    }
    Outcome::Json(200, json!({ "ok": true }).to_string())
}

// ---- SSE board (self-ticking) -----------------------------------------------

fn stream_events(response_out: ResponseOutparam, _path: &str) {
    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[b"text/event-stream".to_vec()]);
    let _ = headers.set(&"cache-control".to_string(), &[b"no-cache".to_vec()]);
    let _ = headers.set(&"access-control-allow-origin".to_string(), &[b"*".to_vec()]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(200);
    let body = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));

    {
        let stream = body.write().expect("write stream");
        if !write_all(&stream, b": connected\n\n") {
            return;
        }
        let mut last = String::new();
        for _ in 0..MAX_TICKS {
            // the board drives the queue: each tick drains a batch, then pushes
            // the board if it changed.
            drain_once();
            let s = board_json().to_string();
            let frame = if s != last {
                last = s.clone();
                format!("data: {s}\n\n")
            } else {
                ": ping\n\n".to_string()
            };
            if !write_all(&stream, frame.as_bytes()) {
                break;
            }
            monotonic_clock::subscribe_duration(POLL_MS * 1_000_000).block();
        }
    }
    let _ = OutgoingBody::finish(body, None);
}

// ---- http plumbing -----------------------------------------------------------

fn run_err_msg(e: &workflow::RunError) -> String {
    match e {
        workflow::RunError::NotFound(m) => format!("no such workflow: {m}"),
        workflow::RunError::InvalidInput(m) => format!("invalid input: {m}"),
        workflow::RunError::WorkerFailed(m) => m.clone(),
        workflow::RunError::Unavailable(m) => format!("execution backend unavailable: {m}"),
    }
}

fn outbox_err(e: outbox::OutboxError) -> Outcome {
    match e {
        outbox::OutboxError::NotFound => Outcome::Err(404, "not_found".into()),
        outbox::OutboxError::BackendUnavailable(m) => Outcome::Err(503, m),
    }
}

fn store_err(e: records::StoreError) -> Outcome {
    match e {
        records::StoreError::NotFound => Outcome::Err(404, "not_found".into()),
        records::StoreError::InvalidJson(m) => Outcome::Err(422, m),
        records::StoreError::RevisionConflict(_) => Outcome::Err(409, "conflict".into()),
        records::StoreError::BackendUnavailable(m) => Outcome::Err(503, m),
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
            Err(_) => break,
        }
    }
    Ok(buf)
}

fn emit(response_out: ResponseOutparam, result: Outcome) {
    match result {
        Outcome::Json(code, body) => respond(response_out, code, "application/json", body.as_bytes()),
        Outcome::Html(code, body) => respond(response_out, code, "text/html; charset=utf-8", body.as_bytes()),
        Outcome::Err(code, msg) => respond(
            response_out,
            code,
            "application/json",
            json!({ "error": msg }).to_string().as_bytes(),
        ),
    }
}

fn respond(response_out: ResponseOutparam, status: u16, content_type: &str, body: &[u8]) {
    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[content_type.as_bytes().to_vec()]);
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
