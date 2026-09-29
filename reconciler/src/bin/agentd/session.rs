//! A session's state and its event log. Every change a caller can observe is
//! an event appended here, under one lock, so `seq` has no gaps and a reader
//! resuming at `after_seq` sees exactly what it missed.
//!
//! With a state directory, a session is three files under `<dir>/<id>/`:
//! `session.json` (what it was created with, whether it is closed, its
//! idempotency keys), `events.jsonl` (the log, one event per line, appended
//! as it happens) and `history.json` (the conversation, rewritten when a task
//! ends). Everything else — spend, task states — is derived from the log on
//! load, so there is one truth and nothing to keep in step.

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, watch};

use super::model::Msg;
use super::wire::{self, Decision, Event, FailCode, Payload, SessionStatus, TaskStatus, Usage};

/// A human's answer to one pending tool call.
pub struct Answer {
    pub decision: Decision,
    pub reason: String,
    pub decided_by: String,
}

pub struct Session {
    pub id: String,
    pub model: String,
    pub workspace: PathBuf,
    pub max_budget: i64,
    pub auto_approve: Vec<String>,
    pub labels: BTreeMap<String, String>,
    pub created_at: String,
    /// Where this session's files live; `None` keeps it in memory only.
    dir: Option<PathBuf>,
    state: Mutex<State>,
    /// The last seq written; readers wait on it instead of polling.
    seq: watch::Sender<u64>,
    /// The conversation so far. Held for the length of a task, and a session
    /// runs one task at a time, so nothing else contends for it.
    pub history: tokio::sync::Mutex<Vec<Msg>>,
}

// ponytail: every session's whole event log is also held in memory, and
// nothing is evicted. Fine for one operator's daemon; page the log from disk
// and drop closed sessions if it ever serves many users.
#[derive(Default)]
struct State {
    closed: bool,
    usage: Usage,
    active: Option<Active>,
    events: Vec<Event>,
    pending: HashMap<String, oneshot::Sender<Answer>>,
    tasks: HashMap<String, wire::Task>,
    by_key: HashMap<String, String>,
}

struct Active {
    task_id: String,
    usage: Usage,
    handle: Option<tokio::task::JoinHandle<()>>,
}

pub enum Refusal {
    Closed,
    Busy,
}

/// `session.json`.
#[derive(Serialize, Deserialize)]
struct Meta {
    id: String,
    model: String,
    workspace_dir: PathBuf,
    max_budget_usd_micros: i64,
    auto_approve_tools: Vec<String>,
    labels: BTreeMap<String, String>,
    created_at: String,
    closed: bool,
    idempotency_keys: HashMap<String, String>,
}

impl Session {
    pub fn new(
        req: wire::CreateSession,
        workspace: PathBuf,
        state_dir: Option<&Path>,
    ) -> Result<Self> {
        let id = wire::new_id("ses");
        let dir = state_dir.map(|d| d.join(&id));
        if let Some(d) = &dir {
            std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
        }
        let s = Session {
            id,
            model: req.model,
            workspace,
            max_budget: req.max_budget_usd_micros,
            auto_approve: req.auto_approve_tools,
            labels: req.labels,
            created_at: wire::now(),
            dir,
            state: Mutex::default(),
            seq: watch::channel(0).0,
            history: tokio::sync::Mutex::default(),
        };
        s.save_meta(&s.lock())?;
        Ok(s)
    }

    /// Bring a session back from its directory. A task the log shows as
    /// started and never ended was cut off by a restart; it is failed here,
    /// in the log, so every reader sees how it ended.
    pub fn load(dir: &Path) -> Result<Self> {
        let meta: Meta = read_json(&dir.join("session.json"))?;
        let history: Vec<Msg> = match std::fs::read(dir.join("history.json")) {
            Ok(b) => serde_json::from_slice(&b).context("history.json")?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e).context("history.json"),
        };
        let mut st =
            State { closed: meta.closed, by_key: meta.idempotency_keys, ..State::default() };
        let text = match std::fs::read_to_string(dir.join("events.jsonl")) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e).context("events.jsonl"),
        };
        let mut open: Option<(String, Usage)> = None;
        let last = text.lines().count();
        for (n, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
            let e: Event = match serde_json::from_str(line) {
                Ok(e) => e,
                // A torn last line is what a crash mid-append leaves; anything
                // earlier is corruption, and silently skipping it would renumber
                // every event after it.
                Err(_) if n + 1 == last => break,
                Err(err) => anyhow::bail!("events.jsonl line {}: {err}", n + 1),
            };
            match &e.payload {
                Payload::TaskStarted { .. } => {
                    st.tasks.insert(e.task_id.clone(), task(&meta.id, &e, TaskStatus::Running));
                    open = Some((e.task_id.clone(), Usage::default()));
                }
                Payload::UsageUpdated { delta, .. } => {
                    st.usage.add(delta);
                    if let Some((_, u)) = open.as_mut() {
                        u.add(delta);
                    }
                }
                Payload::TaskCompleted { .. } | Payload::TaskFailed { .. } => {
                    let done = matches!(e.payload, Payload::TaskCompleted { .. });
                    if let Some(t) = st.tasks.get_mut(&e.task_id) {
                        t.status = if done { TaskStatus::Completed } else { TaskStatus::Failed };
                    }
                    open = None;
                }
                _ => {}
            }
            st.events.push(e);
        }
        let s = Session {
            id: meta.id,
            model: meta.model,
            workspace: meta.workspace_dir,
            max_budget: meta.max_budget_usd_micros,
            auto_approve: meta.auto_approve_tools,
            labels: meta.labels,
            created_at: meta.created_at,
            dir: Some(dir.to_path_buf()),
            seq: watch::channel(st.events.len() as u64).0,
            state: Mutex::new(st),
            history: tokio::sync::Mutex::new(history),
        };
        if let Some((task_id, usage)) = open {
            let mut st = s.lock();
            if let Some(t) = st.tasks.get_mut(&task_id) {
                t.status = TaskStatus::Failed;
            }
            let payload = Payload::TaskFailed {
                code: FailCode::Internal,
                message: "comp-agentd restarted while this task ran".into(),
                task_usage: usage,
            };
            s.push(&mut st, &task_id, payload);
        }
        Ok(s)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn save_meta(&self, s: &State) -> Result<()> {
        let Some(dir) = &self.dir else { return Ok(()) };
        let meta = Meta {
            id: self.id.clone(),
            model: self.model.clone(),
            workspace_dir: self.workspace.clone(),
            max_budget_usd_micros: self.max_budget,
            auto_approve_tools: self.auto_approve.clone(),
            labels: self.labels.clone(),
            created_at: self.created_at.clone(),
            closed: s.closed,
            idempotency_keys: s.by_key.clone(),
        };
        write_atomic(&dir.join("session.json"), &serde_json::to_vec_pretty(&meta)?)
    }

    /// Rewrite `history.json`. Called when a task ends.
    pub fn save_history(&self, h: &[Msg]) {
        if let Some(dir) = &self.dir {
            let r = serde_json::to_vec(h)
                .map_err(anyhow::Error::from)
                .and_then(|b| write_atomic(&dir.join("history.json"), &b));
            if let Err(e) = r {
                eprintln!("comp-agentd: {}: saving history: {e:#}", self.id);
            }
        }
    }

    pub fn view(&self) -> wire::Session {
        let s = self.lock();
        let status = if s.closed {
            SessionStatus::Closed
        } else if !s.pending.is_empty() {
            SessionStatus::AwaitingApproval
        } else if s.active.is_some() {
            SessionStatus::Running
        } else {
            SessionStatus::Idle
        };
        wire::Session {
            id: self.id.clone(),
            model: self.model.clone(),
            workspace_dir: self.workspace.display().to_string(),
            status,
            usage: s.usage,
            max_budget_usd_micros: self.max_budget,
            auto_approve_tools: self.auto_approve.clone(),
            labels: self.labels.clone(),
            created_at: self.created_at.clone(),
            active_task_id: s.active.as_ref().map(|a| a.task_id.clone()),
        }
    }

    fn push(&self, s: &mut State, task_id: &str, payload: Payload) {
        let seq = s.events.len() as u64 + 1;
        let e = Event {
            seq,
            session_id: self.id.clone(),
            task_id: task_id.to_string(),
            time: wire::now(),
            payload,
        };
        if let Some(dir) = &self.dir {
            // ponytail: one open+append+fsync-free write per event, under the
            // session lock. Durable against a process crash, not a power cut;
            // add sync_data() here if the box can lose power mid-task.
            let line = serde_json::to_string(&e).unwrap_or_default();
            let r = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("events.jsonl"))
                .and_then(|mut f| writeln!(f, "{line}"));
            if let Err(err) = r {
                eprintln!("comp-agentd: {}: appending event {seq}: {err}", self.id);
            }
        }
        s.events.push(e);
        self.seq.send_replace(seq);
    }

    /// Append an event for `task_id` — dropped if that task is no longer the
    /// active one (it was cancelled under the caller).
    pub fn emit(&self, task_id: &str, payload: Payload) {
        let mut s = self.lock();
        if s.active.as_ref().is_some_and(|a| a.task_id == task_id) {
            self.push(&mut s, task_id, payload);
        }
    }

    /// Start a task, or say why not. With `key`, a repeat returns the
    /// original task and starts nothing (the bool is false).
    pub fn begin(&self, prompt: &str, key: Option<String>) -> Result<(wire::Task, bool), Refusal> {
        let mut s = self.lock();
        if let Some(t) = key.as_ref().and_then(|k| s.by_key.get(k)).and_then(|id| s.tasks.get(id)) {
            return Ok((t.clone(), false));
        }
        if s.closed {
            return Err(Refusal::Closed);
        }
        if s.active.is_some() {
            return Err(Refusal::Busy);
        }
        let task = wire::Task {
            id: wire::new_id("task"),
            session_id: self.id.clone(),
            status: TaskStatus::Running,
            created_at: wire::now(),
        };
        s.active = Some(Active { task_id: task.id.clone(), usage: Usage::default(), handle: None });
        s.tasks.insert(task.id.clone(), task.clone());
        if let Some(k) = key {
            s.by_key.insert(k, task.id.clone());
            if let Err(e) = self.save_meta(&s) {
                eprintln!("comp-agentd: {}: saving idempotency key: {e:#}", self.id);
            }
        }
        self.push(&mut s, &task.id, Payload::TaskStarted { prompt: prompt.to_string() });
        Ok((task, true))
    }

    pub fn attach(&self, task_id: &str, handle: tokio::task::JoinHandle<()>) {
        let mut s = self.lock();
        match s.active.as_mut() {
            Some(a) if a.task_id == task_id => a.handle = Some(handle),
            // Already finished or cancelled; nothing left to abort.
            _ => {}
        }
    }

    /// Charge one model call. Returns (session total, remaining budget).
    pub fn charge(&self, task_id: &str, delta: Usage) -> (Usage, i64) {
        let mut s = self.lock();
        s.usage.add(&delta);
        if let Some(a) = s.active.as_mut().filter(|a| a.task_id == task_id) {
            a.usage.add(&delta);
        }
        (s.usage, self.max_budget - s.usage.cost_usd_micros)
    }

    pub fn remaining(&self) -> i64 {
        self.max_budget - self.lock().usage.cost_usd_micros
    }

    /// End `task_id` with its result or a failure, and go idle. A no-op if
    /// it is not the active task any more.
    pub fn finish(&self, task_id: &str, outcome: Result<String, (FailCode, String)>) {
        let mut s = self.lock();
        let Some(a) = s.active.take_if(|a| a.task_id == task_id) else { return };
        let (status, payload) = match outcome {
            Ok(result) => {
                (TaskStatus::Completed, Payload::TaskCompleted { result, task_usage: a.usage })
            }
            Err((code, message)) => {
                (TaskStatus::Failed, Payload::TaskFailed { code, message, task_usage: a.usage })
            }
        };
        if let Some(t) = s.tasks.get_mut(task_id) {
            t.status = status;
        }
        s.pending.clear();
        self.push(&mut s, task_id, payload);
    }

    /// Stop `task_id` if it is the one running. Returns the task as it now
    /// stands, or `None` if there is no such task.
    pub fn cancel(&self, task_id: &str, why: &str) -> Option<wire::Task> {
        let mut s = self.lock();
        if let Some(a) = s.active.take_if(|a| a.task_id == task_id) {
            if let Some(h) = &a.handle {
                h.abort();
            }
            if let Some(t) = s.tasks.get_mut(task_id) {
                t.status = TaskStatus::Failed;
            }
            s.pending.clear();
            let payload = Payload::TaskFailed {
                code: FailCode::Cancelled,
                message: why.into(),
                task_usage: a.usage,
            };
            self.push(&mut s, task_id, payload);
        }
        s.tasks.get(task_id).cloned()
    }

    /// Register a pending approval; the receiver resolves when someone answers.
    pub fn await_approval(&self, call_id: &str) -> oneshot::Receiver<Answer> {
        let (tx, rx) = oneshot::channel();
        self.lock().pending.insert(call_id.to_string(), tx);
        rx
    }

    /// Forget a pending approval that timed out.
    pub fn drop_approval(&self, call_id: &str) {
        self.lock().pending.remove(call_id);
    }

    pub fn answer(&self, call_id: &str, a: Answer) -> bool {
        let tx = self.lock().pending.remove(call_id);
        tx.is_some_and(|tx| tx.send(a).is_ok())
    }

    /// Close, cancelling the running task if there is one.
    pub fn close(&self) {
        let active = self.lock().active.as_ref().map(|a| a.task_id.clone());
        if let Some(t) = active {
            self.cancel(&t, "session closed");
        }
        let mut s = self.lock();
        s.closed = true;
        if let Err(e) = self.save_meta(&s) {
            eprintln!("comp-agentd: {}: saving closed state: {e:#}", self.id);
        }
    }

    pub fn events_after(&self, seq: u64) -> Vec<Event> {
        self.lock().events.iter().skip(seq as usize).cloned().collect()
    }

    pub fn watch(&self) -> watch::Receiver<u64> {
        self.seq.subscribe()
    }
}

fn task(session_id: &str, e: &Event, status: TaskStatus) -> wire::Task {
    wire::Task {
        id: e.task_id.clone(),
        session_id: session_id.into(),
        status,
        created_at: e.time.clone(),
    }
}

fn read_json<T: serde::de::DeserializeOwned>(p: &Path) -> Result<T> {
    let b = std::fs::read(p).with_context(|| format!("reading {}", p.display()))?;
    serde_json::from_slice(&b).with_context(|| format!("parsing {}", p.display()))
}

/// Write via a sibling temp file and a rename, so a crash leaves the old
/// file or the new one, never half of either.
fn write_atomic(p: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, p).with_context(|| format!("renaming onto {}", p.display()))
}

pub type Registry = Mutex<HashMap<String, Arc<Session>>>;

/// Every session under `state_dir`. One that fails to load is reported and
/// skipped rather than taking the others down with it.
pub fn load_all(state_dir: &Path) -> Result<HashMap<String, Arc<Session>>> {
    let mut out = HashMap::new();
    for entry in
        std::fs::read_dir(state_dir).with_context(|| format!("reading {}", state_dir.display()))?
    {
        let dir = entry?.path();
        if !dir.join("session.json").exists() {
            continue;
        }
        match Session::load(&dir) {
            Ok(s) => {
                out.insert(s.id.clone(), Arc::new(s));
            }
            Err(e) => eprintln!("comp-agentd: skipping {}: {e:#}", dir.display()),
        }
    }
    Ok(out)
}
