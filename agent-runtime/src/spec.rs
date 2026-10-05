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
    /// the event payload is the task input.
    Event { topic: String },
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
    /// Longest tool result, in characters, shown to the model; the rest is cut.
    /// Lower it for small-context models (Apple's on-device model has about 4k
    /// tokens, and a page of numbers costs far more than its length suggests).
    #[serde(default = "default_result_chars")]
    pub max_result_chars: usize,
    /// Tokens per rolling 24h across all runs; 0 = unlimited.
    #[serde(default)]
    pub daily_token_budget: u64,
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
            daily_token_budget: 0,
            paused: false,
        }
    }

    pub fn has_capability(&self, name: &str) -> bool {
        self.capabilities.iter().any(|c| c.name == name)
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
}
