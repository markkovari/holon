//! A session's state as a client reads it to catch up (ADR-0107): the event log
//! folded into the transcript a UI draws, the approvals waiting on a person, and
//! the steers not yet applied.
//!
//! A pure function of the events, nothing else. That is the point: the log is
//! already the one truth (`session.rs`), a snapshot that kept its own copy would
//! be a second one to keep in step, and this way the state and the cursor that
//! goes with it — the last `seq` folded — cannot disagree.
//!
//! The conversation itself (`Session::history`) is not read: it is held for the
//! length of a task, so a snapshot of a running session could not take it, and
//! it holds the provider's messages rather than what a client shows.

use super::wire::{
    CallStatus, Entry, Event, Outcome, Payload, PendingApproval, PendingSteer, TranscriptEntry,
};

pub struct Folded {
    /// The last event folded; 0 for an empty log.
    pub seq: u64,
    pub transcript: Vec<TranscriptEntry>,
    pub pending_approvals: Vec<PendingApproval>,
    pub pending_steers: Vec<PendingSteer>,
}

fn at(e: &Event, entry: Entry) -> TranscriptEntry {
    TranscriptEntry { seq: e.seq, task_id: e.task_id.clone(), entry }
}

/// The call `call_id` in `t`, newest first: a call id is unique, and the one
/// being updated is nearly always near the end.
fn call_mut<'a>(t: &'a mut [TranscriptEntry], id: &str) -> Option<&'a mut Entry> {
    t.iter_mut()
        .rev()
        .map(|x| &mut x.entry)
        .find(|e| matches!(e, Entry::ToolCall { call_id, .. } if call_id == id))
}

pub fn fold(events: &[Event]) -> Folded {
    let mut t: Vec<TranscriptEntry> = Vec::new();
    let mut steers: Vec<PendingSteer> = Vec::new();

    for e in events {
        match &e.payload {
            Payload::TaskStarted { prompt } => {
                t.push(at(e, Entry::UserMessage { text: prompt.clone(), steered: false }));
            }
            Payload::TextDelta { message_id, text } => match t.last_mut() {
                // One model turn's deltas are contiguous: no other entry begins between them.
                Some(TranscriptEntry {
                    entry: Entry::AssistantMessage { message_id: m, text: acc, .. },
                    ..
                }) if m == message_id => acc.push_str(text),
                _ => t.push(at(
                    e,
                    Entry::AssistantMessage {
                        message_id: message_id.clone(),
                        text: text.clone(),
                        in_progress: false,
                    },
                )),
            },
            Payload::ToolCallStarted { call_id, tool_name, input } => {
                t.push(at(
                    e,
                    Entry::ToolCall {
                        call_id: call_id.clone(),
                        tool_name: tool_name.clone(),
                        input: input.clone(),
                        status: CallStatus::Running,
                        decision: None,
                        decided_by: None,
                        expires_at: None,
                        output: None,
                        is_error: None,
                        duration_ms: None,
                    },
                ));
            }
            Payload::ToolApprovalRequired { call_id, expires_at: exp, .. } => {
                if let Some(Entry::ToolCall { status, expires_at, .. }) = call_mut(&mut t, call_id)
                {
                    *status = CallStatus::AwaitingApproval;
                    *expires_at = exp.clone();
                }
            }
            Payload::ToolApprovalResolved { call_id, decision: d, decided_by: by } => {
                if let Some(Entry::ToolCall { status, decision, decided_by, .. }) =
                    call_mut(&mut t, call_id)
                {
                    *status = CallStatus::Running;
                    *decision = Some(*d);
                    *decided_by = Some(by.clone());
                }
            }
            Payload::ToolCallFinished { call_id, output: out, is_error: err, duration_ms: ms } => {
                if let Some(Entry::ToolCall { status, output, is_error, duration_ms, .. }) =
                    call_mut(&mut t, call_id)
                {
                    *status = CallStatus::Finished;
                    *output = Some(out.clone());
                    *is_error = Some(*err);
                    *duration_ms = Some(*ms);
                }
            }
            Payload::UsageUpdated { .. } => {}
            Payload::TaskCompleted { result, task_usage } => {
                end_task(&mut t, &mut steers, &e.task_id);
                t.push(at(
                    e,
                    Entry::TaskEnded {
                        outcome: Outcome::Completed,
                        result: Some(result.clone()),
                        code: None,
                        message: None,
                        usage: *task_usage,
                    },
                ));
            }
            Payload::TaskFailed { code, message, task_usage } => {
                end_task(&mut t, &mut steers, &e.task_id);
                t.push(at(
                    e,
                    Entry::TaskEnded {
                        outcome: Outcome::Failed,
                        result: None,
                        code: Some(*code),
                        message: Some(message.clone()),
                        usage: *task_usage,
                    },
                ));
            }
            Payload::TaskSteerQueued { steer_id, message } => steers.push(PendingSteer {
                task_id: e.task_id.clone(),
                steer_id: steer_id.clone(),
                message: message.clone(),
            }),
            Payload::TaskSteered { steer_id } => {
                if let Some(i) = steers.iter().position(|s| &s.steer_id == steer_id) {
                    let s = steers.remove(i);
                    t.push(at(e, Entry::UserMessage { text: s.message, steered: true }));
                }
            }
        }
    }

    // The message being written when the log ends is the only one still in progress.
    if let Some(Event { payload: Payload::TextDelta { message_id, .. }, .. }) = events.last() {
        if let Some(TranscriptEntry {
            entry: Entry::AssistantMessage { message_id: m, in_progress, .. },
            ..
        }) = t.last_mut()
        {
            *in_progress = m == message_id;
        }
    }

    let pending_approvals = t
        .iter()
        .filter_map(|x| match &x.entry {
            Entry::ToolCall {
                call_id,
                tool_name,
                input,
                status: CallStatus::AwaitingApproval,
                expires_at,
                ..
            } => Some(PendingApproval {
                task_id: x.task_id.clone(),
                call_id: call_id.clone(),
                tool_name: tool_name.clone(),
                input: input.clone(),
                expires_at: expires_at.clone(),
            }),
            _ => None,
        })
        .collect();

    Folded {
        seq: events.last().map_or(0, |e| e.seq),
        transcript: t,
        pending_approvals,
        pending_steers: steers,
    }
}

/// A task is over: whatever of it was still waiting or running never will be,
/// and a steer it never applied is gone with it.
fn end_task(t: &mut [TranscriptEntry], steers: &mut Vec<PendingSteer>, task: &str) {
    for x in t.iter_mut().filter(|x| x.task_id == task) {
        if let Entry::ToolCall { status, .. } = &mut x.entry {
            if matches!(status, CallStatus::AwaitingApproval | CallStatus::Running) {
                *status = CallStatus::Cancelled;
            }
        }
    }
    steers.retain(|s| s.task_id != task);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{Decision, FailCode, Usage};
    use serde_json::json;

    fn log(payloads: Vec<(&str, Payload)>) -> Vec<Event> {
        payloads
            .into_iter()
            .enumerate()
            .map(|(i, (task, payload))| Event {
                seq: i as u64 + 1,
                session_id: "ses".into(),
                task_id: task.into(),
                time: "t".into(),
                payload,
            })
            .collect()
    }

    fn delta(m: &str, t: &str) -> Payload {
        Payload::TextDelta { message_id: m.into(), text: t.into() }
    }

    fn started(call: &str) -> Payload {
        Payload::ToolCallStarted {
            call_id: call.into(),
            tool_name: "run".into(),
            input: json!({"command": "ls"}),
        }
    }

    #[test]
    fn an_empty_log_is_an_empty_state_at_zero() {
        let f = fold(&[]);
        assert_eq!(f.seq, 0);
        assert!(f.transcript.is_empty() && f.pending_approvals.is_empty());
    }

    #[test]
    fn deltas_of_one_message_join_and_the_last_one_is_in_progress() {
        let f = fold(&log(vec![
            ("t1", Payload::TaskStarted { prompt: "look".into() }),
            ("t1", delta("m1", "Let me ")),
            ("t1", delta("m1", "look")),
        ]));
        assert_eq!(f.seq, 3);
        assert_eq!(f.transcript.len(), 2);
        assert_eq!(
            f.transcript[1].entry,
            Entry::AssistantMessage {
                message_id: "m1".into(),
                text: "Let me look".into(),
                in_progress: true
            }
        );
        assert_eq!(f.transcript[1].seq, 2, "an entry carries the seq that began it");
    }

    #[test]
    fn a_message_that_something_followed_is_complete() {
        let f = fold(&log(vec![
            ("t1", Payload::TaskStarted { prompt: "x".into() }),
            ("t1", delta("m1", "hi")),
            ("t1", started("c1")),
        ]));
        assert!(matches!(
            &f.transcript[1].entry,
            Entry::AssistantMessage { in_progress: false, .. }
        ));
    }

    #[test]
    fn a_call_waiting_for_a_person_is_pending_until_it_is_answered() {
        let waiting = vec![
            ("t1", Payload::TaskStarted { prompt: "x".into() }),
            ("t1", started("c1")),
            (
                "t1",
                Payload::ToolApprovalRequired {
                    call_id: "c1".into(),
                    tool_name: "run".into(),
                    input: json!({"command": "ls"}),
                    expires_at: Some("later".into()),
                },
            ),
        ];
        let f = fold(&log(waiting.clone()));
        assert_eq!(f.pending_approvals.len(), 1);
        assert_eq!(f.pending_approvals[0].call_id, "c1");
        assert_eq!(f.pending_approvals[0].expires_at.as_deref(), Some("later"));

        let mut answered = waiting;
        answered.push((
            "t1",
            Payload::ToolApprovalResolved {
                call_id: "c1".into(),
                decision: Decision::Approve,
                decided_by: "ann".into(),
            },
        ));
        let f = fold(&log(answered));
        assert!(f.pending_approvals.is_empty());
        let Entry::ToolCall { status, decided_by, .. } = &f.transcript[1].entry else { panic!() };
        assert_eq!((*status, decided_by.as_deref()), (CallStatus::Running, Some("ann")));
    }

    #[test]
    fn a_finished_call_carries_what_it_returned() {
        let f = fold(&log(vec![
            ("t1", Payload::TaskStarted { prompt: "x".into() }),
            ("t1", started("c1")),
            (
                "t1",
                Payload::ToolCallFinished {
                    call_id: "c1".into(),
                    output: "a\nb".into(),
                    is_error: false,
                    duration_ms: 7,
                },
            ),
        ]));
        let Entry::ToolCall { status, output, duration_ms, .. } = &f.transcript[1].entry else {
            panic!()
        };
        assert_eq!(
            (*status, output.as_deref(), *duration_ms),
            (CallStatus::Finished, Some("a\nb"), Some(7))
        );
    }

    /// A cancel mid-approval ends the task with the call still waiting; it must
    /// not stay "pending" for a person to answer, or a bot asks about a ghost.
    #[test]
    fn a_task_that_ends_abandons_its_waiting_call_and_its_unapplied_steer() {
        let f = fold(&log(vec![
            ("t1", Payload::TaskStarted { prompt: "x".into() }),
            ("t1", started("c1")),
            (
                "t1",
                Payload::ToolApprovalRequired {
                    call_id: "c1".into(),
                    tool_name: "run".into(),
                    input: json!({}),
                    expires_at: None,
                },
            ),
            ("t1", Payload::TaskSteerQueued { steer_id: "s1".into(), message: "hurry".into() }),
            (
                "t1",
                Payload::TaskFailed {
                    code: FailCode::Cancelled,
                    message: "cancelled".into(),
                    task_usage: Usage::default(),
                },
            ),
        ]));
        assert!(f.pending_approvals.is_empty());
        assert!(f.pending_steers.is_empty(), "{:?}", f.pending_steers);
        let Entry::ToolCall { status, .. } = &f.transcript[1].entry else { panic!() };
        assert_eq!(*status, CallStatus::Cancelled);
        assert!(matches!(
            &f.transcript[2].entry,
            Entry::TaskEnded { outcome: Outcome::Failed, code: Some(FailCode::Cancelled), .. }
        ));
    }

    #[test]
    fn a_steer_is_pending_until_it_joins_the_conversation_and_then_it_is_a_message() {
        let queued = vec![
            ("t1", Payload::TaskStarted { prompt: "x".into() }),
            ("t1", Payload::TaskSteerQueued { steer_id: "s1".into(), message: "be brief".into() }),
        ];
        let f = fold(&log(queued.clone()));
        assert_eq!(f.pending_steers.len(), 1);
        assert_eq!(f.transcript.len(), 1);

        let mut applied = queued;
        applied.push(("t1", Payload::TaskSteered { steer_id: "s1".into() }));
        let f = fold(&log(applied));
        assert!(f.pending_steers.is_empty());
        assert_eq!(
            f.transcript[1].entry,
            Entry::UserMessage { text: "be brief".into(), steered: true }
        );
    }
}
