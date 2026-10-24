//! Coding sessions on `comp-agentd` (ADR-0119), polled rather than streamed: no reconnect logic.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::agent::{Cause, Event, Host};
use crate::runtime::{unix_now_ms, Pending, Runtime};
use crate::spec::AgentSpec;

#[derive(Clone, Debug)]
pub struct AgentdConfig {
    pub url: String,
    pub token: Option<String>,
    pub model: String,
    pub poll: Duration,
}

impl AgentdConfig {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            token: None,
            model: String::new(),
            poll: Duration::from_millis(700),
        }
    }
}

pub use agent_wire::{Phase, SessionView};

pub trait Describe {
    fn to_json(&self) -> Value;
    fn for_model(&self) -> String;
}

impl Describe for SessionView {
    fn to_json(&self) -> Value {
        json!({
            "id": self.id, "agent": self.agent, "run": self.run_id, "span": self.call_span,
            "task": self.task, "model": self.model, "workspace": self.workspace,
            "state": self.phase.label(), "cost_usd_micros": self.cost_micros,
            "tokens_in": self.tokens_in, "tokens_out": self.tokens_out, "tools": self.tools,
            "calls": self.calls, "activity": self.activity, "started_ms": self.started_ms,
            "finished_ms": self.finished_ms, "result": self.result,
            "pending": self.pending.iter().map(|p| json!({
                "id": p.id, "tool": p.tool, "args": p.args,
            })).collect::<Vec<_>>(),
        })
    }

    fn for_model(&self) -> String {
        let spent = format!("${:.2}, {} tool call(s)", self.cost_micros as f64 / 1e6, self.calls);
        match self.phase {
            Phase::Done => format!(
                "done: {}\n({spent}; used {})",
                self.result.as_deref().unwrap_or(""),
                if self.tools.is_empty() { "no tools".into() } else { self.tools.join(", ") }
            ),
            Phase::Failed => {
                format!(
                    "failed: {}\n({spent})",
                    self.result.as_deref().unwrap_or("no reason given")
                )
            }
            Phase::Cancelled => format!("stopped before it finished ({spent})"),
            Phase::NeedsYou => format!(
                "still running; it is waiting for a person to approve {} ({spent}). A person \
                 has been asked; check again later.",
                self.pending.first().map_or("a tool call".to_string(), |p| p.tool.clone())
            ),
            Phase::Running => format!(
                "still running{} ({spent})",
                if self.activity.is_empty() {
                    String::new()
                } else {
                    format!(": {}", self.activity)
                }
            ),
        }
    }
}

// Unknown fields and entry kinds are ignored, as the daemon's contract asks of every client.

#[derive(Deserialize, Default, Clone, Debug)]
pub(crate) struct UsageDoc {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_tokens: u64,
    #[serde(default)]
    cache_write_tokens: u64,
    #[serde(default)]
    cost_usd_micros: u64,
}

#[derive(Deserialize, Clone, Debug)]
pub(crate) struct SessionDoc {
    id: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    usage: UsageDoc,
}

#[derive(Deserialize, Clone, Debug)]
pub(crate) struct TaskDoc {
    id: String,
}

#[derive(Deserialize, Clone, Debug)]
pub(crate) struct PendingDoc {
    #[serde(default)]
    task_id: String,
    call_id: String,
    tool_name: String,
    #[serde(default)]
    input: Value,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Entry {
    AssistantMessage {
        #[serde(default)]
        task_id: String,
        #[serde(default)]
        in_progress: bool,
    },
    ToolCall {
        #[serde(default)]
        task_id: String,
        call_id: String,
        tool_name: String,
        #[serde(default)]
        input: Value,
        #[serde(default)]
        status: String,
        #[serde(default)]
        decision: Option<String>,
    },
    TaskEnded {
        #[serde(default)]
        task_id: String,
        #[serde(default)]
        outcome: String,
        #[serde(default)]
        result: Option<String>,
        #[serde(default)]
        code: Option<String>,
        #[serde(default)]
        message: Option<String>,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize, Clone, Debug)]
pub(crate) struct StateDoc {
    session: SessionDoc,
    #[serde(default)]
    transcript: Vec<Entry>,
    #[serde(default)]
    pending_approvals: Vec<PendingDoc>,
}

/// RFC 9457.
#[derive(Deserialize, Default)]
struct Problem {
    #[serde(default)]
    title: String,
    #[serde(default)]
    detail: String,
}

/// The part of the daemon's `api/openapi.yaml` a session needs.
pub struct Agentd {
    cfg: AgentdConfig,
    http: reqwest::blocking::Client,
}

impl Agentd {
    pub fn new(cfg: AgentdConfig) -> Result<Self, String> {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self { cfg, http })
    }

    pub fn config(&self) -> &AgentdConfig {
        &self.cfg
    }

    fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
        headers: &[(&str, &str)],
    ) -> Result<Value, String> {
        let url = format!("{}{path}", self.cfg.url.trim_end_matches('/'));
        let mut rb = self.http.request(method, &url);
        if let Some(t) = &self.cfg.token {
            rb = rb.bearer_auth(t);
        }
        for (k, v) in headers {
            rb = rb.header(*k, *v);
        }
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        let resp = rb
            .send()
            .map_err(|e| format!("comp-agentd at {} did not answer: {e}", self.cfg.url))?;
        let status = resp.status();
        let text = resp.text().map_err(|e| e.to_string())?;
        if status.is_success() {
            return Ok(serde_json::from_str(&text).unwrap_or(Value::Null));
        }
        let p: Problem = serde_json::from_str(&text).unwrap_or_default();
        let why = if !p.detail.is_empty() {
            p.detail
        } else if !p.title.is_empty() {
            p.title
        } else {
            text
        };
        Err(format!(
            "comp-agentd refused {path} ({}): {}",
            status.as_u16(),
            if why.trim().is_empty() { "no reason given" } else { why.trim() }
        ))
    }

    pub(crate) fn create(
        &self,
        model: &str,
        workspace: &str,
        budget_micros: u64,
        auto_approve: &[String],
        labels: &BTreeMap<String, String>,
    ) -> Result<SessionDoc, String> {
        let v = self.call(
            reqwest::Method::POST,
            "/v1/sessions",
            Some(json!({
                "model": model, "workspace_dir": workspace,
                "max_budget_usd_micros": budget_micros,
                "auto_approve_tools": auto_approve, "labels": labels,
            })),
            &[],
        )?;
        serde_json::from_value(v)
            .map_err(|e| format!("comp-agentd answered with a session I cannot read: {e}"))
    }

    pub(crate) fn send_task(
        &self,
        session: &str,
        prompt: &str,
        key: &str,
    ) -> Result<TaskDoc, String> {
        let v = self.call(
            reqwest::Method::POST,
            &format!("/v1/sessions/{session}/tasks"),
            Some(json!({ "prompt": prompt })),
            &[("Idempotency-Key", key)],
        )?;
        serde_json::from_value(v)
            .map_err(|e| format!("comp-agentd answered with a task I cannot read: {e}"))
    }

    pub(crate) fn state(&self, session: &str) -> Result<StateDoc, String> {
        let v =
            self.call(reqwest::Method::GET, &format!("/v1/sessions/{session}/state"), None, &[])?;
        serde_json::from_value(v)
            .map_err(|e| format!("comp-agentd answered with a state I cannot read: {e}"))
    }

    pub(crate) fn approve(
        &self,
        session: &str,
        call_id: &str,
        approved: bool,
        scope: &str,
    ) -> Result<(), String> {
        let mut body = json!({
            "decision": if approved { "approve" } else { "deny" },
            "approver": "agent-runtime",
        });
        if approved && scope != "once" {
            body["scope"] = json!(scope);
        }
        if !approved {
            body["reason"] = json!("a person denied this tool call");
        }
        self.call(
            reqwest::Method::POST,
            &format!("/v1/sessions/{session}/tool-calls/{call_id}/approval"),
            Some(body),
            &[],
        )
        .map(|_| ())
    }

    pub(crate) fn close(&self, session: &str) -> Result<(), String> {
        self.call(reqwest::Method::DELETE, &format!("/v1/sessions/{session}"), None, &[])
            .map(|_| ())
    }
}

fn brief(tool: &str, input: &Value) -> String {
    let key = if tool == "run" { "command" } else { "path" };
    let s = input.get(key).and_then(Value::as_str).unwrap_or("");
    let one: String = s.lines().next().unwrap_or("").chars().take(60).collect();
    one
}

type Ended<'a> = (&'a str, &'a Option<String>, &'a Option<String>, &'a Option<String>);

/// Pure: `pending` is left as it was, since approval ids are the runtime's to assign.
pub(crate) fn fold(prev: &SessionView, st: &StateDoc, task: &str, now_ms: u64) -> SessionView {
    let mut v = prev.clone();
    let u = &st.session.usage;
    v.cost_micros = u.cost_usd_micros;
    v.tokens_in = u.input_tokens + u.cache_read_tokens + u.cache_write_tokens;
    v.tokens_out = u.output_tokens;

    let mine = |t: &str| t == task;
    v.tools.clear();
    v.calls = 0;
    let mut activity = String::new();
    let mut ended: Option<Ended> = None;
    for e in &st.transcript {
        match e {
            Entry::ToolCall { task_id, tool_name, input, status, .. } if mine(task_id) => {
                v.calls += 1;
                if !v.tools.contains(tool_name) {
                    v.tools.push(tool_name.clone());
                }
                let b = brief(tool_name, input);
                activity = match status.as_str() {
                    "awaiting_approval" => format!("waiting to {tool_name} {b}"),
                    "running" => format!("{tool_name} {b}"),
                    _ => String::new(),
                };
            }
            Entry::AssistantMessage { task_id, in_progress, .. }
                if mine(task_id) && *in_progress =>
            {
                activity = "thinking".into();
            }
            Entry::TaskEnded { task_id, outcome, result, code, message } if mine(task_id) => {
                ended = Some((outcome.as_str(), result, code, message));
            }
            _ => {}
        }
    }
    v.activity = activity.trim().to_string();

    let waiting: Vec<&PendingDoc> =
        st.pending_approvals.iter().filter(|p| p.task_id.is_empty() || mine(&p.task_id)).collect();
    match ended {
        Some((outcome, result, code, message)) => {
            v.phase = match (outcome, code.as_deref()) {
                ("completed", _) => Phase::Done,
                (_, Some("cancelled")) => Phase::Cancelled,
                _ => Phase::Failed,
            };
            v.result = Some(match outcome {
                "completed" => result.clone().unwrap_or_default(),
                _ => match (code, message) {
                    (Some(c), Some(m)) if !m.is_empty() => format!("{c}: {m}"),
                    (Some(c), _) => c.clone(),
                    (None, Some(m)) => m.clone(),
                    _ => "the task failed".into(),
                },
            });
            v.activity.clear();
            v.finished_ms = prev.finished_ms.or(Some(now_ms));
        }
        None if !waiting.is_empty() => v.phase = Phase::NeedsYou,
        None if st.session.status == "closed" => {
            v.phase = Phase::Cancelled;
            v.finished_ms = prev.finished_ms.or(Some(now_ms));
        }
        None => v.phase = Phase::Running,
    }
    v
}

pub(crate) struct Tracked {
    pub view: SessionView,
    /// The task whose end is the session's.
    pub task: String,
    /// (runtime approval id, daemon call id).
    pub approvals: Vec<(u64, String)>,
    pub misses: u32,
}

const MAX_MISSES: u32 = 8;

pub const MAX_WAIT: Duration = Duration::from_secs(60);

impl Runtime {
    pub fn sessions(&self) -> Vec<SessionView> {
        let mut v: Vec<SessionView> =
            self.sessions.lock().unwrap().iter().map(|t| t.view.clone()).collect();
        v.reverse();
        v
    }

    fn emit(&self, ev: Event) {
        Host::observe(self, ev);
    }

    fn session_workspace(&self, spec: &AgentSpec, asked: &str) -> Result<String, String> {
        let asked = asked.trim();
        if asked.is_empty() || asked == "own" {
            let dir = self.store().workspace(&spec.name)?;
            let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
            return Ok(dir.to_string_lossy().to_string());
        }
        let listed = &spec.sessions.workspaces;
        if let Some(w) = listed.iter().find(|w| w.as_str() == asked) {
            return Ok(w.clone());
        }
        let by_name: Vec<&String> = listed
            .iter()
            .filter(|w| std::path::Path::new(w).file_name().is_some_and(|n| n == asked))
            .collect();
        match by_name.as_slice() {
            [one] => Ok((*one).clone()),
            _ => Err(format!(
                "`{asked}` is not a workspace you may open a session in; use `own`{}",
                if listed.is_empty() {
                    String::new()
                } else {
                    format!(" or one of: {}", listed.join(", "))
                }
            )),
        }
    }

    pub(crate) fn start_session(
        &self,
        spec: &AgentSpec,
        run_id: &str,
        cause: &Cause,
        task: &str,
        workspace: &str,
    ) -> Result<String, String> {
        let agentd = self
            .agentd
            .as_ref()
            .ok_or("coding sessions are not available here: no comp-agentd is configured")?;
        let task = task.trim();
        if task.is_empty() {
            return Err("`task` is empty".into());
        }
        let policy = &spec.sessions;
        let open = self
            .sessions
            .lock()
            .unwrap()
            .iter()
            .filter(|t| t.view.agent == spec.name && !t.view.phase.is_over())
            .count();
        if open >= policy.max_concurrent as usize {
            return Err(format!(
                "you already have {open} session(s) open (at most {}): collect one with \
                 session_result or stop one with session_close",
                policy.max_concurrent
            ));
        }
        let model = if policy.model.is_empty() {
            agentd.config().model.clone()
        } else {
            policy.model.clone()
        };
        if model.is_empty() {
            return Err("no model is configured for coding sessions".into());
        }
        let dir = self.session_workspace(spec, workspace)?;
        let budget = ((policy.budget_usd * 1_000_000.0).round() as u64).max(1);
        let call = cause.parent_span_id.clone().unwrap_or_default();
        let labels = BTreeMap::from([
            ("holon.agent".to_string(), spec.name.clone()),
            ("holon.run".to_string(), run_id.to_string()),
            ("holon.span".to_string(), call.clone()),
            ("holon.trace".to_string(), cause.trace_id.clone()),
        ]);
        let session = agentd.create(&model, &dir, budget, &policy.auto_approve, &labels)?;
        let key = format!("{run_id}-{call}");
        let started = match agentd.send_task(&session.id, task, &key) {
            Ok(t) => t,
            Err(e) => {
                let _ = agentd.close(&session.id);
                return Err(e);
            }
        };

        let view = SessionView {
            id: session.id.clone(),
            agent: spec.name.clone(),
            run_id: run_id.to_string(),
            call_span: (!call.is_empty()).then_some(call),
            trace_id: cause.trace_id.clone(),
            task: task.to_string(),
            model,
            workspace: dir,
            phase: Phase::Running,
            cost_micros: 0,
            tokens_in: 0,
            tokens_out: 0,
            tools: Vec::new(),
            calls: 0,
            activity: String::new(),
            started_ms: unix_now_ms(),
            finished_ms: None,
            result: None,
            pending: Vec::new(),
        };
        self.sessions.lock().unwrap().push(Tracked {
            view: view.clone(),
            task: started.id,
            approvals: Vec::new(),
            misses: 0,
        });
        self.emit(Event::Session(Box::new(view)));
        Ok(session.id)
    }

    /// A small model often names no session: then the only live one, else the only one at all.
    fn pick_session(&self, agent: &str, id: &str) -> Result<String, String> {
        if !id.trim().is_empty() {
            return Ok(id.trim().to_string());
        }
        let s = self.sessions.lock().unwrap();
        let mine: Vec<&Tracked> = s.iter().filter(|t| t.view.agent == agent).collect();
        let live: Vec<&Tracked> =
            mine.iter().copied().filter(|t| !t.view.phase.is_over()).collect();
        match (live.as_slice(), mine.as_slice()) {
            ([one], _) | ([], [one]) => Ok(one.view.id.clone()),
            ([], []) => Err("you have not started a session".into()),
            _ => Err(format!(
                "name the session: {}",
                mine.iter().map(|t| t.view.id.as_str()).collect::<Vec<_>>().join(", ")
            )),
        }
    }

    pub(crate) fn session_result(
        &self,
        agent: &str,
        id: &str,
        wait: Duration,
    ) -> Result<SessionView, String> {
        let picked = self.pick_session(agent, id)?;
        let id = picked.as_str();
        let deadline = Instant::now() + wait.min(MAX_WAIT);
        loop {
            let view = self
                .sessions
                .lock()
                .unwrap()
                .iter()
                .find(|t| t.view.id == id && t.view.agent == agent)
                .map(|t| t.view.clone())
                .ok_or_else(|| format!("you have no session {id}"))?;
            if view.phase.is_over() || Instant::now() >= deadline {
                return Ok(view);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    pub(crate) fn close_session(&self, agent: &str, id: &str) -> Result<(), String> {
        let agentd = self.agentd.as_ref().ok_or("no comp-agentd is configured")?;
        let picked = self.pick_session(agent, id)?;
        let id = picked.as_str();
        let (known, over) = {
            let s = self.sessions.lock().unwrap();
            match s.iter().find(|t| t.view.id == id && t.view.agent == agent) {
                Some(t) => (true, t.view.phase.is_over()),
                None => (false, false),
            }
        };
        if !known {
            return Err(format!("you have no session {id}"));
        }
        agentd.close(id)?;
        if !over {
            self.finish_session(id, Phase::Cancelled, Some("stopped by its owner".into()));
        }
        Ok(())
    }

    fn finish_session(&self, id: &str, phase: Phase, result: Option<String>) {
        let mut events = Vec::new();
        {
            let mut s = self.sessions.lock().unwrap();
            let Some(t) = s.iter_mut().find(|t| t.view.id == id) else { return };
            if t.view.phase.is_over() {
                return;
            }
            for (aid, _) in t.approvals.drain(..) {
                events.push(Event::ApprovalResolved { id: aid, approved: false, timed_out: false });
            }
            t.view.pending.clear();
            t.view.phase = phase;
            t.view.activity.clear();
            t.view.finished_ms = Some(unix_now_ms());
            if t.view.result.is_none() {
                t.view.result = result;
            }
            events.push(Event::Session(Box::new(t.view.clone())));
        }
        for e in events {
            self.emit(e);
        }
    }

    pub(crate) fn poll_sessions(&self) {
        let Some(agentd) = &self.agentd else { return };
        let live: Vec<(String, String)> = self
            .sessions
            .lock()
            .unwrap()
            .iter()
            .filter(|t| !t.view.phase.is_over())
            .map(|t| (t.view.id.clone(), t.task.clone()))
            .collect();
        for (id, task) in live {
            match agentd.state(&id) {
                Ok(st) => self.apply_state(&id, &task, st),
                Err(e) => {
                    let lost = {
                        let mut s = self.sessions.lock().unwrap();
                        s.iter_mut().find(|t| t.view.id == id).map(|t| {
                            t.misses += 1;
                            t.misses >= MAX_MISSES
                        })
                    };
                    if lost == Some(true) {
                        self.finish_session(
                            &id,
                            Phase::Failed,
                            Some(format!("lost the daemon: {e}")),
                        );
                    }
                }
            }
        }
    }

    fn apply_state(&self, id: &str, task: &str, st: StateDoc) {
        let mut events = Vec::new();
        {
            let mut s = self.sessions.lock().unwrap();
            let Some(t) = s.iter_mut().find(|t| t.view.id == id) else { return };
            t.misses = 0;
            let mut view = fold(&t.view, &st, task, unix_now_ms());

            let waiting: Vec<&PendingDoc> = st
                .pending_approvals
                .iter()
                .filter(|p| p.task_id.is_empty() || p.task_id == task)
                .collect();
            let mut pending = Vec::new();
            let mut registered = Vec::new();
            for p in &waiting {
                let known = t.approvals.iter().find(|(_, c)| *c == p.call_id).map(|(i, _)| *i);
                let aid = known.unwrap_or_else(|| {
                    self.next_approval.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                });
                let item = Pending {
                    id: aid,
                    agent: view.agent.clone(),
                    tool: format!("session:{}", p.tool_name),
                    args: p.input.clone(),
                    chain: vec![format!("session {}", id)],
                };
                if known.is_none() {
                    events.push(Event::ApprovalRequested {
                        id: aid,
                        agent: item.agent.clone(),
                        tool: item.tool.clone(),
                        args: item.args.clone(),
                        chain: item.chain.clone(),
                    });
                }
                registered.push((aid, p.call_id.clone()));
                pending.push(item);
            }
            for (aid, call) in &t.approvals {
                if !registered.iter().any(|(_, c)| c == call) {
                    let approved = st.transcript.iter().any(|e| {
                        matches!(e, Entry::ToolCall { call_id, decision: Some(d), .. }
                            if call_id == call && d == "approve")
                    });
                    events.push(Event::ApprovalResolved { id: *aid, approved, timed_out: false });
                }
            }
            t.approvals = registered;
            view.pending = pending;

            if view != t.view {
                t.view = view.clone();
                events.push(Event::Session(Box::new(view)));
            }
        }
        for e in events {
            self.emit(e);
        }
    }

    /// `None`: `id` is not a session's approval.
    pub(crate) fn resolve_session_approval(
        &self,
        id: u64,
        approved: bool,
        scope: &str,
    ) -> Option<bool> {
        let agentd = self.agentd.as_ref()?;
        let (session, call) = {
            let s = self.sessions.lock().unwrap();
            s.iter().find_map(|t| {
                t.approvals
                    .iter()
                    .find(|(a, _)| *a == id)
                    .map(|(_, c)| (t.view.id.clone(), c.clone()))
            })?
        };
        // The daemon first: if it refuses, the approval stays and can be answered again.
        if agentd.approve(&session, &call, approved, scope).is_err() {
            return Some(false);
        }
        {
            let mut s = self.sessions.lock().unwrap();
            if let Some(t) = s.iter_mut().find(|t| t.view.id == session) {
                t.approvals.retain(|(a, _)| *a != id);
                t.view.pending.retain(|p| p.id != id);
            }
        }
        self.emit(Event::ApprovalResolved { id, approved, timed_out: false });
        Some(true)
    }

    pub(crate) fn start_session_watcher(self: &std::sync::Arc<Self>) {
        let Some(agentd) = &self.agentd else { return };
        let poll = agentd.config().poll;
        let rt = self.clone();
        std::thread::spawn(move || {
            while !rt.is_stopping() {
                rt.poll_sessions();
                std::thread::sleep(poll);
            }
            rt.close_all_sessions();
        });
    }

    /// A session left open keeps a task running with nobody to answer it.
    pub fn close_all_sessions(&self) {
        let Some(agentd) = &self.agentd else { return };
        let live: Vec<String> = self
            .sessions
            .lock()
            .unwrap()
            .iter()
            .filter(|t| !t.view.phase.is_over())
            .map(|t| t.view.id.clone())
            .collect();
        for id in live {
            let _ = agentd.close(&id);
            self.finish_session(&id, Phase::Cancelled, Some("the runtime shut down".into()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> SessionView {
        SessionView {
            id: "s1".into(),
            agent: "boss".into(),
            run_id: "boss-1-1".into(),
            call_span: Some("span".into()),
            trace_id: "t".into(),
            task: "fix the test".into(),
            model: "m".into(),
            workspace: "/w".into(),
            phase: Phase::Running,
            cost_micros: 0,
            tokens_in: 0,
            tokens_out: 0,
            tools: Vec::new(),
            calls: 0,
            activity: String::new(),
            started_ms: 1_000,
            finished_ms: None,
            result: None,
            pending: Vec::new(),
        }
    }

    fn state(v: Value) -> StateDoc {
        serde_json::from_value(v).expect("a state document")
    }

    fn usage(micros: u64) -> Value {
        json!({"input_tokens": 100, "output_tokens": 20, "cache_read_tokens": 30,
               "cache_write_tokens": 0, "cost_usd_micros": micros})
    }

    #[test]
    fn a_session_that_is_working_shows_what_it_is_doing_and_what_it_has_cost() {
        let st = state(json!({
            "session": {"id": "s1", "status": "running", "usage": usage(2_500)},
            "transcript": [
                {"type": "user_message", "task_id": "t1", "text": "go"},
                {"type": "tool_call", "task_id": "t1", "call_id": "c1", "tool_name": "read_file",
                 "input": {"path": "src/lib.rs"}, "status": "finished"},
                {"type": "tool_call", "task_id": "t1", "call_id": "c2", "tool_name": "run",
                 "input": {"command": "cargo test\nsecond line"}, "status": "running"},
            ],
        }));
        let v = fold(&view(), &st, "t1", 5_000);
        assert_eq!(v.phase, Phase::Running);
        assert_eq!(v.cost_micros, 2_500);
        // the prompt cache counts as prompt: it is what was read, however it was billed
        assert_eq!((v.tokens_in, v.tokens_out), (130, 20));
        assert_eq!(v.tools, ["read_file", "run"]);
        assert_eq!(v.calls, 2);
        assert_eq!(v.activity, "run cargo test");
        assert_eq!(v.finished_ms, None);
    }

    #[test]
    fn a_session_waiting_for_a_person_needs_you_and_says_for_what() {
        let st = state(json!({
            "session": {"id": "s1", "status": "awaiting_approval", "usage": usage(10)},
            "transcript": [
                {"type": "tool_call", "task_id": "t1", "call_id": "c1", "tool_name": "write_file",
                 "input": {"path": "a.txt", "content": "x"}, "status": "awaiting_approval"},
            ],
            "pending_approvals": [
                {"task_id": "t1", "call_id": "c1", "tool_name": "write_file",
                 "input": {"path": "a.txt", "content": "x"}},
            ],
        }));
        let v = fold(&view(), &st, "t1", 5_000);
        assert_eq!(v.phase, Phase::NeedsYou);
        assert_eq!(v.activity, "waiting to write_file a.txt");
    }

    #[test]
    fn a_finished_task_is_done_with_its_answer_and_stays_finished() {
        let st = state(json!({
            "session": {"id": "s1", "status": "idle", "usage": usage(900_000)},
            "transcript": [
                {"type": "task_ended", "task_id": "t1", "outcome": "completed", "result": "fixed it",
                 "usage": usage(900_000)},
            ],
        }));
        let v = fold(&view(), &st, "t1", 7_000);
        assert_eq!(v.phase, Phase::Done);
        assert_eq!(v.result.as_deref(), Some("fixed it"));
        assert_eq!(v.finished_ms, Some(7_000));
        let again = fold(&v, &st, "t1", 9_000);
        assert_eq!(again.finished_ms, Some(7_000));
        assert!(again.for_model().starts_with("done: fixed it"));
    }

    #[test]
    fn a_task_that_failed_says_why_and_a_cancelled_one_is_stopped_not_failed() {
        let failed = state(json!({
            "session": {"id": "s1", "status": "idle", "usage": usage(1)},
            "transcript": [{"type": "task_ended", "task_id": "t1", "outcome": "failed",
                            "code": "budget_exceeded", "message": "over the $1 ceiling"}],
        }));
        let v = fold(&view(), &failed, "t1", 1);
        assert_eq!(v.phase, Phase::Failed);
        assert_eq!(v.result.as_deref(), Some("budget_exceeded: over the $1 ceiling"));

        let cancelled = state(json!({
            "session": {"id": "s1", "status": "closed", "usage": usage(1)},
            "transcript": [{"type": "task_ended", "task_id": "t1", "outcome": "failed",
                            "code": "cancelled", "message": "cancelled"}],
        }));
        assert_eq!(fold(&view(), &cancelled, "t1", 1).phase, Phase::Cancelled);
    }

    #[test]
    fn another_task_in_the_same_session_is_not_this_ones_end() {
        let st = state(json!({
            "session": {"id": "s1", "status": "running", "usage": usage(1)},
            "transcript": [
                {"type": "task_ended", "task_id": "older", "outcome": "completed", "result": "x"},
                {"type": "unknown_future_entry", "task_id": "t1"},
            ],
        }));
        assert_eq!(fold(&view(), &st, "t1", 1).phase, Phase::Running);
    }

    #[test]
    fn what_the_model_is_told_depends_on_the_state() {
        let mut v = view();
        v.cost_micros = 120_000;
        v.calls = 3;
        v.activity = "write_file a.txt".into();
        assert_eq!(v.for_model(), "still running: write_file a.txt ($0.12, 3 tool call(s))");
        v.phase = Phase::NeedsYou;
        v.pending = vec![Pending {
            id: 1,
            agent: "boss".into(),
            tool: "session:run".into(),
            args: Value::Null,
            chain: vec![],
        }];
        assert!(v.for_model().contains("waiting for a person to approve session:run"));
    }

    #[test]
    fn a_policy_that_makes_no_sense_is_refused() {
        use crate::spec::SessionPolicy;
        assert!(SessionPolicy::default().validate().is_ok());
        let bad = |f: fn(&mut SessionPolicy)| {
            let mut p = SessionPolicy::default();
            f(&mut p);
            p.validate().is_err()
        };
        assert!(bad(|p| p.budget_usd = 0.0));
        assert!(bad(|p| p.budget_usd = 1000.0));
        assert!(bad(|p| p.max_concurrent = 0));
        assert!(bad(|p| p.auto_approve = vec!["rm".into()]));
        assert!(bad(|p| p.workspaces = vec!["relative/dir".into()]));
    }
}
