//! What an agent IS: a name, a description, the capabilities it may use, how
//! it is triggered, and which model drives it. Persisted as one JSON file per
//! agent, so editing an agent is editing a file — nothing is rebuilt.

use serde::{Deserialize, Serialize};

/// One thing an agent may do. `description` is what the MODEL reads when
/// choosing; `wit` is what the PLATFORM can check — a ref like
/// `os:fs/watcher.changes` naming the interface function this binds to. Text
/// alone is unenforceable; WIT alone is not something a model can choose
/// from. `wit` is optional so a capability can start as prose.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Capability {
    /// Matches a built-in tool name (`remember`, `http_get`, ...) or an
    /// agent name prefixed `agent:` to delegate to another agent.
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wit: Option<String>,
}

impl Capability {
    pub fn named(name: &str) -> Self {
        Self { name: name.to_string(), description: String::new(), wit: None }
    }
}

/// When an agent runs without anyone typing at it. Every agent ALSO answers
/// over HTTP through the lattice ingress (the gateway component) — that
/// trigger needs no declaration.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Trigger {
    /// `cron` is a 5-field expression (UTC), an `@hourly`-style macro, or
    /// `@every 30s|5m|2h`. `prompt` is the task given to the agent.
    Schedule { cron: String, prompt: String },
    /// Runs when `topic` is emitted (by another agent, or `POST /events/<topic>`);
    /// the event payload is the task input. Durable: events published while the
    /// agent is paused or the runtime is down are delivered later, in order.
    /// `filter`, when set, only wakes the agent for payloads containing that
    /// text (case-insensitive).
    Event {
        topic: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filter: Option<String>,
    },
    /// Runs when a `store_put` CHANGES a value in shared store `ns` (an
    /// identical write wakes nobody) under `key_prefix`. The task input is a
    /// JSON object `{ns, key, version, value, by}`. This is how an agent can
    /// say "tell me when X changes" without anyone remembering to announce it.
    StoreChange {
        ns: String,
        #[serde(default)]
        key_prefix: String,
    },
}

impl Trigger {
    /// The bus topic this trigger listens on, if it is bus-driven.
    pub fn topic(&self) -> Option<String> {
        match self {
            Trigger::Event { topic, .. } => Some(topic.clone()),
            Trigger::StoreChange { ns, .. } => Some(format!("store.{ns}")),
            Trigger::Schedule { .. } => None,
        }
    }

    /// Does an envelope on this trigger's topic actually concern it?
    pub fn matches(&self, payload: &str) -> bool {
        match self {
            Trigger::Event { filter, .. } => {
                filter.as_deref().is_none_or(|f| payload.to_lowercase().contains(&f.to_lowercase()))
            }
            Trigger::StoreChange { key_prefix, .. } => {
                serde_json::from_str::<serde_json::Value>(payload)
                    .ok()
                    .and_then(|v| v["key"].as_str().map(|k| k.starts_with(key_prefix.as_str())))
                    .unwrap_or(false)
            }
            Trigger::Schedule { .. } => false,
        }
    }
}

/// Which SHARED store namespaces an agent may touch. Its own `private`
/// namespace is always readable and writable and needs no entry here; write
/// implies read.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct StoreAccess {
    #[serde(default)]
    pub read: Vec<String>,
    #[serde(default)]
    pub write: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModelSpec {
    /// The runtime's local model (Apple `fm serve`, or any OpenAI-compatible
    /// server it was pointed at).
    Local,
    /// Any OpenAI-compatible `/v1/chat/completions` server. The key is read
    /// from the environment variable named here, never stored in the spec.
    OpenAi {
        base_url: String,
        model: String,
        #[serde(default)]
        api_key_env: String,
    },
    /// Anthropic's Messages API. Same rule for the key.
    Anthropic { model: String, api_key_env: String },
    /// Scripted replies, for tests.
    Mock { replies: Vec<String> },
}

fn default_steps() -> u32 {
    8
}
fn default_tokens() -> u64 {
    20_000
}
fn default_rate() -> u32 {
    30
}
fn default_result_chars() -> usize {
    8_000
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct AgentSpec {
    pub name: String,
    /// The agent's purpose; becomes the system prompt.
    pub description: String,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    /// Tool names that may run without asking a human. Anything sensitive
    /// (`write_file`, `http_get`) that is NOT listed waits for approval.
    #[serde(default)]
    pub auto_approve: Vec<String>,
    /// Hosts `http_get` may reach. Empty = none; there is no wildcard.
    #[serde(default)]
    pub allow_hosts: Vec<String>,
    #[serde(default)]
    pub triggers: Vec<Trigger>,
    #[serde(default = "default_model")]
    pub model: ModelSpec,
    /// Model calls per run before the agent is told to answer now.
    #[serde(default = "default_steps")]
    pub max_steps: u32,
    /// Tokens (in + out) one run may spend before it is stopped.
    #[serde(default = "default_tokens")]
    pub max_tokens: u64,
    /// Tools every run must call (successfully) before its plain-text reply
    /// counts as finished. A model that writes an intermediate note as prose
    /// otherwise ends its own task early; this turns "do these steps" into
    /// something the runtime checks. A reply that comes too soon is answered
    /// with a reminder (twice); a run that still never calls them FAILS, so
    /// the gap is visible rather than a quiet no-op.
    #[serde(default)]
    pub must_call: Vec<String>,
    /// Longest tool result, in characters, shown to the model; the rest is cut.
    /// Lower it for small-context models (Apple's on-device model has about 4k
    /// tokens, and a page of numbers costs far more than its length suggests).
    #[serde(default = "default_result_chars")]
    pub max_result_chars: usize,
    /// Tokens per rolling 24h across all runs; 0 = unlimited.
    #[serde(default)]
    pub daily_token_budget: u64,
    /// Topics this agent may `emit_event` to: exact names or globs with `*`
    /// (`deploy.*`). Empty means it may emit nothing — waking another agent
    /// is something a spec grants, not a side effect of having the tool.
    #[serde(default)]
    pub topics_out: Vec<String>,
    /// Shared store namespaces this agent may read/write.
    #[serde(default)]
    pub store: StoreAccess,
    /// Runs this agent will start per rolling minute, whatever woke it; 0 =
    /// unlimited. A flood of events is dropped (and logged), not queued
    /// forever behind a model that takes seconds per call.
    #[serde(default = "default_rate")]
    pub max_runs_per_min: u32,
    /// A paused agent fires no schedule or event triggers and refuses HTTP.
    #[serde(default)]
    pub paused: bool,
}

fn default_model() -> ModelSpec {
    ModelSpec::Local
}

impl AgentSpec {
    pub fn new(name: &str, description: &str) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            capabilities: vec![
                Capability::named("remember"),
                Capability::named("recall"),
                Capability::named("now"),
            ],
            auto_approve: Vec::new(),
            allow_hosts: Vec::new(),
            triggers: Vec::new(),
            model: ModelSpec::Local,
            max_steps: default_steps(),
            max_tokens: default_tokens(),
            max_result_chars: default_result_chars(),
            must_call: Vec::new(),
            topics_out: Vec::new(),
            store: StoreAccess::default(),
            max_runs_per_min: default_rate(),
            daily_token_budget: 0,
            paused: false,
        }
    }

    pub fn has_capability(&self, name: &str) -> bool {
        self.capabilities.iter().any(|c| c.name == name)
    }
}

/// `*` matches any run of characters (including none); everything else is
/// literal. Enough for topic names like `deploy.*` without a regex engine.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !text.starts_with(first) || !text[first.len()..].ends_with(last) {
        return false;
    }
    if text.len() < first.len() + last.len() {
        return false;
    }
    let mut rest = &text[first.len()..text.len() - last.len()];
    for mid in &parts[1..parts.len() - 1] {
        match rest.find(mid) {
            Some(i) => rest = &rest[i + mid.len()..],
            None => return false,
        }
    }
    true
}

/// Topic and store-namespace names: lowercase letters, digits, `.`, `-`, `_`.
/// They become file names, so nothing path-like gets through.
pub fn validate_topic(t: &str) -> Result<(), String> {
    let ok = !t.is_empty()
        && t.len() <= 64
        && !t.starts_with('.')
        && !t.contains("..")
        && t.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-' | '_'));
    if ok {
        Ok(())
    } else {
        Err(format!("`{t}`: use up to 64 of a-z 0-9 . - _ (no leading dot, no ..)"))
    }
}

/// WIT-kebab-safe, and safe as a file name and a host label: lowercase
/// letters/digits, dash-separated, every word starting with a letter.
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("name is empty".into());
    }
    if name.len() > 40 {
        return Err("name is longer than 40 characters".into());
    }
    let ok = name.split('-').all(|w| {
        let mut c = w.chars();
        c.next().is_some_and(|f| f.is_ascii_lowercase())
            && c.all(|x| x.is_ascii_lowercase() || x.is_ascii_digit())
    });
    if ok {
        Ok(())
    } else {
        Err(format!(
            "{name}: use lowercase words separated by single dashes, each starting with a letter"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(validate_name("weather-bot").is_ok());
        assert!(validate_name("agent-1").is_err());
        assert!(validate_name("Bot").is_err());
        assert!(validate_name("a--b").is_err());
        assert!(validate_name("../x").is_err());
    }

    #[test]
    fn spec_round_trips_and_defaults() {
        let j = r#"{"name":"a","description":"d","triggers":[{"kind":"schedule","cron":"@every 5m","prompt":"p"}]}"#;
        let s: AgentSpec = serde_json::from_str(j).unwrap();
        assert_eq!(s.max_steps, 8);
        assert_eq!(s.model, ModelSpec::Local);
        let back: AgentSpec = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn globs() {
        assert!(glob_match("deploy.*", "deploy.prod"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("a*c", "abc") && glob_match("a*c", "ac"));
        assert!(glob_match("a*b*c", "a-x-b-y-c"));
        assert!(!glob_match("a*c", "ab"));
        assert!(!glob_match("deploy", "deploy.prod"));
        assert!(!glob_match("ab*ba", "aba"), "prefix and suffix must not overlap");
    }

    #[test]
    fn topic_names_are_file_safe() {
        assert!(validate_topic("new-workout").is_ok());
        assert!(validate_topic("store.rowing").is_ok());
        for bad in ["", ".x", "a..b", "a/b", "A", "x y", &"a".repeat(65)] {
            assert!(validate_topic(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn triggers_know_their_topic_and_what_concerns_them() {
        let e = Trigger::Event { topic: "deploy".into(), filter: Some("PROD".into()) };
        assert_eq!(e.topic().as_deref(), Some("deploy"));
        assert!(e.matches("shipped to prod"));
        assert!(!e.matches("shipped to staging"));
        let w = Trigger::StoreChange { ns: "rowing".into(), key_prefix: "latest".into() };
        assert_eq!(w.topic().as_deref(), Some("store.rowing"));
        assert!(w.matches(r#"{"key":"latest/row","value":"x"}"#));
        assert!(!w.matches(r#"{"key":"other","value":"x"}"#));
        assert!(!w.matches("not json"));
    }
}
