//! `holon:park/lot` over [`ParkStore`]; a ticket's status only moves forward.

use std::future::Future;

use crate::error::{ParkError, Result};
use crate::model::{
    Agent, CallResult, GroupSpec, Interruption, OutboundCall, ParkOutcome, ParkResult, Resolution,
    SessionId, TicketEntry, TicketId, TurnStatus, WakeMessage, MAX_GROUP_CHILDREN,
};
use crate::store::{IndexChild, ParkStore, Record, StoredStatus};
use crate::ticket::ticket_id;

/// Contention is at most a couple of racing writers, so this is a correctness net, not a throttle.
pub(crate) const MAX_CAS_RETRIES: u32 = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeOutcome {
    pub ticket: TicketId,
    pub session: SessionId,
    pub freshly_woken: bool,
    /// A child is never announced on its own; its group is, if this call resolved it (ADR-0109).
    pub announce: Vec<WakeMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterruptOutcome {
    pub ticket: TicketId,
    pub session: SessionId,
    pub status: TurnStatus,
    /// This call made the transition, so the daemon announces once per real one.
    pub fresh: bool,
    pub announce: Vec<WakeMessage>,
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
        self.park_in(session, call, by, now_ms, None).await
    }

    /// Stamped with its batch (ADR-0110), so a rollback cancels exactly what it created.
    pub(crate) async fn park_in(
        &self,
        session: &SessionId,
        call: OutboundCall,
        by: Agent,
        now_ms: u64,
        batch: Option<&str>,
    ) -> Result<ParkResult> {
        check_shape(&call)?;
        let ticket = ticket_id(session, &call.correlation);

        if let Some((existing, _)) = self.store.get(&ticket).await? {
            if existing.call.parent != call.parent {
                return Err(ParkError::Invalid(format!(
                    "ticket {ticket} was parked {}, not {}",
                    describe_parent(&existing.call.parent),
                    describe_parent(&call.parent)
                )));
            }
            // A crash between `create` and the index writes below leaves an unfindable record:
            // this retry heals it.
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

        // The place is taken before the record is written, so the bound holds when children
        // race for the last one.
        let group = match &call.parent {
            Some(parent) => Some((parent.clone(), self.open_group(session, parent).await?)),
            None => None,
        };
        if let Some((name, (group_ticket, spec))) = &group {
            if self.store.index_child(group_ticket, &ticket, spec.children).await?
                == IndexChild::Full
            {
                return Err(ParkError::Invalid(format!(
                    "group {name:?} is full: it was declared with {} children",
                    spec.children
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
            joined: None,
            batch: batch.map(str::to_string),
        };

        if !self.store.create(&ticket, &record).await? {
            // Lost a race to create the identical ticket: report against the winner's record.
            let (existing, _) = self.store.get(&ticket).await?.ok_or_else(|| {
                ParkError::storage(
                    "create() said the ticket already existed, then get() found nothing",
                )
            })?;
            self.reindex_or_refuse(&existing).await?;
            return Ok(ParkResult { ticket, outcome: outcome_of(&existing) });
        }

        // A crash here leaves the ticket reachable only by id until a `park` retry or
        // `Engine::repair` reindexes it.
        if let Some(other) = self.reindex(&record).await? {
            debug_assert_eq!(
                other, ticket,
                "claim_correlation raced with a DIFFERENT ticket after create() won"
            );
        }

        // The group may have resolved meanwhile: cancel a child it will never join.
        if let Some((name, (group_ticket, _))) = &group {
            let open = matches!(
                self.store.get(group_ticket).await?,
                Some((g, _)) if g.status == StoredStatus::Parked
            );
            if !open {
                self.cancel_if_parked(&ticket, now_ms).await?;
                return Err(ParkError::Invalid(format!(
                    "group {name:?} resolved while this call was being parked"
                )));
            }
        }

        Ok(ParkResult { ticket, outcome: ParkOutcome::Parked })
    }

    pub(crate) async fn open_group(
        &self,
        session: &SessionId,
        parent: &str,
    ) -> Result<(TicketId, GroupSpec)> {
        let ticket = ticket_id(session, parent);
        let Some((g, _)) = self.store.get(&ticket).await? else {
            return Err(ParkError::Invalid(format!(
                "no group {parent:?} in this session: park the group before its children"
            )));
        };
        let Some(spec) = g.call.group else {
            return Err(ParkError::Invalid(format!("{parent:?} is not a group")));
        };
        if g.status != StoredStatus::Parked {
            return Err(ParkError::Invalid(format!(
                "group {parent:?} is already resolved or closed"
            )));
        }
        Ok((ticket, spec))
    }

    /// `Some(other)`: two sessions hold one correlation, refused by design but let in by a crash
    /// between `create` and the claim.
    pub(crate) async fn reindex(&self, record: &Record) -> Result<Option<TicketId>> {
        self.store.index_session(&record.session, &record.ticket).await?;
        let holder = self.store.claim_correlation(&record.call.correlation, &record.ticket).await?;
        if let Some(parent) = &record.call.parent {
            let group = ticket_id(&record.session, parent);
            if let Some((g, _)) = self.store.get(&group).await? {
                if let Some(spec) = g.call.group {
                    self.store.index_child(&group, &record.ticket, spec.children).await?;
                }
            }
        }
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

        let (mut outcome, parent) = retry(|| async {
            let (record, rev) = self.store.get(&ticket).await?.ok_or_else(|| {
                ParkError::storage(format!(
                    "correlation index named {ticket}, which does not exist"
                ))
            })?;
            if record.call.group.is_some() {
                return Err(ParkError::Invalid(format!(
                    "{correlation:?} is a group: it is resolved by its children, not by wake"
                )));
            }
            let session = record.session.clone();
            let parent = record.call.parent.clone();
            let outcome = |freshly_woken| WakeOutcome {
                ticket: ticket.clone(),
                session: session.clone(),
                freshly_woken,
                announce: Vec::new(),
            };

            if !matches!(record.status, StoredStatus::Parked) {
                // A redelivered webhook is not an error (ADR-0100): accepted, nothing rewritten.
                return Ok(Some((outcome(false), parent)));
            }

            let mut next = record;
            next.status = StoredStatus::Ready;
            next.woken_at = Some(now_ms);
            next.result = Some(answer.clone());
            if self.store.cas(&ticket, rev, &next).await? {
                Ok(Some((outcome(true), parent)))
            } else {
                Ok(None)
            }
        })
        .await?;
        self.announce_for(
            &mut outcome.announce,
            &outcome.ticket,
            &outcome.session,
            &parent,
            outcome.freshly_woken,
            now_ms,
        )
        .await?;
        Ok(outcome)
    }

    /// The group's join runs even if this call did not move the child, so a retry after a crash
    /// heals it (ADR-0109).
    async fn announce_for(
        &self,
        announce: &mut Vec<WakeMessage>,
        ticket: &TicketId,
        session: &SessionId,
        parent: &Option<String>,
        moved: bool,
        now_ms: u64,
    ) -> Result<()> {
        match parent {
            Some(p) => {
                if let Some(g) = self.settle(&ticket_id(session, p), now_ms).await? {
                    announce.push(g.announcement());
                }
            }
            None if moved => announce.push(WakeMessage::resolved(session, ticket)),
            None => {}
        }
        Ok(())
    }

    /// Not tied to the deadline: whoever calls decides (ADR-0106).
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
        let (mut outcome, parent) = retry(|| async {
            let (record, rev) =
                self.store.get(ticket).await?.ok_or_else(|| ParkError::NotFound(ticket.clone()))?;
            if record.call.group.is_some() {
                return Err(ParkError::Invalid(
                    "a group is resolved by its children or by its deadline: cancel it to give it up"
                        .into(),
                ));
            }
            let session = record.session.clone();
            let parent = record.call.parent.clone();
            let outcome = |status, fresh| InterruptOutcome {
                ticket: ticket.clone(),
                session: session.clone(),
                status,
                fresh,
                announce: Vec::new(),
            };

            match record.status {
                StoredStatus::Resumed | StoredStatus::Cancelled => {
                    return Err(ParkError::AlreadyClosed(ticket.clone()))
                }
                StoredStatus::Ready => return Ok(Some((outcome(TurnStatus::Ready, false), parent))),
                StoredStatus::Interrupted => {
                    return Ok(Some((outcome(TurnStatus::Interrupted, false), parent)))
                }
                StoredStatus::Parked => {}
            }

            let mut next = record;
            next.status = StoredStatus::Interrupted;
            next.interruption =
                Some(Interruption { reason: reason.to_string(), at: now_ms, by: by.clone() });
            if self.store.cas(ticket, rev, &next).await? {
                Ok(Some((outcome(TurnStatus::Interrupted, true), parent)))
            } else {
                Ok(None)
            }
        })
        .await?;
        self.announce_for(
            &mut outcome.announce,
            &outcome.ticket,
            &outcome.session,
            &parent,
            outcome.fresh,
            now_ms,
        )
        .await?;
        Ok(outcome)
    }

    /// Leaves out tickets past their deadline: `to_entry` reads those as `Expired` (ADR-0100).
    pub async fn pending(&self, session: &SessionId, now_ms: u64) -> Result<Vec<TicketEntry>> {
        let mut entries = self.entries_of(session, now_ms).await?;
        entries.retain(|e| {
            matches!(e.status, TurnStatus::Parked | TurnStatus::Ready | TurnStatus::Interrupted)
        });
        Ok(entries)
    }

    /// Deliberately not idempotent: one result handed to two resumers is the bug this prevents.
    pub async fn take_ready(&self, ticket: &TicketId, now_ms: u64) -> Result<Resolution> {
        retry(|| async {
            let (record, rev) =
                self.store.get(ticket).await?.ok_or_else(|| ParkError::NotFound(ticket.clone()))?;
            refuse_a_child(&record)?;
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

    /// Does not consume, so a resumer that dies between delivering and closing redelivers
    /// instead of losing the answer.
    pub async fn resolution(&self, ticket: &TicketId) -> Result<Resolution> {
        let (record, _) =
            self.store.get(ticket).await?.ok_or_else(|| ParkError::NotFound(ticket.clone()))?;
        refuse_a_child(&record)?;
        resolution_of(&record)
    }

    pub async fn cancel(&self, ticket: &TicketId, by: Agent, now_ms: u64) -> Result<()> {
        self.cancel_announcing(ticket, by, now_ms).await.map(|_| ())
    }

    /// Bottom-up, so a crash leaves a live group a retry finishes, never a cancelled group with
    /// children still running.
    pub async fn cancel_announcing(
        &self,
        ticket: &TicketId,
        by: Agent,
        now_ms: u64,
    ) -> Result<Vec<WakeMessage>> {
        let (record, _) =
            self.store.get(ticket).await?.ok_or_else(|| ParkError::NotFound(ticket.clone()))?;
        if record.call.group.is_some() {
            for c in self.children_of(ticket).await? {
                self.cancel_open(&c.ticket, &by, now_ms).await?;
            }
        }
        let moved = self.cancel_ticket(ticket, &by, now_ms).await;
        let mut announce = Vec::new();
        if let Some(parent) = &record.call.parent {
            // Run even when the child was already cancelled: that is a retry of a call that died
            // before this step.
            if let Some(g) = self.settle(&ticket_id(&record.session, parent), now_ms).await? {
                announce.push(g.announcement());
            }
        }
        moved.map(|()| announce)
    }

    async fn cancel_ticket(&self, ticket: &TicketId, by: &Agent, now_ms: u64) -> Result<()> {
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

fn resolution_of(record: &Record) -> Result<Resolution> {
    match record.status {
        StoredStatus::Parked => Err(ParkError::NotFound(record.ticket.clone())),
        StoredStatus::Resumed | StoredStatus::Cancelled => {
            Err(ParkError::AlreadyClosed(record.ticket.clone()))
        }
        StoredStatus::Ready if record.joined.is_some() => {
            Ok(Resolution::Joined(record.joined.clone().expect("checked by the guard")))
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
        StoredStatus::Ready | StoredStatus::Interrupted => ParkOutcome::AlreadyWoken,
        StoredStatus::Parked | StoredStatus::Resumed | StoredStatus::Cancelled => {
            ParkOutcome::AlreadyParked
        }
    }
}

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
        parent: record.call.parent.as_ref().map(|p| ticket_id(&record.session, p)),
        group: record.call.group,
    }
}

pub(crate) fn check_shape(call: &OutboundCall) -> Result<()> {
    if call.group.is_some() && call.parent.is_some() {
        return Err(ParkError::Invalid(
            "a group cannot be a child of another: groups do not nest".into(),
        ));
    }
    if let Some(g) = &call.group {
        if g.children == 0 || g.children > MAX_GROUP_CHILDREN {
            return Err(ParkError::Invalid(format!(
                "a group takes 1 to {MAX_GROUP_CHILDREN} children, not {}",
                g.children
            )));
        }
    }
    if call.parent.as_deref() == Some(call.correlation.as_str()) {
        return Err(ParkError::Invalid("a call cannot be its own parent".into()));
    }
    Ok(())
}

fn describe_parent(p: &Option<String>) -> String {
    match p {
        Some(p) => format!("as a child of {p:?}"),
        None => "on its own".into(),
    }
}

/// A child is consumed by its group: handing its answer out here too would deliver it twice.
fn refuse_a_child(record: &Record) -> Result<()> {
    match &record.call.parent {
        Some(p) => Err(ParkError::Invalid(format!(
            "{:?} is a child of group {p:?}: take the group",
            record.call.correlation
        ))),
        None => Ok(()),
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// `f` returns `Ok(Some(_))` when done, `Ok(None)` after losing the CAS, `Err` to stop.
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
