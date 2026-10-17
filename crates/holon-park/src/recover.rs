//! The recovery pass (ADR-0106): what happens to a call whose answer never
//! came, and to an answer nobody took.
//!
//! A resumer calls [`Engine::recover`] on a timer. Each pass reads every
//! ticket and, for the ones that need it, makes ONE compare-and-set move:
//!
//! | ticket | `replay` | past its deadline | the pass |
//! |---|---|---|---|
//! | `parked` | `unsafe` | yes | `interrupted` — the turn will be told the call may or may not have happened |
//! | `parked` | `abandon` | yes | `cancelled` — nobody is told |
//! | `parked` | `safe` | yes, redispatches left | grants a re-dispatch: `redispatches + 1`, deadline pushed out |
//! | `parked` | `safe` | yes, none left | `interrupted`, like an unsafe one |
//! | `parked` | any | no, or no deadline | nothing |
//! | `ready` / `interrupted` | — | untaken for `reannounce_after_ms` | announced again (a lost `PARK_WAKE` nudge) |
//!
//! "Past its deadline" is the deadline plus [`RecoveryPolicy::deadline_grace_ms`]:
//! a pass acts irreversibly, and a deadline is a wall-clock instant set by
//! whoever parked the call, possibly on a box whose clock disagrees with the
//! resumer's.
//!
//! The pass makes the move; telling anyone is the caller's job, from
//! [`Recovered::announcement`]. A pass that dies between the two leaves a ticket
//! the next pass announces again, because an `interrupted` ticket nobody took
//! is exactly the re-announce row.
//!
//! This is the whole of the policy and it is a pure function of a record and a
//! clock ([`plan`]), so the table above is tested as a table.

use crate::engine::Engine;
use crate::error::Result;
use crate::model::{Agent, Interruption, ReplayPolicy, SessionId, TicketId, WakeMessage};
use crate::store::{ParkStore, Record, StoredStatus};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryPolicy {
    /// How long past its deadline a parked call must sit before a pass acts.
    pub deadline_grace_ms: u64,
    /// Re-dispatches a `safe` call is granted before it is interrupted like an
    /// unsafe one. Bounds a call whose far side is simply down.
    pub max_redispatches: u32,
    /// How long a re-dispatched call gets before a pass looks at it again.
    pub retry_after_ms: u64,
    /// How long a resolved ticket may sit untaken before it is announced again,
    /// and the gap between one such re-announcement and the next.
    pub reannounce_after_ms: u64,
}

impl Default for RecoveryPolicy {
    fn default() -> Self {
        RecoveryPolicy {
            deadline_grace_ms: 10_000,
            max_redispatches: 3,
            retry_after_ms: 60_000,
            reannounce_after_ms: 30_000,
        }
    }
}

/// What one pass did to one ticket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryAction {
    /// `parked` to `interrupted`.
    Interrupted,
    /// `parked` to `cancelled`.
    Abandoned,
    /// A re-dispatch was granted; this is the number it brought the ticket to.
    Redispatch { attempt: u32 },
    /// A resolved ticket nobody took; no state moved, only `announced_at`.
    Reannounce,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recovered {
    pub ticket: TicketId,
    pub session: SessionId,
    pub correlation: String,
    pub action: RecoveryAction,
}

impl Recovered {
    /// What to put on `PARK_WAKE` for this move, if anything: an abandoned call
    /// has nobody to tell.
    pub fn announcement(&self) -> Option<WakeMessage> {
        match &self.action {
            RecoveryAction::Interrupted | RecoveryAction::Reannounce => {
                Some(WakeMessage::resolved(&self.session, &self.ticket))
            }
            RecoveryAction::Redispatch { attempt } => {
                Some(WakeMessage::redispatch(&self.session, &self.ticket, *attempt))
            }
            RecoveryAction::Abandoned => None,
        }
    }
}

const UNSAFE_REASON: &str = "no answer arrived by the deadline, and this call is not \
     replay-safe: it may or may not have taken effect. Check on the far side before repeating it.";

fn exhausted_reason(redispatches: u32) -> String {
    format!(
        "no answer arrived after {redispatches} re-dispatches: the call may or may not \
         have taken effect. Check on the far side before repeating it."
    )
}

/// The one move a pass would make on `record` at `now_ms`, and the record it
/// would write, or `None` if it leaves the ticket alone.
pub fn plan(
    record: &Record,
    by: &Agent,
    policy: &RecoveryPolicy,
    now_ms: u64,
) -> Option<(Record, RecoveryAction)> {
    match record.status {
        StoredStatus::Parked => {
            let deadline = record.call.deadline?;
            if now_ms < deadline.saturating_add(policy.deadline_grace_ms) {
                return None;
            }
            let mut next = record.clone();
            let interrupt = |next: &mut Record, reason: String| {
                next.status = StoredStatus::Interrupted;
                next.interruption = Some(Interruption { reason, at: now_ms, by: by.clone() });
                RecoveryAction::Interrupted
            };
            let action = match record.call.replay {
                ReplayPolicy::Abandon => {
                    next.status = StoredStatus::Cancelled;
                    next.cancelled_by = Some(by.clone());
                    next.cancelled_at = Some(now_ms);
                    RecoveryAction::Abandoned
                }
                ReplayPolicy::Unsafe => interrupt(&mut next, UNSAFE_REASON.to_string()),
                ReplayPolicy::Safe if record.redispatches >= policy.max_redispatches => {
                    interrupt(&mut next, exhausted_reason(record.redispatches))
                }
                ReplayPolicy::Safe => {
                    next.redispatches += 1;
                    next.call.deadline = Some(now_ms.saturating_add(policy.retry_after_ms));
                    RecoveryAction::Redispatch { attempt: next.redispatches }
                }
            };
            Some((next, action))
        }
        StoredStatus::Ready | StoredStatus::Interrupted => {
            let resolved_at = match record.status {
                StoredStatus::Ready => record.woken_at,
                _ => record.interruption.as_ref().map(|i| i.at),
            }?;
            let after = policy.reannounce_after_ms;
            let due = now_ms >= resolved_at.saturating_add(after)
                && record.announced_at.is_none_or(|a| now_ms >= a.saturating_add(after));
            if !due {
                return None;
            }
            let mut next = record.clone();
            next.announced_at = Some(now_ms);
            Some((next, RecoveryAction::Reannounce))
        }
        StoredStatus::Resumed | StoredStatus::Cancelled => None,
    }
}

/// What [`Engine::repair`] put back.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Repaired {
    /// Tickets added back to their session's list: without it `pending` and
    /// `oplog` cannot see them.
    pub reindexed: Vec<TicketId>,
    /// Tickets whose correlation was claimed again: without it a webhook's `wake`
    /// is refused `not-found`.
    pub reclaimed: Vec<TicketId>,
    /// Tickets whose correlation another ticket holds. Two sessions parked one
    /// correlation, which is refused by design, and a crash between `create` and
    /// the claim let the second in. Not something a pass can settle.
    pub conflicts: Vec<TicketId>,
}

impl Repaired {
    pub fn is_empty(&self) -> bool {
        self.reindexed.is_empty() && self.reclaimed.is_empty() && self.conflicts.is_empty()
    }
}

impl<S: ParkStore> Engine<S> {
    /// Put back the two index entries a crash can lose: a ticket's place in its session's
    /// list, and the claim on its correlation.
    ///
    /// `park` writes the record first and the entries after, so a process that dies between
    /// leaves a ticket reachable only by its own id. A caller that comes back repairs its
    /// own with a retry; this is for the one that does not. A pass reads each session's list
    /// once and each ticket's claim once, and writes only what is missing, so on a healthy
    /// bucket it writes nothing.
    pub async fn repair(&self) -> Result<Repaired> {
        let mut tickets = self.store.list_tickets().await?;
        tickets.sort();
        let mut by_session: std::collections::BTreeMap<SessionId, Vec<Record>> =
            std::collections::BTreeMap::new();
        for ticket in tickets {
            if let Some((record, _)) = self.store.get(&ticket).await? {
                by_session.entry(record.session.clone()).or_default().push(record);
            }
        }

        let mut out = Repaired::default();
        for (session, records) in by_session {
            let listed: std::collections::HashSet<TicketId> =
                self.store.list_session(&session).await?.into_iter().collect();
            for record in records {
                if !listed.contains(&record.ticket) {
                    self.store.index_session(&session, &record.ticket).await?;
                    out.reindexed.push(record.ticket.clone());
                }
                match self.store.find_correlation(&record.call.correlation).await? {
                    Some(holder) if holder == record.ticket => {}
                    Some(_) => out.conflicts.push(record.ticket.clone()),
                    None => {
                        match self
                            .store
                            .claim_correlation(&record.call.correlation, &record.ticket)
                            .await?
                        {
                            None => out.reclaimed.push(record.ticket.clone()),
                            Some(holder) if holder == record.ticket => {}
                            Some(_) => out.conflicts.push(record.ticket.clone()),
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    /// One recovery pass over every ticket in the store (see the module doc).
    ///
    /// One compare-and-set per ticket, no retry: a lost race means another
    /// pass, or a `wake`, got there first, and the next pass reads fresh. So two
    /// resumers sweeping at once grant each re-dispatch and each interruption
    /// exactly once between them.
    pub async fn recover(
        &self,
        by: &Agent,
        policy: &RecoveryPolicy,
        now_ms: u64,
    ) -> Result<Vec<Recovered>> {
        let mut tickets = self.store.list_tickets().await?;
        tickets.sort();
        let mut out = Vec::new();
        for ticket in tickets {
            let Some((record, rev)) = self.store.get(&ticket).await? else { continue };
            let Some((next, action)) = plan(&record, by, policy, now_ms) else { continue };
            if self.store.cas(&ticket, rev, &next).await? {
                out.push(Recovered {
                    ticket,
                    session: record.session,
                    correlation: record.call.correlation,
                    action,
                });
            }
        }
        Ok(out)
    }
}
