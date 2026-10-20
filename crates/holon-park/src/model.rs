//! `holon:park/types`, mirrored one to one (`wit/park/park.wit`).

use serde::{Deserialize, Serialize};

pub type SessionId = String;
pub type TicketId = String;

/// Duplicates `holon:vcs/types.agent` rather than sharing a crate (ADR-0095).
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

/// Chosen by the caller: only it knows whether sending the call twice repeats an effect (ADR-0106).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReplayPolicy {
    /// Past its deadline it is re-dispatched, a bounded number of times.
    Safe,
    /// Never re-sent: past its deadline the turn is told it was `interrupted`.
    #[default]
    Unsafe,
    /// Past its deadline the ticket is cancelled and nobody is told.
    Abandon,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct OutboundCall {
    /// What a webhook or poller presents to `wake` to find this ticket again.
    pub correlation: String,
    pub description: String,
    /// Unix milliseconds; without one, nothing ever gives up on this call.
    pub deadline: Option<u64>,
    pub poll: Option<PollSpec>,
    #[serde(default)]
    pub replay: ReplayPolicy,
    /// The group's `correlation` (ADR-0109); a child is never announced, or taken, on its own.
    #[serde(default)]
    pub parent: Option<String>,
    /// Makes this call a group: resolved by its children, not by `wake`.
    #[serde(default)]
    pub group: Option<GroupSpec>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JoinPolicy {
    AllSettled,
    /// Outstanding children are cancelled first: a group never resolves with work still running.
    FailFast,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct GroupSpec {
    pub policy: JoinPolicy,
    /// Fewer cannot resolve an `all-settled` group; more are refused.
    pub children: u32,
}

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Interruption {
    /// For the model and its reader; never parsed.
    pub reason: String,
    /// Unix milliseconds.
    pub at: u64,
    pub by: Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JoinOutcome {
    AllAnswered,
    /// At least one child failed: an `ok: false` answer, an interruption, or a cancellation.
    SomeFailed,
    FailedFast,
    TimedOut,
}

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

/// `children` are in the order they were parked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct JoinResult {
    pub outcome: JoinOutcome,
    pub children: Vec<ChildOutcome>,
    /// Children the group was declared with that were never parked.
    pub missing: u32,
}

/// One thing a commit does (ADR-0110), as the call of the same name would.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Effect {
    Park(OutboundCall),
    Wake(WakeEffect),
    Interrupt(InterruptEffect),
    /// The call's `correlation`.
    Cancel(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct WakeEffect {
    pub correlation: String,
    pub answer: CallResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct InterruptEffect {
    pub correlation: String,
    pub reason: String,
}

/// `parked` is in the order the `park` effects were listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct CommitResult {
    pub batch: String,
    pub parked: Vec<ParkResult>,
}

/// Bounded because the intent is one record, read whole.
pub const MAX_BATCH_EFFECTS: usize = 256;

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
    /// A ticket id is a hash; this is how a restarted turn finds the call in its own journal.
    #[serde(default)]
    pub correlation: String,
    #[serde(default)]
    pub replay: ReplayPolicy,
    /// The current one: a re-dispatch pushes it out.
    #[serde(default)]
    pub deadline: Option<u64>,
    #[serde(default)]
    pub redispatches: u32,
    #[serde(default)]
    pub interrupted_at: Option<u64>,
    #[serde(default)]
    pub parent: Option<TicketId>,
    #[serde(default)]
    pub group: Option<GroupSpec>,
}

/// The ticket record, not this message, is the source of truth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakeMessage {
    pub session: SessionId,
    pub ticket: TicketId,
    /// Absent from pre-ADR-0106 publishers, which only ever announced an answer.
    #[serde(default)]
    pub kind: WakeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WakeKind {
    /// The ticket is `ready` or `interrupted`.
    #[default]
    Resolved,
    /// `attempt` is the `redispatches` count; a message whose count no longer matches is stale.
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
