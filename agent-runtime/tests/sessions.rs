//! An agent starts a coding session (docs/adr/0119): the limits its spec grants, the hand-off to
//! `comp-agentd`, the watching, and the approvals that come back to people.
//!
//! The daemon here is a stand-in that speaks the REST contract (`api/openapi.yaml`) and is driven
//! by what the runtime asks of it, so these are deterministic and need no model and no network. A
//! run against the real `comp-agentd --provider mock` is `sessions_against_the_real_daemon`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agent_runtime::agent::{Cause, Event};
use agent_runtime::session::{AgentdConfig, Phase};
use agent_runtime::store::{RunRecord, Step};
use agent_runtime::{AgentSpec, Capability, Config, ModelSpec, Runtime};
use serde_json::{json, Value};

fn dir(tag: &str) -> std::path::PathBuf {
    tempfile::Builder::new().prefix(&format!("ar-sess-{tag}-")).tempdir().unwrap().keep()
}

fn wait_for<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(30));
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Stage {
    Working,
    Waiting,
    Done,
}

/// What the stand-in daemon was asked, and where its one session stands.
struct Stub {
    stage: Stage,
    /// State reads so far: the session asks for approval on the third.
    reads: usize,
    created: Vec<Value>,
    tasks: Vec<(String, String)>,
    approvals: Vec<(String, Value)>,
    closed: Vec<String>,
    auth: Vec<String>,
    /// Stop answering, as a daemon that died does.
    down: bool,
}

fn usage(micros: u64) -> Value {
    json!({"input_tokens": 100, "output_tokens": 20, "cache_read_tokens": 0,
           "cache_write_tokens": 0, "cost_usd_micros": micros})
}

fn read_call() -> Value {
    json!({"type": "tool_call", "seq": 3, "task_id": "task-1", "call_id": "call-1",
           "tool_name": "read_file", "input": {"path": "src/lib.rs"}, "status": "finished",
           "decision": null, "output": "..", "is_error": false, "duration_ms": 4})
}

fn write_call(status: &str, decision: Value) -> Value {
    json!({"type": "tool_call", "seq": 5, "task_id": "task-1", "call_id": "call-2",
           "tool_name": "write_file", "input": {"path": "fix.txt", "content": "x"},
           "status": status, "decision": decision})
}

impl Stub {
    fn state(&self) -> Value {
        match self.stage {
            Stage::Working => json!({
                "session": {"id": "ses-1", "model": "test-model", "workspace_dir": "/w",
                            "status": "running", "usage": usage(1_500)},
                "seq": 3, "transcript": [read_call()], "pending_approvals": [],
            }),
            Stage::Waiting => json!({
                "session": {"id": "ses-1", "model": "test-model", "workspace_dir": "/w",
                            "status": "awaiting_approval", "usage": usage(40_000)},
                "seq": 5,
                "transcript": [read_call(), write_call("awaiting_approval", Value::Null)],
                "pending_approvals": [{"task_id": "task-1", "call_id": "call-2",
                    "tool_name": "write_file", "input": {"path": "fix.txt", "content": "x"}}],
            }),
            Stage::Done => json!({
                "session": {"id": "ses-1", "model": "test-model", "workspace_dir": "/w",
                            "status": "idle", "usage": usage(250_000)},
                "seq": 9,
                "transcript": [read_call(), write_call("finished", json!("approve")),
                    {"type": "task_ended", "seq": 9, "task_id": "task-1", "outcome": "completed",
                     "result": "all fixed", "usage": usage(250_000)}],
                "pending_approvals": [],
            }),
        }
    }
}

/// Starts the stand-in; returns its URL and a handle on what it saw.
fn stub_daemon() -> (String, Arc<Mutex<Stub>>) {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}", server.server_addr().to_ip().unwrap());
    let stub = Arc::new(Mutex::new(Stub {
        stage: Stage::Working,
        reads: 0,
        created: Vec::new(),
        tasks: Vec::new(),
        approvals: Vec::new(),
        closed: Vec::new(),
        auth: Vec::new(),
        down: false,
    }));
    let s = stub.clone();
    std::thread::spawn(move || {
        for mut rq in server.incoming_requests() {
            let mut body = String::new();
            let _ = std::io::Read::read_to_string(rq.as_reader(), &mut body);
            let body: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            let headers: HashMap<String, String> = rq
                .headers()
                .iter()
                .map(|h| (h.field.to_string().to_lowercase(), h.value.to_string()))
                .collect();
            let method = rq.method().to_string();
            let path = rq.url().to_string();
            let mut st = s.lock().unwrap();
            if st.down {
                // no answer at all: drop the connection
                drop(rq);
                continue;
            }
            st.auth.push(headers.get("authorization").cloned().unwrap_or_default());
            let (code, reply) = match (method.as_str(), path.as_str()) {
                ("POST", "/v1/sessions") => {
                    st.created.push(body);
                    (
                        201,
                        json!({"id": "ses-1", "model": "test-model", "workspace_dir": "/w",
                               "status": "idle", "usage": usage(0)}),
                    )
                }
                ("POST", "/v1/sessions/ses-1/tasks") => {
                    let key = headers.get("idempotency-key").cloned().unwrap_or_default();
                    st.tasks.push((body["prompt"].as_str().unwrap_or("").to_string(), key));
                    (202, json!({"id": "task-1", "session_id": "ses-1", "status": "running"}))
                }
                ("GET", "/v1/sessions/ses-1/state") => {
                    st.reads += 1;
                    if st.stage == Stage::Working && st.reads >= 3 {
                        st.stage = Stage::Waiting;
                    }
                    (200, st.state())
                }
                ("POST", p) if p.ends_with("/approval") => {
                    let call = p.split('/').nth(5).unwrap_or("").to_string();
                    st.approvals.push((call, body.clone()));
                    st.stage =
                        if body["decision"] == "approve" { Stage::Done } else { Stage::Working };
                    (200, json!({"call_id": "call-2", "decision": body["decision"]}))
                }
                ("DELETE", "/v1/sessions/ses-1") => {
                    st.closed.push("ses-1".into());
                    (200, json!({"id": "ses-1", "status": "closed"}))
                }
                _ => (404, json!({"type": "x", "title": "not found", "status": 404})),
            };
            drop(st);
            let resp = tiny_http::Response::from_string(reply.to_string())
                .with_status_code(code)
                .with_header(
                    tiny_http::Header::from_bytes(&b"content-type"[..], &b"application/json"[..])
                        .unwrap(),
                );
            let _ = rq.respond(resp);
        }
    });
    (url, stub)
}

fn runtime_with(url: Option<&str>, tag: &str) -> Arc<Runtime> {
    let mut cfg = Config::new(dir(tag));
    if let Some(url) = url {
        let mut a = AgentdConfig::new(url);
        a.token = Some("tok".into());
        a.model = "test-model".into();
        a.poll = Duration::from_millis(30);
        cfg.agentd = Some(a);
    }
    let rt = Runtime::new(cfg).unwrap();
    rt.start_scheduler();
    rt
}

/// An agent granted the session tools whose model says exactly `replies`.
fn boss(replies: &[&str]) -> AgentSpec {
    let mut s = AgentSpec::new("boss", "starts coding sessions");
    for t in ["session_start", "session_result", "session_close"] {
        s.capabilities.push(Capability::named(t));
    }
    s.model = ModelSpec::Mock { replies: replies.iter().map(|r| r.to_string()).collect() };
    s
}

fn tool_steps(rec: &RunRecord) -> Vec<(String, String, bool)> {
    rec.steps
        .iter()
        .filter_map(|s| match s {
            Step::Tool { name, result, error, .. } => Some((name.clone(), result.clone(), *error)),
            _ => None,
        })
        .collect()
}

const START: &str = r#"{"tool":"session_start","args":{"task":"make the test pass"}}"#;
const RESULT: &str = r#"{"tool":"session_result","args":{"wait_secs":10}}"#;

#[test]
fn an_agent_starts_a_session_a_person_approves_a_call_and_the_agent_gets_the_answer() {
    let (url, stub) = stub_daemon();
    let rt = runtime_with(Some(&url), "flow");
    let events = rt.subscribe();
    rt.store().put(&boss(&[START, RESULT, "the session said it is fixed"])).unwrap();

    let run = {
        let rt = rt.clone();
        std::thread::spawn(move || {
            rt.run_agent("boss", "http", "get the test passing", &Cause::default(), "", true)
        })
    };

    // The daemon is waiting on a person, and the runtime shows it as it shows any approval.
    let pending = wait_for("the session's approval", || rt.pending_approvals().pop());
    assert_eq!(pending.agent, "boss");
    assert_eq!(pending.tool, "session:write_file");
    assert_eq!(pending.args["path"], "fix.txt");
    assert!(rt.sessions()[0].phase == Phase::NeedsYou);
    assert!(rt.resolve_approval(pending.id, true));

    let rec = run.join().unwrap().unwrap();
    assert!(rec.answer.contains("the session said it is fixed"), "{}", rec.answer);
    let steps = tool_steps(&rec);
    assert_eq!(steps[0].0, "session_start");
    assert!(steps[0].1.contains("session ses-1 started"), "{}", steps[0].1);
    assert!(!steps[0].2);
    assert_eq!(steps[1].0, "session_result");
    assert!(steps[1].1.starts_with("done: all fixed"), "{}", steps[1].1);
    assert!(steps[1].1.contains("$0.25"), "{}", steps[1].1);
    assert!(steps[1].1.contains("read_file, write_file"), "{}", steps[1].1);

    // What the daemon was asked, under the limits the spec grants.
    let st = stub.lock().unwrap();
    assert_eq!(st.created.len(), 1);
    let made = &st.created[0];
    assert_eq!(made["model"], "test-model");
    assert_eq!(made["max_budget_usd_micros"], 1_000_000);
    assert_eq!(made["auto_approve_tools"], json!([]));
    assert!(made["workspace_dir"].as_str().unwrap().ends_with("workspaces/boss"));
    assert_eq!(made["labels"]["holon.agent"], "boss");
    assert_eq!(made["labels"]["holon.run"], rec.id);
    assert_eq!(st.tasks[0].0, "make the test pass");
    assert!(st.tasks[0].1.starts_with(&rec.id), "an idempotency key from the run and the call");
    assert_eq!(st.approvals.len(), 1);
    assert_eq!(st.approvals[0].0, "call-2");
    assert_eq!(st.approvals[0].1["decision"], "approve");
    assert_eq!(st.approvals[0].1["approver"], "agent-runtime");
    assert!(st.auth.iter().all(|a| a == "Bearer tok"), "{:?}", st.auth);
    assert!(st.closed.is_empty());
    drop(st);

    // What a window watching the runtime saw, in order.
    let seen: Vec<Event> = events.try_iter().collect();
    let phases: Vec<Phase> = seen
        .iter()
        .filter_map(|e| match e {
            Event::Session(v) => Some(v.phase),
            _ => None,
        })
        .collect();
    assert_eq!(phases.first(), Some(&Phase::Running));
    assert!(phases.contains(&Phase::NeedsYou));
    assert_eq!(phases.last(), Some(&Phase::Done));
    let started = seen
        .iter()
        .find_map(|e| match e {
            Event::Session(v) if v.phase == Phase::Running => Some(v.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(started.run_id, rec.id);
    assert_eq!(started.trace_id, rec.trace_id);
    let call_span = tool_span(&rec, "session_start");
    assert_eq!(started.call_span.as_deref(), Some(call_span.as_str()), "its parent in the trace");
    let last = rt.sessions().remove(0);
    assert_eq!((last.cost_micros, last.calls), (250_000, 2));
    assert_eq!(last.tools, ["read_file", "write_file"]);
    assert_eq!(last.result.as_deref(), Some("all fixed"));
    assert!(last.pending.is_empty() && last.finished_ms.is_some());
    assert!(seen.iter().any(|e| matches!(e, Event::ApprovalRequested { tool, .. }
        if tool == "session:write_file")));
    assert!(seen.iter().any(|e| matches!(e, Event::ApprovalResolved { approved: true, .. })));
    rt.shutdown();
}

fn tool_span(rec: &RunRecord, tool: &str) -> String {
    rec.steps
        .iter()
        .find_map(|s| match s {
            Step::Tool { name, span_id, .. } if name == tool => Some(span_id.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no {tool} step"))
}

#[test]
fn a_denial_reaches_the_daemon_and_an_approval_can_reach_beyond_one_call() {
    for (approve, scope, want) in [(false, "once", "deny"), (true, "task", "approve")] {
        let (url, stub) = stub_daemon();
        let rt = runtime_with(Some(&url), "scope");
        rt.store().put(&boss(&[START, "started"])).unwrap();
        rt.run_agent("boss", "http", "go", &Cause::default(), "", true).unwrap();
        let p = wait_for("the approval", || rt.pending_approvals().pop());
        assert!(rt.resolve_approval_scoped(p.id, approve, scope));
        let st = stub.lock().unwrap();
        assert_eq!(st.approvals[0].1["decision"], want);
        if approve {
            assert_eq!(st.approvals[0].1["scope"], scope);
        } else {
            assert!(st.approvals[0].1["reason"].is_string(), "a denial says why");
        }
        drop(st);
        assert!(rt.pending_approvals().is_empty(), "answered, so no longer waiting");
        rt.shutdown();
    }
}

#[test]
fn an_approval_the_daemon_will_not_take_stays_where_it_is() {
    let (url, stub) = stub_daemon();
    let rt = runtime_with(Some(&url), "refused");
    rt.store().put(&boss(&[START, "started"])).unwrap();
    rt.run_agent("boss", "http", "go", &Cause::default(), "", true).unwrap();
    let p = wait_for("the approval", || rt.pending_approvals().pop());
    stub.lock().unwrap().down = true;
    assert!(!rt.resolve_approval(p.id, true), "the daemon did not answer");
    assert_eq!(rt.pending_approvals().len(), 1, "so it can be answered again");
    rt.shutdown();
}

#[test]
fn a_workspace_outside_the_grant_is_refused_before_the_daemon_hears_of_it() {
    let (url, stub) = stub_daemon();
    let rt = runtime_with(Some(&url), "ws");
    let mut spec = boss(&[
        r#"{"tool":"session_start","args":{"task":"x","workspace":"/etc"}}"#,
        r#"{"tool":"session_start","args":{"task":"x","workspace":"repo"}}"#,
        "done",
    ]);
    spec.sessions.workspaces = vec!["/srv/work/repo".into(), "/srv/work/other".into()];
    rt.store().put(&spec).unwrap();
    let rec = rt.run_agent("boss", "http", "go", &Cause::default(), "", true).unwrap();
    let steps = tool_steps(&rec);
    assert!(steps[0].2, "an error, which the model reads");
    assert!(steps[0].1.contains("not a workspace you may open a session in"), "{}", steps[0].1);
    assert!(steps[0].1.contains("/srv/work/repo, /srv/work/other"), "{}", steps[0].1);
    // a listed one may be named by its last component
    assert!(!steps[1].2, "{}", steps[1].1);
    let st = stub.lock().unwrap();
    assert_eq!(st.created.len(), 1);
    assert_eq!(st.created[0]["workspace_dir"], "/srv/work/repo");
    drop(st);
    rt.shutdown();
}

#[test]
fn an_agent_may_have_only_as_many_sessions_open_as_its_spec_allows() {
    let (url, stub) = stub_daemon();
    let rt = runtime_with(Some(&url), "cap");
    let mut spec = boss(&[START, START, "done"]);
    spec.sessions.max_concurrent = 1;
    spec.sessions.budget_usd = 2.5;
    spec.sessions.auto_approve = vec!["read_file".into(), "list_dir".into()];
    rt.store().put(&spec).unwrap();
    let rec = rt.run_agent("boss", "http", "go", &Cause::default(), "", true).unwrap();
    let steps = tool_steps(&rec);
    assert!(!steps[0].2);
    assert!(steps[1].2 && steps[1].1.contains("already have 1 session(s) open"), "{}", steps[1].1);
    let st = stub.lock().unwrap();
    assert_eq!(st.created.len(), 1);
    assert_eq!(st.created[0]["max_budget_usd_micros"], 2_500_000);
    assert_eq!(st.created[0]["auto_approve_tools"], json!(["read_file", "list_dir"]));
    drop(st);
    rt.shutdown();
}

#[test]
fn without_a_daemon_the_tool_says_so_and_a_session_it_does_not_own_is_not_found() {
    let rt = runtime_with(None, "none");
    rt.store().put(&boss(&[START, RESULT, "done"])).unwrap();
    let rec = rt.run_agent("boss", "http", "go", &Cause::default(), "", true).unwrap();
    let steps = tool_steps(&rec);
    assert!(steps[0].2 && steps[0].1.contains("no comp-agentd is configured"), "{}", steps[0].1);
    assert!(steps[1].2 && steps[1].1.contains("you have not started a session"), "{}", steps[1].1);
    rt.shutdown();

    // another agent's session id is no session of yours
    let (url, _stub) = stub_daemon();
    let rt = runtime_with(Some(&url), "own");
    rt.store().put(&boss(&[START, "started"])).unwrap();
    rt.run_agent("boss", "http", "go", &Cause::default(), "", true).unwrap();
    let err = rt.run_agent("nosy", "http", "go", &Cause::default(), "", true);
    assert!(err.is_err(), "no such agent");
    let mut nosy =
        boss(&[r#"{"tool":"session_result","args":{"session":"ses-1","wait_secs":1}}"#, "done"]);
    nosy.name = "nosy".into();
    rt.store().put(&nosy).unwrap();
    let rec = rt.run_agent("nosy", "http", "go", &Cause::default(), "", true).unwrap();
    let steps = tool_steps(&rec);
    assert!(steps[0].2 && steps[0].1.contains("you have no session ses-1"), "{}", steps[0].1);
    rt.shutdown();
}

#[test]
fn closing_a_session_stops_it_and_takes_its_approval_away() {
    let (url, stub) = stub_daemon();
    let rt = runtime_with(Some(&url), "close");
    // two runs of the same agent: the first starts the session, the second stops it
    rt.store()
        .put(&boss(&[
            START,
            "started",
            r#"{"tool":"session_close","args":{"session":"ses-1"}}"#,
            "stopped it",
        ]))
        .unwrap();
    rt.run_agent("boss", "http", "start", &Cause::default(), "", true).unwrap();
    wait_for("the approval", || rt.pending_approvals().pop());
    let rec = rt.run_agent("boss", "http", "stop", &Cause::default(), "", true).unwrap();
    assert!(rec.answer.contains("stopped it"));
    assert_eq!(stub.lock().unwrap().closed, ["ses-1"]);
    assert_eq!(rt.sessions()[0].phase, Phase::Cancelled);
    assert!(rt.pending_approvals().is_empty());
    rt.shutdown();
}

#[test]
fn a_daemon_that_goes_away_ends_its_sessions_instead_of_leaving_them_running() {
    let (url, stub) = stub_daemon();
    let rt = runtime_with(Some(&url), "lost");
    rt.store().put(&boss(&[START, "started"])).unwrap();
    rt.run_agent("boss", "http", "go", &Cause::default(), "", true).unwrap();
    stub.lock().unwrap().down = true;
    let v = wait_for("the session to be given up on", || {
        Some(rt.sessions().remove(0)).filter(|s| s.phase.is_over())
    });
    assert_eq!(v.phase, Phase::Failed);
    assert!(v.result.unwrap().contains("lost the daemon"));
    rt.shutdown();
}

// ---------------------------------------------------------------------------------------------
// Against the real daemon.
// ---------------------------------------------------------------------------------------------

/// What the console starts its daemon with: a long random token and a state directory.
const TOKEN: &str = "3f9a1c7e5b2d4a60918e7c3b5d1f0a42";

/// A `comp-agentd` process that is killed when this is dropped.
struct Daemon(std::process::Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn daemon_binary() -> std::path::PathBuf {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target");
    let mut candidates = Vec::new();
    for p in ["debug", "release"] {
        candidates.push(root.join(p).join("holon-agentd"));
        candidates.push(root.join(p).join("comp-agentd"));
    }
    candidates.into_iter().find(|p| p.exists()).unwrap_or_else(|| {
        panic!(
            "holon-agentd is not built: run `cargo build -p holon-agentd` \
                 (looked in {})",
            root.display()
        )
    })
}

/// The runtime's contract with the daemon as it really is, not as the stand-in above reads it:
/// the daemon's scripted `mock` provider lists the workspace, which asks a person first, and
/// answers with what it found. Ignored by default because it needs the daemon built:
///   cargo test --test sessions -- --ignored
#[test]
#[ignore]
fn sessions_against_the_real_daemon() {
    let state = dir("real");
    let root = {
        let w = state.join("workspaces");
        std::fs::create_dir_all(&w).unwrap();
        std::fs::canonicalize(w).unwrap()
    };
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let _daemon = Daemon(
        std::process::Command::new(daemon_binary())
            .args(["--addr", &format!("127.0.0.1:{port}"), "--provider", "mock", "--token", TOKEN])
            .arg("--workspace-root")
            .arg(&root)
            .arg("--state-dir")
            .arg(state.join("agentd"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("start comp-agentd"),
    );
    let url = format!("http://127.0.0.1:{port}");
    wait_for("the daemon to listen", || {
        reqwest::blocking::get(format!("{url}/health")).ok().filter(|r| r.status().is_success())
    });

    let mut cfg = Config::new(&state);
    let mut a = AgentdConfig::new(&url);
    a.token = Some(TOKEN.into());
    a.model = "mock".into();
    a.poll = Duration::from_millis(50);
    cfg.agentd = Some(a);
    let rt = Runtime::new(cfg).unwrap();
    rt.start_scheduler();
    rt.store().put(&boss(&[START, RESULT, "all done"])).unwrap();
    // something in the workspace for the session to list
    std::fs::write(rt.store().workspace("boss").unwrap().join("notes.txt"), "hi").unwrap();

    let run = {
        let rt = rt.clone();
        std::thread::spawn(move || rt.run_agent("boss", "http", "go", &Cause::default(), "", true))
    };
    let p = wait_for("the daemon to ask about list_dir", || rt.pending_approvals().pop());
    assert_eq!(p.tool, "session:list_dir");
    assert!(rt.resolve_approval(p.id, true));
    let rec = run.join().unwrap().unwrap();
    let steps = tool_steps(&rec);
    assert!(steps[1].1.starts_with("done: The workspace holds:"), "{}", steps[1].1);
    assert!(steps[1].1.contains("notes.txt"), "{}", steps[1].1);
    let v = rt.sessions().remove(0);
    assert_eq!(v.phase, Phase::Done);
    assert!(v.cost_micros > 0, "the daemon priced it");
    assert_eq!(v.tools, ["list_dir"]);
    rt.shutdown();
}
