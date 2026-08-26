//! pipeline:app — a reliable event pipeline over composed contracts.
//!
//! Events live entirely in `outbox:dispatch` (its `event` record carries id,
//! topic, payload, state, attempts, …). The domain adds nothing durable of its
//! own — it PUMPS the outbox: on every tick it `claim`s due events and, if the
//! (simulated) downstream sink is up, `ack`s them; if the sink is down it
//! `fail`s them, so the outbox reschedules with backoff and — after the retry
//! ceiling — moves them to `dead`. Every transition is published on event:bus
//! (`pipeline`), and `GET /api/stream` sets its HTTP response early then LOOPS,
//! pumping + writing each transition as an SSE `data:` frame while the host
//! streams the body to the browser. Real server-push on wasip2 — same trick as
//! pulse — carrying a reliability story: retry, backoff, dead-letter, replay.

#[allow(warnings)]
mod bindings;

use serde_json::{json, Value};

use bindings::event::bus::bus;
use bindings::id::generate::generator as ids;
use bindings::outbox::dispatch::queue;
use bindings::wasi::clocks::monotonic_clock;
use bindings::wasi::clocks::wall_clock;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

guestio::guest_write_all!();

struct Component;

/// event-bus topic every state transition is published on (also the SSE cursor).
const XITIONS: &str = "pipeline";
/// event-bus topic holding the demo sink up/down control events (latest wins).
const SINK_CTRL: &str = "_sink";
/// consumer group the relay pump uses to read the latest sink-control event.
const CTRL_GROUP: &str = "relay";
const POLL_MS: u64 = 500;
const MAX_TICKS: u32 = 800; // ~7 min connection cap; the browser reconnects.
const CLAIM_MAX: u32 = 32;
const LEASE_SECS: u64 = 30;

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        match (&method, seg.as_slice()) {
            // The SSE route owns the response (it streams); everything else
            // computes an Outcome and emits once. Routes under /api so the
            // static-dir SPA fallback (index.html for unknown GETs) leaves them.
            (Method::Get, ["api", "stream"]) => {
                stream_events(response_out, &path);
            }
            _ => {
                let outcome = match (&method, seg.as_slice()) {
                    (Method::Get, [""]) => usage_json(),
                    (Method::Post, ["api", "events"]) => enqueue_event(&request),
                    (Method::Get, ["api", "events"]) => snapshot(&path),
                    (Method::Post, ["api", "sink"]) => set_sink(&request),
                    (Method::Get, ["api", "dead-letters"]) => dead_letters(),
                    (Method::Post, ["api", "dead-letters", id, "replay"]) => replay(id),
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
            "service": "pipeline",
            "about": "reliable event pipeline — enqueue, dispatch at-least-once, retry+backoff, dead-letter, replay; live over SSE",
            "enqueue": "POST /api/events {topic, payload}",
            "snapshot": "GET /api/events?after=seq",
            "stream": "GET /api/stream?after=seq   (text/event-stream)",
            "sink": "POST /api/sink {up: bool}   (demo knob: take the downstream sink up/down)",
            "dead_letters": "GET /api/dead-letters",
            "replay": "POST /api/dead-letters/{id}/replay"
        })
        .to_string(),
    )
}

// ---- ingress -----------------------------------------------------------------

/// Enqueue an event into the durable outbox. Immediately eligible for the relay.
fn enqueue_event(request: &IncomingRequest) -> Outcome {
    let body = match parse_body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let topic = body["topic"].as_str().unwrap_or("").trim().to_string();
    if topic.is_empty() {
        return Outcome::Err(422, "topic required".into());
    }
    // The payload is opaque to the outbox; we carry the caller's JSON verbatim.
    let payload = match body.get("payload") {
        Some(v) => v.to_string(),
        None => json!({}).to_string(),
    };
    match queue::enqueue(&topic, payload.as_bytes(), 0) {
        Ok(id) => {
            publish_xition(&id, &topic, "enqueued", 0);
            Outcome::Json(201, json!({"id": id, "topic": topic, "state": "pending"}).to_string())
        }
        Err(e) => queue_err(e),
    }
}

/// The demo knob: record the sink up/down as the newest control event. The relay
/// reads the latest on each pump; a down sink makes every dispatch `fail`.
fn set_sink(request: &IncomingRequest) -> Outcome {
    let body = match parse_body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let up = body["up"].as_bool().unwrap_or(true);
    let _ = bus::publish(SINK_CTRL, json!({"up": up, "at": now()}).to_string().as_bytes());
    // pump once so the effect is visible without waiting for a stream tick.
    pump();
    Outcome::Json(200, json!({"sink_up": up}).to_string())
}

// ---- the relay pump ----------------------------------------------------------

/// Is the downstream sink currently up? Reads the latest control event; default
/// up (no control event yet == healthy). Polls without acking so the latest is
/// always visible (offset never advances past the newest control event).
fn sink_up() -> bool {
    let events = bus::poll(SINK_CTRL, CTRL_GROUP, 256).unwrap_or_default();
    events
        .last()
        .and_then(|e| serde_json::from_slice::<Value>(&e.payload).ok())
        .and_then(|v| v["up"].as_bool())
        .unwrap_or(true)
}

/// One relay pass: claim due events, dispatch to the (simulated) sink, ack the
/// delivered ones / fail the rest. Publishes a transition per event so open
/// boards see it live. Idempotent to call — an empty queue is a no-op.
fn pump() {
    let up = sink_up();
    let claimed = match queue::claim(CLAIM_MAX, LEASE_SECS) {
        Ok(c) => c,
        Err(_) => return,
    };
    for ev in claimed {
        publish_xition(&ev.id, &ev.topic, "in-flight", ev.attempts);
        if up {
            // simulated successful dispatch to the downstream sink.
            if queue::ack(&ev.id).is_ok() {
                publish_xition(&ev.id, &ev.topic, "acked", ev.attempts);
            }
        } else {
            // sink down: report the failure; the outbox reschedules with backoff
            // and, once attempts exceed the cap, moves it to `dead`.
            match queue::fail(&ev.id) {
                Ok(queue::State::Dead) => {
                    publish_xition(&ev.id, &ev.topic, "dead", ev.attempts + 1)
                }
                Ok(_) => publish_xition(&ev.id, &ev.topic, "retry", ev.attempts + 1),
                Err(_) => {}
            }
        }
    }
}

fn publish_xition(id: &str, topic: &str, state: &str, attempts: u32) {
    let frame = json!({
        "xid": ids::short_code(8),
        "id": id,
        "topic": topic,
        "state": state,
        "attempts": attempts,
        "at": now(),
    });
    let _ = bus::publish(XITIONS, frame.to_string().as_bytes());
}

// ---- snapshot + dead-letters + replay ----------------------------------------

/// Every transition with bus seq > after, oldest-first, plus the new cursor.
/// The board rebuilds its lane state from this on load, then tails the stream.
fn snapshot(path: &str) -> Outcome {
    pump(); // advance the pipeline so a plain GET reflects the latest state.
    let after = query_i64(path, "after").unwrap_or(-1);
    let (rows, cursor) = xitions_after(after);
    Outcome::Json(200, json!({ "transitions": rows, "cursor": cursor }).to_string())
}

/// Read transitions from the bus log with id (seq) > after. event-bus ids are
/// stringified monotonic per-topic sequences — the same cursor trick as pulse.
fn xitions_after(after: i64) -> (Vec<Value>, i64) {
    // Poll a fresh, throwaway group each call so we always see the full log;
    // we never ack (read-only view), so offsets stay at zero for this group.
    let events = bus::poll(XITIONS, "snapshot", 4096).unwrap_or_default();
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

fn dead_letters() -> Outcome {
    match queue::dead_letters(256) {
        Ok(events) => {
            let rows: Vec<Value> = events.iter().map(event_json).collect();
            Outcome::Json(200, json!({ "dead": rows }).to_string())
        }
        Err(e) => queue_err(e),
    }
}

fn replay(id: &str) -> Outcome {
    match queue::replay(id) {
        Ok(()) => {
            publish_xition(id, "", "replayed", 0);
            pump(); // give it a chance to redeliver right away.
            Outcome::Json(200, json!({"id": id, "state": "pending"}).to_string())
        }
        Err(e) => queue_err(e),
    }
}

fn event_json(e: &queue::Event) -> Value {
    let state = match e.state {
        queue::State::Pending => "pending",
        queue::State::InFlight => "in-flight",
        queue::State::Dead => "dead",
    };
    json!({
        "id": e.id,
        "topic": e.topic,
        "state": state,
        "attempts": e.attempts,
        "created": e.created,
        "payload": String::from_utf8_lossy(&e.payload),
    })
}

// ---- the SSE stream ----------------------------------------------------------

/// Hold the connection open, pump the relay each tick, and push every new
/// transition as an SSE `data:` frame. Sets the response early, then loops until
/// the client disconnects (a write error) or the connection cap is hit.
fn stream_events(response_out: ResponseOutparam, path: &str) {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"text/event-stream".to_vec()]);
    let _ = headers.set("cache-control", &[b"no-cache".to_vec()]);
    let _ = headers.set("access-control-allow-origin", &[b"*".to_vec()]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(200);
    let body = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));

    // default: only transitions produced after we connect. `?after=` catches up.
    let mut cursor = query_i64(path, "after").unwrap_or_else(current_seq);

    {
        let stream = body.write().expect("write stream");
        if !write_all(&stream, b": connected\n\n") {
            return;
        }
        for _ in 0..MAX_TICKS {
            pump(); // drive the pipeline forward each tick.
            let (rows, new_cursor) = xitions_after(cursor);
            cursor = new_cursor;
            let frame = if rows.is_empty() {
                ": ping\n\n".to_string() // heartbeat — also how we notice a hangup
            } else {
                rows.iter().map(|r| format!("data: {r}\n\n")).collect::<String>()
            };
            if !write_all(&stream, frame.as_bytes()) {
                break; // client disconnected
            }
            monotonic_clock::subscribe_duration(POLL_MS * 1_000_000).block();
        }
    }
    let _ = OutgoingBody::finish(body, None);
}

/// Highest transition seq so far (the "only new" starting cursor).
fn current_seq() -> i64 {
    bus::poll(XITIONS, "snapshot", 4096)
        .unwrap_or_default()
        .iter()
        .filter_map(|e| e.id.parse::<i64>().ok())
        .max()
        .unwrap_or(-1)
}

// ---- http plumbing -----------------------------------------------------------

fn queue_err(e: queue::OutboxError) -> Outcome {
    match e {
        queue::OutboxError::NotFound => Outcome::Err(404, "not_found".into()),
        queue::OutboxError::BackendUnavailable(m) => Outcome::Err(503, m),
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

/// Read query param `key` as an i64 (for the `after` cursor).
fn query_i64(path: &str, key: &str) -> Option<i64> {
    let query = path.split('?').nth(1)?;
    query.split('&').find_map(|pair| {
        let mut it = pair.splitn(2, '=');
        (it.next()? == key).then(|| it.next().unwrap_or("").parse().ok())?
    })
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
