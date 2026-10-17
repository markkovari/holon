//! `holon:park/types`, mirrored one to one (`wit/park/park.wit`).

use serde::{Deserialize, Serialize};

pub type SessionId = String;
pub type TicketId = String;

/// The same three fields as `holon:vcs/types.agent` — duplicated, not shared,
/// per ADR-0095's reasoning for why components duplicate small helpers rather
/// than depend on one another's crate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Agent {
    pub id: String,
    pub goal: Option<String>,
    pub model: Option<String>,
}

impl Agent {
    pub fn named(id: impl Into<String>) -> Self {
        Agent { id: id.into(), goal: None, model: None }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct PollSpec {
    pub url: String,
    pub interval_secs: u32,
}

/// What may be done about a call that was still outstanding when whatever made
/// it died (ADR-0106). The caller says so when it parks the call: only it knows
/// whether sending the call twice repeats an effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReplayPolicy {
    /// Sending it again is harmless: a read, or a write the far side
    /// deduplicates on `correlation`. Past its deadline it is re-dispatched, a
    /// bounded number of times.
    Safe,
    /// Sending it again might repeat an effect. Nothing here ever does; past its
    /// deadline the turn is told it was `interrupted`. The default: a caller
    /// that never thought about it gets the cautious one.
    #[default]
    Unsafe,
    /// A restarted turn has no use for the answer. Past its deadline the ticket
    /// is cancelled and nobody is told.
    Abandon,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct OutboundCall {
    /// What a webhook or poller presents to `wake` to find this ticket again.
    pub correlation: String,
    pub description: String,
    /// Unix milliseconds. Without one, nothing ever gives up on this call.
    pub deadline: Option<u64>,
    pub poll: Option<PollSpec>,
    /// Absent on the wire means [`ReplayPolicy::Unsafe`].
    #[serde(default)]
    pub replay: ReplayPolicy,
    /// The group this call is a child of, named by its `correlation` in the same
    /// session (ADR-0109). A child is settled by its own answer and consumed by
    /// its group: it is never announced, or taken, on its own.
    #[serde(default)]
    pub parent: Option<String>,
    /// Makes this call a group: resolved by its children, not by `wake`.
    #[serde(default)]
    pub group: Option<GroupSpec>,
}

/// How a group is resolved by its children (ADR-0109).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JoinPolicy {
    /// Wait for every child to settle, however each ends.
    AllSettled,
    /// Resolve as soon as one child fails; the children still outstanding are
    /// cancelled first, so the group never resolves with work still running.
    FailFast,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct GroupSpec {
    pub policy: JoinPolicy,
    /// How many children this group will have. Fewer cannot resolve an
    /// `all-settled` group; more are refused.
    pub children: u32,
}

/// The most children a group may be declared with.
pub const MAX_GROUP_CHILDREN: u32 = 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct CallResult {
    pub ok: bool,
    pub body: Vec<u8>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ParkOutcome {
    Parked,
    AlreadyParked,
    AlreadyWoken,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ParkResult {
    pub ticket: TicketId,
    pub outcome: ParkOutcome,
}

/// Why a call is given up on without an answer: its fate is unknown, and the
/// turn is told so rather than left to guess (ADR-0106).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Interruption {
    /// For the model, or the person reading what the model was told. Never parsed.
    pub reason: String,
    /// Unix milliseconds.
    pub at: u64,
    pub by: Agent,
}

/// Why a group resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JoinOutcome {
    /// Every child answered, and every answer was `ok`.
    AllAnswered,
    /// Every child settled, and at least one failed: an `ok: false` answer, an
    /// interruption, or a cancellation.
    SomeFailed,
    /// `fail-fast`: a child failed, and the ones still outstanding were cancelled.
    FailedFast,
    /// The group's own deadline passed first; the children still outstanding were
    /// cancelled and `missing` counts the ones never parked.
    TimedOut,
}

/// How one child of a group ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChildResult {
    Answered(CallResult),
    Interrupted(Interruption),
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ChildOutcome {
    pub ticket: TicketId,
    pub correlation: String,
    pub ended: ChildResult,
}

/// What a group is resolved with: every child, in the order it was parked, and how each ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct JoinResult {
    pub outcome: JoinOutcome,
    pub children: Vec<ChildOutcome>,
    /// Children the group was declared with that were never parked.
    pub missing: u32,
}

/// What `take-ready` hands over: the far side's answer, or the news that there
/// will not be one, or — for a group — how its children ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Resolution {
    Answered(CallResult),
    Interrupted(Interruption),
    Joined(JoinResult),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TurnStatus {
    Parked,
    Ready,
    Resumed,
    Cancelled,
    Expired,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct TicketEntry {
    pub ticket: TicketId,
    pub session: SessionId,
    pub status: TurnStatus,
    /// Unix milliseconds.
    pub parked_at: u64,
    pub woken_at: Option<u64>,
    pub resumed_at: Option<u64>,
    /// What the caller named the call: a ticket id is a hash, and this is how a
    /// restarted turn finds the call in its own journal.
    #[serde(default)]
    pub correlation: String,
    #[serde(default)]
    pub replay: ReplayPolicy,
    /// The current one: a re-dispatch pushes it out.
    #[serde(default)]
    pub deadline: Option<u64>,
    /// How many times a recovery pass has granted a re-dispatch.
    #[serde(default)]
    pub redispatches: u32,
    #[serde(default)]
    pub interrupted_at: Option<u64>,
    /// The group this ticket belongs to, as a ticket id.
    #[serde(default)]
    pub parent: Option<TicketId>,
    /// Set on a group: how it is resolved, and how many children it expects.
    #[serde(default)]
    pub group: Option<GroupSpec>,
}

/// What lands on `PARK_WAKE`: enough for a resumer to find the ticket; the
/// ticket record, not this message, is the source of truth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakeMessage {
    pub session: SessionId,
    pub ticket: TicketId,
    /// Absent on a message a pre-ADR-0106 `comp-park` published: those only
    /// ever announced an answer.
    #[serde(default)]
    pub kind: WakeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WakeKind {
    /// The ticket is `ready` or `interrupted`: something is waiting to be taken.
    #[default]
    Resolved,
    /// A recovery pass granted this re-dispatch (the `redispatches` count it
    /// saw). A message whose count no longer matches the ticket is stale.
    Redispatch { attempt: u32 },
}

impl WakeMessage {
    pub fn resolved(session: impl Into<String>, ticket: impl Into<String>) -> Self {
        WakeMessage { session: session.into(), ticket: ticket.into(), kind: WakeKind::Resolved }
    }

    pub fn redispatch(session: impl Into<String>, ticket: impl Into<String>, attempt: u32) -> Self {
        WakeMessage {
            session: session.into(),
            ticket: ticket.into(),
            kind: WakeKind::Redispatch { attempt },
        }
    }
}
