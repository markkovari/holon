//! The operations both transports serve. `rest.rs` and `grpc.rs` only
//! translate: a request into one of these calls, a `Fail` into a status.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use futures::Stream;

use super::agent;
use super::session::{Answer, Refusal, Registry, Session};
use super::wire;

pub struct Daemon {
    pub sessions: Registry,
    pub roots: Vec<PathBuf>,
    pub state_dir: Option<PathBuf>,
    pub ctx: Arc<agent::Ctx>,
}

/// Why a call was refused, in terms both transports map: REST to a status
/// and an RFC 9457 `type` slug, gRPC to a code.
pub enum Fail {
    NotFound(String),
    Invalid(&'static str, String),
    Conflict(&'static str, String),
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

    pub fn send_task(&self, id: &str, prompt: String, key: Option<String>) -> R<wire::Task> {
        let sess = self.get(id)?;
        if prompt.trim().is_empty() {
            return Err(Fail::Invalid("bad-request", "prompt is required".into()));
        }
        match sess.begin(&prompt, key.filter(|k| !k.is_empty())) {
            Ok((task, fresh)) => {
                if fresh {
                    let run = agent::run(sess.clone(), self.ctx.clone(), task.id.clone(), prompt);
                    sess.attach(&task.id, tokio::spawn(run));
                }
                Ok(task)
            }
            Err(Refusal::Busy) => Err(Fail::Conflict(
                "task-running",
                "a task is already running; one at a time per session".into(),
            )),
            Err(Refusal::Closed) => {
                Err(Fail::Conflict("session-closed", "the session is closed".into()))
            }
        }
    }

    /// Cancel a running task. Cancelling one that already ended is not an
    /// error — it answers with how it ended — so a retry is harmless.
    pub fn cancel_task(&self, id: &str, task_id: &str) -> R<wire::Task> {
        self.get(id)?
            .cancel(task_id, "cancelled by the caller")
            .ok_or_else(|| Fail::NotFound(format!("no task {task_id} in session {id}")))
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
