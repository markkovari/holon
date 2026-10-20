//! agent-runtime's events and run records as plain data, so the board can be built for wasm.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Step {
    /// The model's raw reply for one turn.
    Model {
        text: String,
        /// Span id and wall-clock bounds (unix ms) of this model call, and what
        /// it cost — enough to export it as an OpenTelemetry `chat` span.
        #[serde(default)]
        span_id: String,
        #[serde(default)]
        t0: u64,
        #[serde(default)]
        t1: u64,
        #[serde(default)]
        tokens_in: u64,
        #[serde(default)]
        tokens_out: u64,
    },
    /// A tool the agent called. `approved` is `None` when none was needed.
    Tool {
        name: String,
        args: serde_json::Value,
        result: String,
        error: bool,
        approved: Option<bool>,
        /// Anything this call wakes (an event, a store write, a delegated
        /// agent) has this as its parent span.
        #[serde(default)]
        span_id: String,
        #[serde(default)]
        t0: u64,
        #[serde(default)]
        t1: u64,
    },
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    Failed,
    /// Stopped for spending more than `max_tokens`, or the daily budget.
    OverBudget,
    /// Refused before it started: a loop, too many hops, a rate limit or an
    /// open circuit. Logged so a dropped wake-up is visible, not silent.
    Dropped,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RunRecord {
    pub id: String,
    pub agent: String,
    /// `http`, `schedule: <cron>`, `event: <topic>`, `agent: <caller>`, `ui`.
    pub trigger: String,
    pub input: String,
    pub started: u64,
    pub finished: u64,
    pub status: Status,
    pub answer: String,
    pub steps: Vec<Step>,
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// This run's wall-clock bounds in unix ms (`started`/`finished` are seconds).
    #[serde(default)]
    pub started_ms: u64,
    #[serde(default)]
    pub finished_ms: u64,
    /// `provider/model` this run's agent used, e.g. `anthropic/claude-haiku-4-5`
    /// or `local/system` — what the `chat` spans are attributed to.
    #[serde(default)]
    pub model: String,
    /// W3C trace id (32 hex). Every run one original trigger set off, across
    /// agents, shares it.
    #[serde(default)]
    pub trace_id: String,
    /// This run's span (16 hex).
    #[serde(default)]
    pub span_id: String,
    /// The span that woke or called this one — usually a tool call in another
    /// agent's run, or the upstream caller's span from a `traceparent` header.
    #[serde(default)]
    pub parent_span_id: Option<String>,
    #[serde(default)]
    pub hops: u32,
    /// `agent|topic` for each wake-up that led here, oldest first.
    #[serde(default)]
    pub chain: Vec<String>,
    /// The project this run was for: a schedule's, a reminder's, or the room it was talked
    /// to in. Empty for none; a chat bridge uses it to say where the result belongs.
    #[serde(default)]
    pub project: String,
    /// Photos that came with it, by file name in the agent's inbox.
    #[serde(default)]
    pub attachments: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Pending {
    pub id: u64,
    pub agent: String,
    pub tool: String,
    pub args: Value,
    /// What led to this request (`agent|topic` per wake-up), oldest first.
    pub chain: Vec<String>,
}

/// How a session stands, in the words a person watching it would use.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Working: the model is writing, or a tool is running.
    Running,
    /// Waiting for a person to approve a tool call.
    NeedsYou,
    /// The task ended with an answer.
    Done,
    /// The task ended without one: out of money or turns, a model error.
    Failed,
    /// Stopped on purpose, by the agent or a person.
    Cancelled,
}

impl Phase {
    /// No more changes will come.
    pub fn is_over(self) -> bool {
        matches!(self, Phase::Done | Phase::Failed | Phase::Cancelled)
    }

    pub fn label(self) -> &'static str {
        match self {
            Phase::Running => "running",
            Phase::NeedsYou => "needs-you",
            Phase::Done => "done",
            Phase::Failed => "failed",
            Phase::Cancelled => "stopped",
        }
    }
}

/// One session as a person watching it draws it. An [`Event::Session`] carries a whole one each
/// time anything in it changes, so a consumer keys by `id` and replaces.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SessionView {
    pub id: String,
    /// The agent that started it.
    pub agent: String,
    /// The run whose tool call started it, and that call's span: this session's parent in a trace.
    pub run_id: String,
    pub call_span: Option<String>,
    pub trace_id: String,
    pub task: String,
    pub model: String,
    pub workspace: String,
    pub phase: Phase,
    /// What the daemon has charged so far, in micro-USD.
    pub cost_micros: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// The session's tools used so far, each once, in the order first used.
    pub tools: Vec<String>,
    /// Tool calls so far.
    pub calls: u32,
    /// What it is doing now, in a few words: `write_file src/lib.rs`, `waiting to run cargo test`.
    pub activity: String,
    pub started_ms: u64,
    pub finished_ms: Option<u64>,
    /// The answer, or why there is none.
    pub result: Option<String>,
    /// Approvals waiting, as the runtime shows approvals.
    pub pending: Vec<Pending>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    RunStarted {
        agent: String,
        run_id: String,
        trigger: String,
        input: String,
        trace_id: String,
        span_id: String,
        /// The tool call (in another run) that woke this one, when one did.
        parent_span_id: Option<String>,
        /// `provider/model`, as `RunRecord::model` will say it.
        model: String,
        chain: Vec<String>,
    },
    /// A tool call is about to be asked about or made. `span_id` is the call's span: whatever it
    /// wakes (a delegated agent, an event) names it as its parent, so a window can hang the woken
    /// run under this one while both are still going.
    ToolStarted {
        agent: String,
        run_id: String,
        span_id: String,
        tool: String,
        args: Value,
    },
    StepDone {
        agent: String,
        run_id: String,
        step: Step,
    },
    RunFinished(Box<RunRecord>),
    ApprovalRequested {
        id: u64,
        agent: String,
        tool: String,
        args: Value,
        chain: Vec<String>,
    },
    ApprovalResolved {
        id: u64,
        approved: bool,
    },
    /// A coding session an agent started changed: the whole of it, to be replaced by id.
    Session(Box<SessionView>),
}
