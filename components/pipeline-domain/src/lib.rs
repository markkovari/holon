//! pipeline:app — a reliable event pipeline over composed contracts.

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "../../wit/deps/wasi-random-0.2.0",
            "../outbox/wit",
            "../event-bus/wit",
            "../id-generate/wit",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "wit",
        ],
        world: "pipeline:app/pipeline-domain",
        generate_all,
    });
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::clocks0_3_0_rc_2026_03_15 as clocks;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}

use serde_json::{json, Value};

use bindings::event::bus::bus;
use bindings::id::generate::generator as ids;
use bindings::outbox::dispatch::queue;
use bindings::p3::clocks::monotonic_clock;
use bindings::p3::clocks::system_clock;

use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Fields, Method, Request, Response};

guestio::guest_p3_respond!();

struct Component;

/// Every transition is published here; its seq is the SSE cursor.
const XITIONS: &str = "pipeline";
/// Demo sink up/down control events; latest wins.
const SINK_CTRL: &str = "_sink";
const CTRL_GROUP: &str = "relay";
const POLL_MS: u64 = 500;
const MAX_TICKS: u32 = 800; // ~7 min connection cap; the browser reconnects.
const CLAIM_MAX: u32 = 32;
const LEASE_SECS: u64 = 30;

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let method = request.get_method();
        let path = request.get_path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        match (&method, seg.as_slice()) {
            // Routes live under /api so the static-dir SPA fallback leaves them.
            (Method::Get, ["api", "stream"]) => stream_events(&path),
            _ => {
                let outcome = match (&method, seg.as_slice()) {
                    (Method::Get, [""]) | (Method::Get, ["index.html"]) => {
                        Outcome::Html(include_str!("../ui/index.html").to_string())
                    }
                    (Method::Get, ["styles.css"]) => {
                        Outcome::Css(include_str!("../ui/styles.css").to_string())
                    }
                    (Method::Get, ["app.js"]) => {
                        Outcome::Js(include_str!("../ui/app.js").to_string())
                    }
                    (Method::Get, ["api", "info"]) => usage_json(),
                    (Method::Post, ["api", "events"]) => enqueue_event(request).await,
                    (Method::Get, ["api", "events"]) => snapshot(&path),
                    (Method::Post, ["api", "sink"]) => set_sink(request).await,
                    (Method::Get, ["api", "dead-letters"]) => dead_letters(),
                    (Method::Post, ["api", "dead-letters", id, "replay"]) => replay(id),
                    _ => Outcome::Err(404, "not_found".into()),
                };
                emit(outcome)
            }
        }
    }
}

enum Outcome {
    Html(String),
    Css(String),
    Js(String),
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

async fn enqueue_event(request: Request) -> Outcome {
    let body = match parse_body(request).await {
        Ok(v) => v,
        Err(o) => return o,
    };
    let topic = body["topic"].as_str().unwrap_or("").trim().to_string();
    if topic.is_empty() {
        return Outcome::Err(422, "topic required".into());
    }
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

async fn set_sink(request: Request) -> Outcome {
    let body = match parse_body(request).await {
        Ok(v) => v,
        Err(o) => return o,
    };
    let up = body["up"].as_bool().unwrap_or(true);
    let _ = bus::publish(SINK_CTRL, json!({"up": up, "at": now()}).to_string().as_bytes());
    pump();
    Outcome::Json(200, json!({"sink_up": up}).to_string())
}

/// Polls without acking so the latest stays visible.
fn sink_up() -> bool {
    let events = bus::poll(SINK_CTRL, CTRL_GROUP, 256).unwrap_or_default();
    events
        .last()
        .and_then(|e| serde_json::from_slice::<Value>(&e.payload).ok())
        .and_then(|v| v["up"].as_bool())
        .unwrap_or(true)
}

fn pump() {
    let up = sink_up();
    let claimed = match queue::claim(CLAIM_MAX, LEASE_SECS) {
        Ok(c) => c,
        Err(_) => return,
    };
    for ev in claimed {
        publish_xition(&ev.id, &ev.topic, "in-flight", ev.attempts);
        if up {
            if queue::ack(&ev.id).is_ok() {
                publish_xition(&ev.id, &ev.topic, "acked", ev.attempts);
            }
        } else {
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

fn snapshot(path: &str) -> Outcome {
    pump(); // advance the pipeline so a plain GET reflects the latest state.
    let after = query_i64(path, "after").unwrap_or(-1);
    let (rows, cursor) = xitions_after(after);
    Outcome::Json(200, json!({ "transitions": rows, "cursor": cursor }).to_string())
}

/// event-bus ids are stringified monotonic per-topic sequences.
fn xitions_after(after: i64) -> (Vec<Value>, i64) {
    // Poll a fresh, throwaway group each call so we always see the full log; never ack (read-only view).
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

fn stream_events(path: &str) -> Result<Response, ErrorCode> {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"text/event-stream".to_vec()]);
    let _ = headers.set("cache-control", &[b"no-cache".to_vec()]);
    let _ = headers.set("access-control-allow-origin", &[b"*".to_vec()]);

    // default: only transitions produced after we connect. `?after=` catches up.
    let mut cursor = query_i64(path, "after").unwrap_or_else(current_seq);

    let (mut tx, rx) = bindings::wit_stream::new();
    wit_bindgen::spawn_local(async move {
        // `write_all` hands back what the reader did not take: non-empty = hangup.
        if !tx.write_all(b": connected\n\n".to_vec()).await.is_empty() {
            return;
        }
        for _ in 0..MAX_TICKS {
            pump();
            let (rows, new_cursor) = xitions_after(cursor);
            cursor = new_cursor;
            let frame = if rows.is_empty() {
                ": ping\n\n".to_string() // heartbeat — also how we notice a hangup
            } else {
                rows.iter().map(|r| format!("data: {r}\n\n")).collect::<String>()
            };
            if !tx.write_all(frame.into_bytes()).await.is_empty() {
                break;
            }
            monotonic_clock::wait_for(POLL_MS * 1_000_000).await;
        }
    });
    let (trailers_tx, trailers_rx) = bindings::wit_future::new(|| Ok(None));
    drop(trailers_tx);
    let (response, _sent) = Response::new(headers, Some(rx), trailers_rx);
    Ok(response)
}

fn current_seq() -> i64 {
    bus::poll(XITIONS, "snapshot", 4096)
        .unwrap_or_default()
        .iter()
        .filter_map(|e| e.id.parse::<i64>().ok())
        .max()
        .unwrap_or(-1)
}

fn queue_err(e: queue::OutboxError) -> Outcome {
    match e {
        queue::OutboxError::NotFound => Outcome::Err(404, "not_found".into()),
        queue::OutboxError::BackendUnavailable(m) => Outcome::Err(503, m),
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

/// Backstop under wasmtime's 64 MiB per-store cap, which traps instead of erroring.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_p3_read_body!(MAX_BODY_BYTES);

fn query_i64(path: &str, key: &str) -> Option<i64> {
    let query = path.split('?').nth(1)?;
    query.split('&').find_map(|pair| {
        let mut it = pair.splitn(2, '=');
        (it.next()? == key).then(|| it.next().unwrap_or("").parse().ok())?
    })
}

fn emit(result: Outcome) -> Result<Response, ErrorCode> {
    match result {
        Outcome::Html(html) => respond_cors(200, "text/html; charset=utf-8", html),
        Outcome::Css(css) => respond_cors(200, "text/css; charset=utf-8", css),
        Outcome::Js(js) => respond_cors(200, "application/javascript; charset=utf-8", js),
        Outcome::Json(code, body) => respond_cors(code, "application/json", body),
        Outcome::Err(code, msg) => {
            respond_cors(code, "application/json", json!({ "error": msg }).to_string())
        }
    }
}

fn respond_cors(status: u16, content_type: &str, body: String) -> Result<Response, ErrorCode> {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[content_type.as_bytes().to_vec()]);
    let _ = headers.set("access-control-allow-origin", &[b"*".to_vec()]);
    respond_with(status, headers, body.into_bytes())
}

bindings::export!(Component with_types_in bindings);
