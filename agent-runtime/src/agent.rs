//! One run of one agent: a bounded loop of model turns and tool calls.
//!
//! Tool use is a JSON protocol in plain text rather than a provider's native
//! tool-calling, so it behaves the same on Apple's on-device model, an
//! OpenAI-compatible server and Anthropic. The model replies with ONE JSON
//! object `{"tool": "...", "args": {...}}` to act, or with plain text to
//! finish. Everything is bounded: steps, tokens per run, tokens per day.

use std::time::Duration;

use serde_json::{json, Value};

use crate::model::{Msg, Reply, Usage};
use crate::spec::AgentSpec;
use crate::store::{confine, RunRecord, Status, Step, Store};

/// What the loop needs from whoever runs it. The runtime implements this for
/// real; tests implement it with fakes.
pub trait Host: Sync {
    fn now(&self) -> u64;
    fn store(&self) -> &Store;
    fn model(&self, spec: &AgentSpec, system: &str, msgs: &[Msg]) -> Result<Reply, String>;
    /// Blocks until a human answers, or denies on timeout.
    fn approve(&self, agent: &str, tool: &str, args: &Value) -> bool;
    fn call_agent(
        &self,
        caller: &str,
        target: &str,
        message: &str,
        depth: u32,
    ) -> Result<String, String>;
    fn emit(&self, from: &str, topic: &str, payload: &str, depth: u32);
    fn observe(&self, event: Event);
}

#[derive(Clone, Debug)]
pub enum Event {
    RunStarted { agent: String, run_id: String, trigger: String, input: String },
    StepDone { agent: String, run_id: String, step: Step },
    RunFinished(Box<RunRecord>),
    ApprovalRequested { id: u64, agent: String, tool: String, args: Value },
    ApprovalResolved { id: u64, approved: bool },
}

pub const MAX_DEPTH: u32 = 3;

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
             your answer, reply in plain text with no JSON.\n\nTools:\n",
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

/// The first JSON object in `text` that has a string `tool` field.
pub fn parse_tool_call(text: &str) -> Option<(String, Value)> {
    let start = text.find('{')?;
    let v: Value =
        serde_json::Deserializer::from_str(&text[start..]).into_iter::<Value>().next()?.ok()?;
    let name = v.get("tool")?.as_str()?.to_string();
    Some((name, v.get("args").cloned().unwrap_or_else(|| json!({}))))
}

fn arg(args: &Value, k: &str) -> String {
    args.get(k).and_then(Value::as_str).unwrap_or_default().to_string()
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
        .build()
        .map_err(|e| e.to_string())?;
    let r = client.get(url).send().map_err(|e| e.to_string())?;
    let status = r.status();
    let body = r.text().map_err(|e| e.to_string())?;
    Ok(format!("HTTP {status}\n{}", clip(&body, 8_000)))
}

/// Runs one tool. `Err` is returned to the model as the tool's result, so it
/// can adapt, exactly like a denial is.
fn exec(
    host: &dyn Host,
    spec: &AgentSpec,
    name: &str,
    args: &Value,
    depth: u32,
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
            if topic.is_empty() {
                return Err("`topic` is empty".into());
            }
            host.emit(&spec.name, &topic, &arg(args, "payload"), depth + 1);
            Ok(format!("emitted {topic}"))
        }
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
            Some(target) => host.call_agent(&spec.name, target, &arg(args, "message"), depth + 1),
            None => Err(format!("no such tool: {other}")),
        },
    }
}

pub fn run(host: &dyn Host, spec: &AgentSpec, trigger: &str, input: &str, depth: u32) -> RunRecord {
    let started = host.now();
    let run_id = format!(
        "{}-{}-{}",
        spec.name,
        started,
        RUN_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    host.observe(Event::RunStarted {
        agent: spec.name.clone(),
        run_id: run_id.clone(),
        trigger: trigger.to_string(),
        input: input.to_string(),
    });

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
    };
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

    for turn in 0..=spec.max_steps {
        // The final turn forbids tools, so a run always ends in an answer.
        let last_turn = turn == spec.max_steps;
        if last_turn {
            msgs.push(Msg::user("You are out of steps. Give your final answer now, in plain text, with no tool call."));
        }
        let reply = match host.model(spec, &system, &msgs) {
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
        push(&mut rec, Step::Model { text: reply.text.clone() });
        last_text = reply.text.clone();

        let call = if last_turn { None } else { parse_tool_call(&reply.text) };
        let Some((name, args)) = call else {
            rec.answer = reply.text;
            break;
        };

        let (result, error, approved) = if !spec.has_capability(&name) {
            (format!("`{name}` is not one of your capabilities"), true, None)
        } else {
            let needs =
                tool_def(&name).is_some_and(|t| t.sensitive) && !spec.auto_approve.contains(&name);
            let ok = !needs || host.approve(&spec.name, &name, &args);
            if ok {
                match exec(host, spec, &name, &args, depth) {
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
            },
        );
        msgs.push(Msg::assistant(reply.text));
        msgs.push(Msg::user(format!(
            "Result of {name}{}:\n{}",
            if error { " (error)" } else { "" },
            clip(&result, 4_000)
        )));

        if usage.input + usage.output > spec.max_tokens {
            rec.status = Status::OverBudget;
            rec.answer = format!(
                "stopped: spent {} tokens, over this agent's {} per run",
                usage.input + usage.output,
                spec.max_tokens
            );
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
    let _ = host.store().record_run(&rec);
    host.observe(Event::RunFinished(Box::new(rec.clone())));
    rec
}

/// Scripted `Host` for tests, here and in `runtime`'s tests.
#[cfg(test)]
pub mod testkit {
    use std::sync::Mutex;

    use super::*;
    use crate::model::Usage;

    pub struct Fake {
        pub store: Store,
        pub replies: Mutex<Vec<String>>,
        pub approve: bool,
        pub seen: Mutex<Vec<Vec<Msg>>>,
        pub asked: Mutex<Vec<String>>,
        pub emitted: Mutex<Vec<String>>,
    }

    impl Fake {
        pub fn new(tag: &str, replies: &[&str], approve: bool) -> Self {
            let d = crate::testutil::dir("ar-fake");
            Self {
                store: Store::open(d).unwrap(),
                replies: Mutex::new(replies.iter().rev().map(|s| s.to_string()).collect()),
                approve,
                seen: Mutex::new(vec![]),
                asked: Mutex::new(vec![]),
                emitted: Mutex::new(vec![]),
            }
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
            self.seen.lock().unwrap().push(msgs.to_vec());
            let text = self.replies.lock().unwrap().pop().ok_or("out of replies")?;
            Ok(Reply { text, usage: Usage { input: 10, output: 5 } })
        }
        fn approve(&self, _: &str, tool: &str, _: &Value) -> bool {
            self.asked.lock().unwrap().push(tool.to_string());
            self.approve
        }
        fn call_agent(&self, _: &str, target: &str, m: &str, _: u32) -> Result<String, String> {
            Ok(format!("{target} says re: {m}"))
        }
        fn emit(&self, _: &str, topic: &str, _: &str, _: u32) {
            self.emitted.lock().unwrap().push(topic.to_string());
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
        let r = run(&h, &spec(), "http", "what do I like?", 0);
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
        let r = run(&h, &spec(), "http", "go", 0);
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
        let r = run(&h, &spec(), "http", "go", 0);
        assert_eq!(h.asked.lock().unwrap().as_slice(), ["write_file"]);
        assert!(matches!(&r.steps[1], Step::Tool { approved: Some(false), error: true, .. }));
        assert!(!h.store.workspace("bot").unwrap().join("a.txt").exists());

        let h = Fake::new("allow", &[call, "done"], true);
        run(&h, &spec(), "http", "go", 0);
        assert_eq!(
            std::fs::read_to_string(h.store.workspace("bot").unwrap().join("a.txt")).unwrap(),
            "hi"
        );

        // auto_approve skips the question entirely
        let mut s = spec();
        s.auto_approve.push("write_file".into());
        let h = Fake::new("auto", &[call, "done"], false);
        run(&h, &s, "http", "go", 0);
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
        let r = run(&h, &s, "http", "go", 0);
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
        let r = run(&h, &spec(), "http", "go", 0);
        assert!(matches!(&r.steps[1], Step::Tool { result, .. } if result == "helper says re: hi"));
        assert_eq!(h.emitted.lock().unwrap().as_slice(), ["done"]);
        assert_eq!(r.answer, "fin");
    }

    #[test]
    fn a_looping_agent_is_cut_off_and_still_gets_a_final_turn() {
        let mut s = spec();
        s.max_steps = 2;
        let call = r#"{"tool":"now"}"#;
        let h = Fake::new("loopcut", &[call, call, "I give up"], true);
        let r = run(&h, &s, "http", "go", 0);
        assert_eq!(r.answer, "I give up");
        assert_eq!(r.status, Status::Ok);
    }

    #[test]
    fn token_budget_stops_a_run() {
        let mut s = spec();
        s.max_tokens = 20;
        let call = r#"{"tool":"now"}"#;
        let h = Fake::new("budget", &[call, call, call], true);
        let r = run(&h, &s, "http", "go", 0);
        assert_eq!(r.status, Status::OverBudget);
    }

    #[test]
    fn daily_budget_refuses_before_calling_the_model() {
        let mut s = spec();
        s.daily_token_budget = 50;
        let h = Fake::new("daily", &["a", "b"], true);
        run(&h, &s, "http", "go", 0); // spends 15
        s.daily_token_budget = 10;
        let r = run(&h, &s, "http", "go", 0);
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
}
