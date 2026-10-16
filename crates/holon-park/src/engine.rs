//! The contract's operations (`holon:park/lot`) over [`ParkStore`].
//!
//! # The state machine
//!
//! One ticket moves forward only:
//!
//! ```text
//! parked ──(wake)──────────────▶ ready ─────────┐
//!   │                              │             ├─(take-ready)─▶ resumed   [terminal]
//!   └──(interrupt, recover)──▶ interrupted ──────┘
//!
//! parked, ready, interrupted ──(cancel, recover of an `abandon` call)──▶ cancelled   [terminal]
//! ```
//!
//! `interrupted` is how a turn is told a call will get no answer and may or
//! may not have happened (ADR-0106). It is taken exactly like `ready`; only
//! what [`Engine::take_ready`] hands over differs.
//!
//! `expired` is not a stored state — it is `to_entry`'s reading of a `parked`
//! record once `now` passed its `call.deadline`. A late `wake` still lands;
//! only [`Engine::pending`] stops surfacing it (ADR-0100). Nothing here sweeps
//! expired tickets: there is no background task in this crate (ADR-0095).
//! [`Engine::recover`] is what a resumer calls on a timer to act on them.
//!
//! `now_ms` is passed in rather than read inside the engine so that `expired`,
//! the one time-dependent classification, is deterministic under test. Writes
//! still stamp a real timestamp ([`now_ms`]).
//! # Idempotency
//!
//! * `park` on an existing ticket never writes a second record; it reports
//!   `already-woken` (if `ready` or `interrupted`) or `already-parked`, so a
//!   retrying caller learns whether it can `take-ready`.
//! * `wake` on a `ready`, `interrupted`, `resumed` or `cancelled` ticket is
//!   accepted without rewriting (a redelivered webhook is not an error,
//!   ADR-0100); only a `parked` ticket moves to `ready`. An answer that
//!   arrives after the ticket was interrupted is dropped, like one that
//!   arrives after a cancel.
//! * `interrupt` on an `interrupted` ticket is accepted without rewriting;
//!   on a `ready` one it changes nothing, because the answer beat it.
//! * `take-ready` is deliberately NOT idempotent: a second call on a `resumed`
//!   ticket is `already-closed`, since handing one result to two resumers is
//!   the bug this contract prevents. [`Engine::resolution`] is the read that
//!   does not consume, for a resumer that must deliver before it closes.

use std::future::Future;

use crate::error::{ParkError, Result};
use crate::model::{
    Agent, CallResult, Interruption, OutboundCall, ParkOutcome, ParkResult, Resolution, SessionId,
    TicketEntry, TicketId, TurnStatus,
};
use crate::store::{ParkStore, Record, StoredStatus};
use crate::ticket::ticket_id;

/// CAS retry bound. Contention is at most a couple of racing writers (duplicate
/// `park`, `wake` vs `cancel`), so this is a correctness net, not a throttle.
const MAX_CAS_RETRIES: u32 = 8;

/// What [`Engine::wake`] tells its caller beyond the bare ticket id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeOutcome {
    pub ticket: TicketId,
    pub session: SessionId,
    pub freshly_woken: bool,
}

/// What [`Engine::interrupt`] tells its caller: the status the ticket is in
/// now, and whether this call is the one that put it there (so the daemon
/// announces once per real transition, as for [`WakeOutcome`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterruptOutcome {
    pub ticket: TicketId,
    pub session: SessionId,
    pub status: TurnStatus,
    pub fresh: bool,
}

pub struct Engine<S: ParkStore> {
    pub(crate) store: S,
}

impl<S: ParkStore> Engine<S> {
    pub fn new(store: S) -> Self {
        Engine { store }
    }

    pub async fn park(
        &self,
        session: &SessionId,
        call: OutboundCall,
        by: Agent,
        now_ms: u64,
    ) -> Result<ParkResult> {
        let ticket = ticket_id(session, &call.correlation);

        if let Some((existing, _)) = self.store.get(&ticket).await? {
            // A retry after a crash lands here. The crash may have come between `create` and
            // the two index writes below, which leaves a record nothing can find by session
            // or by correlation, and this is the caller's one chance to say so.
            self.reindex_or_refuse(&existing).await?;
            return Ok(ParkResult { ticket, outcome: outcome_of(&existing) });
        }

        if let Some(other) = self.store.find_correlation(&call.correlation).await? {
            if other != ticket {
                return Err(ParkError::Invalid(format!(
                    "correlation {:?} is already in use by ticket {other} (session {:?})",
                    call.correlation, session
                )));
            }
        }

        let record = Record {
            ticket: ticket.clone(),
            session: session.clone(),
            call,
            by,
            status: StoredStatus::Parked,
            parked_at: now_ms,
            woken_at: None,
            resumed_at: None,
            cancelled_by: None,
            cancelled_at: None,
            result: None,
            redispatches: 0,
            interruption: None,
            announced_at: None,
        };

        if !self.store.create(&ticket, &record).await? {
            // Lost a race to create the identical ticket. The winner's record
            // is what's there now; report against it, not against ours.
            let (existing, _) = self.store.get(&ticket).await?.ok_or_else(|| {
                ParkError::storage(
                    "create() said the ticket already existed, then get() found nothing",
                )
            })?;
            self.reindex_or_refuse(&existing).await?;
            return Ok(ParkResult { ticket, outcome: outcome_of(&existing) });
        }

        // Best-effort indexes: a lost race on either just means another
        // caller already added the same entry. `session` is read back by
        // `pending`/`oplog` and `correlation` by `wake`; a ticket whose process
        // died right here is reachable by its own id and by nothing else until
        // something writes them again. A `park` retry does (above), and so does
        // `Engine::repair` for a caller that never comes back.
        if let Some(other) = self.reindex(&record).await? {
            debug_assert_eq!(
                other, ticket,
                "claim_correlation raced with a DIFFERENT ticket after create() won"
            );
        }

        Ok(ParkResult { ticket, outcome: ParkOutcome::Parked })
    }

    /// Write `record`'s two index entries, both idempotent: its place in its
    /// session's list, and the claim on its `correlation` that `wake` finds it by.
    /// `Some(other)`: a different ticket holds the correlation, which a ticket
    /// derived from `(session, correlation)` can only mean when two sessions
    /// parked one correlation (refused by design, but a crash between `create`
    /// and the claim lets the second in).
    pub(crate) async fn reindex(&self, record: &Record) -> Result<Option<TicketId>> {
        self.store.index_session(&record.session, &record.ticket).await?;
        let holder = self.store.claim_correlation(&record.call.correlation, &record.ticket).await?;
        Ok(holder.filter(|other| other != &record.ticket))
    }

    async fn reindex_or_refuse(&self, record: &Record) -> Result<()> {
        match self.reindex(record).await? {
            None => Ok(()),
            Some(other) => Err(ParkError::Invalid(format!(
                "correlation {:?} is already in use by ticket {other}",
                record.call.correlation
            ))),
        }
    }

    /// The answer arrived. Returns [`WakeOutcome`] rather than the bare
    /// `ticket-id` so the daemon (`comp-park`) knows whether this call actually
    /// moved `parked` to `ready` ([`WakeOutcome::freshly_woken`]) and publishes
    /// to `PARK_WAKE` once per real answer.
    pub async fn wake(
        &self,
        correlation: &str,
        answer: CallResult,
        now_ms: u64,
    ) -> Result<WakeOutcome> {
        let ticket = self
            .store
            .find_correlation(correlation)
            .await?
            .ok_or_else(|| ParkError::NotFound(correlation.to_string()))?;

        retry(|| async {
            let (record, rev) = self.store.get(&ticket).await?.ok_or_else(|| {
                ParkError::storage(format!(
                    "correlation index named {ticket}, which does not exist"
                ))
            })?;
            let session = record.session.clone();

            if !matches!(record.status, StoredStatus::Parked) {
                // ready, interrupted, resumed or cancelled: accepted, nothing
                // rewritten.
                return Ok(Some(WakeOutcome {
                    ticket: ticket.clone(),
                    session,
                    freshly_woken: false,
                }));
            }

            let mut next = record;
            next.status = StoredStatus::Ready;
            next.woken_at = Some(now_ms);
            next.result = Some(answer.clone());
            if self.store.cas(&ticket, rev, &next).await? {
                Ok(Some(WakeOutcome { ticket: ticket.clone(), session, freshly_woken: true }))
            } else {
                Ok(None) // lost the race; retry() will read again
            }
        })
        .await
    }

    /// Give up on a `parked` ticket without an answer: the turn will be told,
    /// by `take-ready`, that the call's fate is unknown (ADR-0106).
    ///
    /// Not tied to the deadline: whoever calls decides, and `by` and `reason`
    /// are recorded. A `ready` ticket is left alone — the answer beat this —
    /// and the outcome says so.
    pub async fn interrupt(
        &self,
        ticket: &TicketId,
        reason: &str,
        by: Agent,
        now_ms: u64,
    ) -> Result<InterruptOutcome> {
        if reason.trim().is_empty() {
            return Err(ParkError::Invalid(
                "interrupt needs a reason: it is what the turn is told".into(),
            ));
        }
        let by = &by;
        retry(|| async {
            let (record, rev) =
                self.store.get(ticket).await?.ok_or_else(|| ParkError::NotFound(ticket.clone()))?;
            let session = record.session.clone();
            let unchanged = |status| {
                Ok(Some(InterruptOutcome {
                    ticket: ticket.clone(),
                    session: session.clone(),
                    status,
                    fresh: false,
                }))
            };

            match record.status {
                StoredStatus::Resumed | StoredStatus::Cancelled => {
                    return Err(ParkError::AlreadyClosed(ticket.clone()))
                }
                StoredStatus::Ready => return unchanged(TurnStatus::Ready),
                StoredStatus::Interrupted => return unchanged(TurnStatus::Interrupted),
                StoredStatus::Parked => {}
            }

            let mut next = record;
            next.status = StoredStatus::Interrupted;
            next.interruption =
                Some(Interruption { reason: reason.to_string(), at: now_ms, by: by.clone() });
            if self.store.cas(ticket, rev, &next).await? {
                Ok(Some(InterruptOutcome {
                    ticket: ticket.clone(),
                    session,
                    status: TurnStatus::Interrupted,
                    fresh: true,
                }))
            } else {
                Ok(None)
            }
        })
        .await
    }

    /// Every ticket for `session` still `parked` (and not yet past its
    /// deadline), `ready` or `interrupted` — what a resumer, or the session
    /// itself on restart, reads to find outstanding work.
    pub async fn pending(&self, session: &SessionId, now_ms: u64) -> Result<Vec<TicketEntry>> {
        let mut entries = self.entries_of(session, now_ms).await?;
        entries.retain(|e| {
            matches!(e.status, TurnStatus::Parked | TurnStatus::Ready | TurnStatus::Interrupted)
        });
        Ok(entries)
    }

    /// Consume a `ready` or `interrupted` ticket's resolution, exactly once.
    pub async fn take_ready(&self, ticket: &TicketId, now_ms: u64) -> Result<Resolution> {
        retry(|| async {
            let (record, rev) =
                self.store.get(ticket).await?.ok_or_else(|| ParkError::NotFound(ticket.clone()))?;
            let resolution = resolution_of(&record)?;

            let mut next = record;
            next.status = StoredStatus::Resumed;
            next.resumed_at = Some(now_ms);
            if self.store.cas(ticket, rev, &next).await? {
                Ok(Some(resolution))
            } else {
                Ok(None)
            }
        })
        .await
    }

    /// What `take-ready` would hand over, without consuming it: for a resumer
    /// that delivers the resolution somewhere and only then closes the ticket,
    /// so that dying in between redelivers instead of losing the answer.
    /// Same refusals as [`Engine::take_ready`].
    pub async fn resolution(&self, ticket: &TicketId) -> Result<Resolution> {
        let (record, _) =
            self.store.get(ticket).await?.ok_or_else(|| ParkError::NotFound(ticket.clone()))?;
        resolution_of(&record)
    }

    pub async fn cancel(&self, ticket: &TicketId, by: Agent, now_ms: u64) -> Result<()> {
        let by = &by;
        retry(|| async {
            let (record, rev) =
                self.store.get(ticket).await?.ok_or_else(|| ParkError::NotFound(ticket.clone()))?;

            match record.status {
                StoredStatus::Resumed | StoredStatus::Cancelled => {
                    return Err(ParkError::AlreadyClosed(ticket.clone()))
                }
                StoredStatus::Parked | StoredStatus::Ready | StoredStatus::Interrupted => {}
            }

            let mut next = record;
            next.status = StoredStatus::Cancelled;
            next.cancelled_by = Some(by.clone());
            next.cancelled_at = Some(now_ms);
            if self.store.cas(ticket, rev, &next).await? {
                Ok(Some(()))
            } else {
                Ok(None)
            }
        })
        .await
    }

    /// One session's history, oldest first, starting strictly after `after`
    /// milliseconds (`None`: from the start).
    pub async fn oplog(
        &self,
        session: &SessionId,
        after: Option<u64>,
        limit: u32,
        now_ms: u64,
    ) -> Result<Vec<TicketEntry>> {
        let mut entries = self.entries_of(session, now_ms).await?;
        if let Some(after) = after {
            entries.retain(|e| e.parked_at > after);
        }
        entries.truncate(limit as usize);
        Ok(entries)
    }

    async fn entries_of(&self, session: &SessionId, now_ms: u64) -> Result<Vec<TicketEntry>> {
        let tickets = self.store.list_session(session).await?;
        let mut entries = Vec::with_capacity(tickets.len());
        for t in tickets {
            if let Some((record, _)) = self.store.get(&t).await? {
                entries.push(to_entry(&record, now_ms));
            }
        }
        entries.sort_by_key(|e| e.parked_at);
        Ok(entries)
    }
}

/// A ticket's resolution, if it has one: `ready` and `interrupted` do.
fn resolution_of(record: &Record) -> Result<Resolution> {
    match record.status {
        StoredStatus::Parked => Err(ParkError::NotFound(record.ticket.clone())),
        StoredStatus::Resumed | StoredStatus::Cancelled => {
            Err(ParkError::AlreadyClosed(record.ticket.clone()))
        }
        StoredStatus::Ready => record.result.clone().map(Resolution::Answered).ok_or_else(|| {
            ParkError::storage(format!("{} is ready with no result recorded", record.ticket))
        }),
        StoredStatus::Interrupted => {
            record.interruption.clone().map(Resolution::Interrupted).ok_or_else(|| {
                ParkError::storage(format!(
                    "{} is interrupted with no interruption recorded",
                    record.ticket
                ))
            })
        }
    }
}

fn outcome_of(record: &Record) -> ParkOutcome {
    match record.status {
        // A resolution is waiting: an answer, or the news there will not be one.
        StoredStatus::Ready | StoredStatus::Interrupted => ParkOutcome::AlreadyWoken,
        StoredStatus::Parked | StoredStatus::Resumed | StoredStatus::Cancelled => {
            ParkOutcome::AlreadyParked
        }
    }
}

/// The WIT-shaped, deadline-aware projection of a stored [`Record`].
fn to_entry(record: &Record, now_ms: u64) -> TicketEntry {
    let expired = matches!(record.status, StoredStatus::Parked)
        && record.call.deadline.is_some_and(|d| now_ms >= d);
    TicketEntry {
        ticket: record.ticket.clone(),
        session: record.session.clone(),
        status: if expired { TurnStatus::Expired } else { record.status.into() },
        parked_at: record.parked_at,
        woken_at: record.woken_at,
        resumed_at: record.resumed_at,
        correlation: record.call.correlation.clone(),
        replay: record.call.replay,
        deadline: record.call.deadline,
        redispatches: record.redispatches,
        interrupted_at: record.interruption.as_ref().map(|i| i.at),
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Retry a CAS loop up to [`MAX_CAS_RETRIES`] times. `f` returns `Ok(Some(_))`
/// on success (whether it wrote or found nothing to write), `Ok(None)` to try
/// again, `Err` to stop.
pub(crate) async fn retry<T, F, Fut>(mut f: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Option<T>>>,
{
    for _ in 0..MAX_CAS_RETRIES {
        if let Some(v) = f().await? {
            return Ok(v);
        }
    }
    Err(ParkError::storage("gave up after repeated compare-and-set contention"))
}
