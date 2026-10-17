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

/// How far an approval reaches (ADR-0114). It is for one *tool*, not one call's input: approving
/// `run` for the session approves every command.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalScope {
    /// This call, and the next one of the tool asks again. What a client that does not say gets.
    #[default]
    Once,
    /// This call, and the rest of this task's calls of the tool.
    Task,
    /// This call, and every later call of the tool in this session, whatever task it is in.
    Session,
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
    /// A task was accepted onto a busy session with `when_busy: queue` and waits its turn
    /// (ADR-0112). `task_started` follows when it becomes the running task; if it never does,
    /// `task_failed` says how it ended.
    TaskQueued {
        prompt: String,
    },
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
        /// How far an approval reaches (ADR-0114). A log written before it has none: `once`.
        #[serde(default)]
        scope: ApprovalScope,
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
            Payload::TaskQueued { .. } => "task_queued",
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
    /// Accepted onto a busy session and not started (ADR-0112).
    Queued,
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
    /// Tools a person approved beyond one call (ADR-0114): for the session, or, while that task
    /// runs, for the task. Apart from `auto_approve_tools`, which is what the session was created
    /// with and is not changed by anyone's answer.
    pub approved_tools: Vec<String>,
    pub labels: std::collections::BTreeMap<String, String>,
    pub created_at: String,
    pub active_task_id: Option<String>,
}

/// What `SendTask` does when the session already has a task running.
#[derive(Deserialize, Clone, Copy, Debug, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WhenBusy {
    /// Refuse it: `409 task-running`. What a client that does not say gets.
    #[default]
    Reject,
    /// Accept it, to start when the ones before it have ended (ADR-0112).
    Queue,
}

#[derive(Deserialize)]
pub struct SendTask {
    pub prompt: String,
    #[serde(default)]
    pub when_busy: WhenBusy,
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
    /// How far an approval reaches. Only an approval has a reach: a denial is for this call.
    #[serde(default)]
    pub scope: ApprovalScope,
}

#[derive(Serialize)]
pub struct ApprovalAnswer {
    pub call_id: String,
    pub decision: Decision,
    /// What this daemon understood: a server that does not know scopes would not say it.
    pub scope: ApprovalScope,
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

/// A task accepted and not yet started, in the order it will start.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct QueuedTask {
    pub task_id: String,
    pub prompt: String,
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
    pub queued_tasks: Vec<QueuedTask>,
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
