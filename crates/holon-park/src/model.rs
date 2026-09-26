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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct OutboundCall {
    /// What a webhook or poller presents to `wake` to find this ticket again.
    pub correlation: String,
    pub description: String,
    /// Unix milliseconds.
    pub deadline: Option<u64>,
    pub poll: Option<PollSpec>,
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TurnStatus {
    Parked,
    Ready,
    Resumed,
    Cancelled,
    Expired,
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
}
