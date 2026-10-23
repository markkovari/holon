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

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, watch};

use super::model::{Call, Msg, ToolResult};
use super::wire::{
    self, ApprovalScope, Decision, Event, FailCode, Payload, SessionStatus, TaskStatus, Usage,
    WhenBusy,
};

/// A human's answer to one pending tool call.
#[derive(Clone, Debug)]
pub struct Answer {
    pub decision: Decision,
    pub reason: String,
    pub decided_by: String,
    /// How far an approval reaches (ADR-0114). Only an approval has a reach.
    pub scope: ApprovalScope,
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
    /// Tasks accepted onto a busy session, oldest first (ADR-0112). Memory only: it is
    /// derived from the log on load, where a task that never started is ended.
    queue: VecDeque<Queued>,
    /// The running task, if it is waiting on a human with no loop of its own (ADR-0113).
    parked: Option<Parked>,
    /// Tools a person approved for the rest of the session (ADR-0114). Derived from the log on
    /// load, like everything else: an `approve` with scope `session`, for the tool of the call
    /// the log shows it was asked about.
    approved: BTreeSet<String>,
}

/// Where a task stood when it parked on a human (ADR-0113): enough to carry on from the answer
/// without the process that asked. The conversation is `history.json`, written beside it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Checkpoint {
    pub task_id: String,
    /// Which model turn this step belongs to (0-based), so `max_turns` still counts across a park.
    pub turn: u32,
    /// The calls the model asked for in this step, in order.
    pub calls: Vec<Call>,
    /// The one waiting for a human is `calls[next]`; the ones before it have `results`.
    pub next: usize,
    pub results: Vec<ToolResult>,
    /// When to give up on the human, Unix milliseconds, if there is a limit.
    #[serde(default)]
    pub deadline: Option<u64>,
    /// The ticket parked for it, once there is one.
    #[serde(default)]
    pub ticket: Option<String>,
}

impl Checkpoint {
    /// The call a human has yet to decide.
    pub fn call(&self) -> &Call {
        &self.calls[self.next]
    }
}

struct Parked {
    cp: Checkpoint,
    /// A human's answer has been given to the ticket, and the delivery is yet to come: a second
    /// answer is refused, as it is for a wait that is in memory.
    answered: bool,
}

/// What a caller may know of a parked approval.
#[derive(Clone, Debug)]
pub struct ParkedView {
    pub call_id: String,
    pub tool: String,
    pub deadline: Option<u64>,
}

pub enum BeginPark {
    Parked,
    /// The task is not the running one any more (it was cancelled): nothing to park.
    Gone,
    /// The checkpoint could not be written, and a wait that cannot be resumed is not a park.
    Unsaved(String),
}

pub enum Resume {
    /// This answer is the one that moves the task on: here is where it stood.
    Resumed(Checkpoint),
    /// This call was already decided: a delivery that came twice.
    Duplicate,
    /// There is no such wait and never will be: the task ended, or the session did.
    Gone,
}

/// A task or session ended with a human's answer pending: the tickets to cancel.
pub struct Cancelled {
    pub task: Option<wire::Task>,
    pub next: Option<Started>,
    pub tickets: Vec<String>,
}

/// A task waiting for the running one to end.
struct Queued {
    task_id: String,
    prompt: String,
    created_at: String,
}

struct Active {
    task_id: String,
    usage: Usage,
    handle: Option<tokio::task::JoinHandle<()>>,
    /// `(steer_id, message)`, accepted and not yet applied (ADR-0107). They live
    /// and die with the task: a restart ends the task, so nothing here is saved.
    steers: VecDeque<(String, String)>,
    /// The loop has decided to end the task with its final answer; a steer that
    /// arrives now would be accepted and never read, so it is refused instead.
    finishing: bool,
    /// Steer idempotency keys, for the same reason: they only matter while the task runs.
    steer_keys: HashMap<String, String>,
    /// Tools a person approved for the rest of this task (ADR-0114). It ends with the task.
    approved: BTreeSet<String>,
}

impl Active {
    fn new(task_id: &str) -> Active {
        Active {
            task_id: task_id.to_string(),
            usage: Usage::default(),
            handle: None,
            steers: VecDeque::new(),
            finishing: false,
            steer_keys: HashMap::new(),
            approved: BTreeSet::new(),
        }
    }
}

pub enum Refusal {
    Closed,
    Busy,
    /// Queueing was asked for and this many are already waiting.
    QueueFull(usize),
}

/// A task that has just become the running one and has no loop yet. Whoever is handed one
/// (`begin`, `finish`, `cancel`) must give it to `agent::launch`, or it runs nothing.
pub struct Started {
    pub task: wire::Task,
    pub prompt: String,
}

/// What `begin` did with a task.
pub enum Begun {
    /// The session was idle: it is running now, and needs its loop.
    Started(Started),
    /// The session was busy and `when_busy` said queue: it starts when its turn comes.
    Queued(wire::Task),
    /// The same `Idempotency-Key` as an earlier task: that one, and nothing was started.
    Repeat(wire::Task),
}

pub enum SteerRefusal {
    Closed,
    /// There is no running task with that id: it ended, or never ran.
    NotRunning,
    /// The task is writing its final answer. Send a new task.
    Finishing,
}

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
    ///
    /// With `park` (ADR-0113) a task that was waiting on a human is not cut off: it is parked
    /// again, from the checkpoint it wrote, and goes on when the answer is delivered. Without
    /// it, or with no readable checkpoint that matches what the log says it was waiting for, it
    /// is failed as before.
    pub fn load(dir: &Path, park: bool) -> Result<Self> {
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
        // What a person approved beyond one call (ADR-0114), read back out of the log: the tool
        // is named by the approval that was asked, and the reach by the answer that decided it.
        let mut asked: HashMap<String, String> = HashMap::new();
        let mut for_task: HashMap<String, BTreeSet<String>> = HashMap::new();
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
                Payload::TaskQueued { prompt } => {
                    st.tasks.insert(e.task_id.clone(), task(&meta.id, &e, TaskStatus::Queued));
                    st.queue.push_back(Queued {
                        task_id: e.task_id.clone(),
                        prompt: prompt.clone(),
                        created_at: e.time.clone(),
                    });
                }
                Payload::TaskStarted { .. } => {
                    st.queue.retain(|q| q.task_id != e.task_id);
                    let mut t = task(&meta.id, &e, TaskStatus::Running);
                    // A task that waited was created when it was accepted, not when it started.
                    if let Some(queued) = st.tasks.get(&e.task_id) {
                        t.created_at = queued.created_at.clone();
                    }
                    st.tasks.insert(e.task_id.clone(), t);
                    open = Some((e.task_id.clone(), Usage::default()));
                }
                Payload::UsageUpdated { delta, .. } => {
                    st.usage.add(delta);
                    if let Some((_, u)) = open.as_mut() {
                        u.add(delta);
                    }
                }
                Payload::ToolApprovalRequired { call_id, tool_name, .. } => {
                    asked.insert(call_id.clone(), tool_name.clone());
                }
                Payload::ToolApprovalResolved {
                    call_id,
                    decision: Decision::Approve,
                    scope,
                    ..
                } => {
                    if let Some(tool) = asked.get(call_id) {
                        match scope {
                            ApprovalScope::Once => {}
                            ApprovalScope::Task => {
                                for_task.entry(e.task_id.clone()).or_default().insert(tool.clone());
                            }
                            ApprovalScope::Session => {
                                st.approved.insert(tool.clone());
                            }
                        }
                    }
                }
                Payload::TaskCompleted { .. } | Payload::TaskFailed { .. } => {
                    let done = matches!(e.payload, Payload::TaskCompleted { .. });
                    if let Some(t) = st.tasks.get_mut(&e.task_id) {
                        t.status = if done { TaskStatus::Completed } else { TaskStatus::Failed };
                    }
                    st.queue.retain(|q| q.task_id != e.task_id);
                    // Only the running task's end closes it: a queued one cancelled while it ran
                    // ends too, and must not make the running one look finished.
                    if open.as_ref().is_some_and(|(id, _)| *id == e.task_id) {
                        open = None;
                    }
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
            let restored = if park { restore_parked(dir, &task_id, &st.events) } else { None };
            match restored {
                Some(cp) => {
                    // Running, and waiting: what it was. The steers it had not read are in the
                    // log, and its spend so far is what the log says.
                    let mut a = Active::new(&task_id);
                    a.usage = usage;
                    a.approved = for_task.remove(&task_id).unwrap_or_default();
                    a.steers = super::state::fold(&st.events)
                        .pending_steers
                        .into_iter()
                        .filter(|p| p.task_id == task_id)
                        .map(|p| (p.steer_id, p.message))
                        .collect();
                    st.active = Some(a);
                    st.parked = Some(Parked { cp, answered: false });
                }
                None => {
                    let _ = std::fs::remove_file(dir.join("checkpoint.json"));
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
            }
        }
        // The queue was in memory. A task accepted and never started is ended here, in the
        // log, like the running one: it is not run now, on a prompt from a process that is
        // gone, and the caller finds out and decides (ADR-0112).
        {
            let mut st = s.lock();
            let waiting: Vec<Queued> = st.queue.drain(..).collect();
            for q in waiting {
                if let Some(t) = st.tasks.get_mut(&q.task_id) {
                    t.status = TaskStatus::Failed;
                }
                let payload = Payload::TaskFailed {
                    code: FailCode::Internal,
                    message: "comp-agentd restarted before this task started".into(),
                    task_usage: Usage::default(),
                };
                s.push(&mut st, &q.task_id, payload);
            }
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
        self.view_of(&s)
    }

    /// The session and the log it is read from, under one lock, so the cursor
    /// (`seq`) is exactly the last event the rest reflects: a client that tails
    /// from it misses nothing and sees nothing twice.
    pub fn state(&self) -> wire::SessionState {
        let s = self.lock();
        let f = super::state::fold(&s.events);
        wire::SessionState {
            session: self.view_of(&s),
            seq: f.seq,
            transcript: f.transcript,
            pending_approvals: f.pending_approvals,
            pending_steers: f.pending_steers,
            queued_tasks: f.queued_tasks,
        }
    }

    fn view_of(&self, s: &State) -> wire::Session {
        let status = if s.closed {
            SessionStatus::Closed
        } else if !s.pending.is_empty() || s.parked.is_some() {
            SessionStatus::AwaitingApproval
        } else if s.active.is_some() {
            SessionStatus::Running
        } else {
            SessionStatus::Idle
        };
        let approved_tools = s
            .approved
            .iter()
            .chain(s.active.iter().flat_map(|a| a.approved.iter()))
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        wire::Session {
            id: self.id.clone(),
            model: self.model.clone(),
            workspace_dir: self.workspace.display().to_string(),
            status,
            usage: s.usage,
            max_budget_usd_micros: self.max_budget,
            auto_approve_tools: self.auto_approve.clone(),
            approved_tools,
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

    /// Append a session-level event, one no task owns.
    pub fn note(&self, payload: Payload) {
        let mut s = self.lock();
        self.push(&mut s, "", payload);
    }

    /// May `task_id` call `tool` without a person saying so: the session was created to allow it
    /// (`auto_approve_tools`), or a person approved it for the session or for this task
    /// (ADR-0114). Asked at each call, so an approval given a moment ago already counts.
    pub fn runs_without_asking(&self, task_id: &str, tool: &str) -> bool {
        if self.auto_approve.iter().any(|t| t == tool) {
            return true;
        }
        let s = self.lock();
        s.approved.contains(tool)
            || s.active.as_ref().is_some_and(|a| a.task_id == task_id && a.approved.contains(tool))
    }

    /// A person's answer to the approval of `tool`'s call `call_id` is decided: the log says so,
    /// and an approval that was given for more than this call is remembered. One lock for both,
    /// so that what the log says was approved and what runs without asking cannot differ (and
    /// `load` reads the same two things back out of the log).
    ///
    /// A denial is for the call it answers, whatever reach it was sent with, and the log says
    /// `once`: nothing here ever remembers a refusal.
    fn decide(&self, s: &mut State, task_id: &str, call_id: &str, tool: &str, answer: &Answer) {
        let scope = match answer.decision {
            Decision::Approve => answer.scope,
            Decision::Deny => ApprovalScope::Once,
        };
        match scope {
            ApprovalScope::Once => {}
            ApprovalScope::Task => {
                if let Some(a) = s.active.as_mut().filter(|a| a.task_id == task_id) {
                    a.approved.insert(tool.to_string());
                }
            }
            ApprovalScope::Session => {
                s.approved.insert(tool.to_string());
            }
        }
        let payload = Payload::ToolApprovalResolved {
            call_id: call_id.to_string(),
            decision: answer.decision,
            decided_by: answer.decided_by.clone(),
            scope,
        };
        self.push(s, task_id, payload);
    }

    /// The approval of `tool`'s call `call_id`, which a task held in memory was waiting for, has
    /// its answer. Dropped, like `emit`, if the task is no longer the running one.
    pub fn resolved(&self, task_id: &str, call_id: &str, tool: &str, answer: &Answer) {
        let mut s = self.lock();
        if s.active.as_ref().is_some_and(|a| a.task_id == task_id) {
            self.decide(&mut s, task_id, call_id, tool, answer);
        }
    }

    /// Start a task, queue it, or say why not. With `key`, a repeat returns the original
    /// task and starts nothing.
    ///
    /// On a busy session `when_busy` decides: `Reject` is `Refusal::Busy`, `Queue` accepts the
    /// task behind the others (up to `max_queued`; 0 switches queueing off, and a busy
    /// session is then busy). On an idle one the task starts, whatever `when_busy` says.
    pub fn begin(
        &self,
        prompt: &str,
        key: Option<String>,
        when_busy: WhenBusy,
        max_queued: usize,
    ) -> Result<Begun, Refusal> {
        let mut s = self.lock();
        if let Some(t) = key.as_ref().and_then(|k| s.by_key.get(k)).and_then(|id| s.tasks.get(id)) {
            return Ok(Begun::Repeat(t.clone()));
        }
        if s.closed {
            return Err(Refusal::Closed);
        }
        let id = wire::new_id("task");
        let created_at = wire::now();
        if s.active.is_some() {
            if when_busy != WhenBusy::Queue || max_queued == 0 {
                return Err(Refusal::Busy);
            }
            if s.queue.len() >= max_queued {
                return Err(Refusal::QueueFull(max_queued));
            }
            let task = wire::Task {
                id: id.clone(),
                session_id: self.id.clone(),
                status: TaskStatus::Queued,
                created_at: created_at.clone(),
            };
            s.tasks.insert(id.clone(), task.clone());
            s.queue.push_back(Queued {
                task_id: id.clone(),
                prompt: prompt.to_string(),
                created_at,
            });
            self.remember(&mut s, key, &id);
            self.push(&mut s, &id, Payload::TaskQueued { prompt: prompt.to_string() });
            return Ok(Begun::Queued(task));
        }
        debug_assert!(s.queue.is_empty(), "an idle session with tasks waiting");
        let task = wire::Task {
            id: id.clone(),
            session_id: self.id.clone(),
            status: TaskStatus::Running,
            created_at,
        };
        s.active = Some(Active::new(&id));
        s.tasks.insert(id.clone(), task.clone());
        self.remember(&mut s, key, &id);
        self.push(&mut s, &id, Payload::TaskStarted { prompt: prompt.to_string() });
        Ok(Begun::Started(Started { task, prompt: prompt.to_string() }))
    }

    /// Remember `key` for `task_id`, so a retry says it once.
    fn remember(&self, s: &mut State, key: Option<String>, task_id: &str) {
        let Some(k) = key else { return };
        s.by_key.insert(k, task_id.to_string());
        if let Err(e) = self.save_meta(s) {
            eprintln!("comp-agentd: {}: saving idempotency key: {e:#}", self.id);
        }
    }

    /// The oldest queued task becomes the running one. Called by whatever just ended the
    /// running task, under the lock it already holds, so there is no instant at which the
    /// session is idle with a task waiting: a task that was accepted starts, or is ended in
    /// the log (cancelled, or closed with the session), and is never left in a queue nothing
    /// will drain. `None` with nothing waiting, or once the session is closed.
    fn promote(&self, s: &mut State) -> Option<Started> {
        if s.closed {
            return None;
        }
        let q = s.queue.pop_front()?;
        debug_assert!(s.active.is_none(), "promoting a queued task over a running one");
        let task = wire::Task {
            id: q.task_id.clone(),
            session_id: self.id.clone(),
            status: TaskStatus::Running,
            created_at: q.created_at,
        };
        s.tasks.insert(q.task_id.clone(), task.clone());
        s.active = Some(Active::new(&q.task_id));
        self.push(s, &q.task_id, Payload::TaskStarted { prompt: q.prompt.clone() });
        Some(Started { task, prompt: q.prompt })
    }

    /// Accept a steer for the running task `task_id`; it joins the conversation at
    /// the task's next model-call boundary (`take_steers`, `finishing`). With a
    /// `key`, a repeat is accepted and queues nothing: whoever retried a steer
    /// that did get through does not say it twice.
    pub fn steer(
        &self,
        task_id: &str,
        message: &str,
        key: Option<String>,
    ) -> Result<(), SteerRefusal> {
        let mut s = self.lock();
        if s.closed {
            return Err(SteerRefusal::Closed);
        }
        let Some(a) = s.active.as_mut().filter(|a| a.task_id == task_id) else {
            return Err(SteerRefusal::NotRunning);
        };
        if key.as_ref().is_some_and(|k| a.steer_keys.contains_key(k)) {
            return Ok(());
        }
        if a.finishing {
            return Err(SteerRefusal::Finishing);
        }
        let id = wire::new_id("steer");
        if let Some(k) = key {
            a.steer_keys.insert(k, id.clone());
        }
        a.steers.push_back((id.clone(), message.to_string()));
        let payload = Payload::TaskSteerQueued { steer_id: id, message: message.to_string() };
        self.push(&mut s, task_id, payload);
        Ok(())
    }

    /// The steers accepted since the loop last looked, oldest first. Taken under
    /// the lock, so each is applied once.
    pub fn take_steers(&self, task_id: &str) -> Vec<(String, String)> {
        let mut s = self.lock();
        match s.active.as_mut().filter(|a| a.task_id == task_id) {
            Some(a) => a.steers.drain(..).collect(),
            None => Vec::new(),
        }
    }

    /// The loop has the model's final answer and is about to end the task. Steers
    /// that arrived since its last boundary come back instead — the task is not
    /// finishing, it has new input — and with none, no more are accepted. One
    /// lock orders this against `steer`, so a steer is either applied or refused,
    /// never accepted and then dropped on the floor.
    pub fn finishing(&self, task_id: &str) -> Vec<(String, String)> {
        let mut s = self.lock();
        let Some(a) = s.active.as_mut().filter(|a| a.task_id == task_id) else {
            return Vec::new();
        };
        if a.steers.is_empty() {
            a.finishing = true;
            Vec::new()
        } else {
            a.steers.drain(..).collect()
        }
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

    /// End `task_id` with its result or a failure, and go idle, or on to the next queued task,
    /// which comes back to be launched. A no-op if it is not the active task any more.
    pub fn finish(
        &self,
        task_id: &str,
        outcome: Result<String, (FailCode, String)>,
    ) -> Option<Started> {
        let mut s = self.lock();
        let a = s.active.take_if(|a| a.task_id == task_id)?;
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
        self.promote(&mut s)
    }

    pub fn task(&self, task_id: &str) -> Option<wire::Task> {
        self.lock().tasks.get(task_id).cloned()
    }

    /// Stop `task_id`: the one running, or one still queued, which leaves the queue without
    /// ever starting. Returns the task as it now stands, or `None` if there is no such task,
    /// the next queued task if stopping the running one let it start, and the ticket of the
    /// approval it was parked on, if it was.
    pub fn cancel(&self, task_id: &str, why: &str) -> Cancelled {
        let mut s = self.lock();
        let mut next = None;
        let mut tickets = Vec::new();
        if let Some(a) = s.active.take_if(|a| a.task_id == task_id) {
            if let Some(h) = &a.handle {
                h.abort();
            }
            if let Some(t) = s.tasks.get_mut(task_id) {
                t.status = TaskStatus::Failed;
            }
            s.pending.clear();
            if let Some(p) = s.parked.take() {
                tickets.extend(p.cp.ticket);
                self.remove_checkpoint();
            }
            let payload = Payload::TaskFailed {
                code: FailCode::Cancelled,
                message: why.into(),
                task_usage: a.usage,
            };
            self.push(&mut s, task_id, payload);
            next = self.promote(&mut s);
        } else if let Some(i) = s.queue.iter().position(|q| q.task_id == task_id) {
            s.queue.remove(i);
            if let Some(t) = s.tasks.get_mut(task_id) {
                t.status = TaskStatus::Failed;
            }
            let payload = Payload::TaskFailed {
                code: FailCode::Cancelled,
                message: why.into(),
                task_usage: Usage::default(),
            };
            self.push(&mut s, task_id, payload);
        }
        Cancelled { task: s.tasks.get(task_id).cloned(), next, tickets }
    }

    // ---- a wait that is parked (ADR-0113) ----

    fn checkpoint_file(&self) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join("checkpoint.json"))
    }

    fn remove_checkpoint(&self) {
        if let Some(p) = self.checkpoint_file() {
            let _ = std::fs::remove_file(p);
        }
    }

    /// The conversation, written so that a restart can read it back; an error is the caller's,
    /// because a park whose conversation was not saved could not be resumed.
    pub fn save_history_checked(&self, h: &[Msg]) -> Result<()> {
        let Some(dir) = &self.dir else { return Ok(()) };
        write_atomic(&dir.join("history.json"), &serde_json::to_vec(h)?)
    }

    /// The task parks on a human: its checkpoint is written and it is `parked`, which is a
    /// running task without a loop. The approval is not yet announced or ticketed, so the order
    /// is the caller's: checkpoint here, then the event, then the ticket, and a crash between any
    /// two leaves something `load` can finish (it parks the approval again) or refuse.
    pub fn begin_park(&self, cp: Checkpoint) -> BeginPark {
        let mut s = self.lock();
        if !s.active.as_ref().is_some_and(|a| a.task_id == cp.task_id) {
            return BeginPark::Gone;
        }
        if let Some(p) = self.checkpoint_file() {
            let written = serde_json::to_vec(&cp).map_err(anyhow::Error::from);
            if let Err(e) = written.and_then(|b| write_atomic(&p, &b)) {
                return BeginPark::Unsaved(format!("{e:#}"));
            }
        }
        s.parked = Some(Parked { cp, answered: false });
        BeginPark::Parked
    }

    /// The ticket for the approval of `call_id` exists. Written to the checkpoint too, so that a
    /// restart can cancel it.
    pub fn set_ticket(&self, call_id: &str, ticket: &str) {
        let mut s = self.lock();
        let Some(p) = s.parked.as_mut().filter(|p| p.cp.call().id == call_id) else { return };
        p.cp.ticket = Some(ticket.to_string());
        if let Some(path) = self.checkpoint_file() {
            let r = serde_json::to_vec(&p.cp).map_err(anyhow::Error::from);
            if let Err(e) = r.and_then(|b| write_atomic(&path, &b)) {
                eprintln!("comp-agentd: {}: saving the ticket in the checkpoint: {e:#}", self.id);
            }
        }
    }

    /// The park did not happen: undo it, and say which ticket, if any, to cancel.
    pub fn abort_park(&self, call_id: &str) -> Option<String> {
        let mut s = self.lock();
        if !s.parked.as_ref().is_some_and(|p| p.cp.call().id == call_id) {
            return None;
        }
        self.remove_checkpoint();
        s.parked.take().and_then(|p| p.cp.ticket)
    }

    /// The approval the running task is parked on, if it is.
    pub fn parked(&self) -> Option<ParkedView> {
        let s = self.lock();
        s.parked.as_ref().map(|p| view_of_parked(&p.cp))
    }

    /// A human is giving `call_id` its answer: the first to claim it may, and a second is
    /// refused until the first fails to reach the ticket (`unclaim_parked_answer`).
    pub fn claim_parked_answer(&self, call_id: &str) -> Option<ParkedView> {
        let mut s = self.lock();
        let p = s.parked.as_mut().filter(|p| p.cp.call().id == call_id && !p.answered)?;
        p.answered = true;
        Some(view_of_parked(&p.cp))
    }

    pub fn unclaim_parked_answer(&self, call_id: &str) {
        let mut s = self.lock();
        if let Some(p) = s.parked.as_mut().filter(|p| p.cp.call().id == call_id) {
            p.answered = false;
        }
    }

    /// The answer to `call_id` has been delivered. If the task is parked on exactly that call it
    /// is no longer parked, the log says the call was decided, and the checkpoint is taken and
    /// removed under one lock: a delivery that comes twice finds the second time that it is
    /// already decided, and a crash after this finds no checkpoint and fails the task rather
    /// than running a tool that may already have run (ADR-0106's `unsafe`).
    pub fn resume_parked(&self, call_id: &str, answer: &Answer) -> Resume {
        let mut s = self.lock();
        if s.parked.as_ref().is_some_and(|p| p.cp.call().id == call_id) {
            let Some(p) = s.parked.take() else { return Resume::Gone };
            self.remove_checkpoint();
            let task_id = p.cp.task_id.clone();
            self.decide(&mut s, &task_id, call_id, &p.cp.call().name, answer);
            return Resume::Resumed(p.cp);
        }
        let decided = s.events.iter().rev().any(|e| {
            matches!(&e.payload, Payload::ToolApprovalResolved { call_id: c, .. } if c == call_id)
        });
        if decided {
            Resume::Duplicate
        } else {
            Resume::Gone
        }
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

    /// Close, ending the running task and every queued one as cancelled. Closed first, under
    /// one lock, so nothing is promoted into the gap and nothing new is accepted. Returns the
    /// ticket of the approval the running task was parked on, if it was.
    pub fn close(&self) -> Vec<String> {
        let why = "session closed";
        let mut s = self.lock();
        s.closed = true;
        let mut tickets = Vec::new();
        if let Some(a) = s.active.take() {
            if let Some(h) = &a.handle {
                h.abort();
            }
            if let Some(t) = s.tasks.get_mut(&a.task_id) {
                t.status = TaskStatus::Failed;
            }
            s.pending.clear();
            if let Some(p) = s.parked.take() {
                tickets.extend(p.cp.ticket);
                self.remove_checkpoint();
            }
            let payload = Payload::TaskFailed {
                code: FailCode::Cancelled,
                message: why.into(),
                task_usage: a.usage,
            };
            self.push(&mut s, &a.task_id, payload);
        }
        let waiting: Vec<Queued> = s.queue.drain(..).collect();
        for q in waiting {
            if let Some(t) = s.tasks.get_mut(&q.task_id) {
                t.status = TaskStatus::Failed;
            }
            let payload = Payload::TaskFailed {
                code: FailCode::Cancelled,
                message: why.into(),
                task_usage: Usage::default(),
            };
            self.push(&mut s, &q.task_id, payload);
        }
        if let Err(e) = self.save_meta(&s) {
            eprintln!("comp-agentd: {}: saving closed state: {e:#}", self.id);
        }
        tickets
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

fn view_of_parked(cp: &Checkpoint) -> ParkedView {
    ParkedView {
        call_id: cp.call().id.clone(),
        tool: cp.call().name.clone(),
        deadline: cp.deadline,
    }
}

/// The checkpoint a restart may park a task from: readable, for this task, naming a call that
/// exists, and for a call the log shows was asked about and not yet decided. Anything else is a
/// task cut off in the middle of something, which is failed.
fn restore_parked(dir: &Path, task_id: &str, events: &[Event]) -> Option<Checkpoint> {
    let cp: Checkpoint = read_json(&dir.join("checkpoint.json")).ok()?;
    if cp.task_id != task_id || cp.next >= cp.calls.len() {
        return None;
    }
    let call_id = &cp.call().id;
    let asked = events.iter().rev().find_map(|e| match &e.payload {
        Payload::ToolApprovalRequired { call_id: c, .. } if c == call_id => Some(true),
        Payload::ToolApprovalResolved { call_id: c, .. } if c == call_id => Some(false),
        _ => None,
    });
    asked.unwrap_or(false).then_some(cp)
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
pub fn load_all(state_dir: &Path, park: bool) -> Result<HashMap<String, Arc<Session>>> {
    let mut out = HashMap::new();
    for entry in
        std::fs::read_dir(state_dir).with_context(|| format!("reading {}", state_dir.display()))?
    {
        let dir = entry?.path();
        if !dir.join("session.json").exists() {
            continue;
        }
        match Session::load(&dir, park) {
            Ok(s) => {
                out.insert(s.id.clone(), Arc::new(s));
            }
            Err(e) => eprintln!("comp-agentd: skipping {}: {e:#}", dir.display()),
        }
    }
    Ok(out)
}
