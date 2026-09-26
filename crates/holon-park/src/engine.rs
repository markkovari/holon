//! The contract's operations (`holon:park/lot`) over [`ParkStore`].
//!
//! # The state machine
//!
//! One ticket moves forward only:
//!
//! ```text
//! parked ──(wake)──▶ ready ──(take-ready)──▶ resumed   [terminal]
//!   │                  │
//!   └──(cancel)──▶ cancelled                            [terminal]
//! ```
//!
//! `expired` is not a stored state — it is `to_entry`'s READING of a
//! `parked` record once `now` has passed its `call.deadline`. A late `wake`
//! against one still lands (the record is still `parked` until it does); only
//! [`Engine::pending`] stops surfacing it, per ADR-0100. Nothing here sweeps
//! expired tickets on a timer — there is no background task in this crate,
//! same reason `holon-vcs`'s core has none (ADR-0095: that belongs to whatever
//! native daemon serves this contract).
//!
//! # Why every read-shaping method takes `now_ms` explicitly
//!
//! `expired` is the one piece of business logic in this crate that depends on
//! wall-clock time, so it is the one piece of business logic that must be
//! deterministic under test: passing `now_ms` in, rather than reading
//! `SystemTime::now()` inside the engine, is what lets a test park a ticket
//! with a deadline in the past and assert it reads `expired` without a sleep.
//! Every WRITE still stamps a real timestamp ([`now_ms`]) — only classifying
//! what is already stored needs to be pure.
//!
//! # Idempotency, one case at a time
//!
//! * **`park` on an existing ticket** never writes a second record — the
//!   ticket id already names `(session, correlation)`. It reports which
//!   `park-outcome` applies to whatever is there now (`ready` reports
//!   `already-woken`; anything else reports `already-parked`), so a caller
//!   retrying after its own crash learns whether it's still waiting or can
//!   `take-ready` immediately.
//! * **`wake` on a `ready` or `resumed` or `cancelled` ticket** is accepted
//!   without rewriting anything — a redelivered webhook, or one that arrived
//!   after the ticket was cancelled or already taken, is not an error to the
//!   sender (ADR-0100). Only a `parked` ticket is actually moved to `ready`.
//! * **`take-ready`** is the one call that is NOT idempotent on purpose: a
//!   second call against an already-`resumed` ticket is `already-closed`,
//!   because handing the same result to two resumers is the bug this
//!   contract exists to prevent.

use std::future::Future;

use crate::error::{ParkError, Result};
use crate::model::{
    Agent, CallResult, OutboundCall, ParkOutcome, ParkResult, SessionId, TicketEntry, TicketId,
    TurnStatus,
};
use crate::store::{ParkStore, Record, StoredStatus};
use crate::ticket::ticket_id;

/// How many times a CAS is retried before giving up. Contention on one
/// ticket comes from at most a couple of racing writers (a duplicate `park`,
/// a `wake` racing a `cancel`) — never a hot loop, so a small bound is a
/// correctness net, not a throttle.
const MAX_CAS_RETRIES: u32 = 8;

/// What [`Engine::wake`] tells its caller beyond the WIT contract's bare
/// ticket id — see that method's doc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeOutcome {
    pub ticket: TicketId,
    pub session: SessionId,
    pub freshly_woken: bool,
}

pub struct Engine<S: ParkStore> {
    store: S,
}

impl<S: ParkStore> Engine<S> {
    pub fn new(store: S) -> Self {
        Engine { store }
    }

    /// Register an outstanding call before dispatching it.
    pub async fn park(
        &self,
        session: &SessionId,
        call: OutboundCall,
        by: Agent,
        now_ms: u64,
    ) -> Result<ParkResult> {
        let ticket = ticket_id(session, &call.correlation);

        if let Some((existing, _)) = self.store.get(&ticket).await? {
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
        };

        if !self.store.create(&ticket, &record).await? {
            // Lost a race to create the identical ticket. The winner's record
            // is what's there now; report against it, not against ours.
            let (existing, _) = self.store.get(&ticket).await?.ok_or_else(|| {
                ParkError::storage(
                    "create() said the ticket already existed, then get() found nothing",
                )
            })?;
            return Ok(ParkResult { ticket, outcome: outcome_of(&existing) });
        }

        // Best-effort indexes: a lost race on either just means another
        // caller already added the same entry. `session` is read back by
        // `pending`/`oplog`; a ticket that isn't indexed yet because this
        // process died right here is still reachable by its own id — it is
        // only invisible to a LISTING until something re-indexes it, which is
        // exactly what a `park` retry (the crash-recovery path) does.
        self.store.index_session(session, &ticket).await?;
        if let Some(other) = self.store.claim_correlation(&record.call.correlation, &ticket).await?
        {
            debug_assert_eq!(
                other, ticket,
                "claim_correlation raced with a DIFFERENT ticket after create() won"
            );
        }

        Ok(ParkResult { ticket, outcome: ParkOutcome::Parked })
    }

    /// The answer arrived. See the module doc for which prior states this
    /// silently accepts rather than errors on.
    ///
    /// Returns [`WakeOutcome`] rather than the WIT contract's bare
    /// `ticket-id`, because whoever serves the contract over HTTP (`comp-park`)
    /// needs one more thing the wire does not: whether this call actually
    /// moved a `parked` ticket to `ready`, or found one already past that. A
    /// redelivered webhook must not tell a resumer pool about the same ticket
    /// twice — [`WakeOutcome::freshly_woken`] is how the daemon knows to
    /// publish to `PARK_WAKE` only once per real answer.
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
                // ready, resumed or cancelled: accepted, nothing rewritten.
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

    /// Every ticket for `session` still `parked` (and not yet past its
    /// deadline) or `ready` — what a resumer, or the session itself on
    /// restart, reads to find outstanding work.
    pub async fn pending(&self, session: &SessionId, now_ms: u64) -> Result<Vec<TicketEntry>> {
        let mut entries = self.entries_of(session, now_ms).await?;
        entries.retain(|e| matches!(e.status, TurnStatus::Parked | TurnStatus::Ready));
        Ok(entries)
    }

    /// Consume a `ready` ticket's result exactly once.
    pub async fn take_ready(&self, ticket: &TicketId, now_ms: u64) -> Result<CallResult> {
        retry(|| async {
            let (record, rev) =
                self.store.get(ticket).await?.ok_or_else(|| ParkError::NotFound(ticket.clone()))?;

            match record.status {
                StoredStatus::Parked => return Err(ParkError::NotFound(ticket.clone())),
                StoredStatus::Resumed | StoredStatus::Cancelled => {
                    return Err(ParkError::AlreadyClosed(ticket.clone()))
                }
                StoredStatus::Ready => {}
            }
            let result = record.result.clone().ok_or_else(|| {
                ParkError::storage(format!("{ticket} is ready with no result recorded"))
            })?;

            let mut next = record;
            next.status = StoredStatus::Resumed;
            next.resumed_at = Some(now_ms);
            if self.store.cas(ticket, rev, &next).await? {
                Ok(Some(result))
            } else {
                Ok(None)
            }
        })
        .await
    }

    /// Give up on a `parked` or `ready` ticket.
    pub async fn cancel(&self, ticket: &TicketId, by: Agent, now_ms: u64) -> Result<()> {
        let by = &by;
        retry(|| async {
            let (record, rev) =
                self.store.get(ticket).await?.ok_or_else(|| ParkError::NotFound(ticket.clone()))?;

            match record.status {
                StoredStatus::Resumed | StoredStatus::Cancelled => {
                    return Err(ParkError::AlreadyClosed(ticket.clone()))
                }
                StoredStatus::Parked | StoredStatus::Ready => {}
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

/// What `park` reports when a ticket already existed.
fn outcome_of(record: &Record) -> ParkOutcome {
    match record.status {
        StoredStatus::Ready => ParkOutcome::AlreadyWoken,
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
async fn retry<T, F, Fut>(mut f: F) -> Result<T>
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
