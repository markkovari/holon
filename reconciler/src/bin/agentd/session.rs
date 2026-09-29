//! A session's state and its event log. Every change a caller can observe is
//! an event appended here, under one lock, so `seq` has no gaps and a reader
//! resuming at `after_seq` sees exactly what it missed.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

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
    state: Mutex<State>,
    /// The last seq written; readers wait on it instead of polling.
    seq: watch::Sender<u64>,
    /// The conversation so far. Held for the length of a task, and a session
    /// runs one task at a time, so nothing else contends for it.
    pub history: tokio::sync::Mutex<Vec<Msg>>,
}

// ponytail: every session and its whole event log live in memory until the
// process exits. Fine for a single-user daemon; persist the log (JetStream,
// the way comp-park does) and evict closed sessions when it serves many users.
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

impl Session {
    pub fn new(req: wire::CreateSession, workspace: PathBuf) -> Self {
        Session {
            id: wire::new_id("ses"),
            model: req.model,
            workspace,
            max_budget: req.max_budget_usd_micros,
            auto_approve: req.auto_approve_tools,
            labels: req.labels,
            created_at: wire::now(),
            state: Mutex::default(),
            seq: watch::channel(0).0,
            history: tokio::sync::Mutex::default(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
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
        s.events.push(Event {
            seq,
            session_id: self.id.clone(),
            task_id: task_id.to_string(),
            time: wire::now(),
            payload,
        });
        self.seq.send_replace(seq);
    }

    /// Append an event for `task_id` — dropped if that task is no longer the
    /// active one (the session was closed under it).
    pub fn emit(&self, task_id: &str, payload: Payload) {
        let mut s = self.lock();
        if s.active.as_ref().is_some_and(|a| a.task_id == task_id) {
            self.push(&mut s, task_id, payload);
        }
    }

    /// Start `task_id`, or say why not. With `key`, a repeat returns the
    /// original task and starts nothing.
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

    /// End `task_id` with `ok` (the result) or a failure, and go idle.
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

    /// Register a pending approval; the receiver resolves when someone answers.
    pub fn await_approval(&self, call_id: &str) -> oneshot::Receiver<Answer> {
        let (tx, rx) = oneshot::channel();
        self.lock().pending.insert(call_id.to_string(), tx);
        rx
    }

    /// Forget a pending approval (it timed out); false if it was already answered.
    pub fn drop_approval(&self, call_id: &str) -> bool {
        self.lock().pending.remove(call_id).is_some()
    }

    pub fn answer(&self, call_id: &str, a: Answer) -> bool {
        let tx = self.lock().pending.remove(call_id);
        tx.is_some_and(|tx| tx.send(a).is_ok())
    }

    /// Close, cancelling the running task if there is one.
    pub fn close(&self) {
        let mut s = self.lock();
        s.closed = true;
        s.pending.clear();
        if let Some(a) = s.active.take() {
            if let Some(h) = &a.handle {
                h.abort();
            }
            if let Some(t) = s.tasks.get_mut(&a.task_id) {
                t.status = TaskStatus::Failed;
            }
            let payload = Payload::TaskFailed {
                code: FailCode::Cancelled,
                message: "session closed".into(),
                task_usage: a.usage,
            };
            self.push(&mut s, &a.task_id, payload);
        }
    }

    pub fn events_after(&self, seq: u64) -> Vec<Event> {
        self.lock().events.iter().skip(seq as usize).cloned().collect()
    }

    pub fn watch(&self) -> watch::Receiver<u64> {
        self.seq.subscribe()
    }
}

pub type Registry = Mutex<HashMap<String, Arc<Session>>>;
