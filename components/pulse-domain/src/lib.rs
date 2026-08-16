//! pulse:app — a realtime chat room over composed contracts.
//!
//! Messages are an append-only log in `record:store`, each stamped with a global
//! monotonic `seq` that doubles as the stream cursor. `GET /events` is the new
//! trick: it sets its HTTP response early, then LOOPS — polling the log for
//! `seq > cursor` and writing each new message as an SSE `data:` frame — while
//! the host streams the body to the browser. It sleeps between polls with
//! `monotonic-clock` (so it doesn't busy-spin) and stops when a write fails
//! (the client hung up). Real server-push on wasip2, no WebSocket.

#[allow(warnings)]
mod bindings;

use serde_json::{json, Value};

use bindings::event::bus::bus;
use bindings::id::generate::generator as ids;
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

const MESSAGES: &str = "messages";
const PRESENCE: &str = "presence";
const POLL_MS: u64 = 700;
const MAX_TICKS: u32 = 800; // ~9 min connection cap; the client reconnects.
const PRESENCE_WINDOW: u64 = 15; // seconds since last heartbeat to count as "online"

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        match (&method, seg.as_slice()) {
            // The SSE route owns the response (it streams); everything else
            // computes an Outcome and emits once.
            // Routes live under /api so the host's static-dir SPA fallback
            // (which returns index.html for unknown GETs) leaves them alone.
            (Method::Get, ["api", "rooms", room, "events"]) => {
                stream_events(&request, response_out, room, &path);
            }
            _ => {
                let outcome = match (&method, seg.as_slice()) {
                    (Method::Get, [""]) => usage_json(),
                    (Method::Post, ["api", "rooms", room, "messages"]) => post_message(&request, room),
                    (Method::Get, ["api", "rooms", room, "messages"]) => history(room, &path),
                    (Method::Post, ["api", "rooms", room, "presence"]) => heartbeat(&request, room),
                    (Method::Get, ["api", "rooms", room, "presence"]) => presence(room),
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
            "service": "pulse",
            "about": "realtime chat — post a message, watch it stream live to every open window",
            "post": "POST /api/rooms/{room}/messages {user, text}",
            "history": "GET /api/rooms/{room}/messages?after=seq",
            "stream": "GET /api/rooms/{room}/events?after=seq   (text/event-stream)",
            "presence": "POST|GET /api/rooms/{room}/presence"
        })
        .to_string(),
    )
}

// ---- post + history ----------------------------------------------------------

fn post_message(request: &IncomingRequest, room: &str) -> Outcome {
    let body = match parse_body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let user = body["user"].as_str().unwrap_or("").trim().to_string();
    let text = body["text"].as_str().unwrap_or("").trim().to_string();
    if user.is_empty() || text.is_empty() {
        return Outcome::Err(422, "user and text required".into());
    }
    if text.len() > 2000 {
        return Outcome::Err(422, "text too long".into());
    }
    // seq: a global monotonic counter (also the stream cursor).
    let seq = records::count(MESSAGES).unwrap_or(0);
    let data = json!({
        "id": ids::short_code(8),
        "room": room,
        "user": user,
        "text": text,
        "seq": seq,
        "at": now(),
    });
    let entry = match records::create(MESSAGES, &data.to_string(), &["room".to_string()]) {
        Ok(e) => e,
        Err(e) => return store_err(e),
    };
    // fan-out spine: other consumers (notify / moderation / webhooks) subscribe.
    let _ = bus::publish(&format!("room:{room}"), entry.data.as_bytes());
    Outcome::Json(201, msg_json(&data).to_string())
}

fn history(room: &str, path: &str) -> Outcome {
    let after = query_i64(path, "after").unwrap_or(-1);
    let (msgs, cursor) = messages_after(room, after);
    Outcome::Json(200, json!({ "messages": msgs, "cursor": cursor }).to_string())
}

/// Messages in `room` with `seq > after`, oldest-first, plus the new cursor.
fn messages_after(room: &str, after: i64) -> (Vec<Value>, i64) {
    let entries = records::find_by(MESSAGES, "room", &json!(room).to_string()).unwrap_or_default();
    let mut rows: Vec<(i64, Value)> = entries
        .iter()
        .filter_map(|e| {
            let d: Value = serde_json::from_str(&e.data).ok()?;
            let seq = d["seq"].as_i64()?;
            (seq > after).then_some((seq, d))
        })
        .collect();
    rows.sort_by_key(|(seq, _)| *seq);
    let cursor = rows.last().map(|(seq, _)| *seq).unwrap_or(after);
    (rows.into_iter().map(|(_, d)| msg_json(&d)).collect(), cursor)
}

/// Current highest seq in the room (the "only new messages" starting cursor).
fn max_seq(room: &str) -> i64 {
    let entries = records::find_by(MESSAGES, "room", &json!(room).to_string()).unwrap_or_default();
    entries
        .iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok().and_then(|d| d["seq"].as_i64()))
        .max()
        .unwrap_or(-1)
}

fn msg_json(d: &Value) -> Value {
    json!({"id": d["id"], "user": d["user"], "text": d["text"], "seq": d["seq"], "at": d["at"]})
}

// ---- presence (rung 3) -------------------------------------------------------

/// Heartbeat: upsert this user's "last seen" for the room.
fn heartbeat(request: &IncomingRequest, room: &str) -> Outcome {
    let body = match parse_body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let user = body["user"].as_str().unwrap_or("").trim().to_string();
    if user.is_empty() {
        return Outcome::Err(422, "user required".into());
    }
    let existing = records::find_by(PRESENCE, "room", &json!(room).to_string()).unwrap_or_default();
    let mine = existing.into_iter().find(|e| {
        serde_json::from_str::<Value>(&e.data).ok().and_then(|d| d["user"].as_str().map(|u| u == user)).unwrap_or(false)
    });
    let data = json!({"room": room, "user": user, "at": now()});
    match mine {
        Some(e) => {
            let _ = records::update(PRESENCE, &e.id, &data.to_string(), 0);
        }
        None => {
            let _ = records::create(PRESENCE, &data.to_string(), &["room".to_string()]);
        }
    }
    Outcome::Json(200, json!({"ok": true}).to_string())
}

/// Who's online: distinct users heartbeated within the presence window.
fn presence(room: &str) -> Outcome {
    let cutoff = now().saturating_sub(PRESENCE_WINDOW);
    let entries = records::find_by(PRESENCE, "room", &json!(room).to_string()).unwrap_or_default();
    let mut online: Vec<String> = entries
        .iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
        .filter(|d| d["at"].as_u64().unwrap_or(0) >= cutoff)
        .filter_map(|d| d["user"].as_str().map(String::from))
        .collect();
    online.sort();
    online.dedup();
    Outcome::Json(200, json!({ "online": online }).to_string())
}

// ---- the SSE stream ----------------------------------------------------------

/// Hold the connection open and push each new message as an SSE `data:` frame.
/// Sets the response, then loops until the client disconnects (a write error) or
/// the connection cap is hit. `?after=seq` catches up from `seq`; the default is
/// "only messages posted after I connected".
fn stream_events(request: &IncomingRequest, response_out: ResponseOutparam, room: &str, path: &str) {
    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[b"text/event-stream".to_vec()]);
    let _ = headers.set(&"cache-control".to_string(), &[b"no-cache".to_vec()]);
    // let the browser's EventSource reconnect quickly if we hit the cap.
    let _ = headers.set(&"access-control-allow-origin".to_string(), &[b"*".to_vec()]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(200);
    let body = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));

    let mut cursor = query_i64(path, "after").unwrap_or_else(|| max_seq(room));
    let _ = request; // request headers unused; kept for symmetry with other routes

    {
        let stream = body.write().expect("write stream");
        // open the stream (some proxies buffer until the first bytes)
        if !write_all(&stream, b": connected\n\n") {
            return;
        }
        for _ in 0..MAX_TICKS {
            let (msgs, new_cursor) = messages_after(room, cursor);
            cursor = new_cursor;
            let frame = if msgs.is_empty() {
                ": ping\n\n".to_string() // heartbeat — also how we notice a hangup
            } else {
                msgs.iter().map(|m| format!("data: {m}\n\n")).collect::<String>()
            };
            if !write_all(&stream, frame.as_bytes()) {
                break; // client disconnected
            }
            // sleep without busy-spinning: a monotonic-clock pollable that
            // resolves after POLL_MS, blocked on directly.
            monotonic_clock::subscribe_duration(POLL_MS * 1_000_000).block();
        }
    }
    let _ = OutgoingBody::finish(body, None);
}

// ---- http plumbing -----------------------------------------------------------

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
