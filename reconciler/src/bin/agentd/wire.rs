//! The JSON shapes of `api/openapi.yaml`, field for field. Nothing here
//! decides anything; `session.rs` and `agent.rs` do.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Token counts and their price. Per call (`delta`) or a running total.
#[derive(Serialize, Deserialize, Clone, Copy, Default, Debug, PartialEq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub cost_usd_micros: i64,
}

impl Usage {
    pub fn add(&mut self, o: &Usage) {
        self.input_tokens += o.input_tokens;
        self.output_tokens += o.output_tokens;
        self.cache_read_tokens += o.cache_read_tokens;
        self.cache_write_tokens += o.cache_write_tokens;
        self.cost_usd_micros += o.cost_usd_micros;
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Approve,
    Deny,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FailCode {
    BudgetExceeded,
    Cancelled,
    ModelError,
    MaxTurns,
    Internal,
}

/// The payload of one event; `type` is its tag and the SSE `event:` name.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Payload {
    TaskStarted {
        prompt: String,
    },
    TextDelta {
        message_id: String,
        text: String,
    },
    ToolCallStarted {
        call_id: String,
        tool_name: String,
        input: Value,
    },
    ToolApprovalRequired {
        call_id: String,
        tool_name: String,
        input: Value,
        expires_at: Option<String>,
    },
    ToolApprovalResolved {
        call_id: String,
        decision: Decision,
        decided_by: String,
    },
    ToolCallFinished {
        call_id: String,
        output: String,
        is_error: bool,
        duration_ms: u64,
    },
    UsageUpdated {
        delta: Usage,
        session_total: Usage,
        budget_remaining_usd_micros: i64,
    },
    TaskCompleted {
        result: String,
        task_usage: Usage,
    },
    TaskFailed {
        code: FailCode,
        message: String,
        task_usage: Usage,
    },
    /// A caller steered the running task (ADR-0107). It joins the conversation
    /// at the task's next model-call boundary, as a user message.
    TaskSteerQueued {
        steer_id: String,
        message: String,
    },
    /// The steer `steer_id` joined the conversation: the next model call sees it.
    TaskSteered {
        steer_id: String,
    },
}

impl Payload {
    pub fn kind(&self) -> &'static str {
        match self {
            Payload::TaskStarted { .. } => "task_started",
            Payload::TextDelta { .. } => "text_delta",
            Payload::ToolCallStarted { .. } => "tool_call_started",
            Payload::ToolApprovalRequired { .. } => "tool_approval_required",
            Payload::ToolApprovalResolved { .. } => "tool_approval_resolved",
            Payload::ToolCallFinished { .. } => "tool_call_finished",
            Payload::UsageUpdated { .. } => "usage_updated",
            Payload::TaskCompleted { .. } => "task_completed",
            Payload::TaskFailed { .. } => "task_failed",
            Payload::TaskSteerQueued { .. } => "task_steer_queued",
            Payload::TaskSteered { .. } => "task_steered",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Event {
    pub seq: u64,
    pub session_id: String,
    pub task_id: String,
    pub time: String,
    #[serde(flatten)]
    pub payload: Payload,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Idle,
    Running,
    AwaitingApproval,
    Closed,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Running,
    Completed,
    Failed,
}

#[derive(Deserialize)]
pub struct CreateSession {
    pub model: String,
    pub workspace_dir: String,
    pub max_budget_usd_micros: i64,
    #[serde(default)]
    pub auto_approve_tools: Vec<String>,
    #[serde(default)]
    pub labels: std::collections::BTreeMap<String, String>,
}

#[derive(Serialize, Clone)]
pub struct Session {
    pub id: String,
    pub model: String,
    pub workspace_dir: String,
    pub status: SessionStatus,
    pub usage: Usage,
    pub max_budget_usd_micros: i64,
    pub auto_approve_tools: Vec<String>,
    pub labels: std::collections::BTreeMap<String, String>,
    pub created_at: String,
    pub active_task_id: Option<String>,
}

#[derive(Deserialize)]
pub struct SendTask {
    pub prompt: String,
}

#[derive(Deserialize)]
pub struct SteerTask {
    pub message: String,
}

#[derive(Serialize, Clone)]
pub struct Task {
    pub id: String,
    pub session_id: String,
    pub status: TaskStatus,
    pub created_at: String,
}

#[derive(Deserialize)]
pub struct SubmitApproval {
    pub decision: Decision,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub approver: String,
}

#[derive(Serialize)]
pub struct ApprovalAnswer {
    pub call_id: String,
    pub decision: Decision,
}

// ---- The state of a session, as one reads it to catch up (ADR-0107) ----

/// Where a tool call is.
#[derive(Serialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CallStatus {
    /// Waiting for `SubmitApproval`.
    AwaitingApproval,
    /// Approved or auto-approved, and executing.
    Running,
    /// Returned, or was denied; `output` says which.
    Finished,
    /// The task ended while the call was still waiting or running.
    Cancelled,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Completed,
    Failed,
}

/// One line of the conversation as a client renders it, folded from the events
/// that made it. `type` is its tag.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Entry {
    /// A prompt, or a steer that joined the conversation.
    UserMessage { text: String, steered: bool },
    /// What the model said in one turn. `in_progress` while its deltas are
    /// still arriving: more `text_delta` events for `message_id` follow.
    AssistantMessage { message_id: String, text: String, in_progress: bool },
    ToolCall {
        call_id: String,
        tool_name: String,
        input: Value,
        status: CallStatus,
        decision: Option<Decision>,
        decided_by: Option<String>,
        expires_at: Option<String>,
        output: Option<String>,
        is_error: Option<bool>,
        duration_ms: Option<u64>,
    },
    TaskEnded {
        outcome: Outcome,
        result: Option<String>,
        code: Option<FailCode>,
        message: Option<String>,
        usage: Usage,
    },
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct TranscriptEntry {
    /// The `seq` of the event that began this entry.
    pub seq: u64,
    pub task_id: String,
    #[serde(flatten)]
    pub entry: Entry,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct PendingApproval {
    pub task_id: String,
    pub call_id: String,
    pub tool_name: String,
    pub input: Value,
    pub expires_at: Option<String>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct PendingSteer {
    pub task_id: String,
    pub steer_id: String,
    pub message: String,
}

/// Everything a client needs to draw a session it was not watching, and the
/// cursor to keep watching it from: `seq` is the last event this reflects, so
/// the stream resumed with `after_seq = seq` has no gap and no repeat.
#[derive(Serialize, Clone)]
pub struct SessionState {
    pub session: Session,
    pub seq: u64,
    pub transcript: Vec<TranscriptEntry>,
    pub pending_approvals: Vec<PendingApproval>,
    pub pending_steers: Vec<PendingSteer>,
}

/// RFC 9457 problem details.
#[derive(Serialize)]
pub struct Problem {
    #[serde(rename = "type")]
    pub kind: String,
    pub title: String,
    pub status: u16,
    pub detail: String,
}

pub fn now() -> String {
    jiff::Timestamp::now().to_string()
}

/// A random id with a readable prefix, e.g. `ses_3f9a…`.
pub fn new_id(prefix: &str) -> String {
    let mut r = [0u8; 12];
    getrandom::getrandom(&mut r).expect("OS randomness");
    format!("{prefix}_{}", hex::encode(r))
}
