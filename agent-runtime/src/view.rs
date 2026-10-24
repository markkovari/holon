//! What `/v1/state` draws (ADR-0120), folded from the event stream as each event is numbered,
//! under the log's own lock: the runs going, the approvals waiting and the coding sessions, all
//! as of one seq. The write side is whatever emits the events; nothing else updates this.

use agent_wire::{Event, Pending, RunRecord, Sequenced, SessionView, Status};

#[derive(Default)]
pub struct View {
    pub running: Vec<RunRecord>,
    pub approvals: Vec<Pending>,
    /// Newest first, one per session id.
    pub sessions: Vec<SessionView>,
}

impl View {
    pub fn apply(&mut self, e: &Sequenced) {
        match &e.event {
            Event::RunStarted {
                agent,
                run_id,
                trigger,
                input,
                trace_id,
                span_id,
                model,
                chain,
                ..
            } => self.running.push(RunRecord {
                id: run_id.clone(),
                agent: agent.clone(),
                trigger: trigger.clone(),
                input: input.clone(),
                started: e.at_ms / 1000,
                finished: 0,
                status: Status::Ok,
                answer: String::new(),
                steps: Vec::new(),
                tokens_in: 0,
                tokens_out: 0,
                started_ms: e.at_ms,
                finished_ms: 0,
                model: model.clone(),
                trace_id: trace_id.clone(),
                span_id: span_id.clone(),
                parent_span_id: None,
                hops: 0,
                chain: chain.clone(),
                project: String::new(),
                attachments: Vec::new(),
            }),
            Event::RunFinished(r) => self.running.retain(|x| x.id != r.id),
            Event::ApprovalRequested { id, agent, tool, args, chain } => {
                self.approvals.push(Pending {
                    id: *id,
                    agent: agent.clone(),
                    tool: tool.clone(),
                    args: args.clone(),
                    chain: chain.clone(),
                })
            }
            Event::ApprovalResolved { id, .. } => self.approvals.retain(|p| p.id != *id),
            Event::Session(s) => {
                self.sessions.retain(|x| x.id != s.id);
                self.sessions.insert(0, (**s).clone());
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at(seq: u64, event: Event) -> Sequenced {
        Sequenced { seq, at_ms: 1_000 * seq, event }
    }

    #[test]
    fn an_approval_waits_from_its_request_to_its_answer_whatever_asked_it() {
        let mut v = View::default();
        let ask = |id| Event::ApprovalRequested {
            id,
            agent: "a".into(),
            tool: "t".into(),
            args: json!({}),
            chain: vec![],
        };
        v.apply(&at(1, ask(7)));
        v.apply(&at(2, ask(8)));
        v.apply(&at(3, Event::ApprovalResolved { id: 7, approved: true, timed_out: false }));
        assert_eq!(v.approvals.iter().map(|p| p.id).collect::<Vec<_>>(), [8]);
    }
}
