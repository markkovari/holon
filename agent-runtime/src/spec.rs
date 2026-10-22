//! One JSON file per agent, so editing an agent rebuilds nothing.

use serde::{Deserialize, Serialize};

/// `description` is what the model chooses by; `wit` (e.g. `os:fs/watcher.changes`) is what the
/// platform can check, optional so a capability can start as prose.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Capability {
    /// A built-in tool name, or `agent:<name>` to delegate to another agent.
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wit: Option<String>,
    /// Or filled in from `<state>/connectors/<name>.json`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec: Option<crate::connector::Exec>,
}

impl Capability {
    pub fn named(name: &str) -> Self {
        Self { name: name.to_string(), ..Default::default() }
    }
}

/// HTTP through the gateway needs no trigger: every agent answers it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Trigger {
    /// `cron`: 5-field UTC, an `@hourly`-style macro, or `@every 30s|5m|2h`. `notify` only asks a
    /// chat bridge to post the result; the run is logged either way.
    Schedule {
        cron: String,
        prompt: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        notify: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
    },
    /// Durable: events published while paused or down arrive later, in order. `filter` is a
    /// case-insensitive substring.
    Event {
        topic: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filter: Option<String>,
    },
    /// Fires when a write CHANGES a value under `key_prefix` (an identical write wakes nobody).
    StoreChange {
        ns: String,
        #[serde(default)]
        key_prefix: String,
    },
}

impl Trigger {
    pub fn topic(&self) -> Option<String> {
        match self {
            Trigger::Event { topic, .. } => Some(topic.clone()),
            Trigger::StoreChange { ns, .. } => Some(format!("store.{ns}")),
            Trigger::Schedule { .. } => None,
        }
    }

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

/// Shared namespaces only: its own `private` one needs no entry. Write implies read.
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
    Local,
    /// Keys are read from the environment or a file, never stored in the spec.
    OpenAi {
        base_url: String,
        model: String,
        #[serde(default)]
        api_key_env: String,
        /// Read at each call; wins over `api_key_env`.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        api_key_file: String,
        /// launchd label (`io.holon.qwen`): started on first use, stopped when idle.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        service: String,
        /// A model without it is told a photo was attached instead (better than a 400).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        vision: bool,
    },
    Anthropic {
        model: String,
        api_key_env: String,
    },
    /// The first that answers wins; one that errors is skipped for this call only.
    Fallback {
        models: Vec<ModelSpec>,
    },
    Mock {
        replies: Vec<String>,
    },
}

impl ModelSpec {
    /// What a sidebar shows: the model's own name, and for a chain the first it tries and how many
    /// stand behind it.
    pub fn label(&self) -> String {
        match self {
            ModelSpec::Local => "local".into(),
            ModelSpec::OpenAi { model, .. } | ModelSpec::Anthropic { model, .. } => model.clone(),
            ModelSpec::Fallback { models } => match models.split_first() {
                Some((first, [])) => first.label(),
                Some((first, rest)) => format!("{} (+{} fallback)", first.label(), rest.len()),
                None => "fallback (empty)".into(),
            },
            ModelSpec::Mock { .. } => "mock".into(),
        }
    }
}

fn default_steps() -> u32 {
    8
}
fn default_tokens() -> u64 {
    20_000
}
fn default_recent_runs() -> usize {
    3
}
fn default_rate() -> u32 {
    30
}
fn default_result_chars() -> usize {
    8_000
}

/// What `session_start` may open (ADR-0119). The spec author's, never a tool argument.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SessionPolicy {
    /// The agent picks one of these (or `own`); it never writes a path of its own.
    #[serde(default)]
    pub workspaces: Vec<String>,
    /// Empty: the runtime's `--session-model`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    #[serde(default = "default_session_budget")]
    pub budget_usd: f64,
    /// Empty: every call waits for a person.
    #[serde(default)]
    pub auto_approve: Vec<String>,
    #[serde(default = "default_session_concurrency")]
    pub max_concurrent: u32,
}

fn default_session_budget() -> f64 {
    1.0
}
fn default_session_concurrency() -> u32 {
    2
}

impl Default for SessionPolicy {
    fn default() -> Self {
        Self {
            workspaces: Vec::new(),
            model: String::new(),
            budget_usd: default_session_budget(),
            auto_approve: Vec::new(),
            max_concurrent: default_session_concurrency(),
        }
    }
}

/// Must match `reconciler/daemons/agentd/src/tools.rs`.
pub const SESSION_TOOLS: [&str; 4] = ["list_dir", "read_file", "write_file", "run"];

impl SessionPolicy {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn validate(&self) -> Result<(), String> {
        if !(self.budget_usd > 0.0 && self.budget_usd <= 100.0) {
            return Err("sessions.budget_usd must be above 0 and at most 100".into());
        }
        if !(1..=8).contains(&self.max_concurrent) {
            return Err("sessions.max_concurrent must be 1-8".into());
        }
        for t in &self.auto_approve {
            if !SESSION_TOOLS.contains(&t.as_str()) {
                return Err(format!(
                    "sessions.auto_approve `{t}` is not a session tool ({})",
                    SESSION_TOOLS.join(", ")
                ));
            }
        }
        for w in &self.workspaces {
            if !std::path::Path::new(w).is_absolute() {
                return Err(format!("sessions.workspaces `{w}` must be an absolute path"));
            }
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct AgentSpec {
    pub name: String,
    /// Becomes the system prompt.
    pub description: String,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    /// A sensitive tool not listed here waits for approval.
    #[serde(default)]
    pub auto_approve: Vec<String>,
    /// Empty = none; there is no wildcard.
    #[serde(default)]
    pub allow_hosts: Vec<String>,
    /// Vault names (`keys::Vault`); `http_get` sends each only to the host its secret names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub api_keys: Vec<String>,
    #[serde(default)]
    pub triggers: Vec<Trigger>,
    #[serde(default = "default_model")]
    pub model: ModelSpec,
    #[serde(default = "default_steps")]
    pub max_steps: u32,
    #[serde(default = "default_tokens")]
    pub max_tokens: u64,
    /// 0 for an agent that must look answers up: a small model repeats a wrong earlier one it sees.
    #[serde(default = "default_recent_runs")]
    pub recent_runs: usize,
    /// A reply before these were called gets two reminders, then the run FAILS rather than
    /// quietly ending early.
    #[serde(default)]
    pub must_call: Vec<String>,
    /// Lower it for small-context models (Apple's on-device one has about 4k tokens).
    #[serde(default = "default_result_chars")]
    pub max_result_chars: usize,
    /// Rolling 24h; 0 = unlimited.
    #[serde(default)]
    pub daily_token_budget: u64,
    /// Exact names or `*` globs; empty = may emit nothing.
    #[serde(default)]
    pub topics_out: Vec<String>,
    #[serde(default)]
    pub store: StoreAccess,
    /// 0 = unlimited. Over it, runs are dropped and logged, not queued behind a slow model.
    #[serde(default = "default_rate")]
    pub max_runs_per_min: u32,
    #[serde(default)]
    pub paused: bool,
    #[serde(default, skip_serializing_if = "SessionPolicy::is_default")]
    pub sessions: SessionPolicy,
    /// From the project registry at run time (`projects::effective`).
    #[serde(skip)]
    pub projects: Vec<crate::projects::ProjectInfo>,
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
            api_keys: Vec::new(),
            triggers: Vec::new(),
            model: ModelSpec::Local,
            max_steps: default_steps(),
            max_tokens: default_tokens(),
            max_result_chars: default_result_chars(),
            must_call: Vec::new(),
            recent_runs: default_recent_runs(),
            topics_out: Vec::new(),
            store: StoreAccess::default(),
            max_runs_per_min: default_rate(),
            daily_token_budget: 0,
            paused: false,
            sessions: SessionPolicy::default(),
            projects: Vec::new(),
        }
    }

    pub fn has_capability(&self, name: &str) -> bool {
        self.capabilities.iter().any(|c| c.name == name)
    }
}

/// `*` matches any run of characters (including none); everything else is literal.
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

/// Must be safe as a WIT kebab name, a file name and a host label.
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
