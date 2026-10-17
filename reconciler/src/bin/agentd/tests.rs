//! Every test here drives a real `comp-agentd` router on a loopback port,
//! through the same HTTP (or gRPC) a caller would use.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use super::grpc::pb;
use super::service::Daemon;
use super::session::{self, Registry};
use super::{agent, app, model, park};

struct Opts {
    token: Option<&'static str>,
    provider: model::Provider,
    prices: Vec<(String, u64, u64)>,
    approval_timeout: Option<Duration>,
    state_dir: Option<PathBuf>,
    max_queued: usize,
    /// A `comp-park` to park approvals with (ADR-0113).
    park: Option<String>,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            token: None,
            provider: model::Provider::Mock,
            prices: vec![],
            approval_timeout: None,
            state_dir: None,
            max_queued: 16,
            park: None,
        }
    }
}

struct Harness {
    base: String,
    http: reqwest::Client,
    _root: Arc<tempfile::TempDir>,
    ws: String,
}

async fn harness(o: Opts) -> Harness {
    let root = Arc::new(tempfile::tempdir().unwrap());
    serve(o, root).await
}

/// Serve a daemon over `root` (so a second daemon can reuse the first's).
async fn serve(o: Opts, root: Arc<tempfile::TempDir>) -> Harness {
    let rootp = root.path().canonicalize().unwrap();
    std::fs::write(rootp.join("README.md"), "hi").unwrap();
    let parking = o.park.is_some();
    let ctx = agent::Ctx {
        provider: o.provider,
        http: reqwest::Client::new(),
        max_turns: 5,
        approval_timeout: o.approval_timeout,
        prices: o.prices,
        off_peak: || false,
        park: o.park.map(|url| {
            // Quick retries, so a test of a park that is down does not wait seconds for nothing.
            let retry = vec![Duration::from_millis(20), Duration::from_millis(20)];
            park::Park::new(reqwest::Client::new(), url, None).with_retry(retry)
        }),
    };
    let sessions = match &o.state_dir {
        Some(d) => session::load_all(d, parking).unwrap(),
        None => Default::default(),
    };
    let d = Arc::new(Daemon {
        sessions: Registry::new(sessions),
        roots: vec![rootp.clone()],
        state_dir: o.state_dir,
        ctx: Arc::new(ctx),
        max_queued: o.max_queued,
    });
    // What `main` does once the daemon is built: tasks parked when the last one went down get
    // their tickets again.
    d.reassert_parks();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = app(d, o.token.map(String::from));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Harness { base, http: reqwest::Client::new(), ws: rootp.display().to_string(), _root: root }
}

impl Harness {
    async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        let r = self.http.post(format!("{}{path}", self.base)).json(&body).send().await.unwrap();
        (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
    }

    async fn get(&self, path: &str) -> Value {
        self.http.get(format!("{}{path}", self.base)).send().await.unwrap().json().await.unwrap()
    }

    async fn session_with(&self, extra: Value) -> String {
        let mut body =
            json!({"model": "mock", "workspace_dir": self.ws, "max_budget_usd_micros": 1_000_000});
        body.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        let (s, v) = self.post("/v1/sessions", body).await;
        assert_eq!(s, 201, "{v}");
        v["id"].as_str().unwrap().to_string()
    }

    async fn session(&self, budget: i64) -> String {
        self.session_with(json!({"max_budget_usd_micros": budget})).await
    }

    async fn task(&self, sid: &str, prompt: &str) -> Value {
        let (s, t) =
            self.post(&format!("/v1/sessions/{sid}/tasks"), json!({"prompt": prompt})).await;
        assert_eq!(s, 202, "{t}");
        t
    }

    async fn approve(&self, sid: &str, call: &Value, decision: &str) -> u16 {
        let call = call.as_str().unwrap();
        let path = format!("/v1/sessions/{sid}/tool-calls/{call}/approval");
        self.post(&path, json!({"decision": decision, "approver": "test"})).await.0
    }

    async fn stream(&self, sid: &str, after: u64) -> Events {
        let url = format!("{}/v1/sessions/{sid}/events?after_seq={after}", self.base);
        let resp = self.http.get(url).send().await.unwrap();
        assert_eq!(resp.headers()["content-type"], "text/event-stream");
        Events { resp, buf: String::new(), queue: VecDeque::new() }
    }
}

struct Events {
    resp: reqwest::Response,
    buf: String,
    queue: VecDeque<Value>,
}

impl Events {
    async fn next(&mut self) -> Value {
        loop {
            if let Some(v) = self.queue.pop_front() {
                return v;
            }
            let chunk = tokio::time::timeout(Duration::from_secs(5), self.resp.chunk())
                .await
                .expect("an event within 5s")
                .unwrap()
                .expect("stream open");
            self.buf.push_str(&String::from_utf8_lossy(&chunk));
            for data in model::take_frames(&mut self.buf) {
                if let Ok(v) = serde_json::from_str::<Value>(&data) {
                    self.queue.push_back(v);
                }
            }
        }
    }

    async fn until(&mut self, kind: &str) -> (Vec<String>, Value) {
        let mut kinds = Vec::new();
        loop {
            let v = self.next().await;
            kinds.push(v["type"].as_str().unwrap().to_string());
            if v["type"] == kind {
                return (kinds, v);
            }
        }
    }
}

// Mock turns are 1000 input + 50 output tokens on an unknown model, which
// is charged as opus: (1000*1500 + 50*7500) * 100 / 10_000 = 18_750.
const TURN: i64 = 18_750;

#[tokio::test]
async fn a_task_streams_asks_for_approval_and_completes() {
    let h = harness(Opts::default()).await;
    let sid = h.session(1_000_000).await;
    let mut ev = h.stream(&sid, 0).await;
    let task = h.task(&sid, "look").await;

    let (kinds, req) = ev.until("tool_approval_required").await;
    assert_eq!(
        kinds,
        [
            "task_started",
            "text_delta",
            "text_delta",
            "usage_updated",
            "tool_call_started",
            "tool_approval_required"
        ]
    );
    assert_eq!(req["tool_name"], "list_dir");
    assert_eq!(req["task_id"], task["id"]);

    // One task at a time: a second is refused while the first waits.
    let (s, _) = h.post(&format!("/v1/sessions/{sid}/tasks"), json!({"prompt": "again"})).await;
    assert_eq!(s, 409);
    assert_eq!(h.get(&format!("/v1/sessions/{sid}")).await["status"], "awaiting_approval");

    assert_eq!(h.approve(&sid, &req["call_id"], "approve").await, 200);
    assert_eq!(h.approve(&sid, &req["call_id"], "approve").await, 409, "no longer pending");

    let (kinds, done) = ev.until("task_completed").await;
    assert_eq!(kinds[0], "tool_approval_resolved");
    assert!(kinds.contains(&"tool_call_finished".to_string()));
    assert_eq!(done["result"], "The workspace holds: README.md.");
    assert_eq!(done["task_usage"]["cost_usd_micros"], 2 * TURN);
    assert_eq!(done["task_usage"]["input_tokens"], 2000);

    let v = h.get(&format!("/v1/sessions/{sid}")).await;
    assert_eq!(v["status"], "idle");
    assert_eq!(v["usage"]["cost_usd_micros"], 2 * TURN);

    // Resuming mid-log starts at exactly the next event, by query or header.
    assert_eq!(h.stream(&sid, 3).await.next().await["seq"], 4);
    let r = h
        .http
        .get(format!("{}/v1/sessions/{sid}/events", h.base))
        .header("Last-Event-ID", "6")
        .send()
        .await
        .unwrap();
    let mut e = Events { resp: r, buf: String::new(), queue: VecDeque::new() };
    assert_eq!(e.next().await["seq"], 7);
}

#[tokio::test]
async fn a_denied_call_is_an_error_the_model_sees_and_auto_approve_skips_asking() {
    let h = harness(Opts::default()).await;
    let sid = h.session(1_000_000).await;
    let mut ev = h.stream(&sid, 0).await;
    h.task(&sid, "look").await;
    let (_, req) = ev.until("tool_approval_required").await;
    let call = req["call_id"].as_str().unwrap();
    h.post(
        &format!("/v1/sessions/{sid}/tool-calls/{call}/approval"),
        json!({"decision": "deny", "reason": "not now"}),
    )
    .await;
    let (_, fin) = ev.until("tool_call_finished").await;
    assert_eq!(fin["is_error"], true);
    assert_eq!(fin["output"], "denied by a human: not now");
    ev.until("task_completed").await;

    let sid = h.session_with(json!({"auto_approve_tools": ["list_dir"]})).await;
    let mut ev = h.stream(&sid, 0).await;
    h.task(&sid, "look").await;
    let (kinds, _) = ev.until("task_completed").await;
    assert!(!kinds.contains(&"tool_approval_required".to_string()));
}

#[tokio::test]
async fn an_unanswered_approval_times_out_as_a_denial() {
    let h = harness(Opts { approval_timeout: Some(Duration::from_millis(200)), ..Opts::default() })
        .await;
    let sid = h.session(1_000_000).await;
    let mut ev = h.stream(&sid, 0).await;
    h.task(&sid, "look").await;
    let (_, req) = ev.until("tool_approval_required").await;
    assert!(req["expires_at"].is_string());
    let (_, res) = ev.until("tool_approval_resolved").await;
    assert_eq!(
        (res["decision"].as_str(), res["decided_by"].as_str()),
        (Some("deny"), Some("timeout"))
    );
    ev.until("task_completed").await;
}

#[tokio::test]
async fn crossing_the_budget_fails_the_task() {
    let h = harness(Opts::default()).await;
    let sid = h.session(TURN / 2).await;
    let mut ev = h.stream(&sid, 0).await;
    h.task(&sid, "look").await;
    let (kinds, failed) = ev.until("task_failed").await;
    assert!(!kinds.contains(&"tool_call_started".to_string()));
    assert_eq!(failed["code"], "budget_exceeded");
    assert_eq!(h.get(&format!("/v1/sessions/{sid}")).await["usage"]["cost_usd_micros"], TURN);
}

#[tokio::test]
async fn an_operator_price_overrides_the_table() {
    // "MOCK" against model "mock": the match ignores case.
    let h = harness(Opts { prices: vec![("MOCK".into(), 0, 0)], ..Opts::default() }).await;
    let sid = h.session_with(json!({"auto_approve_tools": ["list_dir"]})).await;
    let mut ev = h.stream(&sid, 0).await;
    h.task(&sid, "look").await;
    let (_, done) = ev.until("task_completed").await;
    assert_eq!(done["task_usage"]["cost_usd_micros"], 0);
    assert_eq!(done["task_usage"]["input_tokens"], 2000);
}

#[tokio::test]
async fn cancelling_a_task_keeps_the_session_and_the_next_task_runs() {
    let h = harness(Opts::default()).await;
    let sid = h.session(1_000_000).await;
    let mut ev = h.stream(&sid, 0).await;
    let t = h.task(&sid, "look").await;
    ev.until("tool_approval_required").await;

    let tid = t["id"].as_str().unwrap();
    let (s, c) = h.post(&format!("/v1/sessions/{sid}/tasks/{tid}/cancel"), json!({})).await;
    assert_eq!((s, c["status"].as_str()), (200, Some("failed")));
    let (_, failed) = ev.until("task_failed").await;
    assert_eq!(failed["code"], "cancelled");
    // Again is harmless: it answers with how the task ended.
    let (s, _) = h.post(&format!("/v1/sessions/{sid}/tasks/{tid}/cancel"), json!({})).await;
    assert_eq!(s, 200);
    let (s, _) = h.post(&format!("/v1/sessions/{sid}/tasks/task_nope/cancel"), json!({})).await;
    assert_eq!(s, 404);

    // The cancelled turn left a tool call with no result; the next task must
    // still get a well-formed conversation (the mock lists again).
    assert_eq!(h.get(&format!("/v1/sessions/{sid}")).await["status"], "idle");
    h.task(&sid, "look again").await;
    let (_, req) = ev.until("tool_approval_required").await;
    h.approve(&sid, &req["call_id"], "approve").await;
    ev.until("task_completed").await;
}

#[tokio::test]
async fn closing_cancels_the_waiting_task_and_an_idempotent_retry_starts_nothing() {
    let h = harness(Opts::default()).await;
    let sid = h.session(1_000_000).await;
    let mut ev = h.stream(&sid, 0).await;
    let send = || {
        h.http
            .post(format!("{}/v1/sessions/{sid}/tasks", h.base))
            .header("Idempotency-Key", "k1")
            .json(&json!({"prompt": "look"}))
            .send()
    };
    let a: Value = send().await.unwrap().json().await.unwrap();
    let b = send().await.unwrap();
    assert_eq!(b.status().as_u16(), 202);
    assert_eq!(b.json::<Value>().await.unwrap()["id"], a["id"]);
    ev.until("tool_approval_required").await;

    let r = h.http.delete(format!("{}/v1/sessions/{sid}", h.base)).send().await.unwrap();
    assert_eq!(r.json::<Value>().await.unwrap()["status"], "closed");
    let (_, failed) = ev.until("task_failed").await;
    assert_eq!(failed["code"], "cancelled");
    let (s, _) = h.post(&format!("/v1/sessions/{sid}/tasks"), json!({"prompt": "x"})).await;
    assert_eq!(s, 409);
}

#[tokio::test]
async fn a_session_survives_a_restart_and_a_cut_off_task_is_failed() {
    let state = tempfile::tempdir().unwrap();
    let root = Arc::new(tempfile::tempdir().unwrap());
    let opts = || Opts { state_dir: Some(state.path().to_path_buf()), ..Opts::default() };

    let h = serve(opts(), root.clone()).await;
    let sid = h.session(1_000_000).await;
    let mut ev = h.stream(&sid, 0).await;
    h.task(&sid, "look").await;
    let (_, req) = ev.until("tool_approval_required").await;
    h.approve(&sid, &req["call_id"], "approve").await;
    ev.until("task_completed").await;
    // A second task, cut off by the "restart" while it waits for a human.
    let cut = h.task(&sid, "look again").await;
    let (_, _) = ev.until("tool_approval_required").await;
    let before = h.get(&format!("/v1/sessions/{sid}")).await;

    let h2 = serve(opts(), root).await;
    let after = h2.get(&format!("/v1/sessions/{sid}")).await;
    assert_eq!(after["status"], "idle");
    assert_eq!(after["usage"], before["usage"], "spend is rebuilt from the log");
    assert_eq!(after["model"], "mock");

    let mut ev2 = h2.stream(&sid, 0).await;
    let (kinds, failed) = ev2.until("task_failed").await;
    assert_eq!(kinds.iter().filter(|k| *k == "task_completed").count(), 1);
    assert_eq!(
        (failed["code"].as_str(), failed["task_id"].as_str()),
        (Some("internal"), cut["id"].as_str())
    );
    assert_eq!(failed["task_usage"]["cost_usd_micros"], TURN, "the cut-off task's one turn");

    // The conversation came back too: the next task runs on it.
    h2.task(&sid, "and again").await;
    let (_, req) = ev2.until("tool_approval_required").await;
    h2.approve(&sid, &req["call_id"], "approve").await;
    ev2.until("task_completed").await;
    let history = std::fs::read_to_string(state.path().join(&sid).join("history.json")).unwrap();
    assert!(history.contains("and again") && history.contains("\"look\""));
}

#[tokio::test]
async fn a_workspace_outside_the_roots_and_a_missing_token_are_refused() {
    let h = harness(Opts::default()).await;
    let (s, v) = h
        .post(
            "/v1/sessions",
            json!({"model": "mock", "workspace_dir": "/", "max_budget_usd_micros": 1}),
        )
        .await;
    assert_eq!(s, 400, "{v}");
    assert_eq!(v["type"], "https://holon.dev/errors/bad-workspace");
    let (s, _) = h.post("/v1/sessions/ses_nope/tasks", json!({"prompt": "x"})).await;
    assert_eq!(s, 404);

    let h = harness(Opts { token: Some("sekrit"), ..Opts::default() }).await;
    let (s, _) = h.post("/v1/sessions", json!({})).await;
    assert_eq!(s, 401);
    let ok = h.http.get(format!("{}/health", h.base)).send().await.unwrap();
    assert_eq!(ok.status().as_u16(), 200);
    let mut c =
        pb::agent_service_client::AgentServiceClient::connect(h.base.clone()).await.unwrap();
    let e = c.get_session(pb::GetSessionRequest { session_id: "x".into() }).await.unwrap_err();
    assert_eq!(e.code(), tonic::Code::Unauthenticated);
}

#[tokio::test]
async fn grpc_runs_the_same_session_end_to_end() {
    use pb::agent_event::Payload as P;
    let h = harness(Opts::default()).await;
    let mut c =
        pb::agent_service_client::AgentServiceClient::connect(h.base.clone()).await.unwrap();
    let s = c
        .create_session(pb::CreateSessionRequest {
            model: "mock".into(),
            workspace_dir: h.ws.clone(),
            max_budget_usd_micros: 1_000_000,
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(s.status, pb::SessionStatus::Idle as i32);
    assert!(s.created_at.is_some());

    let mut events = c
        .stream_events(pb::StreamEventsRequest { session_id: s.id.clone(), after_seq: 0 })
        .await
        .unwrap()
        .into_inner();
    let t = c
        .send_task(pb::SendTaskRequest {
            session_id: s.id.clone(),
            prompt: "look".into(),
            idempotency_key: String::new(),
            when_busy: pb::WhenBusy::Unspecified as i32,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(t.status, pb::TaskStatus::Running as i32);
    let busy = c
        .send_task(pb::SendTaskRequest {
            session_id: s.id.clone(),
            prompt: "x".into(),
            idempotency_key: String::new(),
            when_busy: pb::WhenBusy::Unspecified as i32, // what a client that never heard of it sends
        })
        .await
        .unwrap_err();
    assert_eq!(busy.code(), tonic::Code::FailedPrecondition);

    let call_id = loop {
        let e = next(&mut events).await;
        assert_eq!(e.task_id, t.id);
        if let Some(P::ToolApprovalRequired(r)) = e.payload {
            assert_eq!(r.tool_name, "list_dir");
            assert_eq!(
                r.input.unwrap().fields["path"].kind,
                Some(prost_types::value::Kind::StringValue(".".into()))
            );
            break r.call_id;
        }
    };
    let a = c
        .submit_approval(pb::SubmitApprovalRequest {
            session_id: s.id.clone(),
            call_id,
            decision: pb::ApprovalDecision::Approve as i32,
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(a.decision, pb::ApprovalDecision::Approve as i32);
    let done = loop {
        if let Some(P::TaskCompleted(d)) = next(&mut events).await.payload {
            break d;
        }
    };
    assert_eq!(done.result, "The workspace holds: README.md.");
    assert_eq!(done.task_usage.unwrap().cost_usd_micros, 2 * TURN);

    let missing =
        c.get_session(pb::GetSessionRequest { session_id: "nope".into() }).await.unwrap_err();
    assert_eq!(missing.code(), tonic::Code::NotFound);
}

async fn next(s: &mut tonic::Streaming<pb::AgentEvent>) -> pb::AgentEvent {
    tokio::time::timeout(Duration::from_secs(5), s.message()).await.unwrap().unwrap().unwrap()
}

#[tokio::test]
async fn grpc_web_is_answered_on_the_same_port() {
    let h = harness(Opts::default()).await;
    // An empty GetSessionRequest, framed: flag 0, length 0.
    let r = h
        .http
        .post(format!("{}/holon.v1.AgentService/GetSession", h.base))
        .header("content-type", "application/grpc-web+proto")
        .body(vec![0u8, 0, 0, 0, 0])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    // Trailers-only: NOT_FOUND (5) for the empty session id, in the headers.
    assert_eq!(r.headers()["grpc-status"], "5");
}

/// A fake model server that replays `turns` (each a list of SSE `data:`
/// payloads) one request at a time, recording every request body.
async fn fake_model(
    path: &'static str,
    turns: Vec<Vec<String>>,
) -> (String, Arc<std::sync::Mutex<Vec<Value>>>) {
    use axum::response::IntoResponse;
    let seen = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let turns = Arc::new(std::sync::Mutex::new(VecDeque::from(turns)));
    let (s2, t2) = (seen.clone(), turns.clone());
    let app = axum::Router::new().route(
        path,
        axum::routing::post(move |body: axum::Json<Value>| {
            let (seen, turns) = (s2.clone(), t2.clone());
            async move {
                seen.lock().unwrap().push(body.0);
                let frames = turns.lock().unwrap().pop_front().unwrap_or_default();
                let sse: String = frames.iter().map(|f| format!("data: {f}\n\n")).collect();
                ([("content-type", "text/event-stream")], sse).into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, seen)
}

fn frames(v: &[Value]) -> Vec<String> {
    v.iter().map(|f| f.to_string()).collect()
}

#[tokio::test]
async fn the_openai_dialect_runs_a_tool_turn_over_http() {
    let (base, seen) = fake_model(
        "/v1/chat/completions",
        vec![
            frames(&[
                json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","type":"function","function":{"name":"read_file","arguments":"{\"path\":"}}]}}]}),
                json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"README.md\"}"}}]}}]}),
                json!({"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":10}}),
            ])
            .into_iter()
            .chain(["[DONE]".to_string()])
            .collect(),
            frames(&[
                json!({"choices":[{"delta":{"content":"It says "}}]}),
                json!({"choices":[{"delta":{"content":"hi."}}]}),
                json!({"choices":[],"usage":{"prompt_tokens":150,"completion_tokens":4}}),
            ]),
        ],
    )
    .await;
    let provider = model::Provider::OpenAi { base: format!("{base}/v1"), key: "k".into() };
    let h = harness(Opts { provider, ..Opts::default() }).await;
    let sid = h
        .session_with(json!({"model": "claude-sonnet-5", "auto_approve_tools": ["read_file"]}))
        .await;
    let mut ev = h.stream(&sid, 0).await;
    h.task(&sid, "what does the readme say?").await;
    let (kinds, done) = ev.until("task_completed").await;
    assert!(kinds.contains(&"tool_call_finished".to_string()));
    assert_eq!(done["result"], "It says hi.");
    // sonnet $3/$15 per M: (250*300 + 14*1500) * 100 / 10_000 = 960.
    assert_eq!(done["task_usage"]["cost_usd_micros"], 960);

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0]["stream"], true);
    assert_eq!(seen[0]["tools"].as_array().unwrap().len(), 4);
    let tool_msg = &seen[1]["messages"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(
        (tool_msg["role"].as_str(), tool_msg["content"].as_str()),
        (Some("tool"), Some("hi"))
    );
}

#[tokio::test]
async fn the_anthropic_dialect_runs_a_tool_turn_over_http() {
    let (base, seen) = fake_model(
        "/v1/messages",
        vec![
            frames(&[
                json!({"type":"message_start","message":{"usage":{"input_tokens":100,"output_tokens":1}}}),
                json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tu1","name":"list_dir","input":{}}}),
                json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\".\"}"}}),
                json!({"type":"content_block_stop","index":0}),
                json!({"type":"message_delta","usage":{"output_tokens":20}}),
            ]),
            frames(&[
                json!({"type":"message_start","message":{"usage":{"input_tokens":200,"output_tokens":1}}}),
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Only a README."}}),
                json!({"type":"message_delta","usage":{"output_tokens":5}}),
            ]),
        ],
    )
    .await;
    let provider = model::Provider::Anthropic { base, key: "k".into() };
    let h = harness(Opts { provider, ..Opts::default() }).await;
    let sid = h
        .session_with(
            json!({"model": "claude-haiku-4-5-20251001", "auto_approve_tools": ["list_dir"]}),
        )
        .await;
    let mut ev = h.stream(&sid, 0).await;
    h.task(&sid, "what is here?").await;
    let (_, done) = ev.until("task_completed").await;
    assert_eq!(done["result"], "Only a README.");
    // haiku $1/$5 per M: (300*100 + 25*500) * 100 / 10_000 = 425.
    assert_eq!(done["task_usage"]["cost_usd_micros"], 425);

    let seen = seen.lock().unwrap();
    let second = seen[1]["messages"].as_array().unwrap();
    assert_eq!(second.len(), 3);
    assert_eq!(second[2]["content"][0]["type"], "tool_result");
    assert_eq!(second[2]["content"][0]["tool_use_id"], "tu1");
}

#[tokio::test]
async fn a_model_that_cannot_be_reached_fails_the_task() {
    let h = harness(Opts {
        provider: model::Provider::OpenAi {
            base: "http://127.0.0.1:9/v1".into(),
            key: String::new(),
        },
        ..Opts::default()
    })
    .await;
    let sid = h.session(1_000_000).await;
    let mut ev = h.stream(&sid, 0).await;
    h.task(&sid, "hi").await;
    let (_, failed) = ev.until("task_failed").await;
    assert_eq!(failed["code"], "model_error");
}

// ---- steering a running task, and reading a session's state (ADR-0107) ----

impl Harness {
    async fn steer(&self, sid: &str, tid: &str, message: &str, key: Option<&str>) -> (u16, Value) {
        let mut req = self
            .http
            .post(format!("{}/v1/sessions/{sid}/tasks/{tid}/steer", self.base))
            .json(&json!({ "message": message }));
        if let Some(k) = key {
            req = req.header("Idempotency-Key", k);
        }
        let r = req.send().await.unwrap();
        (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
    }

    async fn state(&self, sid: &str) -> Value {
        self.get(&format!("/v1/sessions/{sid}/state")).await
    }
}

fn kinds_of(state: &Value) -> Vec<&str> {
    state["transcript"].as_array().unwrap().iter().map(|e| e["type"].as_str().unwrap()).collect()
}

#[tokio::test]
async fn a_steer_joins_the_running_task_at_its_next_boundary_and_the_model_reads_it() {
    let h = harness(Opts::default()).await;
    let sid = h.session(1_000_000).await;
    let mut ev = h.stream(&sid, 0).await;
    let task = h.task(&sid, "look").await;
    let tid = task["id"].as_str().unwrap();
    let (_, req) = ev.until("tool_approval_required").await;

    // Not a new task: the one that is running, unchanged.
    let (s, t) = h.steer(&sid, tid, "steer: be brief", None).await;
    assert_eq!(s, 202, "{t}");
    assert_eq!((t["id"].as_str(), t["status"].as_str()), (Some(tid), Some("running")));
    let (_, queued) = ev.until("task_steer_queued").await;
    assert_eq!(queued["message"], "steer: be brief");
    assert_eq!(queued["task_id"], task["id"]);

    // While the task waits on a person the state shows both things waiting, at the cursor the
    // stream is at.
    let st = h.state(&sid).await;
    assert_eq!(st["seq"], queued["seq"]);
    assert_eq!(st["pending_steers"][0]["steer_id"], queued["steer_id"]);
    assert_eq!(st["pending_approvals"][0]["call_id"], req["call_id"]);
    assert_eq!(st["session"]["status"], "awaiting_approval");

    // A tool is never interrupted: the steer is read after its result, before the next model call.
    h.approve(&sid, &req["call_id"], "approve").await;
    let (kinds, steered) = ev.until("task_steered").await;
    assert_eq!(kinds, ["tool_approval_resolved", "tool_call_finished", "task_steered"]);
    assert_eq!(steered["steer_id"], queued["steer_id"]);

    // And the model's next turn saw it: it answered the steer, not the listing.
    let (kinds, done) = ev.until("task_completed").await;
    assert_eq!(kinds, ["text_delta", "text_delta", "usage_updated", "task_completed"]);
    assert_eq!(done["result"], "Noted: steer: be brief");
    assert_eq!(done["task_usage"]["cost_usd_micros"], 2 * TURN, "two model turns, no more");

    let st = h.state(&sid).await;
    assert!(st["pending_steers"].as_array().unwrap().is_empty());
    assert!(
        st["transcript"].as_array().unwrap().iter().any(|e| e["type"] == "user_message"
            && e["steered"] == true
            && e["text"] == "steer: be brief"),
        "{st}"
    );
}

#[tokio::test]
async fn a_steer_is_refused_when_there_is_nothing_running_to_steer() {
    let h = harness(Opts::default()).await;
    let sid = h.session(1_000_000).await;
    let problem =
        |v: &Value| v["type"].as_str().unwrap_or_default().rsplit('/').next().unwrap().to_string();

    let (s, _) = h.steer("nope", "task_x", "hi", None).await;
    assert_eq!(s, 404, "no such session");

    let mut ev = h.stream(&sid, 0).await;
    let task = h.task(&sid, "look").await;
    let tid = task["id"].as_str().unwrap().to_string();
    let (_, req) = ev.until("tool_approval_required").await;

    let (s, _) = h.steer(&sid, &tid, "   ", None).await;
    assert_eq!(s, 400, "a blank steer says nothing");
    let (s, _) = h.steer(&sid, "task_nope", "hi", None).await;
    assert_eq!(s, 404, "no such task");

    h.approve(&sid, &req["call_id"], "approve").await;
    ev.until("task_completed").await;
    let (s, v) = h.steer(&sid, &tid, "too late", None).await;
    assert_eq!((s, problem(&v).as_str()), (409, "task-not-running"), "{v}");

    // A session that is closed refuses too.
    h.http.delete(format!("{}/v1/sessions/{sid}", h.base)).send().await.unwrap();
    let (s, v) = h.steer(&sid, &tid, "hi", None).await;
    assert_eq!((s, problem(&v).as_str()), (409, "session-closed"), "{v}");
}

#[tokio::test]
async fn a_retried_steer_with_the_same_key_is_said_once() {
    let h = harness(Opts::default()).await;
    let sid = h.session(1_000_000).await;
    let mut ev = h.stream(&sid, 0).await;
    let task = h.task(&sid, "look").await;
    let tid = task["id"].as_str().unwrap();
    let (_, req) = ev.until("tool_approval_required").await;

    for _ in 0..3 {
        let (s, _) = h.steer(&sid, tid, "steer: once", Some("k1")).await;
        assert_eq!(s, 202);
    }
    assert_eq!(h.state(&sid).await["pending_steers"].as_array().unwrap().len(), 1);

    h.approve(&sid, &req["call_id"], "approve").await;
    ev.until("task_completed").await;
    let st = h.state(&sid).await;
    let said = st["transcript"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["type"] == "user_message" && e["steered"] == true)
        .count();
    assert_eq!(said, 1);
}

fn bare_session() -> (tempfile::TempDir, session::Session, super::wire::Task) {
    let dir = tempfile::tempdir().unwrap();
    let s = session::Session::new(
        super::wire::CreateSession {
            model: "mock".into(),
            workspace_dir: dir.path().display().to_string(),
            max_budget_usd_micros: 1,
            auto_approve_tools: vec![],
            labels: Default::default(),
        },
        dir.path().to_path_buf(),
        None,
    )
    .unwrap();
    let Ok(session::Begun::Started(started)) = s.begin("p", None, super::wire::WhenBusy::Reject, 0)
    else {
        panic!("a fresh session accepts a task")
    };
    (dir, s, started.task)
}

#[test]
fn a_steer_is_applied_or_refused_never_accepted_and_dropped() {
    use session::SteerRefusal;
    let (_dir, s, task) = bare_session();
    let texts = |v: Vec<(String, String)>| v.into_iter().map(|(_, m)| m).collect::<Vec<_>>();

    assert!(s.take_steers(&task.id).is_empty());
    assert!(s.steer(&task.id, "a", None).is_ok());
    assert!(s.steer(&task.id, "b", Some("k".into())).is_ok());
    assert!(s.steer(&task.id, "b again", Some("k".into())).is_ok(), "a retry is accepted, once");

    // The model has answered, and two steers arrived while it wrote: it is not finishing, it has
    // new input, and gets them in the order they came.
    assert_eq!(texts(s.finishing(&task.id)), ["a", "b"]);
    // It answers again, and nothing arrived: now it is finishing, and says so to whoever steers.
    assert!(s.finishing(&task.id).is_empty());
    assert!(matches!(s.steer(&task.id, "late", None), Err(SteerRefusal::Finishing)));
    // But a retry of a steer that did get through is still fine.
    assert!(s.steer(&task.id, "b again", Some("k".into())).is_ok());
    // And a task that is not the running one has nothing to steer.
    assert!(matches!(s.steer("task_other", "x", None), Err(SteerRefusal::NotRunning)));
}

/// The race the `finishing` guard exists for: producers steering while the loop decides to finish.
/// Every steer that was accepted has to have been handed to the loop; none may fall between its
/// last look and the task ending.
#[test]
fn steers_racing_a_task_that_is_finishing_are_each_applied_or_refused() {
    use session::SteerRefusal;
    use std::sync::atomic::{AtomicUsize, Ordering};

    for round in 0..50 {
        let (_dir, s, task) = bare_session();
        let s = Arc::new(s);
        let accepted = Arc::new(AtomicUsize::new(0));
        let refused = Arc::new(AtomicUsize::new(0));

        let producers: Vec<_> = (0..4)
            .map(|p| {
                let (s, id, ok, no) =
                    (s.clone(), task.id.clone(), accepted.clone(), refused.clone());
                std::thread::spawn(move || {
                    for i in 0..100 {
                        match s.steer(&id, &format!("{p}-{i}"), None) {
                            Ok(()) => ok.fetch_add(1, Ordering::SeqCst),
                            Err(SteerRefusal::Finishing) => {
                                no.fetch_add(1, Ordering::SeqCst);
                                break;
                            }
                            Err(_) => panic!("neither closed nor not running"),
                        };
                        if i % 7 == 0 {
                            std::thread::yield_now();
                        }
                    }
                })
            })
            .collect();

        // The loop: a boundary now and then, then the final answer, repeated until nothing new.
        let mut applied = 0;
        for _ in 0..round % 5 {
            applied += s.take_steers(&task.id).len();
            std::thread::yield_now();
        }
        loop {
            let batch = s.finishing(&task.id);
            if batch.is_empty() {
                break;
            }
            applied += batch.len();
        }
        for p in producers {
            p.join().unwrap();
        }
        assert_eq!(
            accepted.load(Ordering::SeqCst),
            applied,
            "round {round}: a steer was accepted that the loop never read ({} refused)",
            refused.load(Ordering::SeqCst)
        );
    }
}

#[tokio::test]
async fn the_state_is_a_snapshot_that_the_stream_continues_exactly() {
    let h = harness(Opts::default()).await;
    let sid = h.session(1_000_000).await;
    let mut ev = h.stream(&sid, 0).await;
    assert_eq!(h.state(&sid).await["seq"], 0, "an empty session is at the start");

    h.task(&sid, "look").await;
    let (mut seen, req) = ev.until("tool_approval_required").await;
    // Mid-task, waiting on a person: the call is the thing to ask about.
    let st = h.state(&sid).await;
    assert_eq!(st["seq"], seen.len());
    assert_eq!(st["pending_approvals"].as_array().unwrap().len(), 1);
    assert_eq!(kinds_of(&st), ["user_message", "assistant_message", "tool_call"]);
    assert_eq!(st["transcript"][2]["status"], "awaiting_approval");

    h.approve(&sid, &req["call_id"], "approve").await;
    let (more, _) = ev.until("task_completed").await;
    seen.extend(more);

    // Done: the whole conversation, as a client draws it.
    let st = h.state(&sid).await;
    assert_eq!(st["seq"], seen.len(), "the cursor is the last event the state reflects");
    assert_eq!(
        kinds_of(&st),
        ["user_message", "assistant_message", "tool_call", "assistant_message", "task_ended"]
    );
    let t = st["transcript"].as_array().unwrap();
    assert_eq!(t[0]["text"], "look");
    assert_eq!(t[1]["text"], "Let me look at the workspace.");
    assert_eq!(t[1]["in_progress"], false);
    assert_eq!(
        (t[2]["status"].as_str(), t[2]["output"].as_str()),
        (Some("finished"), Some("README.md"))
    );
    assert_eq!(t[3]["text"], "The workspace holds: README.md.");
    assert_eq!(
        (t[4]["outcome"].as_str(), t[4]["result"].as_str()),
        (Some("completed"), Some("The workspace holds: README.md."))
    );
    assert!(st["pending_approvals"].as_array().unwrap().is_empty());
    assert_eq!(st["session"]["status"], "idle");
    assert_eq!(st["session"]["usage"]["cost_usd_micros"], 2 * TURN);

    // Snapshot, then tail: the next event after the cursor is the very next one the log makes,
    // with no gap and nothing repeated.
    let cursor = st["seq"].as_u64().unwrap();
    let next_task = h.task(&sid, "look again").await;
    let mut tail = h.stream(&sid, cursor).await;
    let first = tail.next().await;
    assert_eq!(first["seq"], cursor + 1);
    assert_eq!(
        (first["type"].as_str(), first["task_id"].as_str()),
        (Some("task_started"), next_task["id"].as_str())
    );
}

#[tokio::test]
async fn the_state_of_a_session_comes_back_with_it_after_a_restart() {
    let state = tempfile::tempdir().unwrap();
    let root = Arc::new(tempfile::tempdir().unwrap());
    let opts = || Opts { state_dir: Some(state.path().to_path_buf()), ..Opts::default() };

    let h = serve(opts(), root.clone()).await;
    let sid = h.session(1_000_000).await;
    let mut ev = h.stream(&sid, 0).await;
    h.task(&sid, "look").await;
    let (_, req) = ev.until("tool_approval_required").await;
    h.approve(&sid, &req["call_id"], "approve").await;
    ev.until("task_completed").await;
    let before = h.state(&sid).await;

    // A restart rebuilds it from the log it already trusted, so a client that reconnects to the
    // new process gets the same conversation at the same cursor.
    let h2 = serve(opts(), root).await;
    let after = h2.state(&sid).await;
    assert_eq!(after["seq"], before["seq"]);
    assert_eq!(after["transcript"], before["transcript"]);
    assert_eq!(after["session"]["usage"], before["session"]["usage"]);
}

#[tokio::test]
async fn grpc_steers_a_running_task_and_reads_its_state() {
    use pb::agent_event::Payload as P;
    use pb::transcript_entry::Entry as E;
    let h = harness(Opts::default()).await;
    let mut c =
        pb::agent_service_client::AgentServiceClient::connect(h.base.clone()).await.unwrap();
    let s = c
        .create_session(pb::CreateSessionRequest {
            model: "mock".into(),
            workspace_dir: h.ws.clone(),
            max_budget_usd_micros: 1_000_000,
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    let mut events = c
        .stream_events(pb::StreamEventsRequest { session_id: s.id.clone(), after_seq: 0 })
        .await
        .unwrap()
        .into_inner();
    let t = c
        .send_task(pb::SendTaskRequest {
            session_id: s.id.clone(),
            prompt: "look".into(),
            idempotency_key: String::new(),
            when_busy: pb::WhenBusy::Unspecified as i32,
        })
        .await
        .unwrap()
        .into_inner();
    let call_id = loop {
        if let Some(P::ToolApprovalRequired(r)) = next(&mut events).await.payload {
            break r.call_id;
        }
    };

    let steer = |message: &str, key: &str| pb::SteerTaskRequest {
        session_id: s.id.clone(),
        task_id: t.id.clone(),
        message: message.into(),
        idempotency_key: key.into(),
    };
    let steered = c.steer_task(steer("steer: be brief", "k")).await.unwrap().into_inner();
    assert_eq!(
        (steered.id.as_str(), steered.status),
        (t.id.as_str(), pb::TaskStatus::Running as i32)
    );
    c.steer_task(steer("steer: be brief", "k")).await.unwrap(); // a retry: accepted, said once
    let queued = loop {
        let e = next(&mut events).await;
        if let Some(P::TaskSteerQueued(q)) = e.payload {
            assert_eq!(q.message, "steer: be brief");
            break e.seq;
        }
    };
    let blank = c.steer_task(steer("  ", "")).await.unwrap_err();
    assert_eq!(blank.code(), tonic::Code::InvalidArgument);
    let missing = c
        .steer_task(pb::SteerTaskRequest { task_id: "task_nope".into(), ..steer("x", "") })
        .await
        .unwrap_err();
    assert_eq!(missing.code(), tonic::Code::NotFound);

    let st = c
        .get_session_state(pb::GetSessionStateRequest { session_id: s.id.clone() })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(st.seq, queued);
    assert_eq!(st.pending_approvals.len(), 1);
    assert_eq!(st.pending_approvals[0].call_id, call_id);
    assert_eq!(st.pending_steers.len(), 1);
    assert_eq!(st.session.unwrap().status, pb::SessionStatus::AwaitingApproval as i32);

    c.submit_approval(pb::SubmitApprovalRequest {
        session_id: s.id.clone(),
        call_id,
        decision: pb::ApprovalDecision::Approve as i32,
        ..Default::default()
    })
    .await
    .unwrap();
    let done = loop {
        if let Some(P::TaskCompleted(d)) = next(&mut events).await.payload {
            break d;
        }
    };
    assert_eq!(done.result, "Noted: steer: be brief");
    let late = c.steer_task(steer("again", "")).await.unwrap_err();
    assert_eq!(late.code(), tonic::Code::FailedPrecondition, "the task has ended");

    let st = c
        .get_session_state(pb::GetSessionStateRequest { session_id: s.id.clone() })
        .await
        .unwrap()
        .into_inner();
    assert!(st.pending_steers.is_empty() && st.pending_approvals.is_empty());
    let entries: Vec<_> = st.transcript.iter().map(|e| e.entry.clone().unwrap()).collect();
    // user, assistant, tool call, steered user, assistant (the answer to it), task ended
    assert!(matches!(&entries[0], E::UserMessage(u) if u.text == "look" && !u.steered));
    assert!(matches!(&entries[2], E::ToolCall(c)
        if c.status == pb::ToolCallStatus::Finished as i32
            && c.decision == pb::ApprovalDecision::Approve as i32
            && c.output == "README.md"));
    assert!(matches!(&entries[3], E::UserMessage(u) if u.text == "steer: be brief" && u.steered));
    assert!(matches!(entries.last().unwrap(), E::TaskEnded(e)
        if e.outcome == pb::TaskOutcome::Completed as i32 && e.result == "Noted: steer: be brief"));
}

// ---- queueing a task behind the running one (ADR-0112) ----

impl Harness {
    /// `SendTask` asking to wait its turn if the session is busy.
    async fn queue(&self, sid: &str, prompt: &str, key: Option<&str>) -> (u16, Value) {
        let mut req = self
            .http
            .post(format!("{}/v1/sessions/{sid}/tasks", self.base))
            .json(&json!({"prompt": prompt, "when_busy": "queue"}));
        if let Some(k) = key {
            req = req.header("Idempotency-Key", k);
        }
        let r = req.send().await.unwrap();
        (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
    }

    async fn cancel(&self, sid: &str, tid: &str) -> (u16, Value) {
        self.post(&format!("/v1/sessions/{sid}/tasks/{tid}/cancel"), json!({})).await
    }

    /// The task the session is running, if any.
    async fn active(&self, sid: &str) -> Option<String> {
        self.get(&format!("/v1/sessions/{sid}")).await["active_task_id"].as_str().map(String::from)
    }
}

/// The task ids in a state's queue, in order.
fn queued_ids(state: &Value) -> Vec<&str> {
    state["queued_tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|q| q["task_id"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn a_busy_session_queues_a_task_when_asked_and_starts_it_the_moment_the_one_before_it_ends() {
    let h = harness(Opts::default()).await;
    let sid = h.session(1_000_000).await;
    let mut ev = h.stream(&sid, 0).await;
    let a = h.task(&sid, "look a").await;
    let (_, req_a) = ev.until("tool_approval_required").await;

    // Said nothing about a busy session: refused, as it always was.
    let (s, refused) = h.post(&format!("/v1/sessions/{sid}/tasks"), json!({"prompt": "x"})).await;
    assert_eq!(s, 409);
    assert_eq!(refused["type"], "https://holon.dev/errors/task-running");

    // Asked to wait: accepted, as queued, and the log says so.
    let (s, b) = h.queue(&sid, "look b", None).await;
    assert_eq!((s, b["status"].as_str()), (202, Some("queued")), "{b}");
    let (s, c) = h.queue(&sid, "look c", None).await;
    assert_eq!((s, c["status"].as_str()), (202, Some("queued")), "{c}");
    let (_, qb) = ev.until("task_queued").await;
    let (_, qc) = ev.until("task_queued").await;
    assert_eq!((qb["task_id"].as_str(), qb["prompt"].as_str()), (b["id"].as_str(), Some("look b")));
    assert_eq!((qc["task_id"].as_str(), qc["prompt"].as_str()), (c["id"].as_str(), Some("look c")));

    // The state shows the queue in order, and the queued prompts are not in the conversation yet.
    let st = h.state(&sid).await;
    assert_eq!(queued_ids(&st), [b["id"].as_str().unwrap(), c["id"].as_str().unwrap()]);
    assert_eq!(st["queued_tasks"][0]["prompt"], "look b");
    assert_eq!(st["seq"], qc["seq"]);
    let said: Vec<&str> = st["transcript"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["type"] == "user_message")
        .map(|e| e["text"].as_str().unwrap())
        .collect();
    assert_eq!(said, ["look a"]);
    assert_eq!(h.active(&sid).await.as_deref(), a["id"].as_str(), "still a's");
    let cursor = st["seq"].as_u64().unwrap();

    // `a` ends, and `b` starts in the very next event: nothing is in between at which the session
    // is idle with a task waiting.
    assert_eq!(h.approve(&sid, &req_a["call_id"], "approve").await, 200);
    let (_, done_a) = ev.until("task_completed").await;
    assert_eq!(done_a["task_id"], a["id"]);
    let started_b = ev.next().await;
    assert_eq!(
        (started_b["type"].as_str(), started_b["task_id"].as_str(), started_b["prompt"].as_str()),
        (Some("task_started"), b["id"].as_str(), Some("look b"))
    );
    assert_eq!(started_b["seq"].as_u64(), Some(done_a["seq"].as_u64().unwrap() + 1));
    assert_eq!(h.active(&sid).await.as_deref(), b["id"].as_str());
    let st = h.state(&sid).await;
    assert_eq!(queued_ids(&st), [c["id"].as_str().unwrap()], "b left the queue as it started");

    // A client that read the state at `cursor` and tailed from there misses nothing and sees
    // nothing twice, through the queue draining.
    let mut tail = h.stream(&sid, cursor).await;
    let first = tail.next().await;
    assert_eq!(first["seq"].as_u64(), Some(cursor + 1));

    // b, then c, in the order they were accepted.
    let (_, req_b) = ev.until("tool_approval_required").await;
    assert_eq!(h.approve(&sid, &req_b["call_id"], "approve").await, 200);
    let (_, done_b) = ev.until("task_completed").await;
    assert_eq!(done_b["task_id"], b["id"]);
    let started_c = ev.next().await;
    assert_eq!(
        (started_c["type"].as_str(), started_c["task_id"].as_str()),
        (Some("task_started"), c["id"].as_str())
    );
    let (_, req_c) = ev.until("tool_approval_required").await;
    assert_eq!(h.approve(&sid, &req_c["call_id"], "approve").await, 200);
    let (_, done_c) = ev.until("task_completed").await;
    assert_eq!(done_c["task_id"], c["id"]);

    let v = h.get(&format!("/v1/sessions/{sid}")).await;
    assert_eq!(v["status"], "idle");
    assert!(h.state(&sid).await["queued_tasks"].as_array().unwrap().is_empty());
    // And each ran on the conversation the one before it left: three listings, one conversation.
    let st = h.state(&sid).await;
    let said: Vec<&str> = st["transcript"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["type"] == "user_message")
        .map(|e| e["text"].as_str().unwrap())
        .collect();
    assert_eq!(said, ["look a", "look b", "look c"]);
}

#[tokio::test]
async fn a_task_sent_to_an_idle_session_starts_whatever_it_says_about_a_busy_one() {
    let h = harness(Opts::default()).await;
    let sid = h.session(1_000_000).await;
    let mut ev = h.stream(&sid, 0).await;
    let (s, t) = h.queue(&sid, "look", None).await;
    assert_eq!((s, t["status"].as_str()), (202, Some("running")), "{t}");
    let (kinds, _) = ev.until("tool_approval_required").await;
    assert_eq!(kinds[0], "task_started");
    assert!(!kinds.contains(&"task_queued".to_string()), "never queued: {kinds:?}");
}

#[tokio::test]
async fn a_queued_task_can_be_cancelled_and_never_starts() {
    let h = harness(Opts::default()).await;
    let sid = h.session(1_000_000).await;
    let mut ev = h.stream(&sid, 0).await;
    let a = h.task(&sid, "look a").await;
    let (_, req_a) = ev.until("tool_approval_required").await;
    let (_, b) = h.queue(&sid, "look b", None).await;
    let (_, c) = h.queue(&sid, "look c", None).await;
    let (bid, cid) = (b["id"].as_str().unwrap(), c["id"].as_str().unwrap());

    let (s, cancelled) = h.cancel(&sid, bid).await;
    assert_eq!((s, cancelled["status"].as_str()), (200, Some("failed")), "{cancelled}");
    let (_, failed) = ev.until("task_failed").await;
    assert_eq!(
        (failed["task_id"].as_str(), failed["code"].as_str()),
        (Some(bid), Some("cancelled"))
    );
    assert_eq!(failed["task_usage"]["cost_usd_micros"], 0, "it never ran");
    assert_eq!(queued_ids(&h.state(&sid).await), [cid]);
    // Again is harmless, as for any task that ended.
    assert_eq!(h.cancel(&sid, bid).await.0, 200);

    // `a` ends and `c` starts: `b` is skipped, not run and not left in the way.
    h.approve(&sid, &req_a["call_id"], "approve").await;
    let (_, done_a) = ev.until("task_completed").await;
    assert_eq!(done_a["task_id"], a["id"]);
    let next = ev.next().await;
    assert_eq!(
        (next["type"].as_str(), next["task_id"].as_str()),
        (Some("task_started"), Some(cid))
    );
}

#[tokio::test]
async fn cancelling_the_running_task_starts_the_next_queued_one_and_it_runs_to_the_end() {
    let h = harness(Opts::default()).await;
    let sid = h.session(1_000_000).await;
    let mut ev = h.stream(&sid, 0).await;
    let a = h.task(&sid, "look a").await;
    ev.until("tool_approval_required").await;
    let (_, b) = h.queue(&sid, "look b", None).await;
    let (_, c) = h.queue(&sid, "look c", None).await;

    // The running task's loop is aborted by the cancel; that must not take the chain with it.
    let (s, cancelled) = h.cancel(&sid, a["id"].as_str().unwrap()).await;
    assert_eq!((s, cancelled["status"].as_str()), (200, Some("failed")));
    let (_, failed) = ev.until("task_failed").await;
    assert_eq!(failed["task_id"], a["id"]);
    let next = ev.next().await;
    assert_eq!(
        (next["type"].as_str(), next["task_id"].as_str()),
        (Some("task_started"), b["id"].as_str())
    );
    assert_eq!(next["seq"].as_u64(), Some(failed["seq"].as_u64().unwrap() + 1));

    // And b runs to its end on its own loop, then c after it.
    for t in [&b, &c] {
        let (_, req) = ev.until("tool_approval_required").await;
        assert_eq!(req["task_id"], t["id"]);
        assert_eq!(h.approve(&sid, &req["call_id"], "approve").await, 200);
        let (_, done) = ev.until("task_completed").await;
        assert_eq!(done["task_id"], t["id"]);
    }
    assert_eq!(h.get(&format!("/v1/sessions/{sid}")).await["status"], "idle");
}

#[tokio::test]
async fn closing_a_session_ends_the_running_task_and_every_queued_one() {
    let h = harness(Opts::default()).await;
    let sid = h.session(1_000_000).await;
    let mut ev = h.stream(&sid, 0).await;
    let a = h.task(&sid, "look a").await;
    ev.until("tool_approval_required").await;
    let (_, b) = h.queue(&sid, "look b", None).await;
    let (_, c) = h.queue(&sid, "look c", None).await;

    let r = h.http.delete(format!("{}/v1/sessions/{sid}", h.base)).send().await.unwrap();
    assert_eq!(r.json::<Value>().await.unwrap()["status"], "closed");

    // The running one, then the queued ones in order: each ends, none starts.
    let mut ended = Vec::new();
    for _ in 0..3 {
        let (_, failed) = ev.until("task_failed").await;
        assert_eq!(failed["code"], "cancelled");
        ended.push(failed["task_id"].as_str().unwrap().to_string());
    }
    assert_eq!(
        ended,
        [a["id"].as_str().unwrap(), b["id"].as_str().unwrap(), c["id"].as_str().unwrap()]
    );
    assert!(h.state(&sid).await["queued_tasks"].as_array().unwrap().is_empty());
    let (s, _) = h.queue(&sid, "x", None).await;
    assert_eq!(s, 409, "a closed session accepts nothing");
    // Nothing started after the close: the log ends at the third failure.
    let log = h.state(&sid).await["seq"].as_u64().unwrap();
    let after =
        tokio::time::timeout(Duration::from_millis(300), h.stream(&sid, log).await.next()).await;
    assert!(after.is_err(), "no event after the last cancellation");
}

#[tokio::test]
async fn the_queue_is_bounded_and_can_be_switched_off() {
    let h = harness(Opts { max_queued: 2, ..Opts::default() }).await;
    let sid = h.session(1_000_000).await;
    let mut ev = h.stream(&sid, 0).await;
    h.task(&sid, "look a").await;
    ev.until("tool_approval_required").await;
    assert_eq!(h.queue(&sid, "b", None).await.0, 202);
    assert_eq!(h.queue(&sid, "c", None).await.0, 202);
    let (s, full) = h.queue(&sid, "d", None).await;
    assert_eq!(s, 429, "{full}");
    assert_eq!(full["type"], "https://holon.dev/errors/queue-full");
    assert_eq!(
        h.state(&sid).await["queued_tasks"].as_array().unwrap().len(),
        2,
        "nothing was added"
    );
    // Not asking to queue is still the 409 it always was, full queue or not.
    assert_eq!(h.post(&format!("/v1/sessions/{sid}/tasks"), json!({"prompt": "x"})).await.0, 409);

    // 0 turns it off: a busy session is busy, and says why.
    let h = harness(Opts { max_queued: 0, ..Opts::default() }).await;
    let sid = h.session(1_000_000).await;
    let mut ev = h.stream(&sid, 0).await;
    h.task(&sid, "look a").await;
    ev.until("tool_approval_required").await;
    let (s, off) = h.queue(&sid, "b", None).await;
    assert_eq!(s, 409, "{off}");
    assert_eq!(off["type"], "https://holon.dev/errors/task-running");
    assert!(off["detail"].as_str().unwrap().contains("does not queue"), "{off}");
}

#[tokio::test]
async fn a_retried_queue_request_with_the_same_key_queues_once_and_always_finds_its_task() {
    let h = harness(Opts::default()).await;
    let sid = h.session(1_000_000).await;
    let mut ev = h.stream(&sid, 0).await;
    let a = h.task(&sid, "look a").await;
    let (_, req_a) = ev.until("tool_approval_required").await;

    let (_, b1) = h.queue(&sid, "look b", Some("kb")).await;
    let (s, b2) = h.queue(&sid, "look b", Some("kb")).await;
    assert_eq!((s, &b2["id"], b2["status"].as_str()), (202, &b1["id"], Some("queued")));
    assert_eq!(h.state(&sid).await["queued_tasks"].as_array().unwrap().len(), 1, "said once");

    // Once it has started, the same key still finds the same task and starts nothing more.
    h.approve(&sid, &req_a["call_id"], "approve").await;
    ev.until("task_completed").await;
    let started = ev.next().await;
    assert_eq!(started["task_id"], b1["id"]);
    let (s, b3) = h.queue(&sid, "look b", Some("kb")).await;
    assert_eq!((s, &b3["id"], b3["status"].as_str()), (202, &b1["id"], Some("running")));
    assert!(h.state(&sid).await["queued_tasks"].as_array().unwrap().is_empty());
    assert_ne!(a["id"], b1["id"]);
}

#[tokio::test]
async fn a_queued_task_cannot_be_steered_until_it_runs() {
    let h = harness(Opts::default()).await;
    let sid = h.session(1_000_000).await;
    let mut ev = h.stream(&sid, 0).await;
    h.task(&sid, "look a").await;
    ev.until("tool_approval_required").await;
    let (_, b) = h.queue(&sid, "look b", None).await;
    let (s, refused) = h.steer(&sid, b["id"].as_str().unwrap(), "steer: be brief", None).await;
    assert_eq!(s, 409, "{refused}");
    assert_eq!(refused["type"], "https://holon.dev/errors/task-not-running");
    assert!(refused["detail"].as_str().unwrap().contains("queued"), "{refused}");
}

#[tokio::test]
async fn a_restart_ends_a_task_that_was_accepted_and_never_started_and_the_caller_finds_out() {
    let state = tempfile::tempdir().unwrap();
    let root = Arc::new(tempfile::tempdir().unwrap());
    let opts = || Opts { state_dir: Some(state.path().to_path_buf()), ..Opts::default() };

    let h = serve(opts(), root.clone()).await;
    let sid = h.session(1_000_000).await;
    let mut ev = h.stream(&sid, 0).await;
    let a = h.task(&sid, "look a").await;
    ev.until("tool_approval_required").await;
    let (_, b) = h.queue(&sid, "look b", Some("kb")).await;
    let (_, c) = h.queue(&sid, "look c", None).await;
    let (_, d) = h.queue(&sid, "look d", None).await;
    // One of them is cancelled while `a` still runs. That is a task ending in the log in the
    // middle of another's run, which the rebuild must not mistake for the running task's end.
    assert_eq!(h.cancel(&sid, c["id"].as_str().unwrap()).await.0, 200);
    ev.until("task_failed").await;
    let before = h.state(&sid).await;
    assert_eq!(queued_ids(&before), [b["id"].as_str().unwrap(), d["id"].as_str().unwrap()]);

    // The "restart": a second daemon over the same directory. It does not run the queue, on a
    // prompt a process that is gone accepted. It ends it, in the log, where every reader sees.
    let h2 = serve(opts(), root).await;
    let mut ev2 = h2.stream(&sid, 0).await;
    let mut ended = Vec::new();
    loop {
        let (_, failed) = ev2.until("task_failed").await;
        ended.push((
            failed["task_id"].as_str().unwrap().to_string(),
            failed["code"].as_str().unwrap().to_string(),
            failed["message"].as_str().unwrap().to_string(),
        ));
        if ended.len() == 4 {
            break;
        }
    }
    // `c` was cancelled before the restart and is in the log as that; what the restart ended is
    // the running task, then the two still waiting, in order.
    assert_eq!(ended[0].1, "cancelled");
    assert_eq!(ended[0].0, c["id"].as_str().unwrap());
    assert_eq!(ended[1].0, a["id"].as_str().unwrap());
    assert!(ended[1].2.contains("while this task ran"), "{}", ended[1].2);
    assert_eq!((ended[2].0.as_str(), ended[2].1.as_str()), (b["id"].as_str().unwrap(), "internal"));
    assert_eq!((ended[3].0.as_str(), ended[3].1.as_str()), (d["id"].as_str().unwrap(), "internal"));
    assert!(ended[2].2.contains("before this task started"), "{}", ended[2].2);

    // Nothing is queued or running after it, and the state says the same.
    let after = h2.state(&sid).await;
    assert!(after["queued_tasks"].as_array().unwrap().is_empty());
    assert_eq!(after["session"]["status"], "idle");
    assert_eq!(h2.active(&sid).await, None);
    // The key that was said for b still finds b, and starts nothing: a retry after the restart
    // is told how it ended, not given a second copy.
    let (s, again) = h2.queue(&sid, "look b", Some("kb")).await;
    assert_eq!((s, &again["id"], again["status"].as_str()), (202, &b["id"], Some("failed")));
    // And the session works: a new task runs.
    h2.task(&sid, "look d").await;
    let (_, req) = ev2.until("tool_approval_required").await;
    h2.approve(&sid, &req["call_id"], "approve").await;
    ev2.until("task_completed").await;
}

#[tokio::test]
async fn grpc_queues_a_task_and_says_so_in_the_event_the_state_and_the_status() {
    use pb::agent_event::Payload as P;
    let h = harness(Opts { max_queued: 1, ..Opts::default() }).await;
    let mut c =
        pb::agent_service_client::AgentServiceClient::connect(h.base.clone()).await.unwrap();
    let s = c
        .create_session(pb::CreateSessionRequest {
            model: "mock".into(),
            workspace_dir: h.ws.clone(),
            max_budget_usd_micros: 1_000_000,
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    let mut events = c
        .stream_events(pb::StreamEventsRequest { session_id: s.id.clone(), after_seq: 0 })
        .await
        .unwrap()
        .into_inner();
    let send = |prompt: &str, when_busy: pb::WhenBusy| pb::SendTaskRequest {
        session_id: s.id.clone(),
        prompt: prompt.into(),
        idempotency_key: String::new(),
        when_busy: when_busy as i32,
    };
    let a = c.send_task(send("look a", pb::WhenBusy::Unspecified)).await.unwrap().into_inner();
    assert_eq!(a.status, pb::TaskStatus::Running as i32);
    let call_id = loop {
        if let Some(P::ToolApprovalRequired(r)) = next(&mut events).await.payload {
            break r.call_id;
        }
    };

    // Unspecified is what a client that never heard of the field sends: refused, as before.
    let busy = c.send_task(send("x", pb::WhenBusy::Unspecified)).await.unwrap_err();
    assert_eq!(busy.code(), tonic::Code::FailedPrecondition);
    let busy = c.send_task(send("x", pb::WhenBusy::Reject)).await.unwrap_err();
    assert_eq!(busy.code(), tonic::Code::FailedPrecondition);

    let b = c.send_task(send("look b", pb::WhenBusy::Queue)).await.unwrap().into_inner();
    assert_eq!(b.status, pb::TaskStatus::Queued as i32);
    let queued = loop {
        let e = next(&mut events).await;
        if let Some(P::TaskQueued(q)) = e.payload {
            assert_eq!((e.task_id.as_str(), q.prompt.as_str()), (b.id.as_str(), "look b"));
            break e.seq;
        }
    };
    let full = c.send_task(send("look c", pb::WhenBusy::Queue)).await.unwrap_err();
    assert_eq!(full.code(), tonic::Code::ResourceExhausted);

    let st = c
        .get_session_state(pb::GetSessionStateRequest { session_id: s.id.clone() })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(st.seq, queued);
    assert_eq!(st.queued_tasks.len(), 1);
    assert_eq!(
        (st.queued_tasks[0].task_id.as_str(), st.queued_tasks[0].prompt.as_str()),
        (b.id.as_str(), "look b")
    );

    // A queued task is not the running one: it cannot be steered, and it can be cancelled.
    let steer = c
        .steer_task(pb::SteerTaskRequest {
            session_id: s.id.clone(),
            task_id: b.id.clone(),
            message: "steer: x".into(),
            idempotency_key: String::new(),
        })
        .await
        .unwrap_err();
    assert_eq!(steer.code(), tonic::Code::FailedPrecondition);
    let cancelled = c
        .cancel_task(pb::CancelTaskRequest { session_id: s.id.clone(), task_id: b.id.clone() })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(cancelled.status, pb::TaskStatus::Failed as i32);

    // The running task is untouched by all of that, and the queue took the slot back.
    c.submit_approval(pb::SubmitApprovalRequest {
        session_id: s.id.clone(),
        call_id,
        decision: pb::ApprovalDecision::Approve as i32,
        ..Default::default()
    })
    .await
    .unwrap();
    loop {
        if let Some(P::TaskCompleted(_)) = next(&mut events).await.payload {
            break;
        }
    }
    let st = c
        .get_session_state(pb::GetSessionStateRequest { session_id: s.id.clone() })
        .await
        .unwrap()
        .into_inner();
    assert!(st.queued_tasks.is_empty());
}

/// The property the lock exists for, with threads: whatever order accepts, endings and cancels
/// arrive in, every task a session accepted is started at most once and ends exactly once, in the
/// log, and the queued ones start in the order they were accepted. Nothing is accepted into a
/// queue that nothing will drain.
#[test]
fn a_task_accepted_onto_a_session_always_starts_or_ends_in_the_log_whatever_races_with_it() {
    use super::wire::{Payload, WhenBusy};
    use session::{Begun, Refusal};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;

    for round in 0..40 {
        let (_dir, s, first) = bare_session();
        let s = Arc::new(s);
        let accepted = Arc::new(Mutex::new(vec![first.id.clone()]));
        let producing = Arc::new(AtomicBool::new(true));

        let producers: Vec<_> = (0..4)
            .map(|p| {
                let (s, accepted) = (s.clone(), accepted.clone());
                std::thread::spawn(move || {
                    for i in 0..25 {
                        match s.begin(&format!("{p}-{i}"), None, WhenBusy::Queue, usize::MAX) {
                            Ok(Begun::Started(st)) => accepted.lock().unwrap().push(st.task.id),
                            Ok(Begun::Queued(t)) => accepted.lock().unwrap().push(t.id),
                            Ok(Begun::Repeat(_)) | Err(Refusal::Busy | Refusal::QueueFull(_)) => {
                                panic!("a queueing send is neither a repeat nor refused here")
                            }
                            Err(Refusal::Closed) => panic!("not closed"),
                        }
                        if i % 5 == 0 {
                            std::thread::yield_now();
                        }
                    }
                })
            })
            .collect();

        // Someone cancels tasks of their choosing, running or queued, while it happens.
        let canceller = {
            let (s, accepted, producing) = (s.clone(), accepted.clone(), producing.clone());
            std::thread::spawn(move || {
                let mut n = round;
                while producing.load(Ordering::SeqCst) {
                    let id = {
                        let a = accepted.lock().unwrap();
                        a[n % a.len()].clone()
                    };
                    n += 3;
                    let _ = s.cancel(&id, "test");
                    std::thread::yield_now();
                }
            })
        };

        // The loops: end whatever is running, which promotes the next, until nothing is left.
        let driver = {
            let (s, producing) = (s.clone(), producing.clone());
            std::thread::spawn(move || loop {
                match s.view().active_task_id {
                    Some(id) => {
                        let _ = s.finish(&id, Ok("done".into()));
                    }
                    None if producing.load(Ordering::SeqCst) => std::thread::yield_now(),
                    None => break,
                }
            })
        };
        for p in producers {
            p.join().unwrap();
        }
        producing.store(false, Ordering::SeqCst);
        canceller.join().unwrap();
        driver.join().unwrap();

        let ids = accepted.lock().unwrap().clone();
        assert_eq!(ids.len(), 101, "round {round}: every send was accepted");
        let log = s.events_after(0);
        let (mut started, mut queued_order, mut start_order) = (Vec::new(), Vec::new(), Vec::new());
        for e in &log {
            match &e.payload {
                Payload::TaskQueued { .. } => queued_order.push(e.task_id.clone()),
                Payload::TaskStarted { .. } => {
                    started.push(e.task_id.clone());
                    start_order.push(e.task_id.clone());
                }
                _ => {}
            }
        }
        for id in &ids {
            let starts = log
                .iter()
                .filter(|e| &e.task_id == id && matches!(e.payload, Payload::TaskStarted { .. }))
                .count();
            let ends = log
                .iter()
                .filter(|e| {
                    &e.task_id == id
                        && matches!(
                            e.payload,
                            Payload::TaskCompleted { .. } | Payload::TaskFailed { .. }
                        )
                })
                .count();
            assert!(starts <= 1, "round {round}: {id} started {starts} times");
            assert_eq!(
                ends, 1,
                "round {round}: {id} ended {ends} times, so it was dropped or ended twice"
            );
        }
        // The queued ones started in the order they were accepted: of those that started at all.
        let queued_that_started: Vec<&String> =
            queued_order.iter().filter(|id| started.contains(id)).collect();
        let started_that_were_queued: Vec<&String> =
            start_order.iter().filter(|id| queued_order.contains(id)).collect();
        assert_eq!(
            queued_that_started, started_that_were_queued,
            "round {round}: not first in, first out"
        );
        assert_eq!(s.view().active_task_id, None, "round {round}: nothing is left running");
        assert!(s.state().queued_tasks.is_empty(), "round {round}: nothing is left waiting");
    }
}

#[test]
fn a_queued_task_keeps_the_time_it_was_accepted_when_it_starts() {
    use super::wire::{TaskStatus, WhenBusy};
    use session::Begun;
    let (_dir, s, first) = bare_session();
    let Ok(Begun::Queued(queued)) = s.begin("next", None, WhenBusy::Queue, 4) else {
        panic!("a busy session queues when asked")
    };
    assert_eq!(queued.status, TaskStatus::Queued);
    std::thread::sleep(std::time::Duration::from_millis(5));
    let started = s.finish(&first.id, Ok("done".into())).expect("the queued task starts");
    assert_eq!(started.task.id, queued.id);
    assert_eq!(started.task.status, TaskStatus::Running);
    assert_eq!(
        started.task.created_at, queued.created_at,
        "created when accepted, not when started"
    );
    assert_eq!(s.task(&queued.id).map(|t| t.status), Some(TaskStatus::Running));
}

#[tokio::test]
async fn a_queued_task_meets_the_budget_when_it_starts_not_when_it_is_queued() {
    // One model turn costs TURN. This budget affords the first turn of a task and not the second.
    let h = harness(Opts::default()).await;
    let sid = h.session(TURN + TURN / 2).await;
    let mut ev = h.stream(&sid, 0).await;
    let a = h.task(&sid, "look a").await;
    let (_, req) = ev.until("tool_approval_required").await;
    let (s, b) = h.queue(&sid, "look b", None).await;
    assert_eq!(
        (s, b["status"].as_str()),
        (202, Some("queued")),
        "there is budget left to queue on"
    );

    // a's second turn crosses the ceiling and fails it; b starts, finds the budget spent, and
    // fails at once: it was accepted, it started, and it ended. It was not left waiting.
    h.approve(&sid, &req["call_id"], "approve").await;
    let (_, failed_a) = ev.until("task_failed").await;
    assert_eq!(
        (failed_a["task_id"].as_str(), failed_a["code"].as_str()),
        (a["id"].as_str(), Some("budget_exceeded"))
    );
    let started_b = ev.next().await;
    assert_eq!(
        (started_b["type"].as_str(), started_b["task_id"].as_str()),
        (Some("task_started"), b["id"].as_str())
    );
    let (_, failed_b) = ev.until("task_failed").await;
    assert_eq!(
        (failed_b["task_id"].as_str(), failed_b["code"].as_str()),
        (b["id"].as_str(), Some("budget_exceeded"))
    );
    assert!(failed_b["message"].as_str().unwrap().contains("spent"), "{failed_b}");
    assert_eq!(h.get(&format!("/v1/sessions/{sid}")).await["status"], "idle");
}

// ---- parking an approval (ADR-0113) ----

/// A `comp-park` that keeps what it is told, for the tests of a daemon that parks. The tests play
/// `comp-resume` themselves, by posting what it would deliver to the daemon's sink.
struct FakePark {
    base: String,
    st: Arc<std::sync::Mutex<FakeParkState>>,
}

#[derive(Default)]
struct FakeParkState {
    calls: Vec<(String, Value)>,
    tickets: std::collections::HashMap<String, String>,
    down: bool,
}

impl FakePark {
    async fn start() -> FakePark {
        use axum::http::StatusCode;
        use axum::response::IntoResponse;
        let st = Arc::new(std::sync::Mutex::new(FakeParkState::default()));
        let s2 = st.clone();
        let app = axum::Router::new().route(
            "/v1/{func}",
            axum::routing::post(
                move |axum::extract::Path(func): axum::extract::Path<String>,
                      body: axum::Json<Value>| {
                    let st = s2.clone();
                    async move {
                        let mut g = st.lock().unwrap();
                        g.calls.push((func.clone(), body.0.clone()));
                        if g.down {
                            return (StatusCode::SERVICE_UNAVAILABLE, "down").into_response();
                        }
                        let ok = |v: Value| (StatusCode::OK, axum::Json(v)).into_response();
                        match func.as_str() {
                            "park" => {
                                let corr = body.0["call"]["correlation"].as_str().unwrap().to_string();
                                let existed = g.tickets.contains_key(&corr);
                                let ticket = g
                                    .tickets
                                    .entry(corr.clone())
                                    .or_insert_with(|| format!("tkt-{corr}"))
                                    .clone();
                                ok(json!({"ticket": ticket,
                                          "outcome": if existed { "already-parked" } else { "parked" }}))
                            }
                            "wake" => match g.tickets.get(body.0["correlation"].as_str().unwrap()) {
                                Some(t) => ok(json!(t)),
                                None => (
                                    StatusCode::NOT_FOUND,
                                    axum::Json(json!({"error": "not-found",
                                        "detail": "no such ticket",
                                        "message": "not-found: no such ticket"})),
                                )
                                    .into_response(),
                            },
                            "cancel" => ok(Value::Null),
                            _ => StatusCode::NOT_FOUND.into_response(),
                        }
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        FakePark { base, st }
    }

    fn calls(&self, func: &str) -> Vec<Value> {
        let g = self.st.lock().unwrap();
        g.calls.iter().filter(|(f, _)| f == func).map(|(_, b)| b.clone()).collect()
    }

    fn set_down(&self, down: bool) {
        self.st.lock().unwrap().down = down;
    }

    /// The store loses every ticket it had.
    fn forget_tickets(&self) {
        self.st.lock().unwrap().tickets.clear();
    }

    /// Wait for the `n`th call to `func`, which a daemon makes off the request's path.
    async fn wait_for(&self, func: &str, n: usize) -> Vec<Value> {
        for _ in 0..100 {
            let c = self.calls(func);
            if c.len() >= n {
                return c;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("comp-park never saw {n} `{func}` calls: {:?}", self.calls(func));
    }
}

impl Harness {
    /// What `comp-resume` does with a ticket that has ended: POST it to the daemon's sink.
    async fn deliver(&self, sid: &str, correlation: &str, event: Value) -> (u16, Value) {
        let mut body = json!({"ticket": format!("tkt-{correlation}"), "session": sid,
                              "correlation": correlation});
        body.as_object_mut().unwrap().extend(event.as_object().unwrap().clone());
        self.post("/park/deliver", body).await
    }

    /// The delivery of the answer `comp-park` was given by the last `wake`.
    async fn deliver_answer(&self, park: &FakePark, sid: &str, call_id: &str) -> (u16, Value) {
        let wake = park.calls("wake").pop().expect("a wake");
        self.deliver(sid, call_id, json!({"event": "answered", "answer": wake["answer"]})).await
    }
}

fn interrupted() -> Value {
    json!({"event": "interrupted", "interruption": {"reason": "no answer arrived by the deadline",
           "at": 1700000000000u64, "by": {"id": "comp-resume", "goal": null, "model": null}}})
}

/// Wait until a daemon that is parking has finished: the ticket is in the checkpoint, so nothing
/// of its own is left to write there.
async fn parked_for_good(checkpoint: &std::path::Path) {
    for _ in 0..100 {
        let ticket = std::fs::read(checkpoint)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .map(|cp| cp["ticket"].is_string());
        if ticket == Some(true) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the ticket never reached {}", checkpoint.display());
}

async fn quiet(ev: &mut Events) -> bool {
    tokio::time::timeout(Duration::from_millis(300), ev.next()).await.is_err()
}

#[tokio::test]
async fn a_parked_approval_waits_in_comp_park_and_its_delivered_answer_resumes_the_task() {
    let park = FakePark::start().await;
    let h = harness(Opts { park: Some(park.base.clone()), ..Opts::default() }).await;
    let sid = h.session(1_000_000).await;
    let mut ev = h.stream(&sid, 0).await;
    h.task(&sid, "look").await;
    let (_, req) = ev.until("tool_approval_required").await;
    let call_id = req["call_id"].as_str().unwrap();

    // The wait is a ticket, not a future: parked once, for this call, in this session, and past
    // no deadline because there is no approval timeout.
    let parks = park.wait_for("park", 1).await;
    assert_eq!(parks.len(), 1, "{parks:?}");
    assert_eq!(parks[0]["session"], sid);
    assert_eq!(parks[0]["call"]["correlation"], call_id);
    assert_eq!(parks[0]["call"]["replay"], "unsafe");
    assert!(parks[0]["call"]["deadline"].is_null(), "{}", parks[0]);
    assert!(parks[0]["call"]["description"].as_str().unwrap().contains("list_dir"));
    assert_eq!(h.get(&format!("/v1/sessions/{sid}")).await["status"], "awaiting_approval");

    // An answer goes to the ticket and nowhere else: the task does not move until it is delivered.
    assert_eq!(h.approve(&sid, &req["call_id"], "approve").await, 200);
    let wakes = park.calls("wake");
    assert_eq!((wakes.len(), &wakes[0]["correlation"]), (1, &req["call_id"]));
    assert!(quiet(&mut ev).await, "nothing moves until the answer is delivered");
    // A second answer is refused, as it is for a wait held in memory.
    assert_eq!(h.approve(&sid, &req["call_id"], "deny").await, 409);

    // `comp-resume` delivers it, and the task goes on from where it stood.
    assert_eq!(h.deliver_answer(&park, &sid, call_id).await.0, 200);
    let (kinds, resolved) = ev.until("tool_approval_resolved").await;
    assert!(kinds.len() == 1, "the very next event: {kinds:?}");
    assert_eq!(
        (resolved["decision"].as_str(), resolved["decided_by"].as_str()),
        (Some("approve"), Some("test"))
    );
    let (kinds, done) = ev.until("task_completed").await;
    assert_eq!(kinds[0], "tool_call_finished");
    assert_eq!(done["result"], "The workspace holds: README.md.");
    assert_eq!(
        done["task_usage"]["cost_usd_micros"],
        2 * TURN,
        "the turn before the park and the one after"
    );
    assert_eq!(h.get(&format!("/v1/sessions/{sid}")).await["status"], "idle");

    // A delivery that comes twice is taken and moves nothing.
    assert_eq!(h.deliver_answer(&park, &sid, call_id).await.0, 200);
    assert!(quiet(&mut ev).await, "a repeat resumes nothing");
}

#[tokio::test]
async fn a_tool_that_needs_no_approval_does_not_park() {
    let park = FakePark::start().await;
    let h = harness(Opts { park: Some(park.base.clone()), ..Opts::default() }).await;
    let sid = h.session_with(json!({"auto_approve_tools": ["list_dir"]})).await;
    let mut ev = h.stream(&sid, 0).await;
    h.task(&sid, "look").await;
    let (kinds, done) = ev.until("task_completed").await;
    assert!(!kinds.contains(&"tool_approval_required".to_string()), "{kinds:?}");
    assert_eq!(done["result"], "The workspace holds: README.md.");
    assert!(park.calls("park").is_empty(), "nothing was waiting on a human");
}

#[tokio::test]
async fn an_interrupted_ticket_is_a_denial_by_timeout_and_the_model_is_told() {
    let park = FakePark::start().await;
    let h = harness(Opts { park: Some(park.base.clone()), ..Opts::default() }).await;
    let sid = h.session(1_000_000).await;
    let mut ev = h.stream(&sid, 0).await;
    h.task(&sid, "look").await;
    let (_, req) = ev.until("tool_approval_required").await;

    assert_eq!(h.deliver(&sid, req["call_id"].as_str().unwrap(), interrupted()).await.0, 200);
    let (_, resolved) = ev.until("tool_approval_resolved").await;
    assert_eq!(
        (resolved["decision"].as_str(), resolved["decided_by"].as_str()),
        (Some("deny"), Some("timeout"))
    );
    let (_, done) = ev.until("task_completed").await;
    assert_eq!(done["result"], "The workspace holds: denied by a human: nobody answered in time.");
}

#[tokio::test]
async fn an_approval_timeout_is_the_tickets_deadline() {
    let park = FakePark::start().await;
    let h = harness(Opts {
        park: Some(park.base.clone()),
        approval_timeout: Some(Duration::from_secs(60)),
        ..Opts::default()
    })
    .await;
    let sid = h.session(1_000_000).await;
    let mut ev = h.stream(&sid, 0).await;
    h.task(&sid, "look").await;
    let (_, req) = ev.until("tool_approval_required").await;
    assert!(req["expires_at"].is_string(), "{req}");
    let deadline = park.wait_for("park", 1).await[0]["call"]["deadline"].as_u64().unwrap();
    let now =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()
            as u64;
    assert!((now + 55_000..now + 65_000).contains(&deadline), "{deadline} vs {now}");
}

#[tokio::test]
async fn a_parked_task_survives_a_restart_and_the_new_process_takes_the_answer() {
    let park = FakePark::start().await;
    let state = tempfile::tempdir().unwrap();
    let root = Arc::new(tempfile::tempdir().unwrap());
    let opts = || Opts {
        state_dir: Some(state.path().to_path_buf()),
        park: Some(park.base.clone()),
        ..Opts::default()
    };

    let h = serve(opts(), root.clone()).await;
    let sid = h.session(1_000_000).await;
    let mut ev = h.stream(&sid, 0).await;
    h.task(&sid, "look").await;
    let (_, req) = ev.until("tool_approval_required").await;
    let call_id = req["call_id"].as_str().unwrap().to_string();
    let checkpoint = state.path().join(&sid).join("checkpoint.json");
    parked_for_good(&checkpoint).await;
    let before = h.state(&sid).await;

    // The "restart": a second daemon over the same directory.
    let h2 = serve(opts(), root).await;
    let after = h2.state(&sid).await;
    assert_eq!(after["session"]["status"], "awaiting_approval");
    assert_eq!(after["pending_approvals"][0]["call_id"], call_id.as_str());
    assert_eq!(
        after["seq"], before["seq"],
        "nothing was added to the log: the task was not cut off"
    );
    // It parked the approval again by itself, which is idempotent, and costs nothing if the
    // ticket was there.
    assert_eq!(park.wait_for("park", 2).await.len(), 2);

    // A person answers the new process, and the delivery goes to it.
    assert_eq!(h2.approve(&sid, &req["call_id"], "approve").await, 200);
    assert_eq!(h2.deliver_answer(&park, &sid, &call_id).await.0, 200);
    let mut ev2 = h2.stream(&sid, 0).await;
    let (kinds, done) = ev2.until("task_completed").await;
    assert!(!kinds.contains(&"task_failed".to_string()), "{kinds:?}");
    assert_eq!(done["result"], "The workspace holds: README.md.");
    assert_eq!(
        done["task_usage"]["cost_usd_micros"],
        2 * TURN,
        "the turn before the restart counts too"
    );
    assert!(!checkpoint.exists(), "gone when the task moved on");
    let history = std::fs::read_to_string(state.path().join(&sid).join("history.json")).unwrap();
    assert!(history.contains("\"look\"") && history.contains("README.md"), "{history}");
}

#[tokio::test]
async fn a_restart_that_cannot_park_or_cannot_trust_the_checkpoint_fails_the_task_as_before() {
    for how in [
        "without a park",
        "a corrupt checkpoint",
        "a checkpoint for another task",
        "a checkpoint for a call the log shows decided",
    ] {
        let park = FakePark::start().await;
        let state = tempfile::tempdir().unwrap();
        let root = Arc::new(tempfile::tempdir().unwrap());
        let parked = || Opts {
            state_dir: Some(state.path().to_path_buf()),
            park: Some(park.base.clone()),
            ..Opts::default()
        };
        let h = serve(parked(), root.clone()).await;
        let sid = h.session(1_000_000).await;
        let mut ev = h.stream(&sid, 0).await;
        h.task(&sid, "look").await;
        ev.until("tool_approval_required").await;
        let checkpoint = state.path().join(&sid).join("checkpoint.json");
        parked_for_good(&checkpoint).await;
        let next = match how {
            "without a park" => {
                Opts { state_dir: Some(state.path().to_path_buf()), ..Opts::default() }
            }
            "a corrupt checkpoint" => {
                std::fs::write(&checkpoint, "{ not json").unwrap();
                parked()
            }
            "a checkpoint for another task" => {
                let mut cp: Value =
                    serde_json::from_slice(&std::fs::read(&checkpoint).unwrap()).unwrap();
                cp["task_id"] = json!("task_someone_else");
                std::fs::write(&checkpoint, serde_json::to_vec(&cp).unwrap()).unwrap();
                parked()
            }
            // The log says decided and the file says waiting: resuming from it would run a tool
            // that may already have run.
            _ => {
                let events = state.path().join(&sid).join("events.jsonl");
                let text = std::fs::read_to_string(&events).unwrap();
                let asked: Value = text
                    .lines()
                    .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                    .rfind(|e| e["type"] == "tool_approval_required")
                    .unwrap();
                let decided = json!({"seq": text.lines().count() + 1, "session_id": sid,
                    "task_id": asked["task_id"], "time": "2026-01-01T00:00:00Z",
                    "type": "tool_approval_resolved", "call_id": asked["call_id"],
                    "decision": "approve", "decided_by": "someone"});
                let mut f = std::fs::OpenOptions::new().append(true).open(&events).unwrap();
                std::io::Write::write_all(&mut f, format!("{decided}\n").as_bytes()).unwrap();
                parked()
            }
        };
        let h2 = serve(next, root).await;
        let mut ev2 = h2.stream(&sid, 0).await;
        let (_, failed) = ev2.until("task_failed").await;
        assert_eq!(failed["code"], "internal", "{how}");
        assert!(
            failed["message"].as_str().unwrap().contains("restarted while this task ran"),
            "{how}"
        );
        assert_eq!(h2.get(&format!("/v1/sessions/{sid}")).await["status"], "idle", "{how}");
        assert!(!checkpoint.exists(), "{how}: a checkpoint nothing will use is not kept");
    }
}

#[tokio::test]
async fn cancelling_a_parked_task_cancels_its_ticket_and_a_late_delivery_is_gone() {
    let park = FakePark::start().await;
    let state = tempfile::tempdir().unwrap();
    let h = harness(Opts {
        park: Some(park.base.clone()),
        state_dir: Some(state.path().to_path_buf()),
        ..Opts::default()
    })
    .await;
    let sid = h.session(1_000_000).await;
    let mut ev = h.stream(&sid, 0).await;
    let task = h.task(&sid, "look").await;
    let (_, req) = ev.until("tool_approval_required").await;
    let call_id = req["call_id"].as_str().unwrap();
    park.wait_for("park", 1).await;

    let (s, c) = h.cancel(&sid, task["id"].as_str().unwrap()).await;
    assert_eq!((s, c["status"].as_str()), (200, Some("failed")));
    let cancels = park.wait_for("cancel", 1).await;
    assert_eq!(cancels[0]["ticket"], format!("tkt-{call_id}"));
    let (_, failed) = ev.until("task_failed").await;
    assert_eq!(failed["code"], "cancelled");
    assert!(!state.path().join(&sid).join("checkpoint.json").exists());
    assert_eq!(h.get(&format!("/v1/sessions/{sid}")).await["status"], "idle");

    // An answer that was already on its way finds nobody, which is how comp-resume learns to stop.
    let (s, gone) = h
        .deliver(
            &sid,
            call_id,
            json!({"event": "answered", "answer": {"ok": true, "body": [], "detail": null}}),
        )
        .await;
    assert_eq!((s, gone["type"].as_str()), (410, Some("https://holon.dev/errors/gone")));
    // And the human who answers late is told there is nothing to answer.
    assert_eq!(h.approve(&sid, &req["call_id"], "approve").await, 409);
}

#[tokio::test]
async fn closing_a_session_cancels_the_ticket_it_was_parked_on() {
    let park = FakePark::start().await;
    let h = harness(Opts { park: Some(park.base.clone()), ..Opts::default() }).await;
    let sid = h.session(1_000_000).await;
    let mut ev = h.stream(&sid, 0).await;
    h.task(&sid, "look").await;
    let (_, req) = ev.until("tool_approval_required").await;
    park.wait_for("park", 1).await;
    let r = h.http.delete(format!("{}/v1/sessions/{sid}", h.base)).send().await.unwrap();
    assert_eq!(r.json::<Value>().await.unwrap()["status"], "closed");
    let cancels = park.wait_for("cancel", 1).await;
    assert_eq!(cancels[0]["ticket"], format!("tkt-{}", req["call_id"].as_str().unwrap()));
}

#[tokio::test]
async fn a_delivery_for_a_session_that_is_not_here_is_gone() {
    let h = harness(Opts { park: Some("http://127.0.0.1:1".into()), ..Opts::default() }).await;
    let (s, gone) = h
        .deliver(
            "ses_nope",
            "call_x",
            json!({"event": "answered", "answer": {"ok": true, "body": [], "detail": null}}),
        )
        .await;
    assert_eq!((s, gone["type"].as_str()), (410, Some("https://holon.dev/errors/gone")));
}

#[tokio::test]
async fn an_answer_that_cannot_reach_comp_park_is_a_503_and_can_be_sent_again() {
    let park = FakePark::start().await;
    let h = harness(Opts { park: Some(park.base.clone()), ..Opts::default() }).await;
    let sid = h.session(1_000_000).await;
    let mut ev = h.stream(&sid, 0).await;
    h.task(&sid, "look").await;
    let (_, req) = ev.until("tool_approval_required").await;
    park.wait_for("park", 1).await;

    park.set_down(true);
    let call = req["call_id"].as_str().unwrap();
    let (s, refused) = h
        .post(
            &format!("/v1/sessions/{sid}/tool-calls/{call}/approval"),
            json!({"decision": "approve"}),
        )
        .await;
    assert_eq!(
        (s, refused["type"].as_str()),
        (503, Some("https://holon.dev/errors/park-unavailable"))
    );
    // Nothing happened, so the same answer may be sent again, and takes.
    park.set_down(false);
    assert_eq!(h.approve(&sid, &req["call_id"], "approve").await, 200);
    assert_eq!(h.deliver_answer(&park, &sid, call).await.0, 200);
    ev.until("task_completed").await;
}

#[tokio::test]
async fn a_ticket_comp_park_lost_is_made_again_when_the_answer_comes() {
    let park = FakePark::start().await;
    let h = harness(Opts { park: Some(park.base.clone()), ..Opts::default() }).await;
    let sid = h.session(1_000_000).await;
    let mut ev = h.stream(&sid, 0).await;
    h.task(&sid, "look").await;
    let (_, req) = ev.until("tool_approval_required").await;
    park.wait_for("park", 1).await;

    park.forget_tickets();
    assert_eq!(
        h.approve(&sid, &req["call_id"], "approve").await,
        200,
        "the answer is not refused for a store that lost the ticket"
    );
    assert_eq!(park.calls("park").len(), 2, "made again");
    assert_eq!(park.calls("wake").len(), 2, "the first found nothing, the second took");
    assert_eq!(h.deliver_answer(&park, &sid, req["call_id"].as_str().unwrap()).await.0, 200);
    ev.until("task_completed").await;
}

#[tokio::test]
async fn an_approval_that_cannot_be_parked_fails_the_task_after_a_few_tries() {
    let park = FakePark::start().await;
    let state = tempfile::tempdir().unwrap();
    let h = harness(Opts {
        park: Some(park.base.clone()),
        state_dir: Some(state.path().to_path_buf()),
        ..Opts::default()
    })
    .await;
    let sid = h.session(1_000_000).await;
    let mut ev = h.stream(&sid, 0).await;
    park.set_down(true);
    h.task(&sid, "look").await;
    let (_, failed) = ev.until("task_failed").await;
    assert_eq!(failed["code"], "internal");
    assert!(
        failed["message"].as_str().unwrap().contains("could not park the approval of list_dir"),
        "{failed}"
    );
    assert_eq!(park.calls("park").len(), 3, "once, and twice more");
    assert_eq!(h.get(&format!("/v1/sessions/{sid}")).await["status"], "idle");
    assert!(!state.path().join(&sid).join("checkpoint.json").exists());
    assert!(h.state(&sid).await["pending_approvals"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn a_steer_sent_to_a_parked_task_is_read_when_it_resumes() {
    let park = FakePark::start().await;
    let h = harness(Opts { park: Some(park.base.clone()), ..Opts::default() }).await;
    let sid = h.session(1_000_000).await;
    let mut ev = h.stream(&sid, 0).await;
    let task = h.task(&sid, "look").await;
    let (_, req) = ev.until("tool_approval_required").await;
    park.wait_for("park", 1).await;

    let (s, _) = h.steer(&sid, task["id"].as_str().unwrap(), "steer: be brief", None).await;
    assert_eq!(s, 202);
    ev.until("task_steer_queued").await;
    assert_eq!(h.approve(&sid, &req["call_id"], "approve").await, 200);
    assert_eq!(h.deliver_answer(&park, &sid, req["call_id"].as_str().unwrap()).await.0, 200);
    let (kinds, _) = ev.until("task_steered").await;
    assert_eq!(kinds, ["tool_approval_resolved", "tool_call_finished", "task_steered"]);
    let (_, done) = ev.until("task_completed").await;
    assert_eq!(done["result"], "Noted: steer: be brief");
}

#[tokio::test]
async fn a_task_queued_behind_a_parked_one_waits_for_it_and_then_parks_in_its_turn() {
    let park = FakePark::start().await;
    let h = harness(Opts { park: Some(park.base.clone()), ..Opts::default() }).await;
    let sid = h.session(1_000_000).await;
    let mut ev = h.stream(&sid, 0).await;
    h.task(&sid, "look a").await;
    let (_, req_a) = ev.until("tool_approval_required").await;
    let (s, b) = h.queue(&sid, "look b", None).await;
    assert_eq!((s, b["status"].as_str()), (202, Some("queued")));

    assert_eq!(h.approve(&sid, &req_a["call_id"], "approve").await, 200);
    assert_eq!(h.deliver_answer(&park, &sid, req_a["call_id"].as_str().unwrap()).await.0, 200);
    let (_, done_a) = ev.until("task_completed").await;
    let started_b = ev.next().await;
    assert_eq!(
        (started_b["type"].as_str(), started_b["task_id"].as_str()),
        (Some("task_started"), b["id"].as_str())
    );
    assert_eq!(started_b["seq"].as_u64(), Some(done_a["seq"].as_u64().unwrap() + 1));

    // And b needs a human too: a second ticket, for a second call.
    let (_, req_b) = ev.until("tool_approval_required").await;
    assert_ne!(req_a["call_id"], req_b["call_id"]);
    assert_eq!(park.wait_for("park", 2).await.len(), 2);
    assert_eq!(h.approve(&sid, &req_b["call_id"], "approve").await, 200);
    assert_eq!(h.deliver_answer(&park, &sid, req_b["call_id"].as_str().unwrap()).await.0, 200);
    let (_, done_b) = ev.until("task_completed").await;
    assert_eq!(done_b["task_id"], b["id"]);
}

#[tokio::test]
async fn a_step_with_two_calls_parks_on_each_in_turn_and_the_model_gets_both_results_in_order() {
    let park = FakePark::start().await;
    let (base, seen) = fake_model(
        "/v1/chat/completions",
        vec![
            frames(&[
                json!({"choices":[{"delta":{"tool_calls":[
                    {"index":0,"id":"c1","type":"function","function":{"name":"list_dir","arguments":"{\"path\":\".\"}"}},
                    {"index":1,"id":"c2","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"README.md\"}"}}]}}]}),
                json!({"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":10}}),
            ])
            .into_iter()
            .chain(["[DONE]".to_string()])
            .collect(),
            frames(&[
                json!({"choices":[{"delta":{"content":"Both done."}}]}),
                json!({"choices":[],"usage":{"prompt_tokens":150,"completion_tokens":4}}),
            ]),
        ],
    )
    .await;
    let provider = model::Provider::OpenAi { base: format!("{base}/v1"), key: "k".into() };
    let h = harness(Opts { provider, park: Some(park.base.clone()), ..Opts::default() }).await;
    let sid = h.session_with(json!({"model": "claude-sonnet-5"})).await;
    let mut ev = h.stream(&sid, 0).await;
    h.task(&sid, "look at both").await;

    // The first call parks; the second has not been announced.
    let (kinds, first) = ev.until("tool_approval_required").await;
    assert_eq!(
        (first["call_id"].as_str(), kinds.last().map(String::as_str)),
        (Some("c1"), Some("tool_approval_required"))
    );
    assert_eq!(kinds.iter().filter(|k| *k == "tool_call_started").count(), 1, "{kinds:?}");
    assert_eq!(h.approve(&sid, &first["call_id"], "approve").await, 200);
    assert_eq!(h.deliver_answer(&park, &sid, "c1").await.0, 200);

    // It goes on to the second, with the first's result in hand, and parks on that.
    let (kinds, second) = ev.until("tool_approval_required").await;
    assert_eq!(second["call_id"], "c2");
    assert_eq!(
        kinds,
        [
            "tool_approval_resolved",
            "tool_call_finished",
            "tool_call_started",
            "tool_approval_required"
        ]
    );
    assert_eq!(park.wait_for("park", 2).await.len(), 2);
    assert_eq!(h.approve(&sid, &second["call_id"], "approve").await, 200);
    assert_eq!(h.deliver_answer(&park, &sid, "c2").await.0, 200);
    let (_, done) = ev.until("task_completed").await;
    assert_eq!(done["result"], "Both done.");

    // The model asked twice, and the second time was handed both results, in the order it asked.
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2, "one request before the parks and one after, not one per resume");
    let msgs = seen[1]["messages"].as_array().unwrap();
    let tool: Vec<(&str, &str)> = msgs
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(|m| (m["tool_call_id"].as_str().unwrap(), m["content"].as_str().unwrap()))
        .collect();
    assert_eq!(tool, [("c1", "README.md"), ("c2", "hi")]);
}

#[tokio::test]
async fn grpc_answers_a_parked_approval_through_the_ticket_and_says_unavailable_when_it_cannot() {
    use pb::agent_event::Payload as P;
    let park = FakePark::start().await;
    let h = harness(Opts { park: Some(park.base.clone()), ..Opts::default() }).await;
    let mut c =
        pb::agent_service_client::AgentServiceClient::connect(h.base.clone()).await.unwrap();
    let s = c
        .create_session(pb::CreateSessionRequest {
            model: "mock".into(),
            workspace_dir: h.ws.clone(),
            max_budget_usd_micros: 1_000_000,
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    let mut events = c
        .stream_events(pb::StreamEventsRequest { session_id: s.id.clone(), after_seq: 0 })
        .await
        .unwrap()
        .into_inner();
    c.send_task(pb::SendTaskRequest {
        session_id: s.id.clone(),
        prompt: "look".into(),
        idempotency_key: String::new(),
        when_busy: pb::WhenBusy::Unspecified as i32,
    })
    .await
    .unwrap();
    let call_id = loop {
        if let Some(P::ToolApprovalRequired(r)) = next(&mut events).await.payload {
            break r.call_id;
        }
    };
    park.wait_for("park", 1).await;
    let submit = || pb::SubmitApprovalRequest {
        session_id: s.id.clone(),
        call_id: call_id.clone(),
        decision: pb::ApprovalDecision::Approve as i32,
        ..Default::default()
    };
    park.set_down(true);
    assert_eq!(c.submit_approval(submit()).await.unwrap_err().code(), tonic::Code::Unavailable);
    park.set_down(false);
    c.submit_approval(submit()).await.unwrap();
    assert_eq!(h.deliver_answer(&park, &s.id, &call_id).await.0, 200);
    loop {
        if let Some(P::TaskCompleted(d)) = next(&mut events).await.payload {
            assert_eq!(d.result, "The workspace holds: README.md.");
            break;
        }
    }
}

/// The delivery is at-least-once and a task may be cancelled while one is in flight. However
/// they race, a parked call is resumed at most once, the log says it was decided once, and a
/// task that was cancelled is never resumed.
#[test]
fn deliveries_racing_each_other_and_a_cancel_resume_a_parked_call_at_most_once() {
    use super::wire::{Decision, Payload};
    use session::{BeginPark, Checkpoint, Resume};

    for round in 0..40 {
        let (_dir, s, task) = bare_session();
        let s = Arc::new(s);
        let call = model::Call {
            id: "call_1".into(),
            name: "list_dir".into(),
            input: json!({"path": "."}),
        };
        let cp = Checkpoint {
            task_id: task.id.clone(),
            turn: 0,
            calls: vec![call],
            next: 0,
            results: vec![],
            deadline: None,
            ticket: Some("tkt-call_1".into()),
        };
        assert!(matches!(s.begin_park(cp), BeginPark::Parked));
        let answer = session::Answer {
            decision: Decision::Approve,
            reason: String::new(),
            decided_by: "t".into(),
        };

        let resumed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut threads: Vec<_> = (0..6)
            .map(|_| {
                let (s, answer, resumed) = (s.clone(), answer.clone(), resumed.clone());
                std::thread::spawn(move || match s.resume_parked("call_1", &answer) {
                    Resume::Resumed(_) => {
                        resumed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                    Resume::Duplicate | Resume::Gone => {}
                })
            })
            .collect();
        let cancelled = round % 2 == 0;
        if cancelled {
            let (s, id) = (s.clone(), task.id.clone());
            threads.push(std::thread::spawn(move || {
                let _ = s.cancel(&id, "test");
            }));
        }
        for t in threads {
            t.join().unwrap();
        }

        let n = resumed.load(std::sync::atomic::Ordering::SeqCst);
        assert!(n <= 1, "round {round}: resumed {n} times");
        let decided = s
            .events_after(0)
            .iter()
            .filter(|e| matches!(e.payload, Payload::ToolApprovalResolved { .. }))
            .count();
        assert_eq!(decided, n, "round {round}: the log says decided {decided} times, resumed {n}");
        if !cancelled {
            assert_eq!(n, 1, "round {round}: nobody cancelled, so exactly one delivery moves it");
        }
        // A cancelled task has nothing left parked, and a late delivery finds nobody.
        if cancelled {
            let late = s.resume_parked("call_1", &answer);
            assert!(matches!(late, Resume::Duplicate | Resume::Gone), "round {round}");
        }
    }
}

#[tokio::test]
async fn a_steer_sent_before_a_restart_is_still_read_when_the_parked_task_resumes() {
    let park = FakePark::start().await;
    let state = tempfile::tempdir().unwrap();
    let root = Arc::new(tempfile::tempdir().unwrap());
    let opts = || Opts {
        state_dir: Some(state.path().to_path_buf()),
        park: Some(park.base.clone()),
        ..Opts::default()
    };
    let h = serve(opts(), root.clone()).await;
    let sid = h.session(1_000_000).await;
    let mut ev = h.stream(&sid, 0).await;
    let task = h.task(&sid, "look").await;
    let (_, req) = ev.until("tool_approval_required").await;
    let call_id = req["call_id"].as_str().unwrap().to_string();
    assert_eq!(h.steer(&sid, task["id"].as_str().unwrap(), "steer: be brief", None).await.0, 202);
    ev.until("task_steer_queued").await;
    parked_for_good(&state.path().join(&sid).join("checkpoint.json")).await;

    // The steer was accepted by a process that is gone. It is in the log, so the new one has it.
    let h2 = serve(opts(), root).await;
    let st = h2.state(&sid).await;
    assert_eq!(st["pending_steers"][0]["message"], "steer: be brief");
    assert_eq!(h2.approve(&sid, &req["call_id"], "approve").await, 200);
    assert_eq!(h2.deliver_answer(&park, &sid, &call_id).await.0, 200);
    let mut ev2 = h2.stream(&sid, 0).await;
    let (_, done) = ev2.until("task_completed").await;
    assert_eq!(done["result"], "Noted: steer: be brief");
}
