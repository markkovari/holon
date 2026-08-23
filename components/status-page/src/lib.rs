//! status:app — uptime monitor over composed capability contracts.
//!
//! The pump (`POST /api/tick`): timer::due -> probe each monitor's URL over
//! outgoing HTTP -> fire `recover`/`fail` on its fsm instance when the event
//! is legal in the current state -> on a real transition, publish to the bus
//! and (optionally) alert via webhook. up -> degraded -> down takes two
//! consecutive failures; a single good probe recovers from either.

#[allow(warnings)]
mod bindings;

use serde::Deserialize;
use serde_json::{json, Value};

use bindings::event::bus::bus;
use bindings::fsm::workflow::engine as fsm;
use bindings::notify::dispatch::dispatcher as notify;
use bindings::records::store::store as records;
use bindings::sched::timer::timer;
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::http::outgoing_handler;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingRequest, OutgoingResponse,
    RequestOptions, ResponseOutparam, Scheme,
};

struct Component;

const MONITORS: &str = "status_monitors";
const MACHINE: &str = "monitor";
const TOPIC: &str = "status";
const TICK_BATCH: u32 = 25;
const TICK_LEASE: u64 = 60;

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let query = path.split_once('?').map(|x| x.1).unwrap_or("").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let result = match (&method, seg.as_slice()) {
            (Method::Get, [""]) => Outcome::Html(INDEX_HTML.to_string()),
            (Method::Post, ["api", "monitors"]) => create_monitor(&request),
            (Method::Get, ["api", "monitors"]) | (Method::Get, ["api", "status"]) => status(),
            (Method::Delete, ["api", "monitors", id]) => delete_monitor(id),
            (Method::Get, ["api", "monitors", id, "history"]) => history(id),
            (Method::Post, ["api", "tick"]) => tick(),
            (Method::Get, ["api", "events"]) => events(&query),
            _ => Outcome::NotFound,
        };
        emit(response_out, result);
    }
}

enum Outcome {
    Json(u16, String),
    Html(String),
    Bad(String),
    Err(u16, String),
    NotFound,
}

fn now() -> u64 {
    wall_clock::now().seconds
}

// ---- monitors ----------------------------------------------------------------

#[derive(Deserialize)]
struct CreateMonitor {
    name: String,
    url: String,
    /// probe period in seconds.
    #[serde(default)]
    period: Option<u64>,
    /// webhook to POST on every state transition.
    #[serde(rename = "alert-url", alias = "alert_url", default)]
    alert_url: Option<String>,
}

/// up --fail--> degraded --fail--> down; one `recover` climbs back to up.
fn ensure_machine() -> Result<(), fsm::FsmError> {
    let t = |event: &str, source: &str, target: &str| fsm::Transition {
        event: event.into(),
        source: source.into(),
        target: target.into(),
    };
    fsm::define(
        MACHINE,
        &fsm::Definition {
            states: vec!["up".into(), "degraded".into(), "down".into()],
            initial: "up".into(),
            transitions: vec![
                t("fail", "up", "degraded"),
                t("fail", "degraded", "down"),
                t("recover", "degraded", "up"),
                t("recover", "down", "up"),
            ],
            terminal: vec![],
        },
    )
}

fn create_monitor(request: &IncomingRequest) -> Outcome {
    let req: CreateMonitor =
        match read_body(request).and_then(|b| serde_json::from_slice(&b).map_err(|_| ())) {
            Ok(r) => r,
            Err(_) => {
                return Outcome::Bad("expected json body {name, url, period?, alert-url?}".into())
            }
        };
    if !(req.url.starts_with("http://") || req.url.starts_with("https://")) {
        return Outcome::Bad("url must be http(s)".into());
    }
    let period = req.period.unwrap_or(60);
    if period < 10 {
        return Outcome::Bad("period must be >= 10 seconds".into());
    }
    if let Err(e) = ensure_machine() {
        return Outcome::Err(503, format!("fsm: {e:?}"));
    }

    let data = json!({
        "name": req.name,
        "url": req.url,
        "period": period,
        "alert_url": req.alert_url.unwrap_or_default(),
        "last_status": 0,
        "last_ok": Value::Null,
        "last_checked": 0,
    });
    let entry = match records::create(MONITORS, &data.to_string(), &[]) {
        Ok(e) => e,
        Err(e) => return store_err(e),
    };
    if let Err(e) = fsm::create_instance(MACHINE, &entry.id) {
        let _ = records::delete(MONITORS, &entry.id);
        return Outcome::Err(503, format!("fsm: {e:?}"));
    }
    // first run is due immediately, then every `period`.
    if let Err(e) = timer::schedule_every(&entry.id, period, now(), entry.id.as_bytes()) {
        let _ = records::delete(MONITORS, &entry.id);
        return Outcome::Err(503, format!("timer: {e:?}"));
    }
    Outcome::Json(201, monitor_json(&entry).to_string())
}

fn delete_monitor(id: &str) -> Outcome {
    match records::delete(MONITORS, id) {
        Ok(_) => {}
        Err(records::StoreError::NotFound) => return Outcome::NotFound,
        Err(e) => return store_err(e),
    }
    let _ = timer::cancel(id);
    Outcome::Json(200, "{\"deleted\":true}".into())
}

fn monitor_json(entry: &records::Entry) -> Value {
    let data: Value = serde_json::from_str(&entry.data).unwrap_or(Value::Null);
    let state =
        fsm::get_status(MACHINE, &entry.id).map(|s| s.state).unwrap_or_else(|_| "up".into());
    json!({
        "id": entry.id,
        "name": data["name"],
        "url": data["url"],
        "period": data["period"],
        "state": state,
        "last_status": data["last_status"],
        "last_ok": data["last_ok"],
        "last_checked": data["last_checked"],
    })
}

fn status() -> Outcome {
    match records::list_records(MONITORS, 0, "") {
        Ok(page) => {
            let monitors: Vec<Value> = page.entries.iter().map(monitor_json).collect();
            Outcome::Json(200, json!({ "monitors": monitors, "at": now() }).to_string())
        }
        Err(e) => store_err(e),
    }
}

fn history(id: &str) -> Outcome {
    match fsm::history(MACHINE, id) {
        Ok(entries) => {
            let list: Vec<Value> = entries
                .iter()
                .map(|h| json!({ "event": h.event, "from": h.source, "to": h.target, "at": h.at }))
                .collect();
            Outcome::Json(200, json!({ "history": list }).to_string())
        }
        Err(fsm::FsmError::UnknownInstance) | Err(fsm::FsmError::UnknownMachine) => {
            Outcome::NotFound
        }
        Err(e) => Outcome::Err(503, format!("fsm: {e:?}")),
    }
}

// ---- the pump ------------------------------------------------------------------

fn tick() -> Outcome {
    let at = now();
    let jobs = match timer::due(at, TICK_BATCH, TICK_LEASE) {
        Ok(j) => j,
        Err(e) => return Outcome::Err(503, format!("timer: {e:?}")),
    };
    let mut results = Vec::new();
    for job in &jobs {
        let id = &job.key;
        let Ok(monitor) = records::get(MONITORS, id) else {
            // monitor deleted -> retire the orphaned job.
            let _ = timer::ack(id);
            let _ = timer::cancel(id);
            continue;
        };
        let data: Value = serde_json::from_str(&monitor.data).unwrap_or(Value::Null);
        let url = data["url"].as_str().unwrap_or("");
        let status = probe(url);
        let ok = matches!(status, Some(s) if s < 400);

        let event = if ok { "recover" } else { "fail" };
        let mut transition = None;
        if fsm::can_fire(MACHINE, id, event).unwrap_or(false) {
            let from = fsm::get_status(MACHINE, id).map(|s| s.state).unwrap_or_default();
            if let Ok(st) = fsm::fire(MACHINE, id, event) {
                transition = Some((from, st.state));
            }
        }
        if let Some((from, to)) = &transition {
            let name = data["name"].as_str().unwrap_or(id);
            let change = json!({
                "monitor": id, "name": name, "from": from, "to": to,
                "status": status.unwrap_or(0), "at": at,
            });
            let _ = bus::publish(TOPIC, change.to_string().as_bytes());
            if let Some(alert) = data["alert_url"].as_str().filter(|s| !s.is_empty()) {
                let _ = notify::send(&notify::Message {
                    channel: notify::Channel::Webhook,
                    target: alert.to_string(),
                    subject: format!("{name} is {to}"),
                    body: change.to_string(),
                });
            }
        }

        // last-probe snapshot on the record; best-effort (a concurrent tick
        // losing this write only costs a fresher timestamp).
        let mut updated = data.clone();
        updated["last_status"] = json!(status.unwrap_or(0));
        updated["last_ok"] = json!(ok);
        updated["last_checked"] = json!(at);
        let _ = records::update(MONITORS, id, &updated.to_string(), monitor.revision);
        let _ = timer::ack(id);

        results.push(json!({
            "monitor": id,
            "ok": ok,
            "status": status.unwrap_or(0),
            "transition": transition.map(|(f, t)| format!("{f}->{t}")),
        }));
    }
    Outcome::Json(200, json!({ "due": jobs.len(), "results": results }).to_string())
}

/// GET the target and report the status code; None = unreachable.
fn probe(url: &str) -> Option<u16> {
    let (scheme, rest) = if let Some(r) = url.strip_prefix("https://") {
        (Scheme::Https, r)
    } else {
        let r = url.strip_prefix("http://")?;
        (Scheme::Http, r)
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let req = OutgoingRequest::new(Fields::new());
    req.set_method(&Method::Get).ok()?;
    req.set_scheme(Some(&scheme)).ok()?;
    req.set_authority(Some(authority)).ok()?;
    req.set_path_with_query(Some(path)).ok()?;
    let body = req.body().ok()?;
    OutgoingBody::finish(body, None).ok()?;

    let future = outgoing_handler::handle(req, Some(RequestOptions::new())).ok()?;
    future.subscribe().block();
    let resp = future.get()?.ok()?.ok()?;
    let status = resp.status();
    // drain so the connection is released.
    if let Ok(incoming) = resp.consume() {
        if let Ok(stream) = incoming.stream() {
            while matches!(stream.blocking_read(8192), Ok(c) if !c.is_empty()) {}
        }
    }
    Some(status)
}

// ---- event feed ------------------------------------------------------------------

/// Consumer-group poll of state changes; `&ack=true` acknowledges what was read.
fn events(query: &str) -> Outcome {
    let group = query_param(query, "group").unwrap_or_else(|| "default".into());
    let ack = query_param(query, "ack").as_deref() == Some("true");
    let events = match bus::poll(TOPIC, &group, 50) {
        Ok(evs) => evs,
        Err(e) => return Outcome::Err(503, format!("bus: {e:?}")),
    };
    if ack && !events.is_empty() {
        let ids: Vec<String> = events.iter().map(|e| e.id.clone()).collect();
        let _ = bus::ack(TOPIC, &group, &ids);
    }
    let list: Vec<Value> = events
        .iter()
        .map(|e| {
            json!({
                "id": e.id,
                "at": e.at,
                "change": serde_json::from_slice::<Value>(&e.payload).unwrap_or(Value::Null),
            })
        })
        .collect();
    Outcome::Json(200, json!({ "events": list, "group": group, "acked": ack }).to_string())
}

// ---- helpers ----------------------------------------------------------------------

fn store_err(e: records::StoreError) -> Outcome {
    match e {
        records::StoreError::NotFound => Outcome::NotFound,
        records::StoreError::InvalidJson(m) => Outcome::Bad(m),
        records::StoreError::RevisionConflict(_) => Outcome::Err(409, "conflict".into()),
        records::StoreError::BackendUnavailable(m) => Outcome::Err(503, m),
    }
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

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        let mut it = kv.splitn(2, '=');
        (it.next()? == key).then(|| it.next().unwrap_or("").to_string())
    })
}

// ---- responses --------------------------------------------------------------------

fn emit(response_out: ResponseOutparam, result: Outcome) {
    match result {
        Outcome::Json(code, body) => {
            respond(response_out, code, body.as_bytes(), "application/json")
        }
        Outcome::Html(body) => {
            respond(response_out, 200, body.as_bytes(), "text/html; charset=utf-8")
        }
        Outcome::Bad(msg) => respond(
            response_out,
            400,
            json!({ "error": msg }).to_string().as_bytes(),
            "application/json",
        ),
        Outcome::Err(code, msg) => respond(
            response_out,
            code,
            json!({ "error": msg }).to_string().as_bytes(),
            "application/json",
        ),
        Outcome::NotFound => {
            respond(response_out, 404, b"{\"error\":\"not_found\"}", "application/json")
        }
    }
}

fn respond(response_out: ResponseOutparam, status: u16, body: &[u8], content_type: &str) {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[content_type.as_bytes().to_vec()]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(status);
    let out = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    if !body.is_empty() {
        let stream = out.write().expect("write stream");
        for chunk in body.chunks(4096) {
            let _ = stream.blocking_write_and_flush(chunk);
        }
    }
    let _ = OutgoingBody::finish(out, None);
}

const INDEX_HTML: &str = r#"<!doctype html>
<meta charset="utf-8">
<title>status</title>
<style>
  body { font: 15px/1.5 system-ui, sans-serif; max-width: 640px; margin: 3rem auto; padding: 0 1rem; color: #1a1a2e; }
  h1 { font-size: 1.3rem; } table { width: 100%; border-collapse: collapse; }
  td, th { padding: .5rem .6rem; border-bottom: 1px solid #e5e5ef; text-align: left; }
  .dot { display: inline-block; width: .7em; height: .7em; border-radius: 50%; margin-right: .45em; }
  .up { background: #2fbf71; } .degraded { background: #f2a541; } .down { background: #e5484d; }
  button { padding: .35rem .8rem; } small { color: #888; }
  @media (prefers-color-scheme: dark) { body { background: #16161e; color: #e5e5ef; } td, th { border-color: #2a2a3a; } }
</style>
<h1>Service status <button onclick="tick()">Run checks</button></h1>
<table><thead><tr><th>Monitor</th><th>State</th><th>Last probe</th></tr></thead><tbody id="rows"></tbody></table>
<p><small id="at"></small></p>
<script>
async function load() {
  const r = await fetch('/api/status'); const s = await r.json();
  document.getElementById('rows').innerHTML = s.monitors.map(m =>
    `<tr><td>${m.name}</td><td><span class="dot ${m.state}"></span>${m.state}</td>` +
    `<td>${m.last_ok === null ? '—' : 'HTTP ' + m.last_status}</td></tr>`).join('') ||
    '<tr><td colspan="3">no monitors yet</td></tr>';
  document.getElementById('at').textContent = 'as of ' + new Date(s.at * 1000).toLocaleTimeString();
}
async function tick() { await fetch('/api/tick', {method: 'POST'}); load(); }
load(); setInterval(load, 10000);
</script>
"#;

bindings::export!(Component with_types_in bindings);
