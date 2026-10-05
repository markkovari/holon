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
use crate::spec::{glob_match, validate_topic, AgentSpec};
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
}

/// A spawned task's state, as `task_result` reports it.
#[derive(Clone, Debug, PartialEq)]
pub enum TaskState {
    Running,
    Done { ok: bool, answer: String },
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
    fn observe(&self, event: Event);
}

#[derive(Clone, Debug)]
pub enum Event {
    RunStarted {
        agent: String,
        run_id: String,
        trigger: String,
        input: String,
        trace_id: String,
        span_id: String,
        chain: Vec<String>,
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
}

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
        about: "The current date and time (UTC).",
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
        name: "http_get",
        args: r#"{"url": "..."}"#,
        about: "Fetch a URL (only hosts you were allowed).",
        sensitive: true,
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
        name: "write_file",
        args: r#"{"path": "...", "content": "..."}"#,
        about: "Write a text file in your workspace.",
        sensitive: true,
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
        "http_get" => &["url"],
        "read_file" => &["path"],
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
    let mut tools = String::new();
    let mut abilities = String::new();
    for c in &spec.capabilities {
        let wit = c.wit.as_deref().map(|w| format!(" [{w}]")).unwrap_or_default();
        if let Some(t) = tool_def(&c.name) {
            let about = if c.description.is_empty() { t.about } else { &c.description };
            tools.push_str(&format!("- {} {} — {about}{wit}\n", t.name, t.args));
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
    let memories = host.store().recall(&spec.name, "", 5);
    if !memories.is_empty() {
        s.push_str("Things you remembered earlier (newest first):\n");
        for m in memories {
            s.push_str(&format!("- {}\n", clip(&m.text, 300)));
        }
        s.push('\n');
    }
    let runs = host.store().runs(&spec.name, 3);
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
    found.into_iter().map(|(_, n, a)| (n, a)).collect()
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

fn http_get(spec: &AgentSpec, url: &str) -> Result<String, String> {
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
    if !spec.allow_hosts.iter().any(|a| a == authority || a == host) {
        return Err(format!("{authority} is not in this agent's allowed hosts"));
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none()) // a redirect could leave the allow-list
        .user_agent("holon-agent/0.1")
        .build()
        .map_err(|e| e.to_string())?;
    let r = client.get(url).send().map_err(|e| e.to_string())?;
    let status = r.status();
    let is_html = r
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("html"));
    let body = r.text().map_err(|e| e.to_string())?;
    // Markup is noise to a model and eats its context; give it the text.
    let body = if is_html { crate::html::html_to_text(&body) } else { body };
    Ok(format!("HTTP {status}\n{}", clip(&body, 8_000)))
}

/// Runs one tool. `Err` is returned to the model as the tool's result, so it
/// can adapt, exactly like a denial is.
/// Runs one tool. `Err` is returned to the model as the tool's result, so it
/// can adapt, exactly like a denial is. `cause` is THIS run, so anything it
/// wakes records it as the parent. `child_tokens` accumulates what delegated
/// agents spent, which counts against this run's budget.
fn exec(
    host: &dyn Host,
    spec: &AgentSpec,
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
            let hits = store.recall(&spec.name, &arg(args, "query"), 5);
            Ok(if hits.is_empty() {
                "nothing matched".into()
            } else {
                hits.iter().map(|m| format!("- {}", m.text)).collect::<Vec<_>>().join("\n")
            })
        }
        "now" => Ok(host.now().to_string()),
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
        "http_get" => http_get(spec, &arg(args, "url")),
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
            None => Err(format!("no such tool: {other}")),
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
    let mut msgs = vec![Msg::user(format!("Trigger: {trigger}\nTask: {input}"))];
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
            rec.answer = reply.text;
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
            } else if let Err(e) = check_args(&name, &args) {
                // Malformed: never bother a human to approve a call that cannot run.
                (e, true, None)
            } else {
                let needs = tool_def(&name).is_some_and(|t| t.sensitive)
                    && !spec.auto_approve.contains(&name);
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
                    };
                    match exec(host, spec, &name, &args, &me, &mut child_tokens) {
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
            wit: None,
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
        // the tool result went back to the model
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

        // auto_approve skips the question entirely
        let mut s = spec();
        s.auto_approve.push("write_file".into());
        let h = Fake::new("auto", &[call, "done"], false);
        run(&h, &s, "http", "go", &Cause::default());
        assert!(h.asked.lock().unwrap().is_empty());
        assert!(h.store.workspace("bot").unwrap().join("a.txt").exists());
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
        assert!(http_get(&s, "http://example.com/")
            .unwrap_err()
            .contains("not in this agent's allowed hosts"));
        assert!(http_get(&s, "ftp://example.com").is_err());
        s.allow_hosts.push("example.com".into());
        assert!(http_get(&s, "http://user:pw@example.com/").unwrap_err().contains("credentials"));
        assert!(http_get(&s, "http://example.com.evil.test/").is_err());
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
        let got = http_get(&s, &format!("http://127.0.0.1:{port}/page")).unwrap();
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
        // and says nothing when nothing was granted
        let mut bare = spec();
        bare.topics_out.clear();
        assert!(!system_prompt(&h, &bare).contains("Shared stores"));
    }
}
