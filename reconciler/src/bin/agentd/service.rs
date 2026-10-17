//! The operations both transports serve. `rest.rs` and `grpc.rs` only
//! translate: a request into one of these calls, a `Fail` into a status.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use futures::Stream;

use super::agent;
use super::session::{Answer, Begun, Refusal, Registry, Session, SteerRefusal};
use super::wire;

pub struct Daemon {
    pub sessions: Registry,
    pub roots: Vec<PathBuf>,
    pub state_dir: Option<PathBuf>,
    pub ctx: Arc<agent::Ctx>,
    /// Tasks a session may have waiting behind its running one (ADR-0112). 0: no queueing.
    pub max_queued: usize,
}

/// Why a call was refused, in terms both transports map: REST to a status
/// and an RFC 9457 `type` slug, gRPC to a code.
pub enum Fail {
    NotFound(String),
    Invalid(&'static str, String),
    Conflict(&'static str, String),
    /// A limit the caller can wait out: REST `429`, gRPC `RESOURCE_EXHAUSTED`.
    Exhausted(&'static str, String),
    Internal(String),
}

type R<T> = Result<T, Fail>;

impl Daemon {
    fn get(&self, id: &str) -> R<Arc<Session>> {
        let map = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        map.get(id).cloned().ok_or_else(|| Fail::NotFound(format!("no session {id}")))
    }

    pub fn create_session(&self, req: wire::CreateSession) -> R<wire::Session> {
        if req.model.trim().is_empty() {
            return Err(Fail::Invalid("bad-request", "model is required".into()));
        }
        if req.max_budget_usd_micros <= 0 {
            return Err(Fail::Invalid("bad-request", "max_budget_usd_micros must be > 0".into()));
        }
        let ws = match std::fs::canonicalize(&req.workspace_dir) {
            Ok(p) if p.is_dir() => p,
            _ => {
                return Err(Fail::Invalid(
                    "bad-workspace",
                    format!("{} is not a directory", req.workspace_dir),
                ))
            }
        };
        if !self.roots.iter().any(|r| ws.starts_with(r)) {
            return Err(Fail::Invalid(
                "bad-workspace",
                format!("{} is outside every --workspace-root", ws.display()),
            ));
        }
        let sess = Session::new(req, ws, self.state_dir.as_deref())
            .map_err(|e| Fail::Internal(format!("{e:#}")))?;
        let view = sess.view();
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(sess.id.clone(), Arc::new(sess));
        Ok(view)
    }

    pub fn get_session(&self, id: &str) -> R<wire::Session> {
        Ok(self.get(id)?.view())
    }

    pub fn close_session(&self, id: &str) -> R<wire::Session> {
        let s = self.get(id)?;
        s.close();
        Ok(s.view())
    }

    /// Start a task, or on a busy session refuse it or queue it as `when_busy` says. A queued
    /// task comes back as `queued` and starts when the ones before it have ended (ADR-0112).
    pub fn send_task(
        &self,
        id: &str,
        prompt: String,
        key: Option<String>,
        when_busy: wire::WhenBusy,
    ) -> R<wire::Task> {
        let sess = self.get(id)?;
        if prompt.trim().is_empty() {
            return Err(Fail::Invalid("bad-request", "prompt is required".into()));
        }
        match sess.begin(&prompt, key.filter(|k| !k.is_empty()), when_busy, self.max_queued) {
            Ok(Begun::Started(started)) => {
                let task = started.task.clone();
                agent::launch(sess, self.ctx.clone(), started);
                Ok(task)
            }
            Ok(Begun::Queued(task) | Begun::Repeat(task)) => Ok(task),
            Err(Refusal::Busy) if when_busy == wire::WhenBusy::Queue => Err(Fail::Conflict(
                "task-running",
                "a task is already running and this daemon does not queue (--max-queued 0)".into(),
            )),
            Err(Refusal::Busy) => Err(Fail::Conflict(
                "task-running",
                "a task is already running; one at a time per session \
                 (send with when_busy \"queue\" to run this one after it)"
                    .into(),
            )),
            Err(Refusal::QueueFull(n)) => Err(Fail::Exhausted(
                "queue-full",
                format!("{n} tasks are already waiting behind the running one"),
            )),
            Err(Refusal::Closed) => {
                Err(Fail::Conflict("session-closed", "the session is closed".into()))
            }
        }
    }

    /// Add `message` to the running task `task_id`: it joins the conversation as a user
    /// message at the task's next model-call boundary (ADR-0107). Not a new task, and
    /// not a way past "one task at a time": it is input to the one that is running.
    pub fn steer_task(
        &self,
        id: &str,
        task_id: &str,
        message: String,
        key: Option<String>,
    ) -> R<wire::Task> {
        let sess = self.get(id)?;
        if message.trim().is_empty() {
            return Err(Fail::Invalid("bad-request", "message is required".into()));
        }
        match sess.steer(task_id, &message, key.filter(|k| !k.is_empty())) {
            Ok(()) => sess
                .task(task_id)
                .ok_or_else(|| Fail::NotFound(format!("no task {task_id} in session {id}"))),
            Err(SteerRefusal::Closed) => {
                Err(Fail::Conflict("session-closed", "the session is closed".into()))
            }
            Err(SteerRefusal::NotRunning) => match sess.task(task_id) {
                Some(t) if t.status == wire::TaskStatus::Queued => Err(Fail::Conflict(
                    "task-not-running",
                    format!("{task_id} is queued and has not started; steer it once it runs"),
                )),
                Some(_) => Err(Fail::Conflict(
                    "task-not-running",
                    format!("{task_id} is not the running task; send a new task"),
                )),
                None => Err(Fail::NotFound(format!("no task {task_id} in session {id}"))),
            },
            Err(SteerRefusal::Finishing) => Err(Fail::Conflict(
                "task-finishing",
                format!("{task_id} is writing its final answer; send a new task"),
            )),
        }
    }

    /// What a client needs to draw a session it was not watching, and the cursor to
    /// keep watching it from (ADR-0107).
    pub fn session_state(&self, id: &str) -> R<wire::SessionState> {
        Ok(self.get(id)?.state())
    }

    /// Cancel a running task, or one still queued. Cancelling one that already ended is not an
    /// error — it answers with how it ended — so a retry is harmless. Stopping the running task
    /// lets the next queued one start, and it is launched here.
    pub fn cancel_task(&self, id: &str, task_id: &str) -> R<wire::Task> {
        let sess = self.get(id)?;
        let (task, next) = sess.cancel(task_id, "cancelled by the caller");
        if let Some(started) = next {
            agent::launch(sess, self.ctx.clone(), started);
        }
        task.ok_or_else(|| Fail::NotFound(format!("no task {task_id} in session {id}")))
    }

    pub fn submit_approval(
        &self,
        id: &str,
        call_id: &str,
        req: wire::SubmitApproval,
    ) -> R<wire::ApprovalAnswer> {
        let sess = self.get(id)?;
        let decided_by =
            if req.approver.is_empty() { "anonymous".to_string() } else { req.approver };
        let answer = Answer { decision: req.decision, reason: req.reason, decided_by };
        if !sess.answer(call_id, answer) {
            return Err(Fail::Conflict(
                "not-pending",
                format!("{call_id} is not waiting for approval"),
            ));
        }
        Ok(wire::ApprovalAnswer { call_id: call_id.to_string(), decision: req.decision })
    }

    /// Every event after `after`, then each new one as it is appended. Never
    /// ends on its own; the caller hangs up.
    pub fn events(
        &self,
        id: &str,
        after: u64,
    ) -> R<impl Stream<Item = wire::Event> + Send + 'static> {
        let sess = self.get(id)?;
        // Subscribe BEFORE the first read: an event appended between the read
        // and the wait then still wakes the wait.
        let rx = sess.watch();
        Ok(futures::stream::unfold(
            (sess, after, rx, VecDeque::new()),
            |(sess, mut cursor, mut rx, mut queue)| async move {
                loop {
                    if let Some(e) = queue.pop_front() {
                        let e: wire::Event = e;
                        cursor = e.seq;
                        return Some((e, (sess, cursor, rx, queue)));
                    }
                    queue.extend(sess.events_after(cursor));
                    if queue.is_empty() && rx.changed().await.is_err() {
                        return None;
                    }
                }
            },
        ))
    }

    pub fn session_count(&self) -> usize {
        self.sessions.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}
