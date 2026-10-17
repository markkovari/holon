//! Groups (ADR-0109): a call that fans out, and is resolved by how its children end.
//!
//! A **group** is a ticket parked with a [`GroupSpec`]: how many children it will have and
//! how it joins them. Its **children** are tickets parked with `parent` naming it. A child
//! is an ordinary call — it has its own `replay`, deadline and `wake` — except that it is
//! settled by its own answer and then *consumed by its group*: it is never announced on
//! `PARK_WAKE` or taken on its own, and the group is what a turn waits on.
//!
//! ```text
//!             ┌──────────── group G (parked) ────────────┐
//!   park ───▶ │  child c1   child c2   child c3          │
//!             └──────────────────────────────────────────┘
//!   each child:  parked ──(wake | interrupt | cancel | recover)──▶ settled
//!   the group:   parked ──(join says resolve)──▶ ready ──(take-ready)──▶ resumed
//!                  │                              (a `joined` resolution: how every child ended)
//!                  └──(cancel)──▶ cancelled        children first, then the group: bottom-up
//! ```
//!
//! The join is [`join_decision`], a pure function of the group's spec and its children's
//! records, so the table is tested as a table. Everything that can change a child — `wake`,
//! `interrupt`, `cancel`, and a recovery pass — runs [`Engine::settle`] on its group
//! afterwards, and so does a retry of any of them that finds the child already moved: a
//! crash between the two writes is healed by the retry, or by the next pass if there is none.
//!
//! **Bottom-up.** A group never resolves with work still running. When it resolves because
//! one child failed (`fail-fast`) or because its own deadline passed, the children still
//! parked are cancelled *first*, and only then is the group written. A cancelled group
//! cancels its children first, then itself. So a crash can leave a group unresolved with some
//! children already cancelled — which the next `settle` finishes — and never a resolved or
//! cancelled group with a child still running that nothing will cancel.

use crate::engine::Engine;
use crate::error::{ParkError, Result};
use crate::model::{
    Agent, ChildOutcome, ChildResult, GroupSpec, JoinOutcome, JoinPolicy, JoinResult, SessionId,
    TicketId,
};
use crate::store::{ParkStore, Record, StoredStatus};

/// Who a group's own moves are signed as: the join cancelling the children it no longer needs.
pub fn join_actor() -> Agent {
    Agent { id: "holon-park/join".into(), goal: Some("group join".into()), model: None }
}

/// A group this call resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupResolved {
    pub ticket: TicketId,
    pub session: SessionId,
}

impl GroupResolved {
    /// What to put on `PARK_WAKE`: the group is ready to be taken.
    pub fn announcement(&self) -> crate::model::WakeMessage {
        crate::model::WakeMessage::resolved(&self.session, &self.ticket)
    }
}

/// What the join says about a group, given its children as they are now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Join {
    Wait,
    Resolve(JoinOutcome),
}

/// How a child ended, or `None` while it is still parked (or its record is incoherent).
pub fn child_result(r: &Record) -> Option<ChildResult> {
    match r.status {
        StoredStatus::Parked => None,
        StoredStatus::Cancelled => Some(ChildResult::Cancelled),
        StoredStatus::Interrupted => r.interruption.clone().map(ChildResult::Interrupted),
        // `Resumed` is a child its group consumed: it still holds what it was settled with.
        StoredStatus::Ready | StoredStatus::Resumed => match (&r.result, &r.interruption) {
            (Some(answer), _) => Some(ChildResult::Answered(answer.clone())),
            (None, Some(i)) => Some(ChildResult::Interrupted(i.clone())),
            (None, None) => None,
        },
    }
}

/// A child failed if its answer was `ok: false`, or it was interrupted, or it was cancelled.
pub fn failed(c: &ChildResult) -> bool {
    match c {
        ChildResult::Answered(a) => !a.ok,
        ChildResult::Interrupted(_) | ChildResult::Cancelled => true,
    }
}

/// The join, as a table:
///
/// | policy | a child failed | all `children` parked and settled | |
/// |---|---|---|---|
/// | `fail-fast` | yes | — | resolve `failed-fast` |
/// | `fail-fast`, `all-settled` | no | yes | resolve `all-answered` |
/// | `all-settled` | yes | yes | resolve `some-failed` |
/// | either | — | no | wait |
///
/// "All parked" matters: a group declared with three children is not complete when the first
/// one settles before the other two have been parked.
pub fn join_decision(spec: &GroupSpec, children: &[Record]) -> Join {
    let results: Vec<Option<ChildResult>> = children.iter().map(child_result).collect();
    let all_parked = children.len() as u32 >= spec.children;
    let all_settled = all_parked && results.iter().all(Option::is_some);
    let any_failed = results.iter().flatten().any(failed);
    match spec.policy {
        JoinPolicy::FailFast if any_failed => Join::Resolve(JoinOutcome::FailedFast),
        _ if all_settled => Join::Resolve(if any_failed {
            JoinOutcome::SomeFailed
        } else {
            JoinOutcome::AllAnswered
        }),
        _ => Join::Wait,
    }
}

/// What a group is resolved with. `children` must all be settled: a child still parked is
/// reported cancelled, because the caller has just cancelled it or is about to.
pub fn join_result(outcome: JoinOutcome, spec: &GroupSpec, children: &[Record]) -> JoinResult {
    JoinResult {
        outcome,
        children: children
            .iter()
            .map(|c| ChildOutcome {
                ticket: c.ticket.clone(),
                correlation: c.call.correlation.clone(),
                ended: child_result(c).unwrap_or(ChildResult::Cancelled),
            })
            .collect(),
        missing: spec.children.saturating_sub(children.len() as u32),
    }
}

impl<S: ParkStore> Engine<S> {
    /// A group's children in the order they were parked: the order of its child list, which is
    /// only ever appended to. Not sorted by `parked_at`, which a fan-out registering its children
    /// in one millisecond makes a tie.
    pub(crate) async fn children_of(&self, group: &TicketId) -> Result<Vec<Record>> {
        let mut out = Vec::new();
        for id in self.store.list_children(group).await? {
            if let Some((record, _)) = self.store.get(&id).await? {
                out.push(record);
            }
        }
        Ok(out)
    }

    /// Run `group`'s join now. `Some` only if this call is the one that resolved it.
    pub(crate) async fn settle(
        &self,
        group: &TicketId,
        now_ms: u64,
    ) -> Result<Option<GroupResolved>> {
        self.settle_with(group, now_ms, false).await
    }

    /// [`Engine::settle`], and with `timed_out` the group's own deadline is what resolves it
    /// whatever its children are doing.
    ///
    /// On a group that is already resolved or cancelled it finishes what that implies for the
    /// children — a pass may have died between writing the group and closing them — and says
    /// nothing new.
    pub(crate) async fn settle_with(
        &self,
        group: &TicketId,
        now_ms: u64,
        timed_out: bool,
    ) -> Result<Option<GroupResolved>> {
        for _ in 0..crate::engine::MAX_CAS_RETRIES {
            let Some((g, rev)) = self.store.get(group).await? else { return Ok(None) };
            let Some(spec) = g.call.group else { return Ok(None) };
            if g.status != StoredStatus::Parked {
                self.finish_children(&g, now_ms).await?;
                return Ok(None);
            }

            let children = self.children_of(group).await?;
            let outcome = if timed_out {
                JoinOutcome::TimedOut
            } else {
                match join_decision(&spec, &children) {
                    Join::Wait => return Ok(None),
                    Join::Resolve(o) => o,
                }
            };

            // Bottom-up: the children first. Only the ones still parked — a child whose answer
            // has just landed keeps it, and the join reports it.
            for c in children.iter().filter(|c| c.status == StoredStatus::Parked) {
                self.cancel_if_parked(&c.ticket, now_ms).await?;
            }
            let children = self.children_of(group).await?;
            if children.iter().any(|c| c.status == StoredStatus::Parked) {
                continue; // one slipped back in, or a cancel lost a race: look again
            }

            let mut next = g.clone();
            next.status = StoredStatus::Ready;
            next.woken_at = Some(now_ms);
            next.joined = Some(join_result(outcome, &spec, &children));
            if self.store.cas(group, rev, &next).await? {
                self.finish_children(&next, now_ms).await?;
                return Ok(Some(GroupResolved { ticket: group.clone(), session: g.session }));
            }
        }
        Err(ParkError::storage("gave up after repeated compare-and-set contention"))
    }

    /// What a resolved (or cancelled) group implies for its children, written idempotently:
    ///
    /// * a group that resolved *consumed* its settled children (`ready`/`interrupted` to
    ///   `resumed`), so none of them lingers as an answer nobody will take;
    /// * a child still parked under a group that is no longer — one that registered while the
    ///   group was resolving — is cancelled;
    /// * a cancelled group's children are cancelled, whatever they had.
    pub(crate) async fn finish_children(&self, group: &Record, now_ms: u64) -> Result<()> {
        if group.status == StoredStatus::Parked {
            return Ok(());
        }
        for c in self.children_of(&group.ticket).await? {
            match (group.status, c.status) {
                (_, StoredStatus::Parked)
                | (StoredStatus::Cancelled, StoredStatus::Ready | StoredStatus::Interrupted) => {
                    self.cancel_open(&c.ticket, &join_actor(), now_ms).await?
                }
                (_, StoredStatus::Ready | StoredStatus::Interrupted) => {
                    self.consume_child(&c.ticket, now_ms).await?
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Cancel `ticket` only if it is still parked: for a child the join is giving up on,
    /// which may have been answered a moment ago.
    pub(crate) async fn cancel_if_parked(&self, ticket: &TicketId, now_ms: u64) -> Result<()> {
        crate::engine::retry(|| async {
            let Some((record, rev)) = self.store.get(ticket).await? else { return Ok(Some(())) };
            if record.status != StoredStatus::Parked {
                return Ok(Some(()));
            }
            let mut next = record;
            next.status = StoredStatus::Cancelled;
            next.cancelled_by = Some(join_actor());
            next.cancelled_at = Some(now_ms);
            Ok(self.store.cas(ticket, rev, &next).await?.then_some(()))
        })
        .await
    }

    /// Cancel `ticket` unless it is already over (`resumed` or `cancelled`), saying nothing if
    /// it is: the cascade of a group's cancel must be safe to run twice.
    pub(crate) async fn cancel_open(
        &self,
        ticket: &TicketId,
        by: &Agent,
        now_ms: u64,
    ) -> Result<()> {
        crate::engine::retry(|| async {
            let Some((record, rev)) = self.store.get(ticket).await? else { return Ok(Some(())) };
            if matches!(record.status, StoredStatus::Resumed | StoredStatus::Cancelled) {
                return Ok(Some(()));
            }
            let mut next = record;
            next.status = StoredStatus::Cancelled;
            next.cancelled_by = Some(by.clone());
            next.cancelled_at = Some(now_ms);
            Ok(self.store.cas(ticket, rev, &next).await?.then_some(()))
        })
        .await
    }

    /// A child whose group resolved has been taken, in effect: `ready`/`interrupted` to
    /// `resumed`. Anything else is left as it is.
    async fn consume_child(&self, ticket: &TicketId, now_ms: u64) -> Result<()> {
        crate::engine::retry(|| async {
            let Some((record, rev)) = self.store.get(ticket).await? else { return Ok(Some(())) };
            if !matches!(record.status, StoredStatus::Ready | StoredStatus::Interrupted) {
                return Ok(Some(()));
            }
            let mut next = record;
            next.status = StoredStatus::Resumed;
            next.resumed_at = Some(now_ms);
            Ok(self.store.cas(ticket, rev, &next).await?.then_some(()))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CallResult, Interruption, OutboundCall, ReplayPolicy};

    fn child(ticket: &str, status: StoredStatus, ok: Option<bool>) -> Record {
        let by = Agent::named("t");
        Record {
            ticket: ticket.into(),
            session: "s".into(),
            call: OutboundCall {
                correlation: ticket.into(),
                description: String::new(),
                deadline: None,
                poll: None,
                replay: ReplayPolicy::Unsafe,
                parent: Some("g".into()),
                group: None,
            },
            by: by.clone(),
            status,
            parked_at: 1,
            woken_at: None,
            resumed_at: None,
            cancelled_by: None,
            cancelled_at: None,
            result: ok.map(|ok| CallResult { ok, body: vec![], detail: None }),
            redispatches: 0,
            interruption: (status == StoredStatus::Interrupted).then(|| Interruption {
                reason: "r".into(),
                at: 1,
                by,
            }),
            announced_at: None,
            joined: None,
            batch: None,
        }
    }

    fn spec(policy: JoinPolicy, children: u32) -> GroupSpec {
        GroupSpec { policy, children }
    }

    use JoinOutcome::*;
    use JoinPolicy::*;
    use StoredStatus::*;

    #[test]
    fn the_join_as_a_table() {
        // (policy, expected children, the children, what the join says)
        let ok = |t: &str| child(t, Ready, Some(true));
        let bad = |t: &str| child(t, Ready, Some(false));
        let parked = |t: &str| child(t, Parked, None);
        let interrupted = |t: &str| child(t, Interrupted, None);
        let cancelled = |t: &str| child(t, Cancelled, None);

        let cases: Vec<(JoinPolicy, u32, Vec<Record>, Join)> = vec![
            // nothing has settled
            (AllSettled, 2, vec![parked("a"), parked("b")], Join::Wait),
            (FailFast, 2, vec![parked("a"), parked("b")], Join::Wait),
            // all-settled waits for every one, however they ended
            (AllSettled, 2, vec![ok("a"), parked("b")], Join::Wait),
            (AllSettled, 2, vec![bad("a"), parked("b")], Join::Wait),
            (AllSettled, 2, vec![ok("a"), ok("b")], Join::Resolve(AllAnswered)),
            (AllSettled, 2, vec![ok("a"), bad("b")], Join::Resolve(SomeFailed)),
            (AllSettled, 2, vec![interrupted("a"), ok("b")], Join::Resolve(SomeFailed)),
            (AllSettled, 2, vec![cancelled("a"), cancelled("b")], Join::Resolve(SomeFailed)),
            // fail-fast resolves on the first failure, of any kind
            (FailFast, 3, vec![bad("a"), parked("b"), parked("c")], Join::Resolve(FailedFast)),
            (
                FailFast,
                3,
                vec![parked("a"), interrupted("b"), parked("c")],
                Join::Resolve(FailedFast),
            ),
            (FailFast, 3, vec![parked("a"), cancelled("b")], Join::Resolve(FailedFast)),
            // and an answer that is not a failure does not
            (FailFast, 2, vec![ok("a"), parked("b")], Join::Wait),
            (FailFast, 2, vec![ok("a"), ok("b")], Join::Resolve(AllAnswered)),
            // a group declared with three children is not complete with two settled
            (AllSettled, 3, vec![ok("a"), ok("b")], Join::Wait),
            (FailFast, 3, vec![ok("a"), ok("b")], Join::Wait),
            // …but a failure among the two is already a failure under fail-fast
            (FailFast, 3, vec![ok("a"), bad("b")], Join::Resolve(FailedFast)),
            // an answer already consumed counts as the answer it was
            (AllSettled, 1, vec![child("a", Resumed, Some(true))], Join::Resolve(AllAnswered)),
        ];
        for (policy, n, kids, want) in cases {
            let names: Vec<_> = kids.iter().map(|k| (k.ticket.clone(), k.status)).collect();
            assert_eq!(
                join_decision(&spec(policy, n), &kids),
                want,
                "{policy:?} of {n}: {names:?}"
            );
        }
    }

    #[test]
    fn a_result_lists_every_child_and_counts_the_ones_never_parked() {
        let kids = vec![child("a", Ready, Some(true)), child("b", Cancelled, None)];
        let j = join_result(SomeFailed, &spec(AllSettled, 4), &kids);
        assert_eq!(j.missing, 2);
        assert_eq!(j.children.len(), 2);
        assert!(matches!(&j.children[0].ended, ChildResult::Answered(a) if a.ok));
        assert_eq!(j.children[1].ended, ChildResult::Cancelled);
    }
}
