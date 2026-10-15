//! The one storage primitive [`Engine`](crate::engine::Engine) is written
//! against, and the record it persists.
//!
//! One ticket is one mutable record moved by compare-and-set, plus two small
//! indexes (`by session`, `by correlation`); no graph or oplog rebuild, so no
//! `holon-vcs::store`-style split (ADR-0100).

use std::future::Future;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::model::{Agent, CallResult, OutboundCall, SessionId, TicketId, TurnStatus};

/// [`TurnStatus`] without `expired`, which is only computed at read time from a
/// `parked` record's `call.deadline`; keeping it out of this type means a match
/// cannot forget that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StoredStatus {
    Parked,
    Ready,
    Resumed,
    Cancelled,
}

impl From<StoredStatus> for TurnStatus {
    fn from(s: StoredStatus) -> Self {
        match s {
            StoredStatus::Parked => TurnStatus::Parked,
            StoredStatus::Ready => TurnStatus::Ready,
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
}
