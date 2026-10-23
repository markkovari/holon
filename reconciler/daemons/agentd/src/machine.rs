//! A session's state machine: what its log says, folded one event at a time. `apply` is the only
//! place a task changes state, and it runs the same way as an event is appended and as the log is
//! read back on load, so the two cannot disagree. What only a process has (a task's loop, a
//! waiter for an answer) is `session::Live`, which follows the `Change` this reports.

use std::collections::{BTreeSet, HashMap, VecDeque};

use super::wire::{self, ApprovalScope, Decision, Event, Payload, TaskStatus, Usage};

#[derive(Default)]
pub struct Machine {
    pub tasks: HashMap<String, wire::Task>,
    /// Accepted onto a busy session and not started, oldest first (ADR-0112).
    pub queue: VecDeque<Queued>,
    pub running: Option<Running>,
    pub usage: Usage,
    /// Tools a person approved for the rest of the session (ADR-0114).
    pub approved: BTreeSet<String>,
    /// Code-store ops a rewind has reverted (ADR-0121).
    pub rewound: BTreeSet<u64>,
    /// The tool each approval asked about, by call: what an answer's reach applies to.
    asked: HashMap<String, String>,
}

#[derive(Clone)]
pub struct Queued {
    pub task_id: String,
    pub prompt: String,
}

pub struct Running {
    pub task_id: String,
    pub usage: Usage,
    /// Tools a person approved for the rest of this task (ADR-0114).
    pub approved: BTreeSet<String>,
}

/// What an event did to the running task.
#[derive(Debug, PartialEq)]
pub enum Change {
    None,
    Started,
    Ended,
}

impl Machine {
    pub fn is_running(&self, task_id: &str) -> bool {
        self.running.as_ref().is_some_and(|r| r.task_id == task_id)
    }

    fn running_mut(&mut self, task_id: &str) -> Option<&mut Running> {
        self.running.as_mut().filter(|r| r.task_id == task_id)
    }

    pub fn apply(&mut self, session_id: &str, e: &Event) -> Change {
        let task = |status| wire::Task {
            id: e.task_id.clone(),
            session_id: session_id.to_string(),
            status,
            created_at: e.time.clone(),
        };
        match &e.payload {
            Payload::TaskQueued { prompt } => {
                self.tasks.insert(e.task_id.clone(), task(TaskStatus::Queued));
                self.queue.push_back(Queued { task_id: e.task_id.clone(), prompt: prompt.clone() });
            }
            Payload::TaskStarted { .. } => {
                self.queue.retain(|q| q.task_id != e.task_id);
                let mut t = task(TaskStatus::Running);
                // A task that waited was created when it was accepted, not when it started.
                if let Some(queued) = self.tasks.get(&e.task_id) {
                    t.created_at = queued.created_at.clone();
                }
                self.tasks.insert(e.task_id.clone(), t);
                self.running = Some(Running {
                    task_id: e.task_id.clone(),
                    usage: Usage::default(),
                    approved: BTreeSet::new(),
                });
                return Change::Started;
            }
            Payload::UsageUpdated { delta, .. } => {
                self.usage.add(delta);
                if let Some(r) = self.running_mut(&e.task_id) {
                    r.usage.add(delta);
                }
            }
            Payload::ToolApprovalRequired { call_id, tool_name, .. } => {
                self.asked.insert(call_id.clone(), tool_name.clone());
            }
            Payload::ToolApprovalResolved {
                call_id, decision: Decision::Approve, scope, ..
            } => {
                if let Some(tool) = self.asked.get(call_id).cloned() {
                    match scope {
                        ApprovalScope::Once => {}
                        ApprovalScope::Task => {
                            if let Some(r) = self.running_mut(&e.task_id) {
                                r.approved.insert(tool);
                            }
                        }
                        ApprovalScope::Session => {
                            self.approved.insert(tool);
                        }
                    }
                }
            }
            Payload::TaskCompleted { .. } | Payload::TaskFailed { .. } => {
                let done = matches!(e.payload, Payload::TaskCompleted { .. });
                if let Some(t) = self.tasks.get_mut(&e.task_id) {
                    t.status = if done { TaskStatus::Completed } else { TaskStatus::Failed };
                }
                self.queue.retain(|q| q.task_id != e.task_id);
                // Only the running task's end ends it: a queued one cancelled meanwhile does not.
                if self.is_running(&e.task_id) {
                    self.running = None;
                    return Change::Ended;
                }
            }
            Payload::SessionRewound { ops, .. } => self.rewound.extend(ops),
            _ => {}
        }
        Change::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: u64, task: &str, payload: Payload) -> Event {
        Event {
            seq,
            session_id: "s".into(),
            task_id: task.into(),
            time: format!("t{seq}"),
            payload,
        }
    }

    fn failed() -> Payload {
        Payload::TaskFailed {
            code: wire::FailCode::Cancelled,
            message: String::new(),
            task_usage: Usage::default(),
        }
    }

    #[test]
    fn a_queued_task_starts_keeps_when_it_was_accepted_and_ends_only_itself() {
        let mut m = Machine::default();
        let started = |p: &str| Payload::TaskStarted { prompt: p.into() };
        assert_eq!(m.apply("s", &ev(1, "a", started("a"))), Change::Started);
        m.apply("s", &ev(2, "b", Payload::TaskQueued { prompt: "b".into() }));
        m.apply("s", &ev(3, "c", Payload::TaskQueued { prompt: "c".into() }));
        // A queued task cancelled while another runs ends without touching the running one.
        assert_eq!(m.apply("s", &ev(4, "c", failed())), Change::None);
        assert!(m.is_running("a"));
        assert_eq!(m.queue.iter().map(|q| q.task_id.as_str()).collect::<Vec<_>>(), ["b"]);

        assert_eq!(m.apply("s", &ev(5, "a", failed())), Change::Ended);
        assert_eq!(m.apply("s", &ev(6, "b", started("b"))), Change::Started);
        assert!(m.queue.is_empty());
        let b = &m.tasks["b"];
        assert_eq!((b.status, b.created_at.as_str()), (TaskStatus::Running, "t2"));
        assert_eq!(m.tasks["c"].status, TaskStatus::Failed);
    }

    #[test]
    fn an_approval_reaches_as_far_as_its_answer_said_and_a_task_scope_ends_with_the_task() {
        let mut m = Machine::default();
        m.apply("s", &ev(1, "a", Payload::TaskStarted { prompt: String::new() }));
        for (seq, call, tool, scope) in
            [(2, "c1", "run", ApprovalScope::Task), (4, "c2", "write_file", ApprovalScope::Session)]
        {
            let asked = Payload::ToolApprovalRequired {
                call_id: call.into(),
                tool_name: tool.into(),
                input: serde_json::Value::Null,
                expires_at: None,
            };
            m.apply("s", &ev(seq, "a", asked));
            let answer = Payload::ToolApprovalResolved {
                call_id: call.into(),
                decision: Decision::Approve,
                decided_by: "p".into(),
                scope,
            };
            m.apply("s", &ev(seq + 1, "a", answer));
        }
        assert!(m.running.as_ref().unwrap().approved.contains("run"));
        assert_eq!(m.approved.iter().collect::<Vec<_>>(), ["write_file"]);
        m.apply("s", &ev(6, "a", failed()));
        assert!(m.running.is_none());
        assert_eq!(m.approved.iter().collect::<Vec<_>>(), ["write_file"]);
    }
}
