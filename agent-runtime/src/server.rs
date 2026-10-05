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

use std::io::Read;
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

    let mut body = String::new();
    let _ = req.as_reader().take(1 << 20).read_to_string(&mut body);

    let authed = req
        .headers()
        .iter()
        .find(|h| h.field.equiv("authorization"))
        .is_some_and(|h| !token.is_empty() && h.value.as_str() == format!("Bearer {token}"));

    // ---- open tier --------------------------------------------------------
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
            return match rt.run_agent(name, "http", &input, &cause, "", true) {
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

    // ---- admin tier -------------------------------------------------------
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
        (Method::Get, ["approvals"]) => {
            let v: Vec<Value> = rt
                .pending_approvals()
                .into_iter()
                .map(|p| json!({"id": p.id, "agent": p.agent, "tool": p.tool, "args": p.args, "chain": p.chain}))
                .collect();
            json_reply(req, 200, json!(v))
        }
        (Method::Post, ["approvals", id, verdict @ ("approve" | "deny")]) => {
            match id.parse::<u64>() {
                Ok(id) if rt.resolve_approval(id, *verdict == "approve") => text(req, 200, "ok"),
                _ => text(req, 404, "no such pending approval"),
            }
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

        // pausing refuses HTTP triggers too
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
        spec.triggers.push(Trigger::Schedule { cron: "bogus".into(), prompt: "p".into() });
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
        tick.triggers.push(Trigger::Schedule { cron: "@every 1s".into(), prompt: "tick".into() });
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
        // the change was published, so it shows in the topic log
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
}
