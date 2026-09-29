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
use super::{agent, app, model};

struct Opts {
    token: Option<&'static str>,
    provider: model::Provider,
    prices: Vec<(String, u64, u64)>,
    approval_timeout: Option<Duration>,
    state_dir: Option<PathBuf>,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            token: None,
            provider: model::Provider::Mock,
            prices: vec![],
            approval_timeout: None,
            state_dir: None,
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
    let ctx = agent::Ctx {
        provider: o.provider,
        http: reqwest::Client::new(),
        max_turns: 5,
        approval_timeout: o.approval_timeout,
        prices: o.prices,
        off_peak: || false,
    };
    let sessions = match &o.state_dir {
        Some(d) => session::load_all(d).unwrap(),
        None => Default::default(),
    };
    let d = Arc::new(Daemon {
        sessions: Registry::new(sessions),
        roots: vec![rootp.clone()],
        state_dir: o.state_dir,
        ctx: Arc::new(ctx),
    });
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
