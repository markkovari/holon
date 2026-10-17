//! Commits (ADR-0110): several effects applied together, or not at all.
//!
//! A commit is a list of [`Effect`]s — `park`, `wake`, `interrupt`, `cancel` — each exactly what
//! the call of that name does. The reason to commit them rather than call them one at a time is
//! what happens when something goes wrong in the middle, and there are two ways it can:
//!
//! * **An effect is invalid.** A call parked in a list one at a time leaves the first two parked
//!   and tells you the third was refused. A commit validates every effect before it writes
//!   anything, and a list with a bad effect registers nothing.
//! * **The caller dies.** A fan-out parked one call at a time leaves a group waiting for children
//!   nobody will register. A commit writes its **intent** — the whole list, as one record —
//!   *before* it applies any effect. Once that exists the effects will all be applied or all
//!   undone, by the caller if it retries and by a recovery pass if it does not.
//!
//! ```text
//!   validate ──▶ write the intent (pending) ──▶ park, in order ──▶ wake / interrupt / cancel, in order ──▶ done
//!                                                  │
//!                                  a park refused after the intent was written
//!                                                  ▼
//!                                 cancel what this commit parked, last first ──▶ aborted
//! ```
//!
//! **Parks first.** A `park` is the one effect that can still be refused once the intent exists
//! (another session may take a correlation between validation and the write), and the one that
//! can be undone: a ticket the commit made is cancelled. `wake`, `interrupt` and `cancel` cannot
//! be undone, so they are applied only after every park has succeeded, and each is idempotent. A
//! commit therefore never has an irreversible effect behind it when it has to roll back.
//!
//! **What this is not.** It is atomic in what lasts — a commit ends done or aborted, whoever
//! drives it — and not in what is seen on the way: while it is pending, a reader may see some of
//! its tickets and not others. And it is atomic over park's own records only; a commit cannot
//! include a write to another store (an agent's transcript, a session document), because nothing
//! here can make two stores commit together.

use std::collections::HashMap;

use crate::engine::{check_shape, Engine};
use crate::error::{ParkError, Result};
use crate::group::join_actor;
use crate::model::{
    Agent, CommitResult, Effect, OutboundCall, ParkOutcome, ParkResult, SessionId, WakeMessage,
    MAX_BATCH_EFFECTS,
};
use crate::store::{Batch, BatchState, ParkStore, Revision};
use crate::ticket::{batch_id, ticket_id};

/// A commit that completed: what each park said, and what to announce on `PARK_WAKE` because of
/// its effects (a `wake`, `interrupt` or `cancel` of a child can resolve its group).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Committed {
    pub result: CommitResult,
    pub announce: Vec<WakeMessage>,
}

/// What driving a commit came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Driven {
    Completed(Committed),
    /// It could not be applied; what it had parked is cancelled. The reason is what was refused.
    Aborted(String),
}

/// A pending commit a recovery pass finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchRecovered {
    pub batch: String,
    pub session: SessionId,
    pub outcome: BatchOutcome,
    pub announce: Vec<WakeMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchOutcome {
    Completed,
    Aborted(String),
}

/// What a commit that already finished says when it is committed again: the parks it made, now
/// reported as already parked, as `park` reports a call it has seen.
fn replay_of(batch: &Batch) -> Committed {
    Committed {
        result: CommitResult {
            batch: batch.id.clone(),
            parked: batch
                .results
                .iter()
                .map(|r| ParkResult {
                    ticket: r.ticket.clone(),
                    outcome: ParkOutcome::AlreadyParked,
                })
                .collect(),
        },
        announce: Vec::new(),
    }
}

impl<S: ParkStore> Engine<S> {
    /// Apply `effects` together, or not at all (see the module doc).
    ///
    /// Idempotent: the identical list again is the same commit, and finds it done, or finishes it
    /// if a caller died part-way. `Err(Invalid)` before anything is written if any effect is
    /// invalid, and after the intent was written if one was refused in the meantime, in which
    /// case what the commit parked has been cancelled.
    pub async fn commit(
        &self,
        session: &SessionId,
        effects: Vec<Effect>,
        by: Agent,
        now_ms: u64,
    ) -> Result<Committed> {
        self.check_effects(session, &effects).await?;
        let id = batch_id(session, &effects);
        let batch = Batch {
            id: id.clone(),
            session: session.clone(),
            effects,
            by,
            state: BatchState::Pending,
            created_at: now_ms,
            finished_at: None,
            reason: None,
            results: Vec::new(),
        };
        // The intent first. From here the effects will all be applied or all undone, by this
        // call or by a pass. If it already exists this is a retry, and is driven from where it is.
        self.store.create_batch(&id, &batch).await?;
        let (batch, rev) = self.store.get_batch(&id).await?.ok_or_else(|| {
            ParkError::storage("create_batch said the commit existed, then get_batch found nothing")
        })?;
        match self.drive(batch, rev, now_ms).await? {
            Driven::Completed(c) => Ok(c),
            Driven::Aborted(why) => {
                Err(ParkError::Invalid(format!("the commit was aborted: {why}")))
            }
        }
    }

    /// Finish every commit that has been pending for at least `lease_ms`: one younger is presumed
    /// to be in the hands of the caller that wrote it. Applying is idempotent, so two drivers
    /// meeting on one commit is wasted work, not a fault; the lease only keeps it rare.
    pub async fn complete_batches(
        &self,
        lease_ms: u64,
        now_ms: u64,
    ) -> Result<Vec<BatchRecovered>> {
        let mut ids = self.store.list_batches().await?;
        ids.sort();
        let mut out = Vec::new();
        for id in ids {
            let Some((batch, rev)) = self.store.get_batch(&id).await? else { continue };
            if batch.state != BatchState::Pending
                || now_ms < batch.created_at.saturating_add(lease_ms)
            {
                continue;
            }
            let session = batch.session.clone();
            let (outcome, announce) = match self.drive(batch, rev, now_ms).await? {
                Driven::Completed(c) => (BatchOutcome::Completed, c.announce),
                Driven::Aborted(why) => (BatchOutcome::Aborted(why), Vec::new()),
            };
            out.push(BatchRecovered { batch: id, session, outcome, announce });
        }
        Ok(out)
    }

    /// Apply a pending commit, or report one already finished.
    async fn drive(&self, batch: Batch, rev: Revision, now_ms: u64) -> Result<Driven> {
        match batch.state {
            BatchState::Done => return Ok(Driven::Completed(replay_of(&batch))),
            BatchState::Aborted => {
                return Ok(Driven::Aborted(batch.reason.clone().unwrap_or_default()))
            }
            BatchState::Pending => {}
        }

        // Parks first: the effects that can still be refused, and that can be undone. Each is
        // stamped with this batch, so an abort cancels what this commit made and nothing else.
        let mut parked = Vec::new();
        for e in &batch.effects {
            let Effect::Park(call) = e else { continue };
            let call = call.clone();
            match self
                .park_in(&batch.session, call, batch.by.clone(), batch.created_at, Some(&batch.id))
                .await
            {
                Ok(r) => parked.push(r),
                Err(ParkError::Invalid(why)) => {
                    self.abort(&batch, rev, &why, now_ms).await?;
                    return Ok(Driven::Aborted(why));
                }
                Err(e) => return Err(e),
            }
        }

        // Then what cannot be undone, in the order listed. Each is idempotent, and a ticket that
        // is already closed is the effect already done.
        let mut announce = Vec::new();
        for e in &batch.effects {
            match e {
                Effect::Park(_) => {}
                Effect::Wake(w) => {
                    match self.wake(&w.correlation, w.answer.clone(), now_ms).await {
                        Ok(o) => announce.extend(o.announce),
                        Err(ParkError::AlreadyClosed(_)) => {}
                        Err(e) => return Err(e),
                    }
                }
                Effect::Interrupt(i) => {
                    let ticket = ticket_id(&batch.session, &i.correlation);
                    match self.interrupt(&ticket, &i.reason, batch.by.clone(), now_ms).await {
                        Ok(o) => announce.extend(o.announce),
                        Err(ParkError::AlreadyClosed(_)) => {}
                        Err(e) => return Err(e),
                    }
                }
                Effect::Cancel(correlation) => {
                    let ticket = ticket_id(&batch.session, correlation);
                    match self.cancel_announcing(&ticket, batch.by.clone(), now_ms).await {
                        Ok(a) => announce.extend(a),
                        Err(ParkError::AlreadyClosed(_)) => {}
                        Err(e) => return Err(e),
                    }
                }
            }
        }

        let mut done = batch.clone();
        done.state = BatchState::Done;
        done.finished_at = Some(now_ms);
        done.results = parked.clone();
        if self.store.cas_batch(&batch.id, rev, &done).await? {
            return Ok(Driven::Completed(Committed {
                result: CommitResult { batch: batch.id, parked },
                announce,
            }));
        }
        // Another driver moved it first. Report what is there.
        let (current, _) = self
            .store
            .get_batch(&batch.id)
            .await?
            .ok_or_else(|| ParkError::storage("a commit's intent vanished"))?;
        match current.state {
            BatchState::Done => Ok(Driven::Completed(replay_of(&current))),
            BatchState::Aborted => Ok(Driven::Aborted(current.reason.unwrap_or_default())),
            BatchState::Pending => {
                Err(ParkError::storage("the commit changed while it was being applied: retry it"))
            }
        }
    }

    /// Undo a commit: cancel what it parked, last first so a group's children go before the
    /// group, and mark it aborted. Only tickets stamped with this batch: one that was already
    /// there before the commit is not the commit's to cancel.
    async fn abort(&self, batch: &Batch, rev: Revision, why: &str, now_ms: u64) -> Result<()> {
        for e in batch.effects.iter().rev() {
            let Effect::Park(call) = e else { continue };
            let ticket = ticket_id(&batch.session, &call.correlation);
            if let Some((record, _)) = self.store.get(&ticket).await? {
                if record.batch.as_deref() == Some(batch.id.as_str()) {
                    self.cancel_open(&ticket, &join_actor(), now_ms).await?;
                }
            }
        }
        let mut aborted = batch.clone();
        aborted.state = BatchState::Aborted;
        aborted.finished_at = Some(now_ms);
        aborted.reason = Some(why.to_string());
        // Losing this race means another driver aborted it first: the same result.
        self.store.cas_batch(&batch.id, rev, &aborted).await?;
        Ok(())
    }

    /// Everything about a list of effects that can be checked before anything is written. A list
    /// that passes can still meet a race — another session taking a correlation in the meantime —
    /// and that is what an abort is for.
    async fn check_effects(&self, session: &SessionId, effects: &[Effect]) -> Result<()> {
        let bad = |m: String| Err(ParkError::Invalid(m));
        if effects.is_empty() {
            return bad("a commit needs at least one effect".into());
        }
        if effects.len() > MAX_BATCH_EFFECTS {
            return bad(format!(
                "a commit takes at most {MAX_BATCH_EFFECTS} effects, not {}",
                effects.len()
            ));
        }

        // What the commit itself parks, by correlation.
        let mut parks: HashMap<&str, &OutboundCall> = HashMap::new();
        for e in effects {
            if let Effect::Park(call) = e {
                check_shape(call)?;
                if parks.insert(&call.correlation, call).is_some() {
                    return bad(format!("{:?} is parked twice in one commit", call.correlation));
                }
            }
        }

        // Children added to each group by this commit, to hold against the group's size.
        let mut adding: HashMap<&str, Vec<crate::model::TicketId>> = HashMap::new();
        for (i, e) in effects.iter().enumerate() {
            match e {
                Effect::Park(call) => {
                    let ticket = ticket_id(session, &call.correlation);
                    if let Some(holder) = self.store.find_correlation(&call.correlation).await? {
                        if holder != ticket {
                            return bad(format!(
                                "correlation {:?} is already in use by ticket {holder}",
                                call.correlation
                            ));
                        }
                    }
                    if let Some(parent) = &call.parent {
                        // The group is parked earlier in this list, or is already there and open.
                        let in_batch = effects[..i].iter().any(|x| {
                            matches!(x, Effect::Park(p) if &p.correlation == parent && p.group.is_some())
                        });
                        if !in_batch {
                            if effects[i + 1..]
                                .iter()
                                .any(|x| matches!(x, Effect::Park(p) if &p.correlation == parent))
                            {
                                return bad(format!(
                                    "group {parent:?} must be parked before its children in a commit"
                                ));
                            }
                            self.open_group(session, parent).await?;
                        }
                        adding.entry(parent.as_str()).or_default().push(ticket);
                    }
                }
                Effect::Wake(w) => self.check_target(session, &parks, &w.correlation, true).await?,
                Effect::Interrupt(x) => {
                    self.check_target(session, &parks, &x.correlation, true).await?
                }
                Effect::Cancel(correlation) => {
                    self.check_target(session, &parks, correlation, false).await?
                }
            }
        }

        // A group has the places it was declared with, however many a commit tries to fill.
        for (parent, children) in adding {
            let group = ticket_id(session, parent);
            let (declared, taken) = match parks.get(parent) {
                Some(p) if p.group.is_some() => (p.group.map_or(0, |g| g.children), Vec::new()),
                _ => {
                    let (_, spec) = self.open_group(session, parent).await?;
                    (spec.children, self.store.list_children(&group).await?)
                }
            };
            let new = children.iter().filter(|t| !taken.contains(t)).count();
            if taken.len() + new > declared as usize {
                return bad(format!(
                    "group {parent:?} was declared with {declared} children and this would make {}",
                    taken.len() + new
                ));
            }
        }
        Ok(())
    }

    /// A call a commit acts on must be one the commit parks or one that already exists, and
    /// `wake` and `interrupt` do not act on a group (it is resolved by its children).
    async fn check_target(
        &self,
        session: &SessionId,
        parks: &HashMap<&str, &OutboundCall>,
        correlation: &str,
        refuses_groups: bool,
    ) -> Result<()> {
        let is_group = match parks.get(correlation) {
            Some(call) => call.group.is_some(),
            None => match self.store.get(&ticket_id(session, correlation)).await? {
                Some((record, _)) => record.call.group.is_some(),
                None => {
                    return Err(ParkError::Invalid(format!(
                        "no call {correlation:?} in this session, and this commit does not park one"
                    )))
                }
            },
        };
        if is_group && refuses_groups {
            return Err(ParkError::Invalid(format!(
                "{correlation:?} is a group: it is resolved by its children, cancel it to give it up"
            )));
        }
        Ok(())
    }
}
