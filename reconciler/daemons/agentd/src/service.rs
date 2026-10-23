//! The operations both transports serve. `rest.rs` and `grpc.rs` only
//! translate: a request into one of these calls, a `Fail` into a status.

use std::collections::{BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use futures::Stream;
use holon_park::resume::{Delivery, Event as DeliveryEvent};

use super::agent;
use super::park::Answered;
use super::session::{Answer, Begun, Refusal, Registry, Resume, Session, SteerRefusal};
use super::tools;
use super::wire::{self, ApprovalScope, Decision};

pub struct Daemon {
    pub sessions: Registry,
    pub roots: Vec<PathBuf>,
    pub state_dir: Option<PathBuf>,
    pub ctx: Arc<agent::Ctx>,
    /// Tasks a session may have waiting behind its running one (ADR-0112). 0: no queueing.
    pub max_queued: usize,
    /// Tools every session created here may call without asking (ADR-0114), on top of what the
    /// caller lists. Added to the session when it is created, so that it is what the session was
    /// created with, and a later change of this list does not reach sessions that exist.
    pub auto_approve: Vec<String>,
}

/// Why a call was refused, in terms both transports map: REST to a status
/// and an RFC 9457 `type` slug, gRPC to a code.
pub enum Fail {
    NotFound(String),
    Invalid(&'static str, String),
    Conflict(&'static str, String),
    /// A limit the caller can wait out: REST `429`, gRPC `RESOURCE_EXHAUSTED`.
    Exhausted(&'static str, String),
    /// What a delivery was for is gone, and will not come back: REST `410`, which is what tells
    /// `comp-resume` to cancel the ticket and stop (ADR-0113).
    Gone(String),
    /// Something this call needs is down and may not be soon: REST `503`, gRPC `UNAVAILABLE`.
    Unavailable(&'static str, String),
    Internal(String),
}

type R<T> = Result<T, Fail>;

fn vcs_fail(e: holon_vcs::VcsError) -> Fail {
    match e {
        holon_vcs::VcsError::Storage(d) => {
            Fail::Unavailable("vcs-unavailable", format!("comp-vcs: {d}"))
        }
        e => Fail::Internal(format!("comp-vcs: {e}")),
    }
}

impl Daemon {
    fn get(&self, id: &str) -> R<Arc<Session>> {
        let map = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        map.get(id).cloned().ok_or_else(|| Fail::NotFound(format!("no session {id}")))
    }

    pub fn create_session(&self, mut req: wire::CreateSession) -> R<wire::Session> {
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
        for tool in &self.auto_approve {
            if !req.auto_approve_tools.contains(tool) {
                req.auto_approve_tools.push(tool.clone());
            }
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
        let tickets = s.close();
        self.cancel_tickets(tickets);
        Ok(s.view())
    }

    /// Give up, in `comp-park`, on the approvals of tasks that are over. Best effort and off the
    /// caller's path: one that is missed is answered `410` when it is delivered, which cancels it
    /// from the other side.
    fn cancel_tickets(&self, tickets: Vec<String>) {
        if tickets.is_empty() || self.ctx.park.is_none() {
            return;
        }
        let ctx = self.ctx.clone();
        tokio::spawn(async move {
            let Some(park) = &ctx.park else { return };
            for ticket in tickets {
                if let Err(e) = park.cancel(&ticket).await {
                    eprintln!("comp-agentd: cancelling ticket {ticket}: {e}");
                }
            }
        });
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

    /// Puts the files the events after `after_seq` changed back as they were, by reverting their
    /// ops in the code store, newest first (ADR-0121). The conversation is not rewound. A revert
    /// the store refuses stops it there; what was reverted until then is written and recorded.
    pub async fn rewind(&self, id: &str, after_seq: u64) -> R<wire::Rewound> {
        let sess = self.get(id)?;
        let Some(vcs) = &self.ctx.vcs else {
            return Err(Fail::Conflict(
                "edits-not-recorded",
                "this daemon records no edits (--vcs-url)".into(),
            ));
        };
        // shortcut: checked, not held, so a task sent while a rewind runs races it; hold the
        // session if callers ever do that.
        if sess.view().status != wire::SessionStatus::Idle {
            return Err(Fail::Conflict(
                "session-busy",
                "rewind a session with no task running".into(),
            ));
        }
        let ops = sess.unrewound_edits(after_seq);
        let by = holon_vcs::model::Agent::named(format!("agentd:{}/rewind", sess.id));
        let (mut done, mut paths, mut refused) = (vec![], BTreeSet::new(), None);
        for (op, path) in &ops {
            match vcs.revert(&sess.id, *op, by.clone()).await {
                Ok(()) => {
                    done.push(*op);
                    paths.insert(path.clone());
                }
                Err(e) => {
                    refused = Some(e);
                    break;
                }
            }
        }
        if !done.is_empty() {
            let files = vcs.read(&sess.id, &paths).await.map_err(vcs_fail)?;
            for (p, content) in files {
                let full = tools::confine(&sess.workspace, &p).map_err(Fail::Internal)?;
                let r = match content {
                    Some(b) => full
                        .parent()
                        .map_or(Ok(()), std::fs::create_dir_all)
                        .and_then(|()| std::fs::write(&full, b)),
                    None => std::fs::remove_file(&full).or_else(|e| match e.kind() {
                        std::io::ErrorKind::NotFound => Ok(()),
                        _ => Err(e),
                    }),
                };
                r.map_err(|e| Fail::Internal(format!("{p}: {e}")))?;
            }
            sess.note(wire::Payload::SessionRewound {
                after_seq,
                ops: done.clone(),
                paths: paths.iter().cloned().collect(),
            });
        }
        if let Some(e) = refused {
            return Err(Fail::Conflict(
                "rewind-refused",
                format!("rewound {} of {} ops, then: {e}", done.len(), ops.len()),
            ));
        }
        Ok(wire::Rewound { ops: done, paths: paths.into_iter().collect() })
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
        let cancelled = sess.cancel(task_id, "cancelled by the caller");
        self.cancel_tickets(cancelled.tickets);
        if let Some(started) = cancelled.next {
            agent::launch(sess, self.ctx.clone(), started);
        }
        cancelled.task.ok_or_else(|| Fail::NotFound(format!("no task {task_id} in session {id}")))
    }

    pub async fn submit_approval(
        &self,
        id: &str,
        call_id: &str,
        req: wire::SubmitApproval,
    ) -> R<wire::ApprovalAnswer> {
        let sess = self.get(id)?;
        let (decision, scope) = (req.decision, req.scope);
        // A denial is for the call it answers. Refused rather than quietly narrowed, so that a
        // client that meant "never this tool" finds out it did not get it (ADR-0114).
        if decision == Decision::Deny && scope != ApprovalScope::Once {
            return Err(Fail::Invalid(
                "bad-request",
                "a denial is for this call only: its scope must be \"once\"".into(),
            ));
        }
        let decided_by =
            if req.approver.is_empty() { "anonymous".to_string() } else { req.approver };
        let answer = Answer { decision, reason: req.reason, decided_by, scope };
        let done = || wire::ApprovalAnswer { call_id: call_id.to_string(), decision, scope };
        // A wait held in memory is answered in memory.
        if sess.answer(call_id, answer.clone()) {
            return Ok(done());
        }
        let not_pending =
            || Fail::Conflict("not-pending", format!("{call_id} is not waiting for approval"));
        // A wait that is parked is answered through its ticket, and it is the delivery of that
        // answer, here or in the process that replaces this one, that moves the task on
        // (ADR-0113). The first to give an answer may; a second is refused, as it is in memory.
        let Some(park) = &self.ctx.park else { return Err(not_pending()) };
        let Some(waiting) = sess.claim_parked_answer(call_id) else { return Err(not_pending()) };
        let what = format!("approve {} in session {}", waiting.tool, sess.id);
        let body = Answered {
            decision: answer.decision,
            reason: answer.reason,
            decided_by: answer.decided_by,
            scope: answer.scope,
        };
        match park.answer(&sess.id, call_id, &what, waiting.deadline, &body).await {
            Ok(()) => Ok(done()),
            Err(e) => {
                sess.unclaim_parked_answer(call_id);
                Err(Fail::Unavailable("park-unavailable", format!(
                    "the answer did not reach comp-park, so nothing has happened ({e}); send it again"
                )))
            }
        }
    }

    /// What `comp-resume` hands this daemon as its sink (ADR-0113): the end of a wait a task
    /// parked on. An answer is the human's decision; an interruption is nobody's, which is a
    /// denial, as a timeout always was. Every reply is one `comp-resume` understands: `2xx` for
    /// "taken, now or before", `410` for "there is nobody to take it", which cancels the ticket.
    pub fn deliver(&self, d: Delivery) -> R<()> {
        let sess = {
            let map = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            map.get(&d.session).cloned()
        };
        let sess = sess.ok_or_else(|| Fail::Gone(format!("no session {}", d.session)))?;
        let answer = match d.event {
            DeliveryEvent::Answered { answer } => {
                match serde_json::from_slice::<Answered>(&answer.body) {
                    Ok(a) => Answer {
                        decision: a.decision,
                        reason: a.reason,
                        decided_by: a.decided_by,
                        scope: a.scope,
                    },
                    // This daemon wrote it, so it cannot be unreadable; if it is, a denial is the safe
                    // reading, and refusing would only have the answer redelivered for ever.
                    Err(e) => Answer {
                        decision: Decision::Deny,
                        reason: format!("the answer could not be read: {e}"),
                        decided_by: "comp-agentd".into(),
                        scope: ApprovalScope::Once,
                    },
                }
            }
            DeliveryEvent::Interrupted { .. } => Answer {
                decision: Decision::Deny,
                reason: "nobody answered in time".into(),
                decided_by: "timeout".into(),
                scope: ApprovalScope::Once,
            },
            // Nothing an approval ticket produces. Taken, so that it is not retried for ever.
            DeliveryEvent::Redispatch { .. } | DeliveryEvent::Joined { .. } => return Ok(()),
        };
        match sess.resume_parked(&d.correlation, &answer) {
            Resume::Resumed(cp) => {
                agent::resume(sess, self.ctx.clone(), cp, answer);
                Ok(())
            }
            Resume::Duplicate => Ok(()),
            Resume::Gone => Err(Fail::Gone(format!(
                "{} in session {} is not waiting for anyone",
                d.correlation, d.session
            ))),
        }
    }

    /// A task that was parked when this daemon started has a ticket for its approval if all went
    /// well; if the process died between writing the checkpoint and parking, or the store lost
    /// it, it has none. Parking is idempotent by correlation, so every approval found parked is
    /// parked again, and until `comp-park` is there to take it, tried again.
    pub fn reassert_parks(&self) {
        if self.ctx.park.is_none() {
            return;
        }
        let parked: Vec<Arc<Session>> = {
            let map = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            map.values().filter(|s| s.parked().is_some()).cloned().collect()
        };
        for sess in parked {
            let ctx = self.ctx.clone();
            tokio::spawn(async move {
                let Some(park) = &ctx.park else { return };
                while let Some(w) = sess.parked() {
                    let what = format!("approve {} in session {}", w.tool, sess.id);
                    match park.park(&sess.id, &w.call_id, &what, w.deadline).await {
                        Ok(ticket) => {
                            sess.set_ticket(&w.call_id, &ticket);
                            return;
                        }
                        Err(e) => {
                            eprintln!("comp-agentd: {}: parking {} again: {e}", sess.id, w.call_id);
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        }
                    }
                }
            });
        }
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
