//! The one storage primitive [`Engine`](crate::engine::Engine) is written
//! against, and the record it persists.
//!
//! One ticket is one mutable record moved by compare-and-set, plus two small
//! indexes (`by session`, `by correlation`); no graph or oplog rebuild, so no
//! `holon-vcs::store`-style split (ADR-0100).

use std::future::Future;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::model::{
    Agent, CallResult, Effect, Interruption, JoinResult, OutboundCall, ParkResult, SessionId,
    TicketId, TurnStatus,
};

/// [`TurnStatus`] without `expired`, which is only computed at read time from a
/// `parked` record's `call.deadline`; keeping it out of this type means a match
/// cannot forget that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StoredStatus {
    Parked,
    Ready,
    /// Given up on without an answer (ADR-0106). Taken exactly like `Ready`.
    Interrupted,
    Resumed,
    Cancelled,
}

impl From<StoredStatus> for TurnStatus {
    fn from(s: StoredStatus) -> Self {
        match s {
            StoredStatus::Parked => TurnStatus::Parked,
            StoredStatus::Ready => TurnStatus::Ready,
            StoredStatus::Interrupted => TurnStatus::Interrupted,
            StoredStatus::Resumed => TurnStatus::Resumed,
            StoredStatus::Cancelled => TurnStatus::Cancelled,
        }
    }
}

/// A record's revision. Opaque: compared for equality only.
pub type Revision = u64;

/// A compare-and-set that did not land: the record is not at the expected
/// revision. `current` is the revision it is at now (`None`: gone, which
/// cannot happen here — nothing deletes a ticket record).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CasMismatch {
    pub current: Option<Revision>,
}

/// The full state of one ticket, as stored. [`crate::model::TicketEntry`] is
/// the public, WIT-shaped projection of this — `by` and `cancelled_by` are
/// provenance nobody outside this crate needs, and `result` is only handed
/// out once, by `take-ready`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub ticket: TicketId,
    pub session: SessionId,
    pub call: OutboundCall,
    pub by: Agent,
    pub status: StoredStatus,
    pub parked_at: u64,
    pub woken_at: Option<u64>,
    pub resumed_at: Option<u64>,
    pub cancelled_by: Option<Agent>,
    pub cancelled_at: Option<u64>,
    pub result: Option<CallResult>,
    // Written by ADR-0106. A record from before it reads these as their
    // defaults, which is what "nothing has happened to this ticket" means.
    /// Re-dispatches a recovery pass has granted so far.
    #[serde(default)]
    pub redispatches: u32,
    /// Set exactly when `status` is `Interrupted`.
    #[serde(default)]
    pub interruption: Option<Interruption>,
    /// When a recovery pass last re-announced this ticket on `PARK_WAKE`
    /// because its answer sat untaken; keeps a down sink from being re-announced
    /// on every pass.
    #[serde(default)]
    pub announced_at: Option<u64>,
    /// What a group was resolved with (ADR-0109). Set exactly when a group is
    /// `Ready`, or `Resumed` after being `Ready`; a group has no `result`.
    #[serde(default)]
    pub joined: Option<JoinResult>,
    /// The commit that created this ticket, if one did (ADR-0110). Stamped with the record, so
    /// rolling a commit back cancels the tickets it made and never one that was already there.
    #[serde(default)]
    pub batch: Option<String>,
}

/// Where a commit is (ADR-0110).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BatchState {
    /// The intent is written; its effects are being applied, or will be by a recovery pass.
    Pending,
    Done,
    /// It could not be applied, and what it had parked was cancelled.
    Aborted,
}

/// A commit's durable intent: written before any effect is applied, so that once it exists the
/// effects will all be applied or all be undone, whoever finds it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Batch {
    /// Content-addressed (`ticket::batch_id`): the identical commit again is the same batch.
    pub id: String,
    pub session: SessionId,
    pub effects: Vec<Effect>,
    pub by: Agent,
    pub state: BatchState,
    pub created_at: u64,
    pub finished_at: Option<u64>,
    /// Why it was aborted.
    pub reason: Option<String>,
    /// What each `park` said, in order. Set when it is done.
    #[serde(default)]
    pub results: Vec<ParkResult>,
}

/// What [`ParkStore::index_child`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexChild {
    Added,
    /// Already listed: the identical park, retried.
    Present,
    /// The group already has as many children as it was declared with.
    Full,
}

/// What the engine needs from storage: one CAS'd record per ticket, and two
/// indexes a lookup needs — `park` and `wake` never scan.
pub trait ParkStore: Send + Sync {
    /// The record and its revision, or `None`. Never cached: a CAS is computed
    /// from this read.
    fn get(&self, ticket: &str) -> impl Future<Output = Result<Option<(Record, Revision)>>> + Send;

    /// Write a BRAND NEW record. `Ok(true)`: this call created it. `Ok(false)`:
    /// one already existed (by this ticket, so an identical `session` and
    /// `call.correlation` — the caller re-reads it with [`Self::get`]).
    fn create(&self, ticket: &str, record: &Record) -> impl Future<Output = Result<bool>> + Send;

    /// Overwrite an EXISTING record, only if it is still at `expected`.
    /// `Ok(true)`: written. `Ok(false)`: lost the race; the caller re-reads.
    fn cas(
        &self,
        ticket: &str,
        expected: Revision,
        record: &Record,
    ) -> impl Future<Output = Result<bool>> + Send;

    /// Claim `correlation` for `ticket`, if nothing already has it.
    /// `Ok(None)`: this call claimed it. `Ok(Some(other))`: `other` already
    /// holds it — which, since a ticket id is `hash(session, correlation)`, can
    /// only mean two different sessions tried to use the same `correlation`
    /// (a caller's bug: `wake` would not know which session to answer).
    fn claim_correlation(
        &self,
        correlation: &str,
        ticket: &str,
    ) -> impl Future<Output = Result<Option<TicketId>>> + Send;

    /// The ticket `correlation` was claimed for, if any.
    fn find_correlation(
        &self,
        correlation: &str,
    ) -> impl Future<Output = Result<Option<TicketId>>> + Send;

    /// Add `ticket` to `session`'s index. Idempotent.
    fn index_session(&self, session: &str, ticket: &str)
        -> impl Future<Output = Result<()>> + Send;

    /// Every ticket ever parked for `session`, in no particular order — the
    /// engine sorts by `parked_at` after reading each one.
    fn list_session(&self, session: &str) -> impl Future<Output = Result<Vec<TicketId>>> + Send;

    /// Every ticket in the store, any session, any state, in no particular
    /// order. Only a recovery pass reads this, and nothing deletes a ticket, so
    /// it grows with every call ever parked.
    // ponytail: a full scan per pass. Fine while tickets number in the
    // thousands; an index of the still-open ones (or of deadlines) is the
    // change if a bucket ever holds millions.
    fn list_tickets(&self) -> impl Future<Output = Result<Vec<TicketId>>> + Send;

    /// Add `child` to `parent`'s list, unless it already has `max` children.
    /// The bound is checked and the list written as one step, so two children
    /// racing for the last place cannot both take it.
    fn index_child(
        &self,
        parent: &str,
        child: &str,
        max: u32,
    ) -> impl Future<Output = Result<IndexChild>> + Send;

    /// Every child indexed under `parent`, in the order they were added.
    fn list_children(&self, parent: &str) -> impl Future<Output = Result<Vec<TicketId>>> + Send;

    /// Write a BRAND NEW commit intent. `Ok(false)`: that commit already exists.
    fn create_batch(&self, id: &str, batch: &Batch) -> impl Future<Output = Result<bool>> + Send;

    fn get_batch(&self, id: &str)
        -> impl Future<Output = Result<Option<(Batch, Revision)>>> + Send;

    /// Overwrite an existing intent, only if it is still at `expected`.
    fn cas_batch(
        &self,
        id: &str,
        expected: Revision,
        batch: &Batch,
    ) -> impl Future<Output = Result<bool>> + Send;

    /// Every commit ever written, in no particular order.
    fn list_batches(&self) -> impl Future<Output = Result<Vec<String>>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ReplayPolicy;

    /// What a bucket written before ADR-0106 holds. Reading it must mean "nothing
    /// has happened to this ticket", or deploying the new daemon over an old
    /// bucket strands every outstanding call.
    #[test]
    fn a_record_written_before_adr_0106_reads_as_untouched() {
        let old = r#"{
            "ticket": "t", "session": "s",
            "call": {"correlation": "c", "description": "d", "deadline": 5, "poll": null},
            "by": {"id": "a", "goal": null, "model": null},
            "status": "ready",
            "parked_at": 1, "woken_at": 2, "resumed_at": null,
            "cancelled_by": null, "cancelled_at": null,
            "result": {"ok": true, "body": [1], "detail": null}
        }"#;
        let r: Record = serde_json::from_str(old).unwrap();
        assert_eq!(r.status, StoredStatus::Ready);
        assert_eq!(r.call.replay, ReplayPolicy::Unsafe);
        assert_eq!((r.redispatches, r.interruption, r.announced_at), (0, None, None));
        assert_eq!(r.result.unwrap().body, [1]);
    }
}
