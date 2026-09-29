//! `comp-agentd` — holon as a persistent agent service (ADR-0102): sessions,
//! tasks, one resumable event stream per session, and human approval of tool
//! calls, over the REST + Server-Sent Events contract in `api/openapi.yaml`.
//!
//! ## ADR-0095's three questions
//!
//! 1. **Something WASI does not give a guest?** Yes: held connections (an SSE
//!    stream per watcher, open for as long as a task runs), a task that waits
//!    minutes or hours for a human, and `run`, which spawns a process.
//! 2. **The smallest it could be?** No, and knowingly: the agent loop and the
//!    two model dialects live here too (`agentd/agent.rs`, `agentd/model.rs`).
//! 3. **A contract a component could have answered?** Partly, and this is the
//!    recorded EXCEPTION (ADR-0102): `llm:inference` has no tool use and no
//!    streaming, and a WASI 0.2 guest cannot stream a reply across a
//!    component boundary. When WASI 0.3 streams land, the model call moves
//!    behind a provider component and this daemon keeps only (1).
//!
//! ## Routes
//!
//! As `api/openapi.yaml`: `POST /v1/sessions`, `GET|DELETE /v1/sessions/{id}`,
//! `POST /v1/sessions/{id}/tasks`, `GET /v1/sessions/{id}/events` (SSE),
//! `POST /v1/sessions/{id}/tool-calls/{call_id}/approval`. `GET /health` is
//! open; everything else takes `Authorization: Bearer <token>` when
//! `--token`/`--token-file` is set.
//!
//!   comp-agentd --workspace-root ~/work --provider anthropic --api-key-file ~/.anthropic
//!   comp-agentd --workspace-root ~/work --provider openai --base-url http://csatapaci:8080/v1
//!   comp-agentd --workspace-root /tmp --provider mock      # free, scripted

#[path = "agentd/agent.rs"]
mod agent;
#[path = "agentd/model.rs"]
mod model;
#[path = "agentd/session.rs"]
mod session;
#[path = "agentd/tools.rs"]
mod tools;
#[path = "agentd/wire.rs"]
mod wire;

use std::collections::VecDeque;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use serde::Deserialize;

use session::{Answer, Refusal, Registry, Session};

#[derive(Parser, Debug)]
#[command(
    name = "comp-agentd",
    about = "Holon as an agent service: sessions, tasks, SSE, approvals (ADR-0102)."
)]
struct Args {
    /// Shared secret a caller must send as `Authorization: Bearer <token>`.
    #[arg(long)]
    token: Option<String>,
    /// Same, read from a file. Wins over `--token`.
    #[arg(long)]
    token_file: Option<PathBuf>,
    /// Where to listen. Loopback by default.
    #[arg(long, default_value = "127.0.0.1:8016")]
    addr: String,

    /// A directory sessions may work under. Repeatable; a session whose
    /// `workspace_dir` is not inside one of these is refused.
    #[arg(long = "workspace-root", required = true)]
    workspace_roots: Vec<PathBuf>,

    /// `anthropic` speaks `/v1/messages`; `openai` speaks `/chat/completions`
    /// (vLLM, llama.cpp, Ollama, mlx_lm); `mock` is scripted and free.
    #[arg(long, value_parser = ["anthropic", "openai", "mock"], default_value = "anthropic")]
    provider: String,
    /// API base. Default: `https://api.anthropic.com` or `https://api.openai.com/v1`.
    #[arg(long, default_value = "")]
    base_url: String,
    /// A file holding the API key. Optional for a local OpenAI-compatible server.
    #[arg(long)]
    api_key_file: Option<PathBuf>,

    /// Model turns one task may take before it fails with `max_turns`.
    #[arg(long, default_value_t = 50)]
    max_turns: u32,
    /// Deny a pending tool call nobody answers within this many seconds.
    /// Unset: wait forever.
    #[arg(long)]
    approval_timeout_secs: Option<u64>,
}

struct Daemon {
    sessions: Registry,
    roots: Vec<PathBuf>,
    ctx: Arc<agent::Ctx>,
}

type Shared = Arc<Daemon>;

fn missing(id: &str) -> Response {
    problem(StatusCode::NOT_FOUND, "no-such-session", id.to_string())
}

fn problem(status: StatusCode, kind: &str, detail: impl Into<String>) -> Response {
    let body = wire::Problem {
        kind: format!("https://holon.dev/errors/{kind}"),
        title: kind.replace('-', " "),
        status: status.as_u16(),
        detail: detail.into(),
    };
    let mut r = (status, Json(body)).into_response();
    r.headers_mut().insert("content-type", "application/problem+json".parse().unwrap());
    r
}

impl Daemon {
    fn get(&self, id: &str) -> Option<Arc<Session>> {
        self.sessions.lock().unwrap_or_else(|e| e.into_inner()).get(id).cloned()
    }
}

async fn create(State(d): State<Shared>, body: axum::body::Bytes) -> Response {
    let req: wire::CreateSession = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return problem(StatusCode::BAD_REQUEST, "bad-request", e.to_string()),
    };
    if req.model.trim().is_empty() {
        return problem(StatusCode::BAD_REQUEST, "bad-request", "model is required");
    }
    if req.max_budget_usd_micros <= 0 {
        return problem(
            StatusCode::BAD_REQUEST,
            "bad-request",
            "max_budget_usd_micros must be > 0",
        );
    }
    let ws = match std::fs::canonicalize(&req.workspace_dir) {
        Ok(p) if p.is_dir() => p,
        _ => {
            return problem(
                StatusCode::BAD_REQUEST,
                "bad-workspace",
                format!("{} is not a directory", req.workspace_dir),
            )
        }
    };
    if !d.roots.iter().any(|r| ws.starts_with(r)) {
        return problem(
            StatusCode::BAD_REQUEST,
            "bad-workspace",
            format!("{} is outside every --workspace-root", ws.display()),
        );
    }
    let sess = Arc::new(Session::new(req, ws));
    let view = sess.view();
    d.sessions.lock().unwrap_or_else(|e| e.into_inner()).insert(sess.id.clone(), sess);
    (StatusCode::CREATED, Json(view)).into_response()
}

async fn show(State(d): State<Shared>, UrlPath(id): UrlPath<String>) -> Response {
    d.get(&id).map_or_else(|| missing(&id), |s| Json(s.view()).into_response())
}

async fn close(State(d): State<Shared>, UrlPath(id): UrlPath<String>) -> Response {
    let Some(s) = d.get(&id) else { return missing(&id) };
    s.close();
    Json(s.view()).into_response()
}

async fn send_task(
    State(d): State<Shared>,
    UrlPath(id): UrlPath<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let Some(sess) = d.get(&id) else { return missing(&id) };
    let req: wire::SendTask = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return problem(StatusCode::BAD_REQUEST, "bad-request", e.to_string()),
    };
    if req.prompt.trim().is_empty() {
        return problem(StatusCode::BAD_REQUEST, "bad-request", "prompt is required");
    }
    let key = headers.get("idempotency-key").and_then(|v| v.to_str().ok()).map(String::from);
    match sess.begin(&req.prompt, key) {
        Ok((task, fresh)) => {
            if fresh {
                let h = tokio::spawn(agent::run(
                    sess.clone(),
                    d.ctx.clone(),
                    task.id.clone(),
                    req.prompt,
                ));
                sess.attach(&task.id, h);
            }
            (StatusCode::ACCEPTED, Json(task)).into_response()
        }
        Err(Refusal::Busy) => problem(
            StatusCode::CONFLICT,
            "task-running",
            "a task is already running; one at a time per session",
        ),
        Err(Refusal::Closed) => {
            problem(StatusCode::CONFLICT, "session-closed", "the session is closed")
        }
    }
}

#[derive(Deserialize)]
struct After {
    after_seq: Option<u64>,
}

async fn events(
    State(d): State<Shared>,
    UrlPath(id): UrlPath<String>,
    Query(q): Query<After>,
    headers: HeaderMap,
) -> Response {
    let Some(sess) = d.get(&id) else { return missing(&id) };
    let last_id =
        headers.get("last-event-id").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok());
    let after = last_id.or(q.after_seq).unwrap_or(0);
    // Subscribe BEFORE the first read: an event appended between the read and
    // the wait then still wakes the wait.
    let rx = sess.watch();
    let stream = futures::stream::unfold(
        (sess, after, rx, VecDeque::new()),
        |(sess, mut cursor, mut rx, mut queue)| async move {
            loop {
                if let Some(e) = queue.pop_front() {
                    let e: wire::Event = e;
                    cursor = e.seq;
                    let frame = SseEvent::default()
                        .id(e.seq.to_string())
                        .event(e.payload.kind())
                        .data(serde_json::to_string(&e).unwrap_or_default());
                    return Some((Ok::<_, Infallible>(frame), (sess, cursor, rx, queue)));
                }
                queue.extend(sess.events_after(cursor));
                if queue.is_empty() && rx.changed().await.is_err() {
                    return None;
                }
            }
        },
    );
    Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)).text("keepalive"))
        .into_response()
}

async fn approve(
    State(d): State<Shared>,
    UrlPath((id, call_id)): UrlPath<(String, String)>,
    body: axum::body::Bytes,
) -> Response {
    let Some(sess) = d.get(&id) else { return missing(&id) };
    let req: wire::SubmitApproval = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return problem(StatusCode::BAD_REQUEST, "bad-request", e.to_string()),
    };
    let decided_by = if req.approver.is_empty() { "anonymous".to_string() } else { req.approver };
    let answer = Answer { decision: req.decision, reason: req.reason, decided_by };
    if !sess.answer(&call_id, answer) {
        return problem(
            StatusCode::CONFLICT,
            "not-pending",
            format!("{call_id} is not waiting for approval"),
        );
    }
    Json(wire::ApprovalAnswer { call_id, decision: req.decision }).into_response()
}

async fn health(State(d): State<Shared>) -> Json<serde_json::Value> {
    let n = d.sessions.lock().unwrap_or_else(|e| e.into_inner()).len();
    Json(serde_json::json!({"ok": true, "sessions": n}))
}

fn app(d: Shared, token: Option<String>) -> Router {
    let api = Router::new()
        .route("/v1/sessions", post(create))
        .route("/v1/sessions/{id}", get(show).delete(close))
        .route("/v1/sessions/{id}/tasks", post(send_task))
        .route("/v1/sessions/{id}/events", get(events))
        .route("/v1/sessions/{id}/tool-calls/{call_id}/approval", post(approve))
        .layer(axum::middleware::from_fn(comp_reconciler::daemon_auth::require_token))
        .layer(axum::Extension(Arc::new(token)))
        .with_state(d.clone());
    Router::new().route("/health", get(health)).with_state(d).merge(api)
}

fn off_peak_now() -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    comp_reconciler::offpeak::deepseek_off_peak(now, &[])
}

fn provider(args: &Args) -> Result<model::Provider> {
    let key = match &args.api_key_file {
        Some(p) => std::fs::read_to_string(p)
            .with_context(|| format!("reading {}", p.display()))?
            .trim()
            .to_string(),
        None => String::new(),
    };
    let base = |default: &str| {
        if args.base_url.is_empty() {
            default.to_string()
        } else {
            args.base_url.clone()
        }
    };
    Ok(match args.provider.as_str() {
        "mock" => model::Provider::Mock,
        "openai" => model::Provider::OpenAi { base: base("https://api.openai.com/v1"), key },
        _ => {
            anyhow::ensure!(!key.is_empty(), "--provider anthropic needs --api-key-file");
            model::Provider::Anthropic { base: base("https://api.anthropic.com"), key }
        }
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let token =
        comp_reconciler::daemon_auth::resolve_token(args.token.clone(), args.token_file.clone());
    comp_reconciler::daemon_auth::warn_if_unauthenticated("comp-agentd", &token);
    let roots = args
        .workspace_roots
        .iter()
        .map(|r| r.canonicalize().with_context(|| format!("--workspace-root {}", r.display())))
        .collect::<Result<Vec<_>>>()?;
    let ctx = agent::Ctx {
        provider: provider(&args)?,
        http: reqwest::Client::builder().connect_timeout(Duration::from_secs(10)).build()?,
        max_turns: args.max_turns,
        approval_timeout: args.approval_timeout_secs.map(Duration::from_secs),
        off_peak: off_peak_now,
    };
    let d = Arc::new(Daemon { sessions: Registry::default(), roots, ctx: Arc::new(ctx) });
    let listener = tokio::net::TcpListener::bind(&args.addr)
        .await
        .with_context(|| format!("binding {}", args.addr))?;
    println!("comp-agentd: listening on http://{} | provider {}", args.addr, args.provider);
    axum::serve(listener, app(d, token)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    struct Harness {
        base: String,
        http: reqwest::Client,
        _root: tempfile::TempDir,
        ws: String,
    }

    async fn harness(token: Option<&str>) -> Harness {
        let root = tempfile::tempdir().unwrap();
        let rootp = root.path().canonicalize().unwrap();
        std::fs::write(rootp.join("README.md"), "hi").unwrap();
        let ctx = agent::Ctx {
            provider: model::Provider::Mock,
            http: reqwest::Client::new(),
            max_turns: 5,
            approval_timeout: None,
            off_peak: || false,
        };
        let d = Arc::new(Daemon {
            sessions: Registry::default(),
            roots: vec![rootp.clone()],
            ctx: Arc::new(ctx),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = app(d, token.map(String::from));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Harness { base, http: reqwest::Client::new(), ws: rootp.display().to_string(), _root: root }
    }

    impl Harness {
        async fn post(&self, path: &str, body: Value) -> (u16, Value) {
            let r =
                self.http.post(format!("{}{path}", self.base)).json(&body).send().await.unwrap();
            (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
        }

        async fn session(&self, budget: i64) -> String {
            let (s, v) = self
                .post(
                    "/v1/sessions",
                    json!({"model": "mock", "workspace_dir": self.ws, "max_budget_usd_micros": budget}),
                )
                .await;
            assert_eq!(s, 201, "{v}");
            v["id"].as_str().unwrap().to_string()
        }

        async fn stream(&self, sid: &str, after: u64) -> Events {
            let url = format!("{}/v1/sessions/{sid}/events?after_seq={after}", self.base);
            Events {
                resp: self.http.get(url).send().await.unwrap(),
                buf: String::new(),
                queue: VecDeque::new(),
            }
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
        let h = harness(None).await;
        let sid = h.session(1_000_000).await;
        let mut ev = h.stream(&sid, 0).await;
        let (s, task) =
            h.post(&format!("/v1/sessions/{sid}/tasks"), json!({"prompt": "look"})).await;
        assert_eq!(s, 202, "{task}");

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
        let r = h.http.get(format!("{}/v1/sessions/{sid}", h.base)).send().await.unwrap();
        assert_eq!(r.json::<Value>().await.unwrap()["status"], "awaiting_approval");

        let call = req["call_id"].as_str().unwrap();
        let approval = format!("/v1/sessions/{sid}/tool-calls/{call}/approval");
        let (s, _) = h.post(&approval, json!({"decision": "approve", "approver": "test"})).await;
        assert_eq!(s, 200);
        let (s, _) = h.post(&approval, json!({"decision": "approve"})).await;
        assert_eq!(s, 409, "a decided call is no longer pending");

        let (kinds, done) = ev.until("task_completed").await;
        assert_eq!(kinds[0], "tool_approval_resolved");
        assert!(kinds.contains(&"tool_call_finished".to_string()));
        assert_eq!(done["result"], "The workspace holds: README.md.");
        assert_eq!(done["task_usage"]["cost_usd_micros"], 2 * TURN);
        assert_eq!(done["task_usage"]["input_tokens"], 2000);

        let r = h.http.get(format!("{}/v1/sessions/{sid}", h.base)).send().await.unwrap();
        let v: Value = r.json().await.unwrap();
        assert_eq!(
            (v["status"].as_str(), v["usage"]["cost_usd_micros"].as_i64()),
            (Some("idle"), Some(2 * TURN))
        );

        // Resuming mid-log starts at exactly the next event.
        let mut again = h.stream(&sid, 3).await;
        assert_eq!(again.next().await["seq"], 4);
    }

    #[tokio::test]
    async fn a_denied_call_is_an_error_the_model_sees_and_auto_approve_skips_asking() {
        let h = harness(None).await;
        let sid = h.session(1_000_000).await;
        let mut ev = h.stream(&sid, 0).await;
        h.post(&format!("/v1/sessions/{sid}/tasks"), json!({"prompt": "look"})).await;
        let (_, req) = ev.until("tool_approval_required").await;
        let call = req["call_id"].as_str().unwrap();
        h.post(
            &format!("/v1/sessions/{sid}/tool-calls/{call}/approval"),
            json!({"decision": "deny", "reason": "not now"}),
        )
        .await;
        let (_, fin) = ev.until("tool_call_finished").await;
        assert_eq!(
            (fin["is_error"].as_bool(), fin["output"].as_str()),
            (Some(true), Some("denied by a human: not now"))
        );
        ev.until("task_completed").await;

        let (_, v) = h
            .post(
                "/v1/sessions",
                json!({"model": "mock", "workspace_dir": h.ws, "max_budget_usd_micros": 1_000_000,
                       "auto_approve_tools": ["list_dir"]}),
            )
            .await;
        let sid = v["id"].as_str().unwrap();
        let mut ev = h.stream(sid, 0).await;
        h.post(&format!("/v1/sessions/{sid}/tasks"), json!({"prompt": "look"})).await;
        let (kinds, _) = ev.until("task_completed").await;
        assert!(!kinds.contains(&"tool_approval_required".to_string()));
    }

    #[tokio::test]
    async fn crossing_the_budget_fails_the_task() {
        let h = harness(None).await;
        let sid = h.session(TURN / 2).await;
        let mut ev = h.stream(&sid, 0).await;
        h.post(&format!("/v1/sessions/{sid}/tasks"), json!({"prompt": "look"})).await;
        let (kinds, failed) = ev.until("task_failed").await;
        assert!(!kinds.contains(&"tool_call_started".to_string()));
        assert_eq!(failed["code"], "budget_exceeded");
        let (_, u) = ev_usage(&h, &sid).await;
        assert_eq!(u, TURN - TURN / 2);
    }

    async fn ev_usage(h: &Harness, sid: &str) -> (i64, i64) {
        let v: Value = h
            .http
            .get(format!("{}/v1/sessions/{sid}", h.base))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let spent = v["usage"]["cost_usd_micros"].as_i64().unwrap();
        (spent, spent - v["max_budget_usd_micros"].as_i64().unwrap())
    }

    #[tokio::test]
    async fn closing_cancels_the_waiting_task_and_an_idempotent_retry_starts_nothing() {
        let h = harness(None).await;
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
    async fn a_workspace_outside_the_roots_and_a_missing_token_are_refused() {
        let h = harness(None).await;
        let (s, v) = h
            .post(
                "/v1/sessions",
                json!({"model": "mock", "workspace_dir": "/", "max_budget_usd_micros": 1}),
            )
            .await;
        assert_eq!(s, 400, "{v}");
        assert_eq!(v["type"], "https://holon.dev/errors/bad-workspace");

        let h = harness(Some("sekrit")).await;
        let (s, _) = h.post("/v1/sessions", json!({})).await;
        assert_eq!(s, 401);
        let ok = h.http.get(format!("{}/health", h.base)).send().await.unwrap();
        assert_eq!(ok.status().as_u16(), 200);
    }
}
