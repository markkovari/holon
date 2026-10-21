//! The HTTP face of the runtime, loopback only.
//!
//! Two tiers, because the lattice's gateway component reaches this port from
//! inside a tenant's sandbox:
//!
//! * **Open** — `GET|POST /agents/<name>/run` and `POST /events/<topic>`. These
//!   are the triggers: anything that may poke an agent may call them.
//! * **Admin** — everything that changes or reads agents (CRUD, runs, memory,
//!   approvals). Needs `Authorization: Bearer <admin token>`, which only the
//!   operator (the console, or whoever started the daemon) holds.

use std::io::{Read, Write};
use std::net::SocketAddr;
use std::sync::Arc;

use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::agent::Cause;
use crate::runtime::Runtime;
use crate::spec::AgentSpec;
use crate::store::Status;
use crate::trace::Traceparent;

/// Starts serving on `listen` (e.g. `127.0.0.1:0`) and returns the bound
/// address. Runs on background threads for the life of the process.
pub fn serve(rt: Arc<Runtime>, listen: &str, admin_token: String) -> Result<SocketAddr, String> {
    let server = Server::http(listen).map_err(|e| format!("bind {listen}: {e}"))?;
    let addr = server.server_addr().to_ip().ok_or("not an IP listener")?;
    std::thread::spawn(move || {
        for req in server.incoming_requests() {
            let (rt, token) = (rt.clone(), admin_token.clone());
            std::thread::spawn(move || handle(&rt, &token, req));
        }
    });
    Ok(addr)
}

/// The operator's admin token for `dir`: read from `<dir>/admin-token`, or
/// generated (24 random bytes, hex) and written there with mode 0600. Shared
/// by the daemon and the console so either can be the one that created it.
pub fn admin_token_in(dir: &std::path::Path) -> Result<String, String> {
    let p = dir.join("admin-token");
    if let Ok(t) = std::fs::read_to_string(&p) {
        return Ok(t.trim().to_string());
    }
    let mut bytes = [0u8; 24];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|e| format!("reading /dev/urandom: {e}"))?;
    let t: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::write(&p, &t).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
    Ok(t)
}

fn reply(req: Request, status: u16, ctype: &str, body: impl Into<String>) {
    let h = Header::from_bytes("content-type", ctype).unwrap();
    let _ = req.respond(Response::from_string(body.into()).with_status_code(status).with_header(h));
}

/// Like `reply`, with extra response headers (here: `traceparent`).
fn reply_with(req: Request, status: u16, body: impl Into<String>, extra: &[(&str, String)]) {
    let mut resp = Response::from_string(body.into())
        .with_status_code(status)
        .with_header(Header::from_bytes("content-type", "text/plain; charset=utf-8").unwrap());
    for (k, v) in extra {
        if let Ok(h) = Header::from_bytes(k.as_bytes(), v.as_bytes()) {
            resp = resp.with_header(h);
        }
    }
    let _ = req.respond(resp);
}

fn header_value(req: &Request, name: &str) -> Option<String> {
    req.headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str().to_string())
}

/// The caller's W3C trace context, if it sent a valid `traceparent`. A
/// malformed one is ignored and a fresh trace starts, as the spec says.
fn caller_trace(req: &Request) -> Option<Traceparent> {
    header_value(req, "traceparent").and_then(|v| Traceparent::parse(&v))
}

fn bytes_reply(req: Request, ctype: &str, bytes: Vec<u8>, extra: &[(&str, String)]) {
    let mut resp =
        Response::from_data(bytes).with_header(Header::from_bytes("content-type", ctype).unwrap());
    for (k, v) in extra {
        if let Ok(h) = Header::from_bytes(k.as_bytes(), v.as_bytes()) {
            resp = resp.with_header(h);
        }
    }
    let _ = req.respond(resp);
}

fn json_reply(req: Request, status: u16, v: Value) {
    reply(req, status, "application/json", v.to_string());
}

fn text(req: Request, status: u16, s: impl Into<String>) {
    reply(req, status, "text/plain; charset=utf-8", s);
}

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len()
                && (b[i + 1] as char).is_ascii_hexdigit()
                && (b[i + 2] as char).is_ascii_hexdigit() =>
            {
                out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query
        .split('&')
        .filter_map(|p| p.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| percent_decode(v))
}

fn handle(rt: &Arc<Runtime>, token: &str, mut req: Request) {
    let url = req.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));
    let segs: Vec<&str> = path.trim_matches('/').split('/').collect();
    let method = req.method().clone();

    // Audio is bytes and can be large; everything else is small text.
    let limit = if path.starts_with("/speech") {
        crate::speech::MAX_AUDIO_IN as u64
    } else if path.ends_with("/inbox") {
        (20 << 20) + 1 // a photo: one byte over the store's limit, so it is refused, not cut
    } else {
        1 << 20
    };
    let mut raw = Vec::new();
    let _ = req.as_reader().take(limit).read_to_end(&mut raw);
    let body = String::from_utf8_lossy(&raw).into_owned();

    let authed = req
        .headers()
        .iter()
        .find(|h| h.field.equiv("authorization"))
        .is_some_and(|h| !token.is_empty() && h.value.as_str() == format!("Bearer {token}"));

    match (&method, segs.as_slice()) {
        (_, ["health"]) => return text(req, 200, "ok"),
        // Liveness for the gateway's callers: answers without spending a model call.
        (Method::Get, ["agents", name, "ping"]) => {
            return match rt.store().get(name) {
                None => text(req, 404, format!("no agent named {name}")),
                Some(s) if s.paused => text(req, 423, format!("{name} is paused")),
                Some(_) => text(req, 200, "pong"),
            };
        }
        (Method::Get | Method::Post, ["agents", name, "run"]) => {
            let input = query_param(query, "q").filter(|q| !q.is_empty()).unwrap_or(body);
            // Join the caller's trace if it sent one; the run becomes its child.
            let cause = match caller_trace(&req) {
                Some(tp) => Cause {
                    trace_id: tp.trace_id,
                    parent_span_id: Some(tp.span_id),
                    ..Default::default()
                },
                None => Cause::default(),
            };
            // `?attach=a.jpg,b.png` names photos in the agent's inbox that came with the message.
            let attachments: Vec<String> = query_param(query, "attach")
                .map(|v| {
                    v.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default();
            if attachments.len() > crate::attach::MAX_ATTACHMENTS {
                return text(
                    req,
                    400,
                    format!("at most {} attachments", crate::attach::MAX_ATTACHMENTS),
                );
            }
            if let Some(missing) =
                attachments.iter().find(|f| !crate::attach::exists(rt.store(), name, f))
            {
                return text(req, 400, format!("no attachment `{missing}` in {name}'s inbox"));
            }
            let cause = Cause { attachments, ..cause };
            // `?project=` says the message was written inside that project's room.
            let cause =
                Cause { project: query_param(query, "project").filter(|p| !p.is_empty()), ..cause };
            // Who or what woke the agent, in words it reads as the run's `Trigger:` line.
            let trigger = query_param(query, "why")
                .map(|w| w.split_whitespace().collect::<Vec<_>>().join(" "))
                .filter(|w| !w.is_empty())
                .map(|w| format!("http: {}", w.chars().take(160).collect::<String>()))
                .unwrap_or_else(|| "http".to_string());
            return match rt.run_agent(name, &trigger, &input, &cause, "", true) {
                Ok(r) => {
                    // Hand the caller the run's own span, so it can continue the trace.
                    let tp = [("traceparent", Traceparent::header(&r.trace_id, &r.span_id))];
                    let status = match r.status {
                        Status::Ok => 200,
                        Status::OverBudget | Status::Dropped => 429,
                        Status::Failed => 502,
                    };
                    reply_with(req, status, r.answer, &tp)
                }
                Err(e) if e.starts_with("no agent") => text(req, 404, e),
                Err(e) if e.contains("paused") => text(req, 423, e),
                Err(e) if e.starts_with("dropped") => text(req, 429, e),
                Err(e) => text(req, 409, e),
            };
        }
        (Method::Post, ["events", topic]) => {
            let tp = caller_trace(&req);
            return match rt.emit_event(
                topic,
                &body,
                tp.as_ref().map(|t| t.trace_id.as_str()),
                tp.as_ref().map(|t| t.span_id.as_str()),
            ) {
                Ok(woken) => json_reply(req, 202, json!({"woken": woken})),
                Err(e) => text(req, 422, e),
            };
        }
        _ => {}
    }

    if !authed {
        return text(req, 401, "admin token required");
    }
    match (&method, segs.as_slice()) {
        (Method::Get, ["agents"]) => json_reply(req, 200, json!(rt.store().list())),
        (Method::Get, ["agents", name]) => match rt.store().get(name) {
            Some(s) => json_reply(req, 200, json!(s)),
            None => text(req, 404, "no such agent"),
        },
        (Method::Put, ["agents", name]) => match serde_json::from_str::<AgentSpec>(&body) {
            Err(e) => text(req, 400, format!("bad spec: {e}")),
            Ok(spec) if spec.name != *name => text(req, 400, "name in body does not match the URL"),
            Ok(spec) => {
                let exists = rt.store().get(name).is_some();
                let r = if exists { rt.update_agent(spec) } else { rt.create_agent(spec) };
                match r {
                    Ok(()) => text(req, if exists { 200 } else { 201 }, "ok"),
                    Err(e) => text(req, 422, e),
                }
            }
        },
        (Method::Delete, ["agents", name]) => match rt.delete_agent(name) {
            Ok(()) => text(req, 200, "deleted"),
            Err(e) => text(req, 404, e),
        },
        // A photo (or other file) a person sent, put in the agent's workspace for its tools.
        (Method::Post, ["agents", name, "inbox"]) => {
            let filename = query_param(query, "name").unwrap_or_default();
            match rt.store().save_inbox(name, &filename, &raw) {
                Ok(file) => {
                    json_reply(req, 201, json!({ "file": file, "path": format!("inbox/{file}") }))
                }
                Err(e) if e.starts_with("no agent") => text(req, 404, e),
                Err(e) => text(req, 422, e),
            }
        }
        (Method::Get, ["trash"]) => json_reply(req, 200, json!(rt.store().trash())),
        (Method::Post, ["trash", id, "restore"]) => match rt.restore_agent(id) {
            Ok(t) => json_reply(req, 200, json!(t)),
            Err(e) => text(req, 409, e),
        },
        (Method::Delete, ["trash", id]) => match rt.store().purge(id) {
            Ok(t) => json_reply(req, 200, json!(t)),
            Err(e) => text(req, 404, e),
        },
        (Method::Post, ["agents", name, action @ ("pause" | "resume")]) => {
            match rt.set_paused(name, *action == "pause") {
                Ok(()) => text(req, 200, "ok"),
                Err(e) => text(req, 404, e),
            }
        }
        (Method::Get, ["agents", name, "runs"]) => {
            let n = query_param(query, "limit").and_then(|v| v.parse().ok()).unwrap_or(20);
            json_reply(req, 200, json!(rt.store().runs(name, n)))
        }
        (Method::Get, ["agents", name, "memory"]) => {
            json_reply(req, 200, json!(rt.store().memories(name)))
        }
        // One trace as OTLP/JSON (open it in any OpenTelemetry tool), or as
        // plain run records with `?format=runs`.
        (Method::Get, ["traces", id]) => match query_param(query, "format").as_deref() {
            Some("runs") => json_reply(req, 200, json!(rt.store().trace(id))),
            _ => json_reply(req, 200, rt.trace_otlp(id)),
        },
        (Method::Get, ["store", ns]) => {
            let prefix = query_param(query, "prefix").unwrap_or_default();
            match rt.kv().list(ns, &prefix) {
                Ok(rows) => json_reply(
                    req,
                    200,
                    json!(rows
                        .into_iter()
                        .map(|(k, e)| json!({"key": k, "entry": e}))
                        .collect::<Vec<_>>()),
                ),
                Err(e) => text(req, 422, e),
            }
        }
        (Method::Get, ["store", ns, key]) => match rt.kv().get(ns, &percent_decode(key)) {
            Ok(Some(e)) => json_reply(req, 200, json!(e)),
            Ok(None) => text(req, 404, "no such key"),
            Err(e) => text(req, 422, e),
        },
        // The body is the value. A write that changes it wakes `StoreChange` watchers.
        (Method::Put, ["store", ns, key]) => {
            match rt.put_store(ns, &percent_decode(key), &body, "admin") {
                Ok(p) => json_reply(req, 200, json!({"changed": p.changed, "version": p.version})),
                Err(e) => text(req, 422, e),
            }
        }
        (Method::Get, ["topics"]) => json_reply(req, 200, json!(rt.bus().topics())),
        (Method::Get, ["topics", topic]) => {
            let after = query_param(query, "after").and_then(|v| v.parse().ok()).unwrap_or(0);
            match rt.bus().read_after(topic, after, 200) {
                Ok(evs) => json_reply(req, 200, json!(evs)),
                Err(e) => text(req, 422, e),
            }
        }
        // Speech (see speech.rs): audio in -> text, text -> audio out. 501 when the
        // engine is not configured, 422 when the input is bad.
        // Embeddings, for the bridge and other callers: the runtime owns the service (and starts it
        // when it is on demand). 501 when none is configured, 503 when it cannot answer.
        (Method::Post, ["embed"]) => match serde_json::from_str::<Value>(&body) {
            Err(e) => text(req, 400, format!("bad json: {e}")),
            Ok(v) => {
                let texts: Vec<String> = v["texts"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect())
                    .unwrap_or_default();
                let kind = if v["kind"] == "query" { "query" } else { "document" };
                if texts.is_empty() {
                    text(req, 400, "`texts` must be a non-empty array of strings")
                } else if !rt.has_embedder() {
                    text(req, 501, "no embedding service is configured")
                } else {
                    match rt.embed_texts(&texts, kind) {
                        Some(vectors) => json_reply(req, 200, json!({"vectors": vectors})),
                        None => text(req, 503, "the embedding service did not answer"),
                    }
                }
            }
        },
        (Method::Get, ["speech"]) => json_reply(
            req,
            200,
            json!({
                "transcribe": rt.speech().can_transcribe(),
                "speak": rt.speech().can_speak(),
                "locale": rt.speech().config().default_locale,
            }),
        ),
        (Method::Post, ["speech", "transcribe"]) => {
            match rt.speech().transcribe(&raw, query_param(query, "lang").as_deref()) {
                Ok(text) => json_reply(req, 200, json!({ "text": text })),
                Err(e) if e.contains("not configured") => text(req, 501, e),
                Err(e) => text(req, 422, e),
            }
        }
        (Method::Post, ["speech", "speak"]) => {
            match rt.speech().speak(&body, query_param(query, "voice").as_deref()) {
                Ok(sp) => bytes_reply(
                    req,
                    "audio/ogg",
                    sp.audio,
                    &[
                        ("x-holon-duration-ms", sp.duration_ms.to_string()),
                        (
                            "x-holon-waveform",
                            sp.waveform.iter().map(u16::to_string).collect::<Vec<_>>().join(","),
                        ),
                    ],
                ),
                Err(e) if e.contains("not available") => text(req, 501, e),
                Err(e) => text(req, 422, e),
            }
        }
        // Projects: membership is the grant (see projects.rs).
        (Method::Get, ["projects"]) => json_reply(req, 200, json!(rt.store().list_projects())),
        (Method::Get, ["projects", name]) => match rt.store().get_project(name) {
            Some(p) => json_reply(req, 200, json!(p)),
            None => text(req, 404, "no such project"),
        },
        (Method::Put, ["projects", name]) => {
            match serde_json::from_str::<crate::projects::Project>(&body) {
                Err(e) => text(req, 400, format!("bad project: {e}")),
                Ok(p) if p.name != *name => text(req, 400, "name in body does not match the URL"),
                Ok(p) => match rt.put_project(p) {
                    Ok(()) => text(req, 200, "ok"),
                    Err(e) => text(req, 422, e),
                },
            }
        }
        (Method::Post, ["projects", name, verb @ ("archive" | "unarchive")]) => {
            let done = if *verb == "archive" {
                rt.archive_project(name).map(|v| json!({"paused": v}))
            } else {
                rt.unarchive_project(name).map(|v| json!({"resumed": v}))
            };
            match done {
                Ok(v) => json_reply(req, 200, v),
                Err(e) => text(req, 404, e),
            }
        }
        (Method::Delete, ["projects", name]) => match rt.delete_project(name) {
            Ok(()) => text(req, 200, "deleted"),
            Err(e) => text(req, 404, e),
        },
        (Method::Post, ["projects", name, "agents", agent]) => match rt.add_to_project(name, agent)
        {
            Ok(()) => text(req, 200, "ok"),
            Err(e) if e.starts_with("no project") => text(req, 404, e),
            Err(e) => text(req, 422, e),
        },
        (Method::Delete, ["projects", name, "agents", agent]) => {
            match rt.remove_from_project(name, agent) {
                Ok(()) => text(req, 200, "ok"),
                Err(e) => text(req, 404, e),
            }
        }
        (Method::Get, ["approvals"]) => {
            let v: Vec<Value> = rt
                .pending_approvals()
                .into_iter()
                .map(|p| json!({"id": p.id, "agent": p.agent, "tool": p.tool, "args": p.args, "chain": p.chain}))
                .collect();
            json_reply(req, 200, json!(v))
        }
        (Method::Post, ["approvals", id, verdict @ ("approve" | "deny")]) => {
            // `?scope=task|session` is how far an approval of a coding session's tool call reaches
            // (ADR-0114); an agent's own sensitive call has one reach and ignores it.
            let scope = query_param(query, "scope").unwrap_or_else(|| "once".into());
            if !matches!(scope.as_str(), "once" | "task" | "session") {
                return text(req, 400, "scope is once, task or session");
            }
            match id.parse::<u64>() {
                Ok(id) if rt.resolve_approval_scoped(id, *verdict == "approve", &scope) => {
                    text(req, 200, "ok")
                }
                _ => text(req, 404, "no such pending approval"),
            }
        }
        // What happens, as it happens: one JSON event per line, for as long as the caller stays.
        // Written to the socket, not through a Response: tiny_http holds a chunked body back in 8 KB blocks.
        (Method::Get, ["stream"]) => {
            let rx = rt.subscribe();
            let mut w = req.into_writer();
            let head = "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n";
            if w.write_all(head.as_bytes()).and_then(|_| w.flush()).is_err() {
                return;
            }
            loop {
                // A blank line when quiet, so a client that left is noticed and this ends.
                let mut line = match rx.recv_timeout(std::time::Duration::from_secs(15)) {
                    Ok(ev) => serde_json::to_vec(&ev).unwrap_or_default(),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Vec::new(),
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                };
                line.push(b'\n');
                if w.write_all(&line).and_then(|_| w.flush()).is_err() {
                    return;
                }
            }
        }
        (Method::Get, ["keys"]) => match rt.keys() {
            Ok(k) => json_reply(req, 200, json!(k)),
            Err(e) => text(req, 503, e),
        },
        // `{"host", "key", "header"?}`; the reply never carries the key back.
        (Method::Put, ["keys", name]) => {
            let v: Value = serde_json::from_str(&body).unwrap_or_default();
            let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
            match rt.put_key(name, &s("host"), &s("key"), &s("header")) {
                Ok(()) => text(req, 200, "ok"),
                Err(e) => text(req, 422, e),
            }
        }
        (Method::Delete, ["keys", name]) => match rt.remove_key(name) {
            Ok(()) => text(req, 200, "deleted"),
            Err(e) => text(req, 422, e),
        },
        (m @ (Method::Put | Method::Delete), ["keys", key, "agents", agent]) => {
            match rt.grant_key(key, agent, *m == Method::Put) {
                Ok(()) => text(req, 200, "ok"),
                Err(e) => text(req, 422, e),
            }
        }
        // The coding sessions agents have started, newest first (docs/adr/0119).
        (Method::Get, ["sessions"]) => {
            let v: Vec<Value> =
                rt.sessions().iter().map(crate::session::Describe::to_json).collect();
            json_reply(req, 200, json!(v))
        }
        _ => text(req, 404, "not found"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::Config;
    use crate::spec::{Capability, ModelSpec, Trigger};

    fn start(tag: &str) -> (Arc<Runtime>, String) {
        let d = crate::testutil::dir(&format!("ar-srv-{tag}"));
        let rt = Runtime::new(Config::new(d)).unwrap();
        let addr = serve(rt.clone(), "127.0.0.1:0", "tok".into()).unwrap();
        (rt, format!("http://{addr}"))
    }

    fn mock(name: &str, replies: &[&str]) -> AgentSpec {
        let mut s = AgentSpec::new(name, "a test agent");
        s.model = ModelSpec::Mock { replies: replies.iter().map(|r| r.to_string()).collect() };
        s
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("a%20b+c%2"), "a b c%2");
        assert_eq!(percent_decode("%e2%9c%93"), "✓");
    }

    #[test]
    fn open_tier_runs_agents_and_admin_tier_needs_the_token() {
        let (rt, base) = start("http");
        rt.create_agent(mock("echo", &["pong"])).unwrap();
        let c = reqwest::blocking::Client::new();

        let r = c.get(format!("{base}/agents/echo/run?q=ping")).send().unwrap();
        assert_eq!((r.status().as_u16(), r.text().unwrap().as_str()), (200, "pong"));
        assert_eq!(c.get(format!("{base}/agents/nobody/run")).send().unwrap().status(), 404);
        assert_eq!(
            c.get(format!("{base}/agents/echo/ping")).send().unwrap().text().unwrap(),
            "pong"
        );
        assert_eq!(rt.store().runs("echo", 10).len(), 1, "a ping is not a run");

        assert_eq!(c.get(format!("{base}/agents")).send().unwrap().status(), 401);
        assert_eq!(
            c.get(format!("{base}/agents")).bearer_auth("wrong").send().unwrap().status(),
            401
        );
        let list: Value =
            c.get(format!("{base}/agents")).bearer_auth("tok").send().unwrap().json().unwrap();
        assert_eq!(list[0]["name"], "echo");
        let runs: Value = c
            .get(format!("{base}/agents/echo/runs"))
            .bearer_auth("tok")
            .send()
            .unwrap()
            .json()
            .unwrap();
        assert_eq!(runs[0]["input"], "ping");
        assert_eq!(runs[0]["trigger"], "http");

        assert!(c
            .post(format!("{base}/agents/echo/pause"))
            .bearer_auth("tok")
            .send()
            .unwrap()
            .status()
            .is_success());
        assert_eq!(c.get(format!("{base}/agents/echo/run?q=x")).send().unwrap().status(), 423);
        assert_eq!(c.get(format!("{base}/agents/echo/ping")).send().unwrap().status(), 423);
    }

    #[test]
    fn the_stream_says_what_happens_as_it_happens_and_only_to_the_operator() {
        use std::io::BufRead;
        let (rt, base) = start("stream");
        rt.create_agent(mock("echo", &["pong"])).unwrap();
        let c = reqwest::blocking::Client::new();
        assert_eq!(c.get(format!("{base}/stream")).send().unwrap().status(), 401);
        let r = c.get(format!("{base}/stream")).bearer_auth("tok").send().unwrap();
        let mut lines = std::io::BufReader::new(r).lines();
        c.get(format!("{base}/agents/echo/run?q=ping")).send().unwrap();
        let first: agent_wire::Event =
            serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
        assert!(
            matches!(first, agent_wire::Event::RunStarted { ref agent, .. } if agent == "echo")
        );
    }

    #[test]
    fn a_message_written_in_a_project_room_records_the_project() {
        let (rt, base) = start("run-project");
        let c = reqwest::blocking::Client::new();
        for n in ["in", "out"] {
            assert_eq!(
                c.put(format!("{base}/agents/{n}"))
                    .bearer_auth("tok")
                    .json(&mock(n, &["hi"]))
                    .send()
                    .unwrap()
                    .status(),
                201
            );
        }
        rt.put_project(crate::projects::Project {
            name: "rowing".into(),
            description: String::new(),
            agents: vec!["in".into()],
            lead: None,
            ..Default::default()
        })
        .unwrap();
        let run = |n: &str| {
            c.post(format!("{base}/agents/{n}/run?project=rowing"))
                .bearer_auth("tok")
                .body("hello")
                .send()
                .unwrap()
        };
        assert_eq!(run("in").status(), 200);
        assert_eq!(rt.store().runs("in", 1)[0].project, "rowing");
        assert_eq!(run("out").status(), 429, "not a member: dropped, not run");
        assert_eq!(rt.store().runs("out", 1)[0].status, Status::Dropped);
    }

    #[test]
    fn a_project_is_archived_and_unarchived_over_http() {
        let (rt, base) = start("archive-http");
        let c = reqwest::blocking::Client::new();
        assert_eq!(
            c.put(format!("{base}/agents/a"))
                .bearer_auth("tok")
                .json(&mock("a", &["hi"]))
                .send()
                .unwrap()
                .status(),
            201
        );
        rt.put_project(crate::projects::Project {
            name: "p".into(),
            agents: vec!["a".into()],
            ..Default::default()
        })
        .unwrap();
        let post = |path: &str| c.post(format!("{base}{path}")).bearer_auth("tok").send().unwrap();
        let v: Value = post("/projects/p/archive").json().unwrap();
        assert_eq!(v["paused"], json!(["a"]));
        let got: Value =
            c.get(format!("{base}/projects/p")).bearer_auth("tok").send().unwrap().json().unwrap();
        assert_eq!(got["archived"], true);
        let v: Value = post("/projects/p/unarchive").json().unwrap();
        assert_eq!(v["resumed"], json!(["a"]));
        assert_eq!(post("/projects/missing/archive").status(), 404);
    }

    #[test]
    fn a_run_can_be_started_with_photos_from_the_agents_inbox() {
        let (rt, base) = start("attach-http");
        let c = reqwest::blocking::Client::new();
        c.put(format!("{base}/agents/chef"))
            .bearer_auth("tok")
            .json(&mock("chef", &["it is rice"]))
            .send()
            .unwrap();
        let file = rt.store().save_inbox("chef", "lunch.png", &[1, 2, 3]).unwrap();
        let run = |query: &str| {
            c.post(format!("{base}/agents/chef/run{query}"))
                .bearer_auth("tok")
                .body("what is this?")
                .send()
                .unwrap()
        };
        // a photo that is not in the inbox, or a name that leaves it, is refused before anything runs
        let missing = run("?attach=nope.png");
        assert_eq!(missing.status(), 400);
        assert!(missing.text().unwrap().contains("no attachment `nope.png`"));
        assert_eq!(run("?attach=..%2F..%2Fetc%2Fpasswd").status(), 400);
        assert_eq!(run("?attach=1.png,2.png,3.png,4.png,5.png").status(), 400, "at most four");
        assert!(rt.store().runs("chef", 5).is_empty(), "nothing ran");
        // an existing one goes, and the record says so
        let ok = run(&format!("?attach={file}"));
        assert_eq!(ok.status(), 200);
        assert_eq!(ok.text().unwrap(), "it is rice");
        assert_eq!(rt.store().runs("chef", 1)[0].attachments, [file]);
        // another agent's inbox is not reachable by name
        c.put(format!("{base}/agents/other"))
            .bearer_auth("tok")
            .json(&mock("other", &["x"]))
            .send()
            .unwrap();
        let theirs = c
            .post(format!(
                "{base}/agents/other/run?attach={}",
                rt.store().runs("chef", 1)[0].attachments[0]
            ))
            .bearer_auth("tok")
            .body("x")
            .send()
            .unwrap();
        assert_eq!(theirs.status(), 400);
    }

    #[test]
    fn a_photo_sent_to_an_agent_lands_in_its_inbox_and_nowhere_else() {
        let (rt, base) = start("inbox-http");
        let c = reqwest::blocking::Client::new();
        c.put(format!("{base}/agents/chef"))
            .bearer_auth("tok")
            .json(&mock("chef", &["hi"]))
            .send()
            .unwrap();
        let send = |agent: &str, name: &str, bytes: Vec<u8>| {
            c.post(format!("{base}/agents/{agent}/inbox?name={name}"))
                .bearer_auth("tok")
                .body(bytes)
                .send()
                .unwrap()
        };
        let r = send("chef", "lunch.JPG", vec![0xff, 0xd8, 0xff, 1, 2, 3]);
        assert_eq!(r.status(), 201);
        let v: Value = r.json().unwrap();
        let file = v["file"].as_str().unwrap().to_string();
        assert!(file.ends_with("-lunch.JPG") && v["path"] == format!("inbox/{file}"), "{v}");
        let on_disk = rt.store().workspace("chef").unwrap().join("inbox").join(&file);
        assert_eq!(std::fs::read(on_disk).unwrap(), [0xff, 0xd8, 0xff, 1, 2, 3]);

        // a path in the name is cut to its last component and made safe: it cannot escape
        let v: Value = send("chef", "../../../etc/evil dinner.png", vec![1]).json().unwrap();
        assert!(v["file"].as_str().unwrap().ends_with("-evil_dinner.png"), "{v}");
        assert!(!rt.store().workspace("chef").unwrap().join("../../etc").exists());
        // not a photo, nothing sent, nobody to send it to, and no token
        assert_eq!(send("chef", "notes.sh", vec![1]).status(), 422);
        assert_eq!(send("chef", "a.png", vec![]).status(), 422);
        assert_eq!(send("nobody", "a.png", vec![1]).status(), 404);
        let anon =
            c.post(format!("{base}/agents/chef/inbox?name=a.png")).body(vec![1u8]).send().unwrap();
        assert_eq!(anon.status(), 401, "an admin route");
    }

    #[test]
    fn a_deleted_agent_is_listed_restored_and_purged_over_http() {
        let (_rt, base) = start("trash-http");
        let c = reqwest::blocking::Client::new();
        let call = |m: reqwest::Method, path: &str| {
            c.request(m, format!("{base}{path}")).bearer_auth("tok").send().unwrap()
        };
        for n in ["a", "b"] {
            let r = c
                .put(format!("{base}/agents/{n}"))
                .bearer_auth("tok")
                .json(&mock(n, &["hi"]))
                .send()
                .unwrap();
            assert_eq!(r.status(), 201);
            assert!(call(reqwest::Method::DELETE, &format!("/agents/{n}")).status().is_success());
        }
        let listed: Value = call(reqwest::Method::GET, "/trash").json().unwrap();
        assert_eq!(listed.as_array().unwrap().len(), 2);
        let back = call(reqwest::Method::POST, "/trash/a/restore");
        assert_eq!(back.status(), 200);
        assert_eq!(call(reqwest::Method::GET, "/agents/a").status(), 200, "it is back");
        assert_eq!(
            call(reqwest::Method::POST, "/trash/a/restore").status(),
            409,
            "nothing to restore"
        );
        assert_eq!(call(reqwest::Method::DELETE, "/trash/b").status(), 200, "purged");
        assert_eq!(call(reqwest::Method::DELETE, "/trash/b").status(), 404);
        assert_eq!(
            call(reqwest::Method::DELETE, "/trash/..%2Fagents").status(),
            404,
            "an id is never a path"
        );
        let unauth = c.get(format!("{base}/trash")).send().unwrap();
        assert_eq!(unauth.status(), 401, "the trash is an admin route");
    }

    #[test]
    fn agents_can_be_created_updated_and_deleted_over_http() {
        let (_rt, base) = start("crud");
        let c = reqwest::blocking::Client::new();
        let mut spec = mock("made", &["hi"]);
        let put = |s: &AgentSpec| {
            c.put(format!("{base}/agents/made")).bearer_auth("tok").json(s).send().unwrap()
        };
        assert_eq!(put(&spec).status(), 201);
        spec.description = "edited".into();
        assert_eq!(put(&spec).status(), 200);
        let got: Value =
            c.get(format!("{base}/agents/made")).bearer_auth("tok").send().unwrap().json().unwrap();
        assert_eq!(got["description"], "edited");
        spec.triggers.push(Trigger::Schedule {
            cron: "bogus".into(),
            prompt: "p".into(),
            notify: false,
            project: None,
        });
        assert_eq!(put(&spec).status(), 422, "a bad cron is refused, not silently never fired");
        assert!(c
            .delete(format!("{base}/agents/made"))
            .bearer_auth("tok")
            .send()
            .unwrap()
            .status()
            .is_success());
        assert_eq!(
            c.get(format!("{base}/agents/made")).bearer_auth("tok").send().unwrap().status(),
            404
        );
    }

    #[test]
    fn an_approval_blocks_the_run_until_a_human_answers() {
        let (rt, base) = start("approve");
        let mut s = mock(
            "writer",
            &[r#"{"tool":"write_file","args":{"path":"n.txt","content":"x"}}"#, "wrote it"],
        );
        s.capabilities.push(Capability::named("write_file"));
        rt.create_agent(s).unwrap();

        let b2 = base.clone();
        let runner = std::thread::spawn(move || {
            reqwest::blocking::get(format!("{b2}/agents/writer/run?q=go")).unwrap().text().unwrap()
        });
        let c = reqwest::blocking::Client::new();
        let pending = loop {
            let v: Value = c
                .get(format!("{base}/approvals"))
                .bearer_auth("tok")
                .send()
                .unwrap()
                .json()
                .unwrap();
            if let Some(p) = v.get(0) {
                break p.clone();
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        assert_eq!(
            (pending["agent"].as_str(), pending["tool"].as_str()),
            (Some("writer"), Some("write_file"))
        );
        assert!(!runner.is_finished(), "run is parked on the approval");
        let id = pending["id"].as_u64().unwrap();
        assert!(c
            .post(format!("{base}/approvals/{id}/approve"))
            .bearer_auth("tok")
            .send()
            .unwrap()
            .status()
            .is_success());
        assert_eq!(runner.join().unwrap(), "wrote it");
        assert!(rt.store().workspace("writer").unwrap().join("n.txt").exists());
    }

    #[test]
    fn events_wake_subscribers_and_schedules_fire() {
        let (rt, base) = start("trig");
        let mut sub = mock("listener", &["handled"]);
        sub.triggers.push(Trigger::Event { topic: "deploy".into(), filter: None });
        rt.create_agent(sub).unwrap();
        let mut tick = mock("ticker", &["tock"]);
        tick.triggers.push(Trigger::Schedule {
            cron: "@every 1s".into(),
            prompt: "tick".into(),
            notify: false,
            project: None,
        });
        rt.create_agent(tick).unwrap();

        let r: Value = reqwest::blocking::Client::new()
            .post(format!("{base}/events/deploy"))
            .body("v2 shipped")
            .send()
            .unwrap()
            .json()
            .unwrap();
        assert_eq!(r["woken"], 1);
        rt.start_scheduler();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let l = rt.store().runs("listener", 5);
            let t = rt.store().runs("ticker", 5);
            if !l.is_empty() && !t.is_empty() {
                assert_eq!(
                    (l[0].trigger.as_str(), l[0].input.as_str()),
                    ("event: deploy", "v2 shipped")
                );
                assert_eq!(t[0].trigger, "schedule: @every 1s");
                assert_eq!(t[0].input, "tick");
                break;
            }
            assert!(std::time::Instant::now() < deadline, "triggers never fired");
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        rt.shutdown();
    }

    #[test]
    fn agents_delegate_to_each_other_and_cannot_loop_forever() {
        let (rt, base) = start("deleg");
        let mut boss = mock(
            "boss",
            &[r#"{"tool":"agent:worker","args":{"message":"do it"}}"#, "worker said: done"],
        );
        boss.capabilities.push(Capability::named("agent:worker"));
        rt.create_agent(boss).unwrap();
        rt.create_agent(mock("worker", &["done"])).unwrap();
        let r = reqwest::blocking::get(format!("{base}/agents/boss/run?q=go")).unwrap();
        assert_eq!(r.text().unwrap(), "worker said: done");
        let w = rt.store().runs("worker", 1);
        assert_eq!((w[0].trigger.as_str(), w[0].input.as_str()), ("agent: boss", "do it"));

        // a ↔ b: each delegates to the other. The busy-lock and the depth cap
        // must end it with an error result, not a hang.
        let mut a = mock("aa", &[r#"{"tool":"agent:bb","args":{"message":"x"}}"#, "a gave up"]);
        a.capabilities.push(Capability::named("agent:bb"));
        let mut b = mock("bb", &[r#"{"tool":"agent:aa","args":{"message":"y"}}"#, "b gave up"]);
        b.capabilities.push(Capability::named("agent:aa"));
        rt.create_agent(a).unwrap();
        rt.create_agent(b).unwrap();
        let r = reqwest::blocking::get(format!("{base}/agents/aa/run?q=go")).unwrap();
        assert_eq!(r.status(), 200);
    }

    #[test]
    fn embed_route_refuses_bad_requests_and_says_when_no_service_is_configured() {
        let (_rt, base) = start("embed-route");
        let post = |body: &str| {
            reqwest::blocking::Client::new()
                .post(format!("{base}/embed"))
                .bearer_auth("tok")
                .body(body.to_string())
                .send()
                .unwrap()
                .status()
                .as_u16()
        };
        assert_eq!(post("not json"), 400);
        assert_eq!(post(r#"{"texts": []}"#), 400);
        assert_eq!(post(r#"{"texts": ["a"]}"#), 501);
    }

    #[test]
    fn a_traceparent_header_is_joined_and_the_runs_own_span_is_handed_back() {
        let (rt, base) = start("traceparent");
        rt.create_agent(mock("traced", &["hello"; 6])).unwrap();
        let c = reqwest::blocking::Client::new();
        let (trace, caller_span) = ("0af7651916cd43dd8448eb211c80319c", "b7ad6b7169203331");

        let r = c
            .get(format!("{base}/agents/traced/run?q=hi"))
            .header("traceparent", format!("00-{trace}-{caller_span}-01"))
            .send()
            .unwrap();
        let back = r.headers().get("traceparent").unwrap().to_str().unwrap().to_string();
        let tp = Traceparent::parse(&back).expect("a valid traceparent comes back");
        assert_eq!(tp.trace_id, trace, "the run joined the caller's trace");
        assert_ne!(tp.span_id, caller_span, "and has a span of its own");

        let run = rt.store().runs("traced", 1).remove(0);
        assert_eq!(run.trace_id, trace);
        assert_eq!(run.parent_span_id.as_deref(), Some(caller_span));
        assert_eq!(run.span_id, tp.span_id);

        // no header, or a malformed one: a fresh trace, never an error
        for bad in
            [None, Some("garbage"), Some("00-00000000000000000000000000000000-b7ad6b7169203331-01")]
        {
            let mut req = c.get(format!("{base}/agents/traced/run?q=again"));
            if let Some(b) = bad {
                req = req.header("traceparent", b);
            }
            let r = req.send().unwrap();
            assert_eq!(r.status(), 200);
            let t = Traceparent::parse(r.headers().get("traceparent").unwrap().to_str().unwrap())
                .unwrap();
            assert_ne!(t.trace_id, trace);
        }

        // the admin API serves the trace as OTLP/JSON, or as plain runs
        let doc: Value = c
            .get(format!("{base}/traces/{trace}"))
            .bearer_auth("tok")
            .send()
            .unwrap()
            .json()
            .unwrap();
        let spans = doc["resourceSpans"][0]["scopeSpans"][0]["spans"].as_array().unwrap();
        assert_eq!(spans[0]["parentSpanId"], caller_span);
        let runs: Value = c
            .get(format!("{base}/traces/{trace}?format=runs"))
            .bearer_auth("tok")
            .send()
            .unwrap()
            .json()
            .unwrap();
        assert_eq!(runs.as_array().unwrap().len(), 1);
        assert_eq!(c.get(format!("{base}/traces/{trace}")).send().unwrap().status(), 401);
    }

    #[test]
    fn an_event_webhook_continues_the_callers_trace_into_the_agents_it_wakes() {
        let (rt, base) = start("webhook-trace");
        rt.create_agent({
            let mut s = mock("hooked", &["handled"]);
            s.triggers.push(Trigger::Event { topic: "hook".into(), filter: None });
            s
        })
        .unwrap();
        let trace = "4bf92f3577b34da6a3ce929d0e0e4736";
        let r = reqwest::blocking::Client::new()
            .post(format!("{base}/events/hook"))
            .header("traceparent", format!("00-{trace}-00f067aa0ba902b7-01"))
            .body("payload")
            .send()
            .unwrap();
        assert_eq!(r.status(), 202);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let run = loop {
            if let Some(r) = rt.store().runs("hooked", 1).pop() {
                break r;
            }
            assert!(std::time::Instant::now() < deadline, "never woke");
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        assert_eq!(run.trace_id, trace);
        assert_eq!(run.parent_span_id.as_deref(), Some("00f067aa0ba902b7"));
    }

    #[test]
    fn the_shared_store_is_readable_and_writable_over_the_admin_api() {
        let (rt, base) = start("store-http");
        let c = reqwest::blocking::Client::new();
        let put = |v: &str| {
            c.put(format!("{base}/store/rowing/latest"))
                .bearer_auth("tok")
                .body(v.to_string())
                .send()
                .unwrap()
        };
        let r: Value = put("12,108m").json().unwrap();
        assert_eq!((r["changed"].as_bool(), r["version"].as_u64()), (Some(true), Some(1)));
        let r: Value = put("12,108m").json().unwrap();
        assert_eq!(r["changed"], false);
        let got: Value = c
            .get(format!("{base}/store/rowing/latest"))
            .bearer_auth("tok")
            .send()
            .unwrap()
            .json()
            .unwrap();
        assert_eq!((got["value"].as_str(), got["by"].as_str()), (Some("12,108m"), Some("admin")));
        let list: Value = c
            .get(format!("{base}/store/rowing?prefix=lat"))
            .bearer_auth("tok")
            .send()
            .unwrap()
            .json()
            .unwrap();
        assert_eq!(list.as_array().unwrap().len(), 1);
        assert_eq!(c.get(format!("{base}/store/rowing/latest")).send().unwrap().status(), 401);
        assert_eq!(
            c.put(format!("{base}/store/..%2Fx/k"))
                .bearer_auth("tok")
                .body("v")
                .send()
                .unwrap()
                .status(),
            422
        );
        let log: Value = c
            .get(format!("{base}/topics/store.rowing"))
            .bearer_auth("tok")
            .send()
            .unwrap()
            .json()
            .unwrap();
        assert_eq!(log.as_array().unwrap().len(), 1);
        assert_eq!(rt.bus().head("store.rowing").unwrap(), 1);
    }

    #[test]
    fn projects_can_be_managed_over_the_admin_api() {
        let (rt, base) = start("projects-http");
        rt.create_agent(mock("rower", &["x"])).unwrap();
        rt.create_agent(mock("coach", &["x"])).unwrap();
        let c = reqwest::blocking::Client::new();
        let put = |body: Value| {
            c.put(format!("{base}/projects/rowing")).bearer_auth("tok").json(&body).send().unwrap()
        };
        assert_eq!(put(json!({"name": "rowing", "agents": ["rower"]})).status(), 200);
        assert_eq!(put(json!({"name": "rowing", "agents": ["ghost"]})).status(), 422);
        assert_eq!(
            put(json!({"name": "other", "agents": []})).status(),
            400,
            "URL and body must agree"
        );

        let add = c
            .post(format!("{base}/projects/rowing/agents/coach"))
            .bearer_auth("tok")
            .send()
            .unwrap();
        assert_eq!(add.status(), 200);
        let p: Value = c
            .get(format!("{base}/projects/rowing"))
            .bearer_auth("tok")
            .send()
            .unwrap()
            .json()
            .unwrap();
        assert_eq!(p["agents"], json!(["rower", "coach"]));
        let list: Value =
            c.get(format!("{base}/projects")).bearer_auth("tok").send().unwrap().json().unwrap();
        assert_eq!(list.as_array().unwrap().len(), 1);

        assert_eq!(
            c.delete(format!("{base}/projects/rowing/agents/rower"))
                .bearer_auth("tok")
                .send()
                .unwrap()
                .status(),
            200
        );
        assert_eq!(
            c.post(format!("{base}/projects/none/agents/coach"))
                .bearer_auth("tok")
                .send()
                .unwrap()
                .status(),
            404
        );
        assert_eq!(c.get(format!("{base}/projects")).send().unwrap().status(), 401, "admin tier");
        assert_eq!(
            c.delete(format!("{base}/projects/rowing")).bearer_auth("tok").send().unwrap().status(),
            200
        );
        assert_eq!(
            c.get(format!("{base}/projects/rowing")).bearer_auth("tok").send().unwrap().status(),
            404
        );
    }
}
