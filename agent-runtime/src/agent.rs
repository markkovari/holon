//! One run of one agent: a bounded loop of model turns and tool calls.
//!
//! Tool use is a JSON protocol in plain text rather than a provider's native
//! tool-calling, so it behaves the same on Apple's on-device model, an
//! OpenAI-compatible server and Anthropic. The model replies with ONE JSON
//! object `{"tool": "...", "args": {...}}` to act, or with plain text to
//! finish. Everything is bounded: steps, tokens per run, tokens per day.

use std::time::Duration;

use serde_json::{json, Value};

use crate::kv::{Entry, Put};
use crate::model::{Msg, Reply, Usage};
use crate::spec::{glob_match, validate_topic, AgentSpec, Trigger};
use crate::store::{confine, RunRecord, Status, Step, Store};
use crate::trace::{new_span_id, new_trace_id};

/// Why a run is happening, and the chain of wake-ups behind it. Every hop
/// copies it forward, which is what lets the runtime stop loops, bound spend
/// across a whole chain, and show a causal trace afterwards.
///
/// `trace_id` and `parent_span_id` are W3C Trace Context ids (see `trace.rs`),
/// so a run joins a trace started anywhere that speaks the standard, and what
/// it wakes continues it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cause {
    /// 32-hex trace id shared by every run one original trigger set off. Empty
    /// on a fresh trigger; the first run starts the trace.
    pub trace_id: String,
    /// The span that woke or called this one: a tool call in another agent's
    /// run, or the upstream caller's span from a `traceparent` header.
    pub parent_span_id: Option<String>,
    /// Wake-ups so far.
    pub hops: u32,
    /// `agent|topic` for each wake-up so far, oldest first.
    pub chain: Vec<String>,
    /// Tokens the rest of the chain may still spend; `None` = this agent's own limit.
    pub budget: Option<u64>,
    /// Photos that came with this run, by file name in the agent's inbox. They are put in front of
    /// the model with the first message; anything the run wakes does not get them.
    pub attachments: Vec<String>,
    /// The project this work is for, when it is for one: a schedule that names it, a reminder
    /// asked for inside it, a message in its room. The agent must be a member to be woken
    /// for it, and a reminder it sets keeps it.
    pub project: Option<String>,
}

/// A spawned task's state, as `task_result` reports it.
#[derive(Clone, Debug, PartialEq)]
pub enum TaskState {
    Running,
    Done { ok: bool, answer: String },
}

/// Pluggable tool handler for domain capabilities (ADR-0125).
pub trait ToolHandler: Send + Sync {
    fn can_handle(&self, name: &str) -> bool;
    fn execute(
        &self,
        host: &dyn Host,
        name: &str,
        args: &Value,
        cause: &Cause,
    ) -> Result<String, String>;
    fn is_sensitive(&self, _name: &str) -> bool {
        false
    }
}

/// What the loop needs from whoever runs it. The runtime implements this for
/// real; tests implement it with fakes.
pub trait Host: Sync {
    fn now(&self) -> u64;
    /// Wall clock in unix milliseconds, for span bounds.
    fn now_ms(&self) -> u64 {
        self.now() * 1000
    }
    fn store(&self) -> &Store;
    fn model(&self, spec: &AgentSpec, system: &str, msgs: &[Msg]) -> Result<Reply, String>;
    /// Blocks until a human answers, or denies on timeout. `chain` says what
    /// led to the request, so the human can see why it is being asked.
    fn approve(&self, agent: &str, tool: &str, args: &Value, chain: &[String]) -> bool;
    /// Runs `target` to completion and returns its answer and the tokens it spent.
    fn call_agent(
        &self,
        caller: &str,
        target: &str,
        message: &str,
        cause: &Cause,
    ) -> Result<(String, u64), String>;
    /// Starts `target` without waiting; returns a task id for `task_result`.
    fn spawn_task(
        &self,
        caller: &str,
        target: &str,
        message: &str,
        cause: &Cause,
    ) -> Result<String, String>;
    fn task_result(&self, id: &str) -> Option<TaskState>;
    /// Publishes to the durable bus; returns how many agents it will wake.
    fn emit(&self, from: &str, topic: &str, payload: &str, cause: &Cause) -> Result<usize, String>;
    fn kv_get(&self, ns: &str, key: &str) -> Result<Option<Entry>, String>;
    fn kv_list(&self, ns: &str, prefix: &str) -> Result<Vec<(String, Entry)>, String>;
    /// A write that CHANGES the value also wakes `StoreChange` subscribers.
    fn kv_put(
        &self,
        by: &str,
        ns: &str,
        key: &str,
        value: &str,
        if_version: Option<u64>,
        cause: &Cause,
    ) -> Result<Put, String>;
    /// A one-shot, persisted wake-up for `agent` `in_secs` from now.
    fn schedule_self(
        &self,
        agent: &str,
        in_secs: u64,
        prompt: &str,
        cause: &Cause,
    ) -> Result<(), String>;
    /// Audio (any format ffmpeg reads) to text; empty when nothing was said.
    fn transcribe(&self, audio: &[u8], lang: Option<&str>) -> Result<String, String>;
    /// Text to a voice message: Ogg/Opus bytes and their length in ms.
    fn speak(&self, text: &str, voice: Option<&str>) -> Result<(Vec<u8>, u64), String>;
    /// Creates or replaces an agent's spec. Only called for agents granted `agent_save`.
    fn save_agent(&self, _spec: AgentSpec) -> Result<String, String> {
        Err("agent management is not available".into())
    }
    fn delete_agent(&self, _name: &str) -> Result<(), String> {
        Err("agent management is not available".into())
    }
    fn restore_agent(&self, _name: &str) -> Result<String, String> {
        Err("agent management is not available".into())
    }
    /// Executes a custom or pluggable domain tool (ADR-0125).
    fn execute_custom_tool(
        &self,
        _name: &str,
        _args: &Value,
        _cause: &Cause,
    ) -> Option<Result<String, String>> {
        None
    }
    /// Whether a custom or pluggable tool is sensitive and requires human approval.
    fn is_custom_tool_sensitive(&self, _name: &str) -> Option<bool> {
        None
    }
    /// Jev, for reading pictures, when this host has a key for it.
    fn jev(&self) -> Option<&dyn crate::jev::Jev> {
        None
    }
    fn add_to_project(&self, _project: &str, _agent: &str) -> Result<(), String> {
        Err("agent management is not available".into())
    }
    fn pause_agent(&self, _name: &str, _paused: bool) -> Result<(), String> {
        Err("agent management is not available".into())
    }
    /// Embed texts with the configured embedding service (`kind` "query" or "document");
    /// `None` when there is none or it is down.
    fn embed(&self, _texts: &[String], _kind: &str) -> Option<Vec<Vec<f32>>> {
        None
    }
    /// Opens a coding session for `spec` on `comp-agentd` and sends it `task`. Returns its id at
    /// once. `run_id` and `cause` say which run and which tool call it is for.
    fn session_start(
        &self,
        _spec: &AgentSpec,
        _run_id: &str,
        _cause: &Cause,
        _task: &str,
        _workspace: &str,
    ) -> Result<String, String> {
        Err("coding sessions are not available here".into())
    }
    /// How one of `agent`'s sessions stands, waiting up to `wait` for it to end.
    fn session_result(
        &self,
        _agent: &str,
        _id: &str,
        _wait: Duration,
    ) -> Result<crate::session::SessionView, String> {
        Err("coding sessions are not available here".into())
    }
    fn session_close(&self, _agent: &str, _id: &str) -> Result<(), String> {
        Err("coding sessions are not available here".into())
    }
    fn observe(&self, event: Event);
}

pub use agent_wire::Event;

/// Longest wake-up chain. Past this, the next hop is dropped and logged.
pub const MAX_HOPS: u32 = 6;

/// Tool calls honoured from one model reply. Models that write several are
/// usually describing a plan; a cap keeps a runaway reply from fanning out.
const MAX_CALLS_PER_TURN: usize = 4;

struct ToolDef {
    name: &'static str,
    args: &'static str,
    about: &'static str,
    sensitive: bool,
}

const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "remember",
        args: r#"{"text": "..."}"#,
        about: "Save a fact for your future runs.",
        sensitive: false,
    },
    ToolDef {
        name: "recall",
        args: r#"{"query": "..."}"#,
        about: "Search what you saved earlier.",
        sensitive: false,
    },
    ToolDef {
        name: "now",
        args: "{}",
        about: "The current date and time: today's local date with its weekday, tomorrow, and the next days, plus UTC and unix time. Use it for any relative date (today, tomorrow, next Friday) instead of working it out yourself.",
        sensitive: false,
    },
    ToolDef {
        name: "list_agents",
        args: "{}",
        about: "The other agents you could delegate to.",
        sensitive: false,
    },
    ToolDef {
        name: "emit_event",
        args: r#"{"topic": "...", "payload": "..."}"#,
        about: "Publish an event that wakes any agent subscribed to the topic.",
        sensitive: false,
    },
    ToolDef {
        name: "store_get",
        args: r#"{"ns": "<store>", "key": "..."}"#,
        about: "Read a key from a store: `private` (only yours) or a shared store you were given.",
        sensitive: false,
    },
    ToolDef {
        name: "store_put",
        args: r#"{"ns": "<store>", "key": "...", "value": "..."}"#,
        about: "Write a key. Returns whether it CHANGED; agents watching the namespace wake only on a change. Add \"if_version\": N for a safe update.",
        sensitive: false,
    },
    ToolDef {
        name: "store_list",
        args: r#"{"ns": "<store>", "prefix": ""}"#,
        about: "List keys (and values) under a prefix.",
        sensitive: false,
    },
    ToolDef {
        name: "schedule_self",
        args: r#"{"in_secs": 300, "prompt": "..."}"#,
        about: "Wake yourself once, later, with this prompt.",
        sensitive: false,
    },
    ToolDef {
        name: "spawn_task",
        args: r#"{"agent": "...", "message": "..."}"#,
        about: "Start another agent on a task without waiting. Returns a task id.",
        sensitive: false,
    },
    ToolDef {
        name: "task_result",
        args: r#"{"id": "..."}"#,
        about: "Check on a task you started: running, or its answer.",
        sensitive: false,
    },
    ToolDef {
        name: "session_start",
        args: r#"{"task": "...", "workspace": "own"}"#,
        about: "Start a coding session: an agent that reads, writes and runs commands in a \
                workspace, each step approved by a person. Returns a session id at once; \
                collect the answer with session_result.",
        sensitive: false,
    },
    ToolDef {
        name: "session_result",
        args: r#"{"session": "...", "wait_secs": 20}"#,
        about: "Check on a coding session you started: still running, waiting for a person, or \
                its answer and what it cost. Waits up to wait_secs (at most 60) for it to end. \
                `session` may be left out when you have only one.",
        sensitive: false,
    },
    ToolDef {
        name: "session_close",
        args: r#"{"session": "..."}"#,
        about: "Stop a coding session you started, and whatever it is doing.",
        sensitive: false,
    },
    ToolDef {
        name: "transcribe",
        args: r#"{"path": "recording.ogg", "lang": "en-US"}"#,
        about: "Turn an audio file in your workspace into text (speech recognition).",
        sensitive: false,
    },
    ToolDef {
        name: "speak",
        args: r#"{"text": "...", "voice": "Samantha", "path": "speech/reply.ogg"}"#,
        about: "Turn text into a spoken voice message saved in your workspace.",
        sensitive: false,
    },
    ToolDef {
        name: "http_get",
        args: r#"{"url": "..."}"#,
        about: "Fetch a URL (only hosts you were allowed).",
        sensitive: false,
    },
    ToolDef {
        name: "list_dir",
        args: r#"{"path": "."}"#,
        about: "List files in your workspace.",
        sensitive: false,
    },
    ToolDef {
        name: "read_file",
        args: r#"{"path": "..."}"#,
        about: "Read a text file from your workspace.",
        sensitive: false,
    },
    ToolDef {
        name: "edit_file",
        args: r#"{"path": "...", "target": "...", "replacement": "..."}"#,
        about: "Replace exact text in a file in your workspace.",
        sensitive: true,
    },
    ToolDef {
        name: "write_file",
        args: r#"{"path": "...", "content": "..."}"#,
        about: "Write a text file in your workspace.",
        sensitive: true,
    },
    ToolDef {
        name: "agents_list",
        args: "{}",
        about: "List every agent: name, what it is for, and its capabilities.",
        sensitive: false,
    },
    ToolDef {
        name: "agent_show",
        args: r#"{"name": "..."}"#,
        about: "Show one agent's full spec.",
        sensitive: false,
    },
    ToolDef {
        name: "agent_save",
        args: r#"{"name": "...", "description": "what it is for, as instructions to it", "capabilities": ["http_get"], "schedule": "@every 1h", "prompt": "what to do when the schedule fires", "project": "rowing", "notify": true, "model": "qwen"}"#,
        about: "Create an agent, or change the fields you give of an existing one; fields you leave out stay as they are. Giving `schedule` REPLACES all of that agent's triggers with that one schedule. `project` adds the agent to that existing project (which GRANTS it the project's shared store and topics) and makes the schedule work for it: the result is posted in that project's room, not the agent's own. `notify: true` asks a chat bridge to post each scheduled result to the owner. The owner approves first.",
        sensitive: true,
    },
    ToolDef {
        name: "agent_delete",
        args: r#"{"name": "..."}"#,
        about: "Delete a whole agent: its settings, schedules, run history, memory and workspace move to the trash and can be brought back with agent_restore for 30 days, after which they are gone. Use it only when the owner clearly asks to delete the agent itself. To stop just a schedule use agent_remove_schedule; to stop an agent for now use agent_pause. The owner approves first.",
        sensitive: true,
    },
    ToolDef {
        name: "agent_restore",
        args: r#"{"name": "..."}"#,
        about: "Bring a deleted agent back from the trash, exactly as it was: settings, schedules, run history, memory, workspace and the projects it was in. It starts running on its schedules again. Works for 30 days after the deletion. The owner approves first.",
        sensitive: true,
    },
    ToolDef {
        name: "agent_remove_schedule",
        args: r#"{"name": "...", "schedule": "@every 1h", "project": "rowing"}"#,
        about: "Remove a schedule from an agent, and only that: the agent, its run history, memory and its other triggers stay. `schedule` is the cron line to remove; leave it out to remove all of the agent's schedules. `project` narrows it to the schedules that work for that project. The owner approves first.",
        sensitive: true,
    },
    ToolDef {
        name: "agent_pause",
        args: r#"{"name": "...", "paused": true}"#,
        about: "Pause (true) or resume (false) an agent: while paused none of its triggers fire, and nothing is deleted. Use this to stop an agent for now.",
        sensitive: false,
    },
];

fn tool_def(name: &str) -> Option<&'static ToolDef> {
    TOOLS.iter().find(|t| t.name == name)
}

/// Arguments a call cannot work without. `recall` and `list_dir` have sensible
/// defaults, so a call without them is not an error.
fn required(name: &str) -> &'static [&'static str] {
    match name {
        "remember" => &["text"],
        "emit_event" => &["topic"],
        "store_get" => &["ns", "key"],
        "store_put" => &["ns", "key", "value"],
        "store_list" => &["ns"],
        "schedule_self" => &["prompt"],
        "spawn_task" => &["agent", "message"],
        "task_result" => &["id"],
        "session_start" => &["task"],
        "transcribe" => &["path"],
        "speak" => &["text"],
        "http_get" => &["url"],
        "agent_show"
        | "agent_delete"
        | "agent_pause"
        | "agent_remove_schedule"
        | "agent_restore" => &["name"],
        "agent_save" => &["name"],
        "read_file" => &["path"],
        "edit_file" => &["path", "target", "replacement"],
        "write_file" => &["path", "content"],
        n if n.starts_with("agent:") => &["message"],
        _ => &[],
    }
}

/// The one argument a bare-string `args` stands for: `"args": "https://..."`
/// from a model that forgot the object means `{"url": "https://..."}`.
fn primary(name: &str) -> Option<&'static str> {
    match name {
        "remember" => Some("text"),
        "recall" => Some("query"),
        "http_get" => Some("url"),
        "read_file" | "list_dir" => Some("path"),
        "emit_event" => Some("topic"),
        "store_get" | "store_list" => Some("ns"),
        "schedule_self" => Some("prompt"),
        "task_result" => Some("id"),
        "session_start" => Some("task"),
        "session_result" | "session_close" => Some("session"),
        "agent_show"
        | "agent_delete"
        | "agent_pause"
        | "agent_save"
        | "agent_remove_schedule"
        | "agent_restore" => Some("name"),
        "transcribe" => Some("path"),
        "speak" => Some("text"),
        n if n.starts_with("agent:") => Some("message"),
        _ => None,
    }
}

/// Small models mangle tool calls in predictable ways. Accept the intent:
/// a bare string for a tool with one obvious parameter, and a missing `args`
/// become the object it meant. Anything else is left for `check_args` to
/// explain, rather than guessed at.
fn normalize_args(name: &str, args: Value) -> Value {
    match args {
        Value::String(s) => match primary(name) {
            Some(k) => json!({ k: s }),
            None => json!({}),
        },
        Value::Null => json!({}),
        other => other,
    }
}

/// `Err` says exactly what was missing and how to call the tool, because the
/// model reads this message to fix its call.
fn check_connector(spec: &AgentSpec, name: &str, args: &Value) -> Result<(), String> {
    let Some(x) = spec.capabilities.iter().find(|c| c.name == name).and_then(|c| c.exec.as_ref())
    else {
        return Ok(());
    };
    let missing: Vec<&str> =
        x.required.iter().map(String::as_str).filter(|k| arg(args, k).is_empty()).collect();
    if missing.is_empty() {
        return Ok(());
    }
    Err(format!(
        "missing {}. Call it as {{\"tool\": \"{name}\", \"args\": {}}}",
        missing.join(", "),
        x.args
    ))
}

fn check_args(name: &str, args: &Value) -> Result<(), String> {
    let missing: Vec<&str> =
        required(name).iter().copied().filter(|k| arg(args, k).is_empty()).collect();
    if missing.is_empty() {
        return Ok(());
    }
    let usage = tool_def(name).map_or(r#"{"message": "..."}"#, |t| t.args);
    Err(format!(
        "missing {}. Call it as {{\"tool\": \"{name}\", \"args\": {usage}}}",
        missing.join(", ")
    ))
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

fn system_prompt(host: &dyn Host, spec: &AgentSpec) -> String {
    let mut s = format!("You are {}. {}\n\n", spec.name, spec.description);
    s.push_str(
        "You are asleep unless something wakes you; the `Trigger:` line of the task says what \
         did (a message, a schedule, another agent, an event) and the task is why you are \
         needed. You remember nothing between runs except what is written below or in the \
         task. If the task shows the conversation so far, use it to understand what is being \
         asked, and answer only the latest message. A `Project:` line means the work is for \
         that project, so work and report as a member of it.\n\n",
    );
    let mut tools = String::new();
    let mut abilities = String::new();
    for c in &spec.capabilities {
        let wit = c.wit.as_deref().map(|w| format!(" [{w}]")).unwrap_or_default();
        if let Some(t) = tool_def(&c.name) {
            let about = if c.description.is_empty() { t.about } else { &c.description };
            tools.push_str(&format!("- {} {} — {about}{wit}\n", t.name, t.args));
        } else if let Some(x) = &c.exec {
            let about = if c.description.is_empty() { "A connector." } else { &c.description };
            tools.push_str(&format!("- {} {} — {about}{wit}\n", c.name, x.args));
        } else if let Some(other) = c.name.strip_prefix("agent:") {
            let about = if c.description.is_empty() {
                "Delegate a task to that agent."
            } else {
                &c.description
            };
            tools.push_str(&format!("- agent:{other} {{\"message\": \"...\"}} — {about}{wit}\n"));
        } else {
            abilities.push_str(&format!("- {}: {}{wit}\n", c.name, c.description));
        }
    }
    if !tools.is_empty() {
        s.push_str(
            "You work in steps. To use a tool, reply with ONLY one JSON object, \
             {\"tool\": \"<name>\", \"args\": {...}}, and wait for the result. When you have \
             your answer, reply in plain text with no JSON. A plain-text reply ENDS the task, \
             so do not write one until you have done everything the task asks, including every \
             tool call it lists.\n\nTools:\n",
        );
        s.push_str(&tools);
        s.push('\n');
    } else {
        s.push_str("Reply in plain text.\n\n");
    }
    if !abilities.is_empty() {
        s.push_str("Other things you can do yourself (no tool needed):\n");
        s.push_str(&abilities);
        s.push('\n');
    }
    // What this agent may reach beyond itself. A model cannot guess a shared
    // store's name, and one that is not told will fall back on `private` —
    // which nobody else can see, so the collaboration silently never happens.
    let mut reach = String::new();
    let (r, w) = (&spec.store.read, &spec.store.write);
    if !r.is_empty() || !w.is_empty() {
        let list: Vec<String> = w
            .iter()
            .map(|n| format!("{n} (read and write)"))
            .chain(r.iter().filter(|n| !w.contains(n)).map(|n| format!("{n} (read only)")))
            .collect();
        reach.push_str(&format!(
            "Shared stores you may use with the store tools: {}. Use these names as `ns` when other \
             agents need to see the data; `private` is visible only to you.\n",
            list.join(", ")
        ));
    }
    if !spec.topics_out.is_empty() {
        reach
            .push_str(&format!("Topics you may emit events to: {}.\n", spec.topics_out.join(", ")));
    }
    if !reach.is_empty() {
        s.push_str(&reach);
        s.push('\n');
    }
    for p in &spec.projects {
        s.push_str(&format!(
            "You are working in the project `{}`{}. Members: {}{}. The project's shared store is `{}` \
             (use that as `ns`, so the others can see it); you may emit events to `{}`.\n",
            p.name,
            if p.description.is_empty() { String::new() } else { format!(" ({})", p.description) },
            p.members.join(", "),
            p.lead.as_ref().map(|l| format!(", led by {l}")).unwrap_or_default(),
            p.store_ns,
            p.topic_glob
        ));
    }
    if !spec.projects.is_empty() {
        s.push_str(
            "If a question needs what another member knows or does, call that member's `agent:` \
             tool yourself and use its answer. Never ask the person which member to use or who \
             they are.\n\n",
        );
    }
    let memories = host.store().recall(&spec.name, "", 5);
    if !memories.is_empty() {
        s.push_str("Things you remembered earlier (newest first):\n");
        for m in memories {
            s.push_str(&format!("- {}\n", clip(&m.text, 300)));
        }
        s.push('\n');
    }
    let runs = host.store().runs(&spec.name, spec.recent_runs);
    if !runs.is_empty() {
        s.push_str("Your last runs (newest first):\n");
        for r in runs {
            s.push_str(&format!(
                "- [{}] {} → {}\n",
                r.trigger,
                clip(&r.input, 80),
                clip(&r.answer, 120)
            ));
        }
        s.push('\n');
    }
    s.push_str(&format!("Current time (UTC, unix seconds): {}\n", host.now()));
    s
}

/// Does this model error mean "your prompt is too big"? Providers word it
/// differently (Apple's on-device model answers 500 with a message about its
/// context window), so match the idea, not a status code.
fn is_context_overflow(err: &str) -> bool {
    let e = err.to_lowercase();
    ["context", "too long", "too large", "exceed", "maximum length", "token limit"]
        .iter()
        .any(|k| e.contains(k))
}

/// Calls the model; if the prompt is too big for it, halves the largest
/// message (always a tool result in practice — they are the only large ones)
/// and tries again, up to three times. The agent then answers from a shorter
/// result instead of the whole run failing on a page that happened to be long.
fn model_with_shrinking(
    host: &dyn Host,
    spec: &AgentSpec,
    system: &str,
    msgs: &mut [Msg],
) -> Result<Reply, String> {
    let mut tries = 0;
    loop {
        match host.model(spec, system, msgs) {
            Err(e) if tries < 3 && is_context_overflow(&e) => {
                let Some(biggest) = msgs.iter_mut().max_by_key(|m| m.content.len()) else {
                    return Err(e);
                };
                if biggest.content.len() < 400 {
                    return Err(e);
                }
                let keep = biggest.content.len() / 2;
                let cut =
                    (0..=keep).rev().find(|&i| biggest.content.is_char_boundary(i)).unwrap_or(0);
                biggest.content = format!(
                    "{}\n[…shortened to fit the model's context window]",
                    &biggest.content[..cut]
                );
                tries += 1;
            }
            other => return other,
        }
    }
}

fn call_from(v: &Value) -> Option<(String, Value)> {
    let name = v.get("tool")?.as_str()?.to_string();
    // `{"tool": "http_get", "url": "..."}` — the arguments beside the name.
    let args = v.get("args").cloned().unwrap_or_else(|| {
        let mut o = v.as_object().cloned().unwrap_or_default();
        o.remove("tool");
        Value::Object(o)
    });
    Some((name, args))
}

/// Every JSON object in `text` that has a string `tool` field, in order. A
/// model often writes several calls in one reply; each is real intent.
pub fn json_calls(text: &str) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(off) = text[i..].find('{') {
        let start = i + off;
        let mut de = serde_json::Deserializer::from_str(&text[start..]).into_iter::<Value>();
        match de.next() {
            Some(Ok(v)) => {
                out.extend(call_from(&v));
                i = start + de.byte_offset().max(1);
            }
            _ => i = start + 1,
        }
    }
    out
}

/// The first of `json_calls`.
pub fn parse_tool_call(text: &str) -> Option<(String, Value)> {
    json_calls(text).into_iter().next()
}

/// The shape small models fall back to when they describe a call instead of
/// writing it: `remember "likes tea"`, `emit_event {"topic": "x"}`. Only names
/// the agent was actually granted count, and only when what follows is a JSON
/// object or string, so ordinary prose that mentions a tool is left alone.
pub fn loose_calls(text: &str, granted: &[&str]) -> Vec<(String, Value)> {
    let mut found: Vec<(usize, String, Value)> = Vec::new();
    for name in granted {
        let mut from = 0;
        while let Some(off) = text[from..].find(name) {
            let at = from + off;
            from = at + name.len();
            let before_ok = text[..at]
                .chars()
                .next_back()
                .is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == ':'));
            if !before_ok {
                continue;
            }
            let rest = text[from..].trim_start_matches(|c: char| c.is_whitespace() || c == '(');
            if !(rest.starts_with('{') || rest.starts_with('"')) {
                continue;
            }
            if let Some(Ok(v)) =
                serde_json::Deserializer::from_str(rest).into_iter::<Value>().next()
            {
                found.push((at, name.to_string(), v));
                from = text.len() - rest.len();
            }
        }
    }
    found.sort_by_key(|(at, ..)| *at);
    let mut calls: Vec<(String, Value)> = found.into_iter().map(|(_, n, a)| (n, a)).collect();
    if calls.is_empty() {
        calls.extend(bare_call(text, granted));
    }
    calls
}

/// A reply that is only `HTTP_GET https://example.com/x` (a tool name in any case and ONE
/// bare argument): what a small model writes when it means to call a tool with a single
/// obvious parameter. Prose that merely starts with a tool's name is left alone: the argument
/// must be the whole rest of a one-line reply, with no spaces in it.
fn bare_call(text: &str, granted: &[&str]) -> Option<(String, Value)> {
    let t = text.trim();
    if t.contains('\n') {
        return None;
    }
    granted.iter().find_map(|name| {
        let key = primary(name)?;
        let head = t.get(..name.len())?;
        if !head.eq_ignore_ascii_case(name) {
            return None;
        }
        let arg =
            t[name.len()..].trim().trim_matches(|c| matches!(c, '`' | '"' | '\'' | '(' | ')'));
        let sep_ok = t[name.len()..].starts_with(|c: char| c.is_whitespace() || c == '(');
        (sep_ok && !arg.is_empty() && !arg.contains(char::is_whitespace))
            .then(|| (name.to_string(), json!({ key: arg })))
    })
}

/// An argument as text. A model that sends `"value": {"a": 1}` or `"n": 5`
/// meant it, so non-strings are rendered as JSON rather than treated as absent.
fn arg(args: &Value, k: &str) -> String {
    match args.get(k) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(v) => v.to_string(),
    }
}

/// Resolves a namespace the model named: `private` is this agent's own and is
/// always allowed; anything else must be granted in the spec's `store`.
fn resolve_ns(spec: &AgentSpec, ns: &str, write: bool) -> Result<String, String> {
    if ns == "private" {
        return Ok(format!("private-{}", spec.name));
    }
    validate_topic(ns)?;
    let allowed = if write {
        spec.store.write.iter().any(|n| n == ns)
    } else {
        spec.store.read.iter().chain(&spec.store.write).any(|n| n == ns)
    };
    if allowed {
        Ok(ns.to_string())
    } else {
        let may: Vec<&String> = if write {
            spec.store.write.iter().collect()
        } else {
            spec.store.read.iter().chain(&spec.store.write).collect()
        };
        Err(format!(
            "you may not {} store `{ns}`; you may {}: private{}",
            if write { "write" } else { "read" },
            if write { "write" } else { "read" },
            may.iter().map(|n| format!(", {n}")).collect::<String>()
        ))
    }
}

fn entry_text(key: &str, e: &Entry) -> String {
    format!("{key} (v{}, by {}): {}", e.version, e.by, e.value)
}

fn http_get(
    spec: &AgentSpec,
    url: &str,
    vault: Option<&crate::keys::Vault>,
) -> Result<String, String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or("only http:// and https:// URLs")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains('@') {
        return Err("URLs with credentials are refused".into());
    }
    let host = authority
        .rsplit_once(':')
        .filter(|(_, p)| p.chars().all(|c| c.is_ascii_digit()))
        .map_or(authority, |(h, _)| h);
    if !spec.api_keys.is_empty() && vault.is_none() {
        return Err(
            "API keys need a vault: add a key in the console's Keys tab, or set HOLON_PLATFORM_URL, HOLON_SECRETS_ORG and HOLON_FETCH_TOKEN".into()
        );
    }
    // shortcut: every key is fetched (and audited) on every call; cache per run if calls get chatty.
    let keys = match vault {
        Some(v) => spec.api_keys.iter().map(|n| v.load(n)).collect::<Result<Vec<_>, _>>()?,
        None => Vec::new(),
    };
    let keys: Vec<_> = keys.into_iter().filter(|k| k.host == authority || k.host == host).collect();
    if keys.is_empty() && !spec.allow_hosts.iter().any(|a| a == authority || a == host) {
        return Err(format!("{authority} is not in this agent's allowed hosts"));
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none()) // a redirect could leave the allow-list
        .user_agent("holon-agent/0.1")
        .build()
        .map_err(|e| e.to_string())?;
    let mut req = client.get(url);
    for k in &keys {
        req = req.header(k.header.as_str(), k.header_value());
    }
    let r = req.send().map_err(|e| e.to_string())?;
    let status = r.status();
    let is_html = r
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("html"));
    let body = r.text().map_err(|e| e.to_string())?;
    // Markup is noise to a model and eats its context; give it the text.
    let mut body = if is_html { crate::html::html_to_text(&body) } else { body };
    for k in &keys {
        body = body.replace(&k.key, "[api key]");
    }
    Ok(format!("HTTP {status}\n{}", clip(&body, 8_000)))
}

/// Runs one tool. `Err` is returned to the model as the tool's result, so it
/// can adapt, exactly like a denial is. `cause` is THIS run, so anything it
/// wakes records it as the parent. `child_tokens` accumulates what delegated
/// agents spent, which counts against this run's budget.
fn exec(
    host: &dyn Host,
    spec: &AgentSpec,
    run_id: &str,
    name: &str,
    args: &Value,
    cause: &Cause,
    child_tokens: &mut u64,
) -> Result<String, String> {
    let store = host.store();
    match name {
        "remember" => {
            let t = arg(args, "text");
            if t.is_empty() {
                return Err("`text` is empty".into());
            }
            store.remember(&spec.name, &t, host.now())?;
            Ok("saved".into())
        }
        "recall" => {
            let query = arg(args, "query");
            let semantic = (!query.is_empty())
                .then(|| host.embed(std::slice::from_ref(&query), "query"))
                .flatten()
                .and_then(|mut v| v.pop())
                .map(|qv| store.recall_semantic(&spec.name, &qv, 5, &|t| host.embed(t, "document")))
                .filter(|h| !h.is_empty());
            let hits = semantic.unwrap_or_else(|| store.recall(&spec.name, &query, 5));
            Ok(if hits.is_empty() {
                "nothing matched".into()
            } else {
                hits.iter().map(|m| format!("- {}", m.text)).collect::<Vec<_>>().join("\n")
            })
        }
        "now" => Ok(crate::clock::now(host.now())),
        "list_agents" => Ok(store
            .list()
            .iter()
            .filter(|a| a.name != spec.name)
            .map(|a| format!("- {}: {}", a.name, clip(&a.description, 100)))
            .collect::<Vec<_>>()
            .join("\n")),
        "emit_event" => {
            let topic = arg(args, "topic");
            validate_topic(&topic)?;
            if !spec.topics_out.iter().any(|p| glob_match(p, &topic)) {
                return Err(format!(
                    "you may not emit `{topic}`; you may emit: {}",
                    if spec.topics_out.is_empty() {
                        "nothing".to_string()
                    } else {
                        spec.topics_out.join(", ")
                    }
                ));
            }
            let woken = host.emit(&spec.name, &topic, &arg(args, "payload"), cause)?;
            Ok(format!("emitted {topic}; {woken} agent(s) will be woken"))
        }
        "store_get" => {
            let ns = resolve_ns(spec, &arg(args, "ns"), false)?;
            let key = arg(args, "key");
            Ok(match host.kv_get(&ns, &key)? {
                Some(e) => entry_text(&key, &e),
                None => format!("{key} is not set"),
            })
        }
        "store_list" => {
            let ns = resolve_ns(spec, &arg(args, "ns"), false)?;
            let rows = host.kv_list(&ns, &arg(args, "prefix"))?;
            Ok(if rows.is_empty() {
                "no keys".into()
            } else {
                rows.iter().take(50).map(|(k, e)| entry_text(k, e)).collect::<Vec<_>>().join("\n")
            })
        }
        "store_put" => {
            let ns = resolve_ns(spec, &arg(args, "ns"), true)?;
            let if_version = args.get("if_version").and_then(Value::as_u64);
            let put = host.kv_put(
                &spec.name,
                &ns,
                &arg(args, "key"),
                &arg(args, "value"),
                if_version,
                cause,
            )?;
            Ok(if put.changed {
                format!("stored; changed: true, version: {}", put.version)
            } else {
                format!("unchanged; changed: false, version: {}", put.version)
            })
        }
        "schedule_self" => {
            let secs = args.get("in_secs").and_then(Value::as_u64).unwrap_or(60);
            host.schedule_self(&spec.name, secs, &arg(args, "prompt"), cause)?;
            Ok(format!("you will be woken in {secs}s"))
        }
        "spawn_task" => {
            let target = arg(args, "agent");
            if !spec.has_capability(&format!("agent:{target}")) {
                return Err(format!(
                    "you may only start agents you have `agent:<name>` for; not `{target}`"
                ));
            }
            let id = host.spawn_task(&spec.name, &target, &arg(args, "message"), cause)?;
            Ok(format!("task {id} started; check it later with task_result"))
        }
        "task_result" => Ok(match host.task_result(&arg(args, "id")) {
            None => return Err("no such task".into()),
            Some(TaskState::Running) => "still running".into(),
            Some(TaskState::Done { ok: true, answer }) => format!("done: {answer}"),
            Some(TaskState::Done { ok: false, answer }) => format!("failed: {answer}"),
        }),
        "session_start" => {
            let id = host.session_start(
                spec,
                run_id,
                cause,
                &arg(args, "task"),
                &arg(args, "workspace"),
            )?;
            Ok(format!("session {id} started; collect it with session_result"))
        }
        "session_result" => {
            let wait = args.get("wait_secs").and_then(Value::as_u64).unwrap_or(20);
            let view = host.session_result(
                &spec.name,
                &arg(args, "session"),
                Duration::from_secs(wait.min(crate::session::MAX_WAIT.as_secs())),
            )?;
            Ok(crate::session::Describe::for_model(&view))
        }
        "session_close" => {
            host.session_close(&spec.name, &arg(args, "session"))?;
            Ok("stopped".into())
        }
        "agents_list" => {
            let all = store.list();
            Ok(if all.is_empty() {
                "no agents".into()
            } else {
                all.iter().map(crate::admin::summary).collect::<Vec<_>>().join("\n")
            })
        }
        "agent_show" => match store.get(&arg(args, "name")) {
            Some(s) => serde_json::to_string_pretty(&s).map_err(|e| e.to_string()),
            None => Err(format!("no agent named {}", arg(args, "name"))),
        },
        "agent_save" => {
            let name = arg(args, "name");
            if name == spec.name {
                return Err("you may not change your own spec".into());
            }
            let project = arg(args, "project");
            if !project.is_empty() && store.get_project(&project).is_none() {
                return Err(format!("no project named {project}; create it first"));
            }
            let existing = store.get(&name);
            let new =
                crate::admin::build_spec_with(existing.as_ref(), args, &|n| store.model_alias(n))?;
            let had = existing.as_ref().map_or(&[][..], |e| &e.api_keys[..]);
            if let Some(k) =
                new.api_keys.iter().find(|k| !had.contains(k) && !spec.api_keys.contains(k))
            {
                return Err(format!("you may not give the API key {k}: you do not hold it"));
            }
            host.save_agent(new)?;
            let joined = if project.is_empty() {
                String::new()
            } else {
                host.add_to_project(&project, &name)?;
                format!(" and added it to the project {project}")
            };
            Ok(if existing.is_some() {
                format!("updated {name}{joined}")
            } else {
                format!("created {name}{joined}")
            })
        }
        "agent_delete" => {
            let name = arg(args, "name");
            if name == spec.name {
                return Err("you may not delete yourself".into());
            }
            let runs = store.runs(&name, usize::MAX).len();
            host.delete_agent(&name)?;
            Ok(format!(
                "deleted {name}: its settings, schedules, {runs} recorded runs, memory and \
                 workspace are in the trash for 30 days; agent_restore brings it back"
            ))
        }
        "agent_restore" => host.restore_agent(&arg(args, "name")),
        "agent_remove_schedule" => {
            let name = arg(args, "name");
            if name == spec.name {
                return Err("you may not change your own spec".into());
            }
            let mut target = store.get(&name).ok_or_else(|| format!("no agent named {name}"))?;
            let only = arg(args, "schedule");
            let in_project = arg(args, "project");
            let wanted = |t: &Trigger| match t {
                Trigger::Schedule { cron, project, .. } => {
                    (only.is_empty() || *cron == only)
                        && (in_project.is_empty()
                            || project.as_deref() == Some(in_project.as_str()))
                }
                _ => false,
            };
            let before = target.triggers.len();
            let have: Vec<String> = target
                .triggers
                .iter()
                .filter_map(|t| match t {
                    Trigger::Schedule { cron, .. } => Some(format!("`{cron}`")),
                    _ => None,
                })
                .collect();
            target.triggers.retain(|t| !wanted(t));
            let removed = before - target.triggers.len();
            if removed == 0 {
                return Err(if have.is_empty() {
                    format!("{name} has no schedule")
                } else {
                    format!(
                        "{name} has no schedule `{only}`; its schedules are {}",
                        have.join(", ")
                    )
                });
            }
            host.save_agent(target)?;
            Ok(format!(
                "removed {removed} schedule(s) from {name}; the agent, its run history, memory \
                 and its other triggers are untouched"
            ))
        }
        "agent_pause" => {
            let paused = !matches!(args.get("paused"), Some(Value::Bool(false)))
                && arg(args, "paused") != "false";
            host.pause_agent(&arg(args, "name"), paused)?;
            Ok(if paused { "paused".into() } else { "resumed".into() })
        }
        "transcribe" => {
            let p = confine(&store.workspace(&spec.name)?, &arg(args, "path"))?;
            let bytes = std::fs::read(&p).map_err(|e| format!("{}: {e}", arg(args, "path")))?;
            let lang = Some(arg(args, "lang")).filter(|l| !l.is_empty());
            let text = host.transcribe(&bytes, lang.as_deref())?;
            Ok(if text.is_empty() { "(no speech recognised)".to_string() } else { text })
        }
        "speak" => {
            let voice = Some(arg(args, "voice")).filter(|v| !v.is_empty());
            let (audio, ms) = host.speak(&arg(args, "text"), voice.as_deref())?;
            let rel = Some(arg(args, "path"))
                .filter(|p| !p.is_empty())
                .unwrap_or_else(|| format!("speech/{}.ogg", host.now()));
            let p = confine(&store.workspace(&spec.name)?, &rel)?;
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::write(&p, audio).map_err(|e| e.to_string())?;
            Ok(format!("wrote {rel} ({:.1}s of speech)", ms as f64 / 1000.0))
        }
        "http_get" => http_get(spec, &arg(args, "url"), crate::keys::Vault::configured().as_ref()),
        "list_dir" => {
            let root = store.workspace(&spec.name)?;
            let rel = arg(args, "path");
            let dir = if matches!(rel.as_str(), "" | ".") { root } else { confine(&root, &rel)? };
            let mut names: Vec<String> = std::fs::read_dir(&dir)
                .map_err(|e| e.to_string())?
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            Ok(names.join("\n"))
        }
        "read_file" => {
            let p = confine(&store.workspace(&spec.name)?, &arg(args, "path"))?;
            std::fs::read_to_string(p).map(|s| clip(&s, 16_000)).map_err(|e| e.to_string())
        }
        "edit_file" => {
            let p = confine(&store.workspace(&spec.name)?, &arg(args, "path"))?;
            let target = arg(args, "target");
            if target.is_empty() {
                return Err("target string cannot be empty".into());
            }
            let content = std::fs::read_to_string(&p).map_err(|e| e.to_string())?;
            let count = content.matches(&target).count();
            if count == 0 {
                return Err("target string not found".into());
            }
            if count > 1 {
                return Err(format!("target string appears {count} times; must be unique in file"));
            }
            let new_content = content.replacen(&target, &arg(args, "replacement"), 1);
            std::fs::write(&p, &new_content).map_err(|e| e.to_string())?;
            Ok("replaced 1 occurrence".into())
        }
        "write_file" => {
            let p = confine(&store.workspace(&spec.name)?, &arg(args, "path"))?;
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let content = arg(args, "content");
            std::fs::write(&p, &content).map_err(|e| e.to_string())?;
            Ok(format!("wrote {} bytes", content.len()))
        }
        other => match other.strip_prefix("agent:") {
            Some(target) => {
                let (answer, spent) =
                    host.call_agent(&spec.name, target, &arg(args, "message"), cause)?;
                *child_tokens += spent;
                Ok(answer)
            }
            None => match spec
                .capabilities
                .iter()
                .find(|c| c.name == other)
                .and_then(|c| c.exec.as_ref())
            {
                Some(x) => {
                    let workspace = store.workspace(&spec.name).ok();
                    crate::connector::run(x, args, workspace.as_deref())
                }
                None => {
                    if let Some(res) = host.execute_custom_tool(other, args, cause) {
                        return res;
                    }
                    Err(format!("no such tool: {other}"))
                }
            },
        },
    }
}

/// `provider/model`, for span attributes.
pub fn model_label(m: &crate::spec::ModelSpec) -> String {
    use crate::spec::ModelSpec::*;
    match m {
        Local => "local/system".to_string(),
        OpenAi { model, .. } => format!("openai/{model}"),
        Anthropic { model, .. } => format!("anthropic/{model}"),
        Gemini { model, .. } => format!("gemini/{model}"),
        Fallback { models } => models.first().map_or("fallback/none".to_string(), model_label),
        Mock { .. } => "mock/mock".to_string(),
    }
}

/// Runs one task. `cause` says what woke this run (`Cause::default()` for a
/// fresh trigger); its `budget` caps this run below the agent's own limit.
pub fn run(
    host: &dyn Host,
    spec: &AgentSpec,
    trigger: &str,
    input: &str,
    cause: &Cause,
) -> RunRecord {
    let started = host.now();
    let seq = RUN_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let run_id = format!("{}-{}-{}", spec.name, started, seq);
    let started_ms = host.now_ms();
    let trace_id = if cause.trace_id.is_empty() { new_trace_id() } else { cause.trace_id.clone() };
    let span_id = new_span_id();
    host.observe(Event::RunStarted {
        agent: spec.name.clone(),
        run_id: run_id.clone(),
        trigger: trigger.to_string(),
        input: input.to_string(),
        trace_id: trace_id.clone(),
        span_id: span_id.clone(),
        parent_span_id: cause.parent_span_id.clone(),
        model: model_label(&spec.model),
        chain: cause.chain.clone(),
    });
    // This chain's remaining spend caps the run below the agent's own limit.
    let max_tokens = cause.budget.map_or(spec.max_tokens, |b| b.min(spec.max_tokens));

    let mut rec = RunRecord {
        id: run_id.clone(),
        agent: spec.name.clone(),
        trigger: trigger.to_string(),
        input: input.to_string(),
        started,
        finished: started,
        status: Status::Ok,
        answer: String::new(),
        steps: Vec::new(),
        tokens_in: 0,
        tokens_out: 0,
        started_ms,
        finished_ms: started_ms,
        model: model_label(&spec.model),
        trace_id: trace_id.clone(),
        span_id: span_id.clone(),
        parent_span_id: cause.parent_span_id.clone(),
        hops: cause.hops,
        chain: cause.chain.clone(),
        project: cause.project.clone().unwrap_or_default(),
        attachments: cause.attachments.clone(),
    };
    let mut child_tokens = 0u64;
    let push = |rec: &mut RunRecord, step: Step| {
        host.observe(Event::StepDone {
            agent: rec.agent.clone(),
            run_id: rec.id.clone(),
            step: step.clone(),
        });
        rec.steps.push(step);
    };

    if spec.daily_token_budget > 0 {
        let used = host.store().tokens_since(&spec.name, started.saturating_sub(86_400));
        if used >= spec.daily_token_budget {
            rec.status = Status::OverBudget;
            rec.answer = format!("daily token budget spent ({used}/{})", spec.daily_token_budget);
            return finish(host, rec);
        }
    }

    let system = system_prompt(host, spec);
    let project_line =
        cause.project.as_ref().map(|p| format!("Project: {p}\n")).unwrap_or_default();
    // Photos that came with the run go to the model with the first message. One that cannot be
    // used is said, so the model tells Mark instead of answering as if it had looked.
    // An agent with `vision_delegate` reads photos through an external tool, not its own model: the model is told
    // the photo is there and what it is called, and nothing is sent to it.
    let (images, problems, seen) = if spec.has_capability("vision_delegate") {
        let ok: Vec<&String> = cause
            .attachments
            .iter()
            .filter(|f| crate::attach::exists(host.store(), &spec.name, f))
            .collect();
        let seen = match ok.len() {
            0 => String::new(),
            _ => format!(
                "\n(attached: {}. Read {} with vision_delegate.)",
                ok.iter().map(|f| format!("photo `{f}`")).collect::<Vec<_>>().join(", "),
                if ok.len() == 1 { "it" } else { "each one, naming it in `file`," }
            ),
        };
        (Vec::new(), Vec::new(), seen)
    } else {
        let (images, problems) = crate::attach::load(host.store(), &spec.name, &cause.attachments);
        let seen = match images.len() {
            0 => String::new(),
            n => format!(
                "\n(attached: {n} photo{}. Look at {}.)",
                if n == 1 { "" } else { "s" },
                if n == 1 { "it" } else { "them" }
            ),
        };
        (images, problems, seen)
    };
    let unread: String = problems.iter().map(|p| format!("\n(attachment: {p})")).collect();
    let mut msgs =
        vec![Msg::user(format!("Trigger: {trigger}\n{project_line}Task: {input}{seen}{unread}"))
            .with_images(images)];
    let mut usage = Usage::default();
    let mut last_text = String::new();
    let mut called_ok: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut nudges = 0;

    for turn in 0..=spec.max_steps {
        // The final turn forbids tools, so a run always ends in an answer.
        let last_turn = turn == spec.max_steps;
        if last_turn {
            msgs.push(Msg::user("You are out of steps. Give your final answer now, in plain text, with no tool call."));
        }
        let m0 = host.now_ms();
        let reply = match model_with_shrinking(host, spec, &system, &mut msgs) {
            Ok(r) => r,
            Err(e) => {
                rec.status = Status::Failed;
                rec.answer = e;
                break;
            }
        };
        usage.input += reply.usage.input;
        usage.output += reply.usage.output;
        rec.tokens_in = usage.input;
        rec.tokens_out = usage.output;
        push(
            &mut rec,
            Step::Model {
                text: reply.text.clone(),
                span_id: new_span_id(),
                t0: m0,
                t1: host.now_ms(),
                tokens_in: reply.usage.input,
                tokens_out: reply.usage.output,
            },
        );
        last_text = reply.text.clone();

        let mut calls = Vec::new();
        if !last_turn {
            calls = json_calls(&reply.text);
            if calls.is_empty() {
                let granted: Vec<&str> =
                    spec.capabilities.iter().map(|c| c.name.as_str()).collect();
                calls = loose_calls(&reply.text, &granted);
            }
            calls.truncate(MAX_CALLS_PER_TURN);
        }
        if calls.is_empty() {
            // A plain-text reply ends the task — unless it comes before a tool
            // the spec says every run must call.
            let missing: Vec<&str> = spec
                .must_call
                .iter()
                .filter(|t| !called_ok.contains(*t))
                .map(String::as_str)
                .collect();
            if !missing.is_empty() {
                if nudges < 2 && !last_turn {
                    nudges += 1;
                    msgs.push(Msg::assistant(reply.text.clone()));
                    msgs.push(Msg::user(format!(
                        "You are not finished: you have not yet called {}, which this task requires. \
                         Call it now, using the tool JSON format.",
                        missing.join(", ")
                    )));
                    continue;
                }
                rec.status = Status::Failed;
                rec.answer = format!(
                    "never called required tool(s) {}; the model answered instead: {}",
                    missing.join(", "),
                    clip(&reply.text, 200)
                );
                break;
            }
            rec.answer = reply.text.trim().to_string();
            break;
        }

        let (mut said, mut results, mut any_error) = (Vec::new(), Vec::new(), false);
        for (name, args) in calls {
            let args = normalize_args(&name, args);
            // The tool call is a span of its own, and the parent of anything
            // it wakes — which is how a trace shows WHICH call caused a run.
            let tool_span = new_span_id();
            let t0 = host.now_ms();
            let (result, error, approved) = if !spec.has_capability(&name) {
                (format!("`{name}` is not one of your capabilities"), true, None)
            } else if let Err(e) =
                check_args(&name, &args).and_then(|_| check_connector(spec, &name, &args))
            {
                // Malformed: never bother a human to approve a call that cannot run.
                (e, true, None)
            } else {
                let sensitive = tool_def(&name)
                    .map(|t| t.sensitive)
                    .or_else(|| host.is_custom_tool_sensitive(&name))
                    .unwrap_or_else(|| {
                        spec.capabilities
                            .iter()
                            .any(|c| c.name == name && c.exec.as_ref().is_some_and(|x| x.sensitive))
                    });
                let needs = sensitive && !spec.auto_approve.contains(&name);
                host.observe(Event::ToolStarted {
                    agent: spec.name.clone(),
                    run_id: run_id.clone(),
                    span_id: tool_span.clone(),
                    tool: name.clone(),
                    args: args.clone(),
                });
                let ok = !needs || host.approve(&spec.name, &name, &args, &cause.chain);
                if ok {
                    // What this run passes on to anything it wakes: itself as
                    // the parent, and whatever budget the chain has left.
                    let me = Cause {
                        trace_id: trace_id.clone(),
                        parent_span_id: Some(tool_span.clone()),
                        hops: cause.hops,
                        chain: cause.chain.clone(),
                        budget: Some(
                            max_tokens.saturating_sub(usage.input + usage.output + child_tokens),
                        ),
                        project: cause.project.clone(),
                        attachments: Vec::new(),
                    };
                    match exec(host, spec, &run_id, &name, &args, &me, &mut child_tokens) {
                        Ok(r) => (r, false, needs.then_some(true)),
                        Err(e) => (e, true, needs.then_some(true)),
                    }
                } else {
                    ("a human denied this tool call".to_string(), true, Some(false))
                }
            };
            push(
                &mut rec,
                Step::Tool {
                    name: name.clone(),
                    args: args.clone(),
                    result: result.clone(),
                    error,
                    approved,
                    span_id: tool_span,
                    t0,
                    t1: host.now_ms(),
                },
            );
            any_error |= error;
            if !error {
                called_ok.insert(name.clone());
            }
            said.push(json!({ "tool": name, "args": args }).to_string());
            results.push(format!(
                "Result of {name}{}:\n{}",
                if error { " (error)" } else { "" },
                clip(&result, spec.max_result_chars)
            ));
        }
        // Record only the calls themselves. Anything the model wrote around
        // them was written BEFORE it saw a result, so it is a guess — and a
        // model that reads its own guess back tends to answer with it.
        msgs.push(Msg::assistant(said.join("\n")));
        // A failure must not read like data. The model otherwise tends to
        // carry on as if the call had worked and invent the answer.
        let advice = if any_error {
            "\nA call failed. Fix it and try again, or tell the user plainly that you could not get the information. Do not guess or make up an answer."
        } else {
            "\nThat is the real result. Continue the task using ONLY it; if it does not contain what you need, say so instead of guessing."
        };
        msgs.push(Msg::user(format!("{}{advice}", results.join("\n\n"))));

        let spent = usage.input + usage.output + child_tokens;
        if spent > max_tokens {
            rec.status = Status::OverBudget;
            rec.answer =
                format!("stopped: spent {spent} tokens, over the {max_tokens} this run may spend");
            break;
        }
    }
    if rec.answer.is_empty() && rec.status == Status::Ok {
        rec.answer = last_text;
    }
    finish(host, rec)
}

static RUN_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn finish(host: &dyn Host, mut rec: RunRecord) -> RunRecord {
    rec.finished = host.now();
    rec.finished_ms = host.now_ms();
    let _ = host.store().record_run(&rec);
    host.observe(Event::RunFinished(Box::new(rec.clone())));
    rec
}

/// Scripted `Host` for tests, here and in `runtime`'s tests.
#[cfg(test)]
pub mod testkit {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;
    use crate::kv::Kv;
    use crate::model::Usage;

    pub struct Fake {
        pub store: Store,
        pub kv: Kv,
        pub replies: Mutex<Vec<String>>,
        pub approve: bool,
        pub seen: Mutex<Vec<Vec<Msg>>>,
        pub asked: Mutex<Vec<String>>,
        /// (topic, payload) of every emit.
        pub emitted: Mutex<Vec<(String, String)>>,
        /// The cause every host call received, in order.
        pub causes: Mutex<Vec<Cause>>,
        pub tasks: Mutex<HashMap<String, TaskState>>,
        pub timers: Mutex<Vec<(String, u64, String)>>,
        /// The model refuses a prompt larger than this many characters.
        pub max_chars: Mutex<usize>,
    }

    impl Fake {
        pub fn new(tag: &str, replies: &[&str], approve: bool) -> Self {
            let d = crate::testutil::dir(&format!("ar-fake-{tag}"));
            Self {
                store: Store::open(&d).unwrap(),
                kv: Kv::open(&d).unwrap(),
                replies: Mutex::new(replies.iter().rev().map(|s| s.to_string()).collect()),
                approve,
                seen: Mutex::new(vec![]),
                asked: Mutex::new(vec![]),
                emitted: Mutex::new(vec![]),
                causes: Mutex::new(vec![]),
                tasks: Mutex::new(HashMap::new()),
                timers: Mutex::new(vec![]),
                max_chars: Mutex::new(usize::MAX),
            }
        }

        pub fn topics_emitted(&self) -> Vec<String> {
            self.emitted.lock().unwrap().iter().map(|(t, _)| t.clone()).collect()
        }
    }

    impl Host for Fake {
        fn now(&self) -> u64 {
            1_000
        }
        fn store(&self) -> &Store {
            &self.store
        }
        fn model(&self, _: &AgentSpec, _: &str, msgs: &[Msg]) -> Result<Reply, String> {
            if msgs.iter().map(|m| m.content.len()).sum::<usize>() > *self.max_chars.lock().unwrap()
            {
                return Err("500: the prompt exceeds the context window".into());
            }
            self.seen.lock().unwrap().push(msgs.to_vec());
            let text = self.replies.lock().unwrap().pop().ok_or("out of replies")?;
            Ok(Reply { text, usage: Usage { input: 10, output: 5 } })
        }
        fn approve(&self, _: &str, tool: &str, _: &Value, _: &[String]) -> bool {
            self.asked.lock().unwrap().push(tool.to_string());
            self.approve
        }
        fn call_agent(
            &self,
            _: &str,
            target: &str,
            m: &str,
            cause: &Cause,
        ) -> Result<(String, u64), String> {
            self.causes.lock().unwrap().push(cause.clone());
            Ok((format!("{target} says re: {m}"), 7))
        }
        fn spawn_task(
            &self,
            _: &str,
            target: &str,
            _: &str,
            cause: &Cause,
        ) -> Result<String, String> {
            self.causes.lock().unwrap().push(cause.clone());
            let id = format!("task-{target}");
            self.tasks.lock().unwrap().insert(id.clone(), TaskState::Running);
            Ok(id)
        }
        fn task_result(&self, id: &str) -> Option<TaskState> {
            self.tasks.lock().unwrap().get(id).cloned()
        }
        fn emit(
            &self,
            _: &str,
            topic: &str,
            payload: &str,
            cause: &Cause,
        ) -> Result<usize, String> {
            self.causes.lock().unwrap().push(cause.clone());
            self.emitted.lock().unwrap().push((topic.to_string(), payload.to_string()));
            Ok(1)
        }
        fn kv_get(&self, ns: &str, key: &str) -> Result<Option<Entry>, String> {
            self.kv.get(ns, key)
        }
        fn kv_list(&self, ns: &str, prefix: &str) -> Result<Vec<(String, Entry)>, String> {
            self.kv.list(ns, prefix)
        }
        fn kv_put(
            &self,
            by: &str,
            ns: &str,
            key: &str,
            value: &str,
            if_version: Option<u64>,
            _: &Cause,
        ) -> Result<Put, String> {
            self.kv.put(ns, key, value, by, 1_000, if_version)
        }
        fn schedule_self(
            &self,
            agent: &str,
            in_secs: u64,
            prompt: &str,
            _: &Cause,
        ) -> Result<(), String> {
            self.timers.lock().unwrap().push((agent.to_string(), in_secs, prompt.to_string()));
            Ok(())
        }
        fn transcribe(&self, audio: &[u8], lang: Option<&str>) -> Result<String, String> {
            Ok(format!("transcript of {} bytes in {}", audio.len(), lang.unwrap_or("default")))
        }
        fn speak(&self, text: &str, _: Option<&str>) -> Result<(Vec<u8>, u64), String> {
            Ok((format!("OggS fake audio of: {text}").into_bytes(), 1_500))
        }
        fn observe(&self, _: Event) {}
    }
}

#[cfg(test)]
mod tests {
    use super::testkit::Fake;
    use super::*;
    use crate::spec::Capability;

    fn spec() -> AgentSpec {
        let mut s = AgentSpec::new("bot", "tests things");
        s.capabilities.push(Capability::named("write_file"));
        s.capabilities.push(Capability::named("emit_event"));
        s.topics_out.push("*".into());
        s.capabilities.push(Capability::named("agent:helper"));
        s.capabilities.push(Capability {
            name: "poetry".into(),
            description: "write a haiku".into(),
            ..Default::default()
        });
        s
    }

    #[test]
    fn tool_call_parsing() {
        assert_eq!(parse_tool_call(r#"{"tool":"now"}"#).unwrap().0, "now");
        let (n, a) = parse_tool_call(
            "Sure!\n```json\n{\"tool\":\"remember\",\"args\":{\"text\":\"x\"}}\n```",
        )
        .unwrap();
        assert_eq!((n.as_str(), a["text"].as_str()), ("remember", Some("x")));
        assert!(parse_tool_call("just an answer").is_none());
        assert!(parse_tool_call(r#"{"answer":"{not a tool}"}"#).is_none());
        assert!(parse_tool_call(r#"it said {"tool": 5}"#).is_none());
    }

    #[test]
    fn a_run_uses_tools_then_answers_and_is_recorded() {
        let h = Fake::new(
            "loop",
            &[
                r#"{"tool":"remember","args":{"text":"user likes tea"}}"#,
                r#"{"tool":"recall","args":{"query":"tea"}}"#,
                "You like tea.",
            ],
            true,
        );
        let r = run(&h, &spec(), "http", "what do I like?", &Cause::default());
        assert_eq!(r.status, Status::Ok);
        assert_eq!(r.answer, "You like tea.");
        assert_eq!(r.steps.len(), 5);
        assert_eq!(r.tokens_in + r.tokens_out, 45);
        assert_eq!(h.store.memories("bot").len(), 1);
        assert_eq!(h.store.runs("bot", 5).len(), 1);
        let seen = h.seen.lock().unwrap();
        assert!(seen[2].last().unwrap().content.contains("user likes tea"));
    }

    #[test]
    fn a_capability_not_granted_cannot_be_called() {
        let h = Fake::new("cap", &[r#"{"tool":"http_get","args":{"url":"http://x"}}"#, "ok"], true);
        let r = run(&h, &spec(), "http", "go", &Cause::default());
        match &r.steps[1] {
            Step::Tool { error, result, .. } => {
                assert!(*error && result.contains("not one of your capabilities"))
            }
            s => panic!("{s:?}"),
        }
        assert!(h.asked.lock().unwrap().is_empty(), "never even asked for approval");
    }

    #[test]
    fn sensitive_tools_wait_for_approval_and_a_denial_reaches_the_model() {
        let call = r#"{"tool":"write_file","args":{"path":"a.txt","content":"hi"}}"#;
        let h = Fake::new("deny", &[call, "ok then"], false);
        let r = run(&h, &spec(), "http", "go", &Cause::default());
        assert_eq!(h.asked.lock().unwrap().as_slice(), ["write_file"]);
        assert!(matches!(&r.steps[1], Step::Tool { approved: Some(false), error: true, .. }));
        assert!(!h.store.workspace("bot").unwrap().join("a.txt").exists());

        let h = Fake::new("allow", &[call, "done"], true);
        run(&h, &spec(), "http", "go", &Cause::default());
        assert_eq!(
            std::fs::read_to_string(h.store.workspace("bot").unwrap().join("a.txt")).unwrap(),
            "hi"
        );

        let mut s = spec();
        s.auto_approve.push("write_file".into());
        let h = Fake::new("auto", &[call, "done"], false);
        run(&h, &s, "http", "go", &Cause::default());
        assert!(h.asked.lock().unwrap().is_empty());
        assert!(h.store.workspace("bot").unwrap().join("a.txt").exists());
    }

    #[test]
    fn edit_file_replaces_target_text_in_place() {
        let mut s = spec();
        s.capabilities.push(Capability::named("edit_file"));
        s.auto_approve.push("write_file".into());
        s.auto_approve.push("edit_file".into());
        let write_call =
            r#"{"tool":"write_file","args":{"path":"sub.txt","content":"hello world\n"}}"#;
        let edit_call = r#"{"tool":"edit_file","args":{"path":"sub.txt","target":"world","replacement":"friend"}}"#;
        let h = Fake::new("edit", &[write_call, edit_call, "done"], true);
        run(&h, &s, "http", "go", &Cause::default());
        assert_eq!(
            std::fs::read_to_string(h.store.workspace("bot").unwrap().join("sub.txt")).unwrap(),
            "hello friend\n"
        );
    }

    #[test]
    fn file_tools_cannot_escape_the_workspace() {
        let mut s = spec();
        s.auto_approve.push("write_file".into());
        let h = Fake::new(
            "esc",
            &[r#"{"tool":"write_file","args":{"path":"../pwn","content":"x"}}"#, "ok"],
            true,
        );
        let r = run(&h, &s, "http", "go", &Cause::default());
        assert!(matches!(&r.steps[1], Step::Tool { error: true, .. }));
    }

    #[test]
    fn delegation_and_events_go_through_the_host() {
        let h = Fake::new(
            "del",
            &[
                r#"{"tool":"agent:helper","args":{"message":"hi"}}"#,
                r#"{"tool":"emit_event","args":{"topic":"done"}}"#,
                "fin",
            ],
            true,
        );
        let r = run(&h, &spec(), "http", "go", &Cause::default());
        assert!(matches!(&r.steps[1], Step::Tool { result, .. } if result == "helper says re: hi"));
        assert_eq!(h.topics_emitted().as_slice(), ["done"]);
        assert_eq!(r.answer, "fin");
    }

    #[test]
    fn a_photo_attached_to_a_run_goes_to_the_model_with_the_first_message() {
        let s = spec();
        let h = Fake::new("attach", &["a plate of rice"], true);
        let inbox = h.store.workspace("bot").unwrap().join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        std::fs::write(inbox.join("1-lunch.png"), [0x89, b'P', b'N', b'G']).unwrap();
        let cause = Cause { attachments: vec!["1-lunch.png".into()], ..Cause::default() };
        let r = run(&h, &s, "http", "what is this?", &cause);
        assert_eq!(r.status, Status::Ok);
        assert_eq!(r.attachments, ["1-lunch.png"], "the record says what came with the run");
        let first = &h.seen.lock().unwrap()[0][0];
        assert_eq!(first.images.len(), 1);
        assert_eq!(first.images[0].media_type, "image/png");
        assert!(
            first.content.contains("Task: what is this?")
                && first.content.contains("(attached: 1 photo. Look at it.)"),
            "{}",
            first.content
        );
    }

    #[test]
    fn an_agent_with_vision_delegate_is_told_the_photo_is_there_and_is_sent_no_picture() {
        let mut s = spec();
        s.capabilities.push(Capability::named("vision_delegate"));
        let h = Fake::new("attach-jev", &["ok"], true);
        let inbox = h.store.workspace("bot").unwrap().join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        std::fs::write(inbox.join("1-lunch.png"), [0x89, b'P', b'N', b'G']).unwrap();
        let cause = Cause { attachments: vec!["1-lunch.png".into()], ..Cause::default() };
        run(&h, &s, "http", "lunch", &cause);
        let first = &h.seen.lock().unwrap()[0][0];
        assert!(first.images.is_empty(), "external tool reads it, not the model");
        assert!(
            first.content.contains("photo `1-lunch.png`")
                && first.content.contains("vision_delegate"),
            "{}",
            first.content
        );
    }

    #[test]
    fn an_attachment_that_cannot_be_used_is_said_not_fatal() {
        let s = spec();
        let h = Fake::new("attach-missing", &["I could not see it"], true);
        let cause = Cause {
            attachments: vec!["gone.jpg".into(), "../../etc/passwd".into()],
            ..Cause::default()
        };
        let r = run(&h, &s, "http", "what is this?", &cause);
        assert_eq!(r.status, Status::Ok, "the run still happens");
        let first = &h.seen.lock().unwrap()[0][0];
        assert!(first.images.is_empty());
        assert!(
            first.content.contains("(attachment: `gone.jpg` is not in your inbox)"),
            "{}",
            first.content
        );
        assert!(
            first.content.contains("`../../etc/passwd` is not in your inbox"),
            "a path is refused, not followed"
        );
        assert!(!first.content.contains("attached:"), "nothing was attached");
    }

    #[test]
    fn a_run_without_attachments_is_untouched() {
        let s = spec();
        let h = Fake::new("no-attach", &["ok"], true);
        run(&h, &s, "http", "hi", &Cause::default());
        let first = &h.seen.lock().unwrap()[0][0];
        assert!(
            first.images.is_empty() && first.content == "Trigger: http\nTask: hi",
            "{}",
            first.content
        );
    }

    #[test]
    fn a_looping_agent_is_cut_off_and_still_gets_a_final_turn() {
        let mut s = spec();
        s.max_steps = 2;
        let call = r#"{"tool":"now"}"#;
        let h = Fake::new("loopcut", &[call, call, "I give up"], true);
        let r = run(&h, &s, "http", "go", &Cause::default());
        assert_eq!(r.answer, "I give up");
        assert_eq!(r.status, Status::Ok);
    }

    #[test]
    fn token_budget_stops_a_run() {
        let mut s = spec();
        s.max_tokens = 20;
        let call = r#"{"tool":"now"}"#;
        let h = Fake::new("budget", &[call, call, call], true);
        let r = run(&h, &s, "http", "go", &Cause::default());
        assert_eq!(r.status, Status::OverBudget);
    }

    #[test]
    fn daily_budget_refuses_before_calling_the_model() {
        let mut s = spec();
        s.daily_token_budget = 50;
        let h = Fake::new("daily", &["a", "b"], true);
        run(&h, &s, "http", "go", &Cause::default()); // spends 15
        s.daily_token_budget = 10;
        let r = run(&h, &s, "http", "go", &Cause::default());
        assert_eq!(r.status, Status::OverBudget);
        assert_eq!(h.seen.lock().unwrap().len(), 1, "second run never reached the model");
    }

    #[test]
    fn text_capabilities_are_prompted_not_callable() {
        let h = Fake::new("txt", &["x"], true);
        let p = system_prompt(&h, &spec());
        assert!(p.contains("poetry: write a haiku"));
        assert!(!p.contains("- poetry {"));
        assert!(p.contains("agent:helper"));
    }

    #[test]
    fn http_get_enforces_the_allow_list() {
        let mut s = spec();
        assert!(http_get(&s, "http://example.com/", None)
            .unwrap_err()
            .contains("not in this agent's allowed hosts"));
        assert!(http_get(&s, "ftp://example.com", None).is_err());
        s.allow_hosts.push("example.com".into());
        assert!(http_get(&s, "http://user:pw@example.com/", None)
            .unwrap_err()
            .contains("credentials"));
        assert!(http_get(&s, "http://example.com.evil.test/", None).is_err());
    }

    #[test]
    fn an_api_key_comes_from_the_vault_reaches_only_its_own_host_and_never_the_model() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let here = format!("127.0.0.1:{port}");
        let secrets = std::collections::HashMap::from([
            ("svc", format!(r#"{{"host":"{here}","key":"s3cret\n"}}"#)),
            (
                "raw",
                format!(r#"{{"host":"{here}","header":"X-Api-Key","scheme":"","key":"raw1"}}"#),
            ),
            ("other", r#"{"host":"api.example.com","key":"nope"}"#.to_string()),
            ("bad", r#"{"host":"h","key":42}"#.to_string()),
        ]);
        std::thread::spawn(move || {
            for req in server.incoming_requests() {
                let headers: Vec<(String, String)> = req
                    .headers()
                    .iter()
                    .map(|h| (h.field.to_string().to_lowercase(), h.value.to_string()))
                    .collect();
                let header = |n: &str| headers.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone());
                let (code, body) = if let Some(r) =
                    req.url().strip_prefix("/api/internal/secret?ref=vault%3A%2F%2Fme%2F")
                {
                    match secrets.get(r) {
                        _ if header("x-fetch-token").as_deref() != Some("tok")
                            || header("x-fetch-nonce").is_none() =>
                        {
                            (401, String::new())
                        }
                        Some(v) => (200, v.clone()),
                        None if r == "denied" => (403, String::new()),
                        None => (404, String::new()),
                    }
                } else {
                    let got: Vec<String> = ["authorization", "x-api-key"]
                        .iter()
                        .filter_map(|n| header(n).map(|v| format!("{n}={v}")))
                        .collect();
                    (200, got.join(","))
                };
                let _ = req.respond(tiny_http::Response::from_string(body).with_status_code(code));
            }
        });
        let vault = crate::keys::Vault {
            url: format!("http://{here}"),
            org: "me".into(),
            token: "tok".into(),
        };
        let get = |keys: &[&str], url: &str, v: &crate::keys::Vault| {
            let mut s = spec();
            s.api_keys = keys.iter().map(|k| k.to_string()).collect();
            http_get(&s, url, Some(v))
        };
        let api = format!("http://{here}/x");
        let got = get(&["svc", "raw", "other"], &api, &vault).unwrap();
        assert!(got.contains("authorization=Bearer [api key]"), "{got}");
        assert!(got.contains("x-api-key=[api key]") && !got.contains("nope"), "{got}");
        assert!(!got.contains("s3cret") && !got.contains("raw1"));

        assert_eq!(get(&["missing"], &api, &vault).unwrap_err(), "no API key called missing");
        assert!(get(&["denied"], &api, &vault).unwrap_err().contains("does not grant"));
        assert!(!get(&["bad"], &api, &vault).unwrap_err().contains("42"));
        let stale = crate::keys::Vault { token: "old".into(), ..vault };
        assert!(get(&["svc"], &api, &stale).unwrap_err().contains("expired"));
        let mut s = spec();
        s.api_keys = vec!["svc".into()];
        assert!(http_get(&s, &api, None).unwrap_err().contains("HOLON_FETCH_TOKEN"));
    }

    #[test]
    fn http_get_gives_the_model_text_not_markup() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        std::thread::spawn(move || {
            for req in server.incoming_requests() {
                let html = "<html><head><script>x()</script></head><body><h1>Log</h1>\
                            <table><tr><td>10/05</td><td>12,108m</td></tr></table></body></html>";
                let h = tiny_http::Header::from_bytes("content-type", "text/html").unwrap();
                let _ = req.respond(tiny_http::Response::from_string(html).with_header(h));
            }
        });
        let mut s = spec();
        s.allow_hosts.push(format!("127.0.0.1:{port}"));
        let got = http_get(&s, &format!("http://127.0.0.1:{port}/page"), None).unwrap();
        assert_eq!(got, "HTTP 200 OK\nLog\n10/05 | 12,108m");
    }

    #[test]
    fn a_bare_string_or_sibling_fields_still_make_a_valid_call() {
        let (n, a) = parse_tool_call(r#"{"tool": "http_get", "url": "http://x/"}"#).unwrap();
        assert_eq!(normalize_args(&n, a), json!({"url": "http://x/"}));
        let (n, a) = parse_tool_call(r#"{"tool": "http_get", "args": "http://x/"}"#).unwrap();
        assert_eq!(normalize_args(&n, a), json!({"url": "http://x/"}));
        let (n, a) = parse_tool_call(r#"{"tool": "now", "args": null}"#).unwrap();
        assert_eq!(normalize_args(&n, a), json!({}));
        assert_eq!(normalize_args("agent:helper", json!("hi")), json!({"message": "hi"}));
    }

    #[test]
    fn a_call_missing_an_argument_is_told_how_to_make_it_and_never_asks_a_human() {
        let mut s = spec();
        s.capabilities.push(Capability::named("http_get"));
        let h = Fake::new("badargs", &[r#"{"tool":"http_get","args":{}}"#, "could not"], false);
        let r = run(&h, &s, "http", "go", &Cause::default());
        match &r.steps[1] {
            Step::Tool { error, result, approved, .. } => {
                assert!(*error && approved.is_none());
                assert!(
                    result.contains("missing url") && result.contains(r#"{"url": "..."}"#),
                    "{result}"
                );
            }
            s => panic!("{s:?}"),
        }
        assert!(h.asked.lock().unwrap().is_empty());
        let seen = h.seen.lock().unwrap();
        assert!(seen[1].last().unwrap().content.contains("Do not guess"));
    }

    #[test]
    fn a_string_arg_reaches_the_tool_in_a_real_run() {
        let h = Fake::new("strarg", &[r#"{"tool":"remember","args":"likes tea"}"#, "ok"], true);
        run(&h, &spec(), "http", "go", &Cause::default());
        assert_eq!(h.store.memories("bot")[0].text, "likes tea");
    }

    #[test]
    fn several_calls_in_one_reply_all_run_in_order() {
        let calls = json_calls(
            r#"first {"tool":"remember","args":{"text":"a"}} then {"tool":"emit_event","args":{"topic":"t"}}"#,
        );
        assert_eq!(
            calls.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(),
            ["remember", "emit_event"]
        );
        let h = Fake::new(
            "multi",
            &[
                r#"{"tool":"remember","args":{"text":"saw it"}} {"tool":"emit_event","args":{"topic":"seen"}}"#,
                "done",
            ],
            true,
        );
        let r = run(&h, &spec(), "http", "go", &Cause::default());
        assert_eq!(h.store.memories("bot")[0].text, "saw it");
        assert_eq!(h.topics_emitted().as_slice(), ["seen"]);
        assert_eq!(r.steps.iter().filter(|s| matches!(s, Step::Tool { .. })).count(), 2);
    }

    #[test]
    fn a_described_call_is_accepted_only_for_granted_tools() {
        let granted = ["remember", "emit_event"];
        let c = loose_calls(
            r#"remember "likes tea"  emit_event {"topic": "x", "payload": "y"}"#,
            &granted,
        );
        assert_eq!(c.len(), 2);
        assert_eq!((c[0].0.as_str(), &c[0].1), ("remember", &json!("likes tea")));
        assert_eq!(c[1].1["topic"], "x");
        // prose that merely mentions a tool, or an ungranted one, is not a call
        assert!(loose_calls("I will remember that for you.", &granted).is_empty());
        assert!(loose_calls(r#"http_get "http://x""#, &granted).is_empty());
        assert!(loose_calls(r#"unremember "x""#, &granted).is_empty());
    }

    #[test]
    fn a_run_follows_a_described_call_through_to_the_tool() {
        let h = Fake::new("loose", &[r#"remember "likes tea""#, "noted"], true);
        let r = run(&h, &spec(), "http", "go", &Cause::default());
        assert_eq!(h.store.memories("bot")[0].text, "likes tea");
        assert_eq!(r.answer, "noted");
    }

    #[test]
    fn an_oversized_tool_result_is_shrunk_and_retried_not_fatal() {
        // `now` is cheap; get a big result by remembering then recalling a huge note.
        let big = "x".repeat(6_000);
        let h = Fake::new(
            "ctx",
            &[r#"{"tool":"recall","args":{"query":"note"}}"#, "I read the note."],
            true,
        );
        h.store.remember("bot", &format!("note {big}"), 1).unwrap();
        *h.max_chars.lock().unwrap() = 3_000;
        let r = run(&h, &spec(), "http", "go", &Cause::default());
        assert_eq!(r.status, Status::Ok, "{}", r.answer);
        assert_eq!(r.answer, "I read the note.");
        let seen = h.seen.lock().unwrap();
        assert!(seen.last().unwrap().iter().any(|m| m.content.contains("shortened to fit")));
    }

    #[test]
    fn a_non_context_model_error_is_not_retried() {
        assert!(is_context_overflow("500: maximum context length exceeded"));
        assert!(is_context_overflow("Input is too long"));
        assert!(!is_context_overflow("401 unauthorized"));
        assert!(!is_context_overflow("model returned 429 rate limited"));
    }

    #[test]
    fn a_required_tool_turns_an_early_plain_text_reply_into_a_reminder() {
        let mut s = spec();
        s.must_call = vec!["remember".into()];
        let h = Fake::new(
            "must",
            &[
                "I found it: likes tea",
                r#"{"tool":"remember","args":{"text":"likes tea"}}"#,
                "all done",
            ],
            true,
        );
        let r = run(&h, &s, "http", "go", &Cause::default());
        assert_eq!(r.status, Status::Ok, "{}", r.answer);
        assert_eq!(r.answer, "all done");
        assert_eq!(h.store.memories("bot")[0].text, "likes tea");
        let seen = h.seen.lock().unwrap();
        assert!(seen[1].last().unwrap().content.contains("have not yet called remember"));
    }

    #[test]
    fn a_run_that_never_calls_its_required_tool_fails_visibly() {
        let mut s = spec();
        s.must_call = vec!["remember".into()];
        let h = Fake::new("never", &["note 1", "note 2", "note 3", "note 4"], true);
        let r = run(&h, &s, "http", "go", &Cause::default());
        assert_eq!(r.status, Status::Failed);
        assert!(r.answer.contains("never called required tool(s) remember"), "{}", r.answer);
        assert_eq!(
            h.seen.lock().unwrap().len(),
            3,
            "the original reply plus exactly two reminders"
        );
    }

    #[test]
    fn a_failed_call_does_not_count_as_calling_the_required_tool() {
        let mut s = spec();
        s.must_call = vec!["store_put".into()];
        s.capabilities.push(Capability::named("store_put"));
        // `private` namespace works; the first call omits the value, so it errors
        let h = Fake::new(
            "failedcall",
            &[
                r#"{"tool":"store_put","args":{"ns":"private","key":"k"}}"#,
                "done",
                r#"{"tool":"store_put","args":{"ns":"private","key":"k","value":"v"}}"#,
                "finished",
            ],
            true,
        );
        let r = run(&h, &s, "http", "go", &Cause::default());
        assert_eq!(r.status, Status::Ok, "{}", r.answer);
        assert_eq!(r.answer, "finished");
    }

    #[test]
    fn the_prompt_names_the_shared_stores_and_topics_the_agent_was_granted() {
        let mut s = spec();
        s.store.write = vec!["rowing".into()];
        s.store.read = vec!["notes".into(), "rowing".into()];
        s.topics_out = vec!["new-workout".into()];
        let h = Fake::new("reach", &["x"], true);
        let p = system_prompt(&h, &s);
        assert!(p.contains("rowing (read and write)"), "{p}");
        assert!(p.contains("notes (read only)"));
        assert!(!p.contains("rowing (read only)"), "write implies read; no duplicate");
        assert!(p.contains("Topics you may emit events to: new-workout"));
        let mut bare = spec();
        bare.topics_out.clear();
        assert!(!system_prompt(&h, &bare).contains("Shared stores"));
    }

    #[test]
    fn recent_runs_can_be_recalled_or_switched_off() {
        let h = Fake::new("recent", &["a", "b"], true);
        run(&h, &spec(), "http", "first question", &Cause::default());
        let with = system_prompt(&h, &spec());
        assert!(with.contains("Your last runs") && with.contains("first question"), "{with}");
        let mut off = spec();
        off.recent_runs = 0;
        assert!(
            !system_prompt(&h, &off).contains("Your last runs"),
            "0 means none, so a wrong answer cannot prime the next run"
        );
        off.recent_runs = 1;
        run(&h, &off, "http", "second question", &Cause::default());
        let one = system_prompt(&h, &off);
        assert!(one.contains("second question") && !one.contains("first question"));
    }

    #[test]
    fn a_bare_tool_name_and_one_argument_is_a_call_but_prose_is_not() {
        let granted = ["http_get", "remember"];
        let c = loose_calls("HTTP_GET https://example.com/joke", &granted);
        assert_eq!(c, vec![("http_get".to_string(), json!({"url": "https://example.com/joke"}))]);
        assert!(
            loose_calls("remember buy some milk", &granted).is_empty(),
            "spaces: this is prose"
        );
        assert!(loose_calls("http_get is a tool I could use", &granted).is_empty());
        assert!(
            loose_calls("HTTP_GET https://a.b/c\nand then I will say more", &granted).is_empty()
        );
        assert!(loose_calls("Here you go: http_get", &granted).is_empty());
    }

    #[test]
    fn a_connector_is_a_tool_backed_by_a_program_and_sensitive_ones_need_approval() {
        let mut s = spec();
        s.capabilities.push(Capability {
            name: "echo_back".into(),
            description: "Echo the args".into(),
            exec: Some(crate::connector::Exec {
                command: vec!["cat".into()],
                args: r#"{"what": "..."}"#.into(),
                required: vec!["what".into()],
                sensitive: true,
                timeout_secs: 5,
            }),
            ..Default::default()
        });
        let call = r#"{"tool":"echo_back","args":{"what":"hi"}}"#;
        let h = Fake::new("conn-deny", &[call, "ok"], false);
        let r = run(&h, &s, "http", "go", &Cause::default());
        assert_eq!(h.asked.lock().unwrap().as_slice(), ["echo_back"]);
        assert!(matches!(&r.steps[1], Step::Tool { approved: Some(false), error: true, .. }));

        let h = Fake::new("conn-allow", &[call, "done"], true);
        let r = run(&h, &s, "http", "go", &Cause::default());
        match &r.steps[1] {
            Step::Tool { result, error: false, .. } => assert_eq!(result, r#"{"what":"hi"}"#),
            other => panic!("{other:?}"),
        }
        // a missing required argument is explained, and no human is asked
        let h = Fake::new("conn-bad", &[r#"{"tool":"echo_back","args":{}}"#, "ok"], true);
        let r = run(&h, &s, "http", "go", &Cause::default());
        assert!(h.asked.lock().unwrap().is_empty());
        assert!(
            matches!(&r.steps[1], Step::Tool { error: true, result, .. } if result.contains("missing what"))
        );
    }

    #[test]
    fn transcribe_and_speak_work_on_the_agents_workspace_only() {
        let mut s = spec();
        s.capabilities.push(Capability::named("transcribe"));
        s.capabilities.push(Capability::named("speak"));
        let h = Fake::new(
            "speech",
            &[
                r#"{"tool":"speak","args":{"text":"Keep the pace steady.","path":"speech/a.ogg"}}"#,
                r#"{"tool":"transcribe","args":{"path":"speech/a.ogg","lang":"hu-HU"}}"#,
                r#"{"tool":"transcribe","args":{"path":"../outside.ogg"}}"#,
                r#"{"tool":"speak","args":{"text":"x"}}"#,
                "done",
            ],
            true,
        );
        let r = run(&h, &s, "http", "go", &Cause::default());
        let results: Vec<(bool, String)> = r
            .steps
            .iter()
            .filter_map(|st| match st {
                Step::Tool { error, result, .. } => Some((*error, result.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(results[0], (false, "wrote speech/a.ogg (1.5s of speech)".to_string()));
        let file = h.store.workspace("bot").unwrap().join("speech/a.ogg");
        assert!(std::fs::read_to_string(&file).unwrap().contains("Keep the pace steady."));
        assert_eq!(
            results[1],
            (
                false,
                format!("transcript of {} bytes in hu-HU", std::fs::metadata(&file).unwrap().len())
            )
        );
        assert!(results[2].0, "a path outside the workspace is refused: {:?}", results[2]);
        assert!(
            !results[3].0 && results[3].1.starts_with("wrote speech/"),
            "the default path is under speech/: {:?}",
            results[3]
        );
    }
}
